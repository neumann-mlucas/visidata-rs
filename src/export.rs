//! `:export path.py|path.sql` — the current view as a script that rebuilds it
//! from the source file: Python (Polars) or SQL (DuckDB dialect).
//!
//! Replays, in `Sheet::view_plan` order: plan steps (rename, currency, derive,
//! setcol) → type casts → filter → sort → visible columns. Only sheets opened
//! from a file can be exported; derived sheets (freq, pivot, join, …) aren't
//! reconstructed.

use std::path::Path;

use polars::prelude::DataType;

use crate::reader::SourceKind;
use crate::sheet::{Origin, PlanStep, Sheet, SortOrder};

/// Script for `sheet`, language picked by `path`'s extension.
pub fn script_for(sheet: &Sheet, path: &Path) -> Result<String, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "py" => to_python(sheet),
        "sql" => to_sql(sheet),
        _ => Err(format!(
            "export: use a .py or .sql path, not {:?}",
            path.display().to_string()
        )),
    }
}

fn source(sheet: &Sheet) -> Result<(&Path, SourceKind), String> {
    if !matches!(sheet.origin, Origin::File | Origin::Tail { .. }) {
        return Err("export: only sheets opened from a file can be exported".into());
    }
    let path = sheet
        .source
        .path
        .as_deref()
        .ok_or("export: sheet has no source file (stdin?)")?;
    Ok((path, sheet.kind))
}

/// Python string literal.
fn py_str(s: &str) -> String {
    format!("{s:?}") // Rust's escaping is valid Python for these strings
}

fn py_dtype(dt: &DataType) -> &'static str {
    match dt {
        DataType::Int64 => "pl.Int64",
        DataType::Float64 => "pl.Float64",
        DataType::Date => "pl.Date",
        _ => "pl.Utf8",
    }
}

pub fn to_python(sheet: &Sheet) -> Result<String, String> {
    let (path, kind) = source(sheet)?;
    let p = py_str(&path.display().to_string());
    let gz = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("gz"));
    let scan = match (kind, gz) {
        (SourceKind::Csv, false) => format!("pl.scan_csv({p})"),
        (SourceKind::Csv, true) => format!("pl.read_csv({p}).lazy()"),
        (SourceKind::Tsv, false) => format!("pl.scan_csv({p}, separator=\"\\t\")"),
        (SourceKind::Tsv, true) => format!("pl.read_csv({p}, separator=\"\\t\").lazy()"),
        (SourceKind::Parquet, _) => format!("pl.scan_parquet({p})"),
        (SourceKind::Arrow, _) => format!("pl.scan_ipc({p})"),
        (SourceKind::Jsonl, _) => format!("pl.scan_ndjson({p})"),
        (SourceKind::Json, _) => format!("pl.read_json({p}).lazy()"),
        (SourceKind::Xlsx, _) => format!("pl.read_excel({p}).lazy()"),
        (SourceKind::Sqlite, _) | (SourceKind::Stdin, _) => {
            return Err(format!("export: {kind:?} sources aren't supported in .py"));
        }
    };
    let mut out = vec![
        "import polars as pl".to_string(),
        String::new(),
        format!("lf = {scan}"),
    ];
    for step in &sheet.state.steps {
        out.push(match step {
            PlanStep::Rename { old, new } => format!("lf = lf.rename({{{}: {}}})", py_str(old), py_str(new)),
            PlanStep::Currency { column } => format!(
                "lf = lf.with_columns(pl.col({c}).cast(pl.Utf8).str.replace_all(r\"[^0-9.\\-]\", \"\").cast(pl.Float64))",
                c = py_str(column)
            ),
            PlanStep::Derive { name, expr } => format!(
                "lf = lf.with_columns(pl.sql_expr({}).alias({}))",
                py_str(expr),
                py_str(name)
            ),
            PlanStep::SetCol { column, value, rids, .. } => format!(
                "lf = lf.with_row_index(\"__rid\").with_columns(pl.when(pl.col(\"__rid\").is_in({rids:?}))\
                 .then(pl.lit({v}).cast(lf.collect_schema()[{c}])).otherwise(pl.col({c})).alias({c})).drop(\"__rid\")",
                v = py_str(value),
                c = py_str(column)
            ),
        });
    }
    let v = &sheet.view;
    for (c, dt) in &v.type_casts {
        out.push(format!(
            "lf = lf.with_columns(pl.col({c}).cast({}, strict=False))",
            py_dtype(dt),
            c = py_str(c)
        ));
    }
    if let Some(f) = &v.filter {
        out.push(format!(
            "lf = lf.filter(pl.col({}).cast(pl.Utf8).str.contains({}))",
            py_str(&f.column),
            py_str(&f.regex)
        ));
    }
    if !v.sort.is_empty() {
        let by: Vec<String> = v.sort.iter().map(|k| py_str(&k.column)).collect();
        let desc: Vec<&str> = v
            .sort
            .iter()
            .map(|k| {
                if k.order == SortOrder::Desc {
                    "True"
                } else {
                    "False"
                }
            })
            .collect();
        out.push(format!(
            "lf = lf.sort([{}], descending=[{}], maintain_order=True)",
            by.join(", "),
            desc.join(", ")
        ));
    }
    if let Some(cols) = visible_columns(sheet)? {
        let cols: Vec<String> = cols.iter().map(|c| py_str(c)).collect();
        out.push(format!("lf = lf.select([{}])", cols.join(", ")));
    }
    out.push(String::new());
    out.push("print(lf.collect())".into());
    Ok(out.join("\n") + "\n")
}

