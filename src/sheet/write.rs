//! Writing a DataFrame to disk by file extension.

use super::*;

/// Write a DataFrame to disk, picking the writer by file extension.
/// Supported: csv (default), tsv/tab, parquet/pq, ipc/arrow/feather, jsonl/ndjson, json.
/// An existing file is replaced by rename from a temp file beside it, so a
/// failed save (unsupported column type, full disk) leaves it untouched. Where
/// that's not allowed the data is rendered first and then written in place, so
/// only a failure during that last write can damage the file. A rename gives
/// the file to the saver and detaches hard links.
pub fn write_df_by_extension(df: &mut DataFrame, path: &std::path::Path) -> PolarsResult<()> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    let known = matches!(
        ext.as_deref(),
        None | Some(
            "csv"
                | "tsv"
                | "tab"
                | "parquet"
                | "pq"
                | "ipc"
                | "arrow"
                | "feather"
                | "jsonl"
                | "ndjson"
                | "json"
        )
    );
    if !known {
        let other = ext.unwrap_or_default();
        return Err(PolarsError::ComputeError(
            format!("save: unknown extension '.{other}'").into(),
        ));
    }
    let ext = ext.as_deref();
    let Ok(target) = std::fs::canonicalize(path) else {
        // New file: nothing to protect, and the umask sets its mode. A dangling
        // symlink lands here too: render first, so a failure creates nothing.
        if std::fs::symlink_metadata(path).is_ok() {
            let mut rendered = Vec::new();
            write_df(df, &mut rendered, ext)?;
            return Ok(std::fs::write(path, rendered)?);
        }
        let result = write_df(df, std::fs::File::create(path)?, ext);
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        return result;
    };
    // Same refusal as writing in place: a read-only file stays read-only.
    drop(std::fs::OpenOptions::new().write(true).open(&target)?);
    let dir = target.parent().unwrap_or(std::path::Path::new("."));
    let tmp = tempfile::Builder::new()
        .prefix(".vdrs-save")
        .tempfile_in(dir);
    let tmp = match tmp {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let mut rendered = Vec::new();
            write_df(df, &mut rendered, ext)?;
            std::fs::write(&target, rendered)?;
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    tmp.as_file()
        .set_permissions(std::fs::metadata(&target)?.permissions())?;
    write_df(df, tmp.as_file(), ext)?;
    match tmp.persist(&target) {
        Ok(_) => Ok(()),
        // Sticky directory: we may write the file but not replace it.
        Err(e) if e.error.kind() == std::io::ErrorKind::PermissionDenied => {
            std::fs::copy(e.file.path(), &target)?;
            Ok(())
        }
        Err(e) => Err(e.error.into()),
    }
}

pub(super) fn write_df<W: std::io::Write>(
    df: &mut DataFrame,
    file: W,
    ext: Option<&str>,
) -> PolarsResult<()> {
    if matches!(ext, None | Some("csv" | "tsv" | "tab"))
        && df.columns().iter().any(|c| nested(c.dtype()))
    {
        return write_df(&mut nested_as_json(df)?, file, ext);
    }
    match ext {
        Some("tsv" | "tab") => CsvWriter::new(file).with_separator(b'\t').finish(df),
        Some("parquet" | "pq") => ParquetWriter::new(file).finish(df).map(|_| ()),
        Some("ipc" | "arrow" | "feather") => IpcWriter::new(file).finish(df),
        Some("jsonl" | "ndjson") => JsonWriter::new(file)
            .with_json_format(JsonFormat::JsonLines)
            .finish(df),
        Some("json") => JsonWriter::new(file)
            .with_json_format(JsonFormat::Json)
            .finish(df),
        _ => CsvWriter::new(file).finish(df),
    }
}

fn nested(dt: &DataType) -> bool {
    matches!(
        dt,
        DataType::List(_) | DataType::Array(..) | DataType::Struct(_)
    )
}

/// Text formats get list/struct cells as JSON text, as VisiData's
/// `json.dumps(v, default=str, ensure_ascii=False)` (#2944; S-49).
fn nested_as_json(df: &DataFrame) -> PolarsResult<DataFrame> {
    let cols = df.columns().iter().map(|c| {
        if !nested(c.dtype()) {
            return Ok(c.clone());
        }
        let s = c.as_materialized_series();
        let text: Vec<Option<String>> = s
            .iter()
            .map(|v| (!v.is_null()).then(|| py_json(&v)))
            .collect();
        Ok(Column::new(c.name().clone(), text))
    });
    DataFrame::new(df.height(), cols.collect::<PolarsResult<Vec<_>>>()?)
}

/// Python's `json.dumps` spacing: `[1, 2]`, `{"k": "v"}`.
// ponytail: floats print as Rust's `{:?}` (`1e20`, Python `1e+20`); dates and
// other scalars as their text, quoted (`default=str`).
pub(crate) fn py_json(v: &AnyValue) -> String {
    let q = |t: &str| serde_json::to_string(t).unwrap_or_default();
    let obj = |fields: &[Field], vals: &[AnyValue]| {
        let kv: Vec<String> = fields
            .iter()
            .zip(vals)
            .map(|(f, v)| format!("{}: {}", q(&f.name), py_json(v)))
            .collect();
        format!("{{{}}}", kv.join(", "))
    };
    match v {
        AnyValue::Null => "null".into(),
        AnyValue::Boolean(b) => b.to_string(),
        AnyValue::String(t) => q(t),
        AnyValue::StringOwned(t) => q(t),
        AnyValue::Float32(f) => py_float(*f as f64),
        AnyValue::Float64(f) => py_float(*f),
        AnyValue::List(s) | AnyValue::Array(s, _) => {
            let items: Vec<String> = s.iter().map(|v| py_json(&v)).collect();
            format!("[{}]", items.join(", "))
        }
        AnyValue::Struct(_, _, fields) => obj(fields, &v._iter_struct_av().collect::<Vec<_>>()),
        AnyValue::StructOwned(p) => obj(&p.1, &p.0),
        v if v.is_integer() => v.to_string(),
        v => q(&v.to_string()),
    }
}

fn py_float(f: f64) -> String {
    match f {
        f if f.is_nan() => "NaN".into(),
        f if f.is_infinite() => (if f > 0.0 { "Infinity" } else { "-Infinity" }).into(),
        f => format!("{f:?}"),
    }
}
