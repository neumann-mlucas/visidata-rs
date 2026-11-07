//! Golden cases: `tests/golden/<case>/` holds input files plus
//!
//! - `script`: one step per line. `> text` types `text` and Enter into the open
//!   prompt, `>> text` types it without Enter; anything else is space-separated
//!   keys. `{dir}` = the case's folder.
//! - `expected.tsv`: the final view, every row.
//! - `expected.screen`: the final screen.
//! - `no-export` (optional): why `:export` can't reproduce this case.
//!
//! The sheet opens `input.csv`/`input.jsonl`. `UPDATE_GOLDEN=1` rewrites the
//! expected files; read the diff before committing it.
//!
//! `docs-*` cases open a file from `docs/fixtures` (named in `open`, default
//! `sales.csv`) on a 100x10 screen, as `vdrs <file>` run in that folder;
//! `{fixtures}/` in a script is that folder. Each case renders, as HTML, the
//! block between `<!-- golden:<case> -->` and `<!-- /golden -->` in
//! `docs/index.html`, so the guide shows real screens. A script with
//! `# caption` lines is a recording: one frame per caption, showing the screen
//! after that step's lines, with the keys they type. `expected.screen` then
//! holds every frame, split by `--- frame` lines; `tests/pty.rs` types each
//! step into the real binary and waits for its frame, so a step may start at
//! most one background job (sort, freq, join…), as its last key.
//!
//! Each case's exported scripts then run through DuckDB and Polars
//! (`VDRS_PYTHON` = an interpreter with polars) and must print the same table.
//! Missing tools skip that step, unless `VDRS_REQUIRE_ROUNDTRIP` is set (CI).

use std::path::{Path, PathBuf};
use std::process::Command;

use super::scenario::{Harness, csv_text};
use super::*;
use unicode_width::UnicodeWidthStr;

const ROOT: &str = "tests/golden";

const GUIDE: &str = "docs/index.html";

fn is_docs(dir: &Path) -> bool {
    dir.file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("docs-")
}

/// Docs screens: what `tests/pty.rs` gives the real binary.
pub const DOCS_SIZE: (u16, u16) = (100, 10);
const FIXTURES: &str = "docs/fixtures";

/// One recorded step: caption, the keys typed (HTML), the screen after.
struct Frame {
    caption: String,
    keys: String,
    screen: String,
    text: String,
}

