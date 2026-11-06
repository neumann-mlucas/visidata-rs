//! Golden cases: `tests/golden/<case>/` holds input files plus
//!
//! - `script`: one step per line. `> text` types `text` and Enter into the open
//!   prompt; anything else is space-separated keys. `{dir}` = the case's folder.
//! - `expected.tsv`: the final view, every row.
//! - `expected.screen`: the final screen.
//! - `no-export` (optional): why `:export` can't reproduce this case.
//!
//! The sheet opens `input.csv`/`input.jsonl`. `UPDATE_GOLDEN=1` rewrites the
//! expected files; read the diff before committing it.
//!
//! Each case's exported scripts then run through DuckDB and Polars
//! (`VDRS_PYTHON` = an interpreter with polars) and must print the same table.
//! Missing tools skip that step, unless `VDRS_REQUIRE_ROUNDTRIP` is set (CI).

use std::path::{Path, PathBuf};
use std::process::Command;

use super::scenario::{Harness, csv_text};
use super::*;

const ROOT: &str = "tests/golden";

async fn play(dir: &Path) -> Harness {
    let input = ["input.csv", "input.jsonl"]
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.exists())
        .expect("case has an input file");
    let loaded = crate::reader::open(input.as_os_str(), None).unwrap();
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let mut h = Harness::with_source(loaded, tmp).await;
    let script = std::fs::read_to_string(dir.join("script")).unwrap_or_default();
    for line in script.lines() {
        let line = line.replace("{dir}", &dir.display().to_string());
        match line.strip_prefix("> ") {
            Some(text) => h.type_(text).await,
            None => h.keys(&line).await,
        }
    }
    h
}

/// Compare `got` with the file, or rewrite it under `UPDATE_GOLDEN`.
fn check(path: PathBuf, got: &str, fails: &mut Vec<String>) {
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    if want != got {
        fails.push(format!(
            "{}\n--- want\n{want}\n--- got\n{got}",
            path.display()
        ));
    }
}

/// CSV → sorted rows of normalized cells, header first: scripts may order
/// unsorted rows differently and print `3.0` as `3`.
fn table(csv: &str) -> Vec<String> {
    let norm = |c: &str| {
        let c = c.trim().trim_matches('"');
        match c.parse::<f64>() {
            Ok(f) => format!("{f:.9e}"), // engines differ in the last float digits
            Err(_) if c == "NULL" => String::new(), // DuckDB's CSV null
            Err(_) if c.eq_ignore_ascii_case("true") => "true".into(),
            Err(_) if c.eq_ignore_ascii_case("false") => "false".into(),
            Err(_) => c.to_string(),
        }
    };
    let mut lines = csv
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| l.split(',').map(norm).collect::<Vec<_>>().join(","));
    let header = lines.next().unwrap_or_default();
    let mut rows: Vec<String> = lines.collect();
    rows.sort();
    std::iter::once(header).chain(rows).collect()
}

fn run_script(cmd: &mut Command) -> Result<String, String> {
    let out = cmd.output().map_err(|e| format!("spawn: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

fn duckdb_csv(sql: &str, dir: &Path) -> Result<String, String> {
    let f = dir.join("export.sql");
    std::fs::write(&f, sql).unwrap();
    // `.mode csv` + read: DuckDB prints each statement's result.
    run_script(
        Command::new("duckdb")
            .args(["-csv", "-c"])
            .arg(format!(".read {}", f.display())),
    )
}

fn polars_csv(python: &str, py: &str, dir: &Path) -> Result<String, String> {
    let (body, last) = py.trim_end().rsplit_once('\n').expect("script has lines");
    let v = last
        .strip_prefix("print(")
        .and_then(|l| l.strip_suffix(".collect())"))
        .expect("script ends printing its view");
    let f = dir.join("export.py");
    std::fs::write(
        &f,
        format!("{body}\nprint({v}.collect().write_csv(), end=\"\")\n"),
    )
    .unwrap();
    run_script(Command::new(python).arg(&f))
}

fn tool_ok(cmd: &str, arg: &str) -> bool {
    let ok = Command::new(cmd)
        .arg(arg)
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        ok || std::env::var_os("VDRS_REQUIRE_ROUNDTRIP").is_none(),
        "VDRS_REQUIRE_ROUNDTRIP: {cmd} isn't usable"
    );
    ok
}

/// The exported scripts rebuild the case's final view.
fn roundtrip(h: &Harness, dir: &Path, want: &str, fails: &mut Vec<String>) {
    let sheet = h.app.sheets.active();
    let scratch = tempfile::tempdir().unwrap();
    let want = table(want);
    let mut compare = |lang: &str, got: Result<String, String>| match got {
        Ok(got) if table(&got) == want => {}
        Ok(got) => fails.push(format!(
            "{} {lang}\n--- want\n{}\n--- got\n{}",
            dir.display(),
            want.join("\n"),
            table(&got).join("\n")
        )),
        Err(e) => fails.push(format!("{} {lang}: {e}", dir.display())),
    };
    if tool_ok("duckdb", "--version") {
        let sql = crate::export::to_sql(sheet);
        compare("sql", sql.and_then(|q| duckdb_csv(&q, scratch.path())));
    }
    let python = std::env::var("VDRS_PYTHON").unwrap_or_else(|_| "python3".into());
    let has_polars = Command::new(&python)
        .args(["-c", "import polars"])
        .output()
        .is_ok_and(|o| o.status.success());
    assert!(
        has_polars || std::env::var_os("VDRS_REQUIRE_ROUNDTRIP").is_none(),
        "VDRS_REQUIRE_ROUNDTRIP: {python} lacks polars"
    );
    if has_polars {
        let py = crate::export::to_python(sheet);
        compare(
            "py",
            py.and_then(|p| polars_csv(&python, &p, scratch.path())),
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn golden_cases() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(ROOT)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    cases.sort();
    assert!(!cases.is_empty());
    let mut fails = Vec::new();
    for dir in &cases {
        let mut h = play(dir).await;
        let tsv = csv_text(h.app.sheets.active(), b'\t');
        check(dir.join("expected.tsv"), &tsv, &mut fails);
        check(
            dir.join("expected.screen"),
            &(h.screen() + "\n"),
            &mut fails,
        );
        if !dir.join("no-export").exists() {
            let csv = csv_text(h.app.sheets.active(), b',');
            roundtrip(&h, dir, &csv, &mut fails);
        }
        h.app.quit = true;
    }
    assert!(fails.is_empty(), "{}", fails.join("\n\n"));
}