/// The projection `view_plan` applies, or None when it's "all columns, as is".
fn visible_columns(sheet: &Sheet) -> Result<Option<Vec<String>>, String> {
    let v = &sheet.view;
    if v.hidden.is_empty() && v.keys.is_empty() {
        return Ok(None);
    }
    let schema = sheet
        .view_plan()
        .collect_schema()
        .map_err(|e| e.to_string())?;
    Ok(Some(schema.iter_names().map(|n| n.to_string()).collect()))
}

/// SQL identifier.
fn q_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// SQL string literal.
fn q_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn sql_dtype(dt: &DataType) -> &'static str {
    match dt {
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => "BIGINT",
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => "UBIGINT",
        DataType::Float32 | DataType::Float64 => "DOUBLE",
        DataType::Boolean => "BOOLEAN",
        DataType::Date => "DATE",
        DataType::Datetime(..) => "TIMESTAMP",
        _ => "VARCHAR",
    }
}

pub fn to_sql(sheet: &Sheet) -> Result<String, String> {
    let (path, kind) = source(sheet)?;
    let p = q_str(&path.display().to_string());
    let mut q = match kind {
        SourceKind::Csv => format!("SELECT * FROM read_csv_auto({p})"),
        SourceKind::Tsv => format!("SELECT * FROM read_csv_auto({p}, delim = '\\t')"),
        SourceKind::Parquet => format!("SELECT * FROM read_parquet({p})"),
        SourceKind::Jsonl | SourceKind::Json => format!("SELECT * FROM read_json_auto({p})"),
        SourceKind::Sqlite => {
            format!("SELECT * FROM sqlite_scan({p}, (SELECT name FROM sqlite_master LIMIT 1))")
        }
        SourceKind::Arrow | SourceKind::Xlsx | SourceKind::Stdin => {
            return Err(format!("export: {kind:?} sources aren't supported in .sql"));
        }
    };
    // Each plan step wraps the query so far (DuckDB `* RENAME` / `* REPLACE`).
    for step in &sheet.state.steps {
        q = match step {
            PlanStep::Rename { old, new } => format!(
                "SELECT * RENAME ({} AS {}) FROM ({q})",
                q_ident(old),
                q_ident(new)
            ),
            PlanStep::Currency { column } => format!(
                "SELECT * REPLACE (TRY_CAST(regexp_replace(CAST({c} AS VARCHAR), '[^0-9.\\-]', '', 'g') AS DOUBLE) AS {c}) FROM ({q})",
                c = q_ident(column)
            ),
            PlanStep::Derive { name, expr } => {
                format!("SELECT *, ({expr}) AS {} FROM ({q})", q_ident(name))
            }
            PlanStep::SetCol {
                column,
                value,
                dtype,
                rids,
            } => {
                let ids: Vec<String> = rids.iter().map(u64::to_string).collect();
                format!(
                    "SELECT * EXCLUDE (__rid) REPLACE (CASE WHEN __rid IN ({}) THEN TRY_CAST({} AS {}) ELSE {c} END AS {c}) \
                     FROM (SELECT *, row_number() OVER () - 1 AS __rid FROM ({q}))",
                    ids.join(", "),
                    q_str(value),
                    sql_dtype(dtype),
                    c = q_ident(column)
                )
            }
        };
    }
    let v = &sheet.view;
    if !v.type_casts.is_empty() {
        let casts: Vec<String> = v
            .type_casts
            .iter()
            .map(|(c, dt)| format!("TRY_CAST({c} AS {}) AS {c}", sql_dtype(dt), c = q_ident(c)))
            .collect();
        q = format!("SELECT * REPLACE ({}) FROM ({q})", casts.join(", "));
    }
    let cols = match visible_columns(sheet)? {
        Some(cols) => cols
            .iter()
            .map(|c| q_ident(c))
            .collect::<Vec<_>>()
            .join(", "),
        None => "*".into(),
    };
    let mut sql = format!("SELECT {cols}\nFROM ({q})");
    if let Some(f) = &v.filter {
        sql += &format!(
            "\nWHERE regexp_matches(CAST({} AS VARCHAR), {})",
            q_ident(&f.column),
            q_str(&f.regex)
        );
    }
    if !v.sort.is_empty() {
        let by: Vec<String> = v
            .sort
            .iter()
            .map(|k| {
                format!(
                    "{} {}",
                    q_ident(&k.column),
                    if k.order == SortOrder::Desc {
                        "DESC"
                    } else {
                        "ASC"
                    }
                )
            })
            .collect();
        sql += &format!("\nORDER BY {}", by.join(", "));
    }
    Ok(sql + ";\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{FilterState, SortKey};
    use std::io::Write;

    fn sheet() -> (Sheet, tempfile::NamedTempFile) {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        writeln!(tmp, "name,n,price\nalice,3,$1.50\nbob,1,$2").unwrap();
        tmp.flush().unwrap();
        let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
        let mut s = Sheet::from_source(0, loaded);
        s.plan = s.plan.clone().rename(["n"], ["qty"], true);
        s.state.steps.push(PlanStep::Rename {
            old: "n".into(),
            new: "qty".into(),
        });
        s.derive("double", "qty * 2").unwrap();
        s.view.filter = Some(FilterState {
            column: "name".into(),
            regex: "^a".into(),
        });
        s.view.sort.push(SortKey {
            column: "qty".into(),
            order: SortOrder::Desc,
        });
        s.view.hidden.insert("price".into());
        (s, tmp)
    }

    #[test]
    fn python_script_replays_view() {
        let (s, _t) = sheet();
        let py = to_python(&s).unwrap();
        for want in [
            "pl.scan_csv(",
            "lf.rename({\"n\": \"qty\"})",
            "pl.sql_expr(\"qty * 2\").alias(\"double\")",
            ".str.contains(\"^a\")",
            "descending=[True]",
            "lf.select([\"name\", \"qty\", \"double\"])",
        ] {
            assert!(py.contains(want), "missing {want:?} in\n{py}");
        }
    }

    #[test]
    fn sql_script_replays_view() {
        let (s, _t) = sheet();
        let sql = to_sql(&s).unwrap();
        for want in [
            "read_csv_auto(",
            "RENAME (\"n\" AS \"qty\")",
            "(qty * 2) AS \"double\"",
            "SELECT \"name\", \"qty\", \"double\"",
            "regexp_matches(CAST(\"name\" AS VARCHAR), '^a')",
            "ORDER BY \"qty\" DESC",
        ] {
            assert!(sql.contains(want), "missing {want:?} in\n{sql}");
        }
    }

    #[test]
    fn derived_sheets_and_bad_extension_refuse() {
        let (s, _t) = sheet();
        let freq = Sheet::freq_from(&s, 1, "name");
        assert!(to_python(&freq).is_err());
        assert!(script_for(&s, Path::new("x.txt")).is_err());
    }
}
