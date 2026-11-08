//! `.duckdb` files through the `duckdb` program on PATH (S-25 P7, option B;
//! a linked `vdrs-ext-duckdb` crate is deferred). Data moves as Parquet
//! tempfiles: DuckDB `COPY`s a table out and Polars scans it, or Polars writes
//! the view and DuckDB creates a table from it. `vd.option("duckdb_cmd", …)`
//! names another program.

use std::path::Path;
use std::process::Command;
use std::sync::RwLock;

use polars::prelude::*;
use tempfile::NamedTempFile;

// ponytail: process-wide, set from init.lua at start and on reload; readers
// run on job threads without the Lua VM.
static CMD: RwLock<Option<String>> = RwLock::new(None);

/// `vd.option("duckdb_cmd")`; `None` = `duckdb`.
pub fn set_cmd(cmd: Option<String>) {
    *CMD.write().unwrap() = cmd;
}

fn cmd() -> String {
    CMD.read()
        .unwrap()
        .clone()
        .unwrap_or_else(|| "duckdb".into())
}

/// `"x"` as a DuckDB identifier.
pub fn ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// `'x'` as a DuckDB string literal.
fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Run `sql` in `db`; stdout is DuckDB's `-json` output (one array per query).
fn run(db: &Path, read_only: bool, sql: &str) -> Result<String, String> {
    run_with(&cmd(), db, read_only, sql)
}

fn run_with(prog: &str, db: &Path, read_only: bool, sql: &str) -> Result<String, String> {
    let mut c = Command::new(prog);
    if read_only {
        c.arg("-readonly");
    }
    let out = c
        .args(["-json", "-bail"])
        .arg(db)
        .args(["-c", sql])
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!(
                "{prog} not found: install DuckDB or set vd.option(\"duckdb_cmd\", …) in init.lua"
            ),
            _ => format!("{prog}: {e}"),
        })?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(err.lines().next().unwrap_or("failed").to_string())
    }
}

fn parquet_tmp() -> Result<NamedTempFile, String> {
    tempfile::Builder::new()
        .prefix("vdrs-duckdb")
        .suffix(".parquet")
        .tempfile()
        .map_err(|e| e.to_string())
}

fn copy_out(db: &Path, query: &str) -> Result<NamedTempFile, String> {
    let tmp = parquet_tmp()?;
    let to = lit(&tmp.path().to_string_lossy());
    run(
        db,
        true,
        &format!("COPY ({query}) TO {to} (FORMAT parquet)"),
    )?;
    Ok(tmp)
}

/// VisiData's index sheet: one row per table and view.
pub fn index(db: &Path) -> Result<DataFrame, String> {
    let stem = db.file_stem().unwrap_or_default().to_string_lossy();
    let q = format!(
        "SELECT name, schema_name, table_type, rows, cols, '' AS keys, {} AS source FROM (\
         SELECT table_name AS name, schema_name, 'BASE TABLE' AS table_type, estimated_size AS rows, column_count AS cols \
         FROM duckdb_tables() WHERE NOT internal AND NOT temporary \
         UNION ALL SELECT view_name, schema_name, 'VIEW', 0, column_count \
         FROM duckdb_views() WHERE NOT internal AND NOT temporary) \
         ORDER BY schema_name, name, table_type",
        lit(&stem)
    );
    let tmp = copy_out(db, &q)?;
    ParquetReader::new(std::fs::File::open(tmp.path()).map_err(|e| e.to_string())?)
        .finish()
        .map_err(|e| e.to_string())
}

/// `schema.table` (no dot: `main`) → a scan of its rows; keep the tempfile alive.
pub fn open_table(db: &Path, part: &str) -> Result<(LazyFrame, NamedTempFile), String> {
    // ponytail: first dot splits; a schema name holding a dot can't be opened.
    let (schema, table) = part.split_once('.').unwrap_or(("main", part));
    let tmp = copy_out(db, &format!("FROM {}.{}", ident(schema), ident(table)))?;
    let path = PlRefPath::try_from_path(tmp.path()).map_err(|e| e.to_string())?;
    let lazy =
        LazyFrame::scan_parquet(path, ScanArgsParquet::default()).map_err(|e| e.to_string())?;
    Ok((lazy, tmp))
}