async fn play(dir: &Path) -> (Harness, Vec<Frame>) {
    let docs = is_docs(dir);
    let input = if docs {
        let name = std::fs::read_to_string(dir.join("open")).unwrap_or("sales.csv".into());
        PathBuf::from(FIXTURES).join(name.trim())
    } else {
        ["input.csv", "input.jsonl"]
            .iter()
            .map(|f| dir.join(f))
            .find(|p| p.exists())
            .expect("case has an input file")
    };
    let mut loaded = crate::reader::open(input.as_os_str(), None).unwrap();
    if docs {
        // as if run from docs/fixtures: `vdrs sales.csv`
        loaded.display_name = input.file_name().unwrap().to_string_lossy().into();
    }
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let mut h = Harness::with_source(loaded, tmp).await;
    if docs {
        h.term.backend_mut().resize(DOCS_SIZE.0, DOCS_SIZE.1);
        h.pump().await;
    }
    let mut frames: Vec<Frame> = Vec::new();
    let script = std::fs::read_to_string(dir.join("script")).unwrap_or_default();
    for line in script.lines() {
        if let Some(caption) = line.strip_prefix("# ") {
            if let Some(f) = frames.last_mut() {
                (f.screen, f.text) = (screen_html(&h), h.screen());
            }
            frames.push(Frame {
                caption: caption.to_string(), // authored HTML
                keys: String::new(),
                screen: String::new(),
                text: String::new(),
            });
            continue;
        }
        if let Some(f) = frames.last_mut() {
            let shown = line.replace("{fixtures}/", "");
            f.keys.push_str(&keys_html(&shown));
        }
        let line = line
            .replace("{dir}", &dir.display().to_string())
            .replace("{fixtures}/", &format!("{FIXTURES}/"));
        if let Some(text) = line.strip_prefix(">> ") {
            h.type_no_enter(text).await;
        } else if let Some(text) = line.strip_prefix("> ") {
            h.type_(text).await;
        } else {
            h.keys(&line).await;
        }
    }
    if let Some(f) = frames.last_mut() {
        (f.screen, f.text) = (screen_html(&h), h.screen());
    }
    (h, frames)
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// A script line as the reader types it: `<kbd>` keys, `<code>` prompt text.
fn keys_html(line: &str) -> String {
    let enter = "<kbd>Enter</kbd>";
    if let Some(text) = line.strip_prefix(">> ") {
        return format!("<code>{}</code> ", esc(text));
    }
    if let Some(text) = line.strip_prefix("> ") {
        return format!("<code>{}</code> {enter} ", esc(text));
    }
    line.split_whitespace()
        .map(|k| match k {
            "<CR>" => enter.to_string(),
            "<Esc>" => "<kbd>Esc</kbd>".into(),
            k => format!("<kbd>{}</kbd>", esc(k)),
        })
        .map(|k| k + " ")
        .collect()
}

/// A recording: every frame stays in the page (readable without JS); the
/// guide's script turns it into a step player.
fn recording_html(frames: &[Frame]) -> String {
    let mut out = String::from("<figure class=\"rec\">\n<ol>\n");
    for (i, f) in frames.iter().enumerate() {
        out.push_str(&format!(
            "<li><p class=\"cap\"><span class=\"no\">{}</span><span>{}</span><span class=\"keys\">{}</span></p>\n{}</li>\n",
            i + 1,
            f.caption,
            f.keys.trim_end(),
            f.screen
        ));
    }
    out + "</ol>\n</figure>"
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

/// The screen as the guide's `<pre class="screen">`: one `.l` span per line,
/// runs of equal style as inline-styled spans.
fn screen_html(h: &Harness) -> String {
    use ratatui::style::{Color, Modifier};
    let hex = |c: Color| match c {
        Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        _ => None,
    };
    let buf = h.term.backend().buffer();
    let mut out =
        String::from(r#"<pre class="screen" role="img" aria-label="vdrs terminal screen">"#);
    for y in 0..buf.area.height {
        let mut runs: Vec<(String, String)> = Vec::new();
        let mut x = 0;
        while x < buf.area.width {
            let cell = &buf[(x, y)];
            let mut css = Vec::new();
            if let Some(c) = hex(cell.bg) {
                css.push(format!("background:{c}"));
            }
            if let Some(c) = hex(cell.fg) {
                css.push(format!("color:{c}"));
            }
            if cell.modifier.contains(Modifier::BOLD) {
                css.push("font-weight:700".into());
            }
            let css = css.join(";");
            let sym = cell.symbol();
            let text = esc(sym);
            match runs.last_mut() {
                Some((c, t)) if *c == css => t.push_str(&text),
                _ => runs.push((css, text)),
            }
            x += sym.width().max(1) as u16;
        }
        out.push_str(r#"<span class="l">"#);
        for (css, text) in runs {
            if css.is_empty() {
                out.push_str(&text);
            } else {
                out.push_str(&format!(r#"<span style="{css}">{text}</span>"#));
            }
        }
        out.push_str("</span>");
    }
    out + "</pre>"
}

/// `docs/index.html` with every `<!-- golden:<case> -->` block re-rendered.
fn render_guide(guide: &str, html: &std::collections::BTreeMap<String, String>) -> String {
    let mut out = String::new();
    let mut rest = guide;
    let mut used = std::collections::BTreeSet::new();
    while let Some(i) = rest.find("<!-- golden:") {
        let (head, tail) = rest.split_at(i);
        out.push_str(head);
        let end = tail.find(" -->").expect("golden marker closes");
        let case = &tail["<!-- golden:".len()..end];
        let close = tail.find("<!-- /golden -->").expect("golden block closes");
        let screen = html
            .get(case)
            .unwrap_or_else(|| panic!("{GUIDE}: no golden case {case}"));
        used.insert(case.to_string());
        out.push_str(&format!("<!-- golden:{case} -->\n{screen}\n"));
        rest = &tail[close..];
    }
    out.push_str(rest);
    let unused: Vec<_> = html.keys().filter(|k| !used.contains(*k)).collect();
    assert!(unused.is_empty(), "{GUIDE} shows no screen for {unused:?}");
    out
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
    let mut html = std::collections::BTreeMap::new();
    for dir in &cases {
        let (mut h, frames) = play(dir).await;
        if is_docs(dir) {
            let name = dir.file_name().unwrap().to_string_lossy().into_owned();
            let block = if frames.is_empty() {
                screen_html(&h)
            } else {
                recording_html(&frames)
            };
            html.insert(name, block);
        }
        let tsv = csv_text(h.app.sheets.active(), b'\t');
        check(dir.join("expected.tsv"), &tsv, &mut fails);
        let screens = if frames.is_empty() {
            h.screen()
        } else {
            let texts: Vec<_> = frames.iter().map(|f| f.text.as_str()).collect();
            texts.join("\n--- frame\n")
        };
        check(dir.join("expected.screen"), &(screens + "\n"), &mut fails);
        if !dir.join("no-export").exists() {
            let csv = csv_text(h.app.sheets.active(), b',');
            roundtrip(&h, dir, &csv, &mut fails);
        }
        h.app.quit = true;
    }
    let guide = std::fs::read_to_string(GUIDE).unwrap();
    let rendered = render_guide(&guide, &html);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(GUIDE, rendered).unwrap();
    } else if rendered != guide {
        fails.push(format!(
            "{GUIDE}: screens are stale; rerun with UPDATE_GOLDEN=1"
        ));
    }
    assert!(fails.is_empty(), "{}", fails.join("\n\n"));
}
