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