/// `:save x.duckdb`: the view becomes table `table`, replacing one of that
/// name; the file's other tables stay.
pub fn save(df: &mut DataFrame, db: &Path, table: &str) -> Result<(), String> {
    let tmp = parquet_tmp()?;
    ParquetWriter::new(tmp.as_file())
        .finish(df)
        .map_err(|e| e.to_string())?;
    let from = lit(&tmp.path().to_string_lossy());
    run(
        db,
        false,
        &format!("CREATE OR REPLACE TABLE {} AS FROM {from}", ident(table)),
    )
    .map(drop)
}

/// `:exec-sql`: run statements read-write; the last query's rows, if any query
/// printed. ponytail: a query followed by DDL still returns that query's rows.
pub fn exec(db: &Path, sql: &str) -> Result<Option<DataFrame>, String> {
    let out = run(db, false, sql)?;
    let Some(last) = out
        .rfind("\n[")
        .map(|i| &out[i + 1..])
        .or(out.starts_with('[').then_some(&out[..]))
    else {
        return Ok(None);
    };
    if last.trim() == "[]" {
        return Ok(Some(DataFrame::empty()));
    }
    JsonReader::new(std::io::Cursor::new(last.as_bytes()))
        .with_json_format(JsonFormat::Json)
        .finish()
        .map(Some)
        .map_err(|e| e.to_string())
}

/// `.duckdb` / `.ddb`.
pub fn is_db(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("duckdb") || e.eq_ignore_ascii_case("ddb"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn have_duckdb() -> bool {
        Command::new("duckdb")
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    }

    #[test]
    fn save_index_open_exec_round_trip() {
        if !have_duckdb() {
            assert!(
                std::env::var_os("VDRS_REQUIRE_ROUNDTRIP").is_none(),
                "VDRS_REQUIRE_ROUNDTRIP: duckdb isn't usable"
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("t.duckdb");
        let mut df = df!("id" => [2i64, 1], "note" => ["東京", "it's"]).unwrap();
        save(&mut df, &db, "a\"b").unwrap();
        save(&mut df.head(Some(1)), &db, "a\"b").unwrap(); // replaces
        assert_eq!(
            exec(&db, "CREATE SCHEMA o; CREATE VIEW o.v AS SELECT 1 AS x")
                .unwrap()
                .map(|d| d.height()),
            None
        );
        let ix = index(&db).unwrap();
        let names: Vec<_> = ix
            .column("name")
            .unwrap()
            .str()
            .unwrap()
            .iter()
            .flatten()
            .collect();
        assert_eq!(names, ["a\"b", "v"]);
        assert_eq!(
            ix.column("rows")
                .unwrap()
                .get(0)
                .unwrap()
                .try_extract::<i64>()
                .unwrap(),
            1
        );
        let (lf, _keep) = open_table(&db, "main.a\"b").unwrap();
        assert_eq!(lf.collect().unwrap(), df.head(Some(1)));
        let got = exec(
            &db,
            "INSERT INTO \"a\"\"b\" VALUES (3, 'x'); SELECT id FROM \"a\"\"b\" ORDER BY id",
        )
        .unwrap()
        .unwrap();
        assert_eq!(got.column("id").unwrap().len(), 2);
        assert_eq!(
            exec(&db, "SELECT 1 WHERE false").unwrap().unwrap().height(),
            0
        );
        let e = exec(&db, "SELECT * FROM nope").unwrap_err();
        assert!(e.contains("nope"), "{e}");
    }

    #[test]
    fn export_reads_the_table_back() {
        if !have_duckdb() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("x.duckdb");
        save(&mut df!("v" => [3i64, 4]).unwrap(), &db, "t").unwrap();
        let loaded =
            crate::reader::open_part(&db, crate::reader::SourceKind::Duckdb, "main.t").unwrap();
        let sheet = crate::sheet::Sheet::from_source(0, loaded);
        let sql = crate::export::to_sql(&sheet).unwrap();
        let out = Command::new("duckdb")
            .args(["-csv", "-c", &sql])
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), "v\n3\n4\n", "{sql}");
        let py = crate::export::to_python(&sheet).unwrap();
        assert!(
            py.contains("import duckdb") && py.contains("FROM \\\"main\\\".\\\"t\\\""),
            "{py}"
        );
    }

    #[test]
    fn missing_program_is_a_message() {
        let e = run_with(
            "vdrs-no-such-duckdb",
            Path::new("x.duckdb"),
            true,
            "SELECT 1",
        )
        .unwrap_err();
        assert!(e.starts_with("vdrs-no-such-duckdb not found:"), "{e}");
    }
}
