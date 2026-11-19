//! Text formats read eagerly into text columns, as VisiData reads them (S-42 B):
//! fixed width, `.usv`, `.lsv`, `.jsonla`; YAML and TOML as JSON records (S-42 D).

use std::path::Path;

use polars::prelude::*;

use super::{OpenError, header_names, unique_headers};

/// The file as lines without their `\n` / `\r\n`.
fn lines(path: &Path) -> Result<Vec<String>, OpenError> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes)
        .lines()
        .map(|l| l.trim_end_matches('\r').to_string())
        .collect())
}

/// Text columns named `headers`, one value per row (short rows: empty).
fn text_frame(headers: Vec<String>, rows: &[Vec<String>]) -> Result<DataFrame, OpenError> {
    let cols: Vec<Column> = unique_headers(headers.into_iter())
        .into_iter()
        .enumerate()
        .map(|(i, name)| {
            let v: Vec<&str> = rows
                .iter()
                .map(|r| r.get(i).map_or("", String::as_str))
                .collect();
            Column::new(name.into(), v)
        })
        .collect();
    DataFrame::new_infer_height(cols).map_err(OpenError::from)
}

/// Rows VisiData looks at to find the columns (`fixed_rows`).
const FIXED_ROWS: usize = 1000;

/// `[start, end)` char spans of fixed-width columns: VisiData's `columnize`
/// (#2265, #2255, #3029). With a header row its text starts the columns;
/// without, columns start where every row has a space before text.
fn columnize(rows: &[Vec<char>], has_header: bool) -> Vec<(usize, usize)> {
    if rows.is_empty() {
        return Vec::new();
    }
    let detect = if has_header { &rows[..1] } else { rows };
    let space = |r: &[char], i: usize| r.get(i).is_none_or(|c| c.is_whitespace());
    let all_space = |i: usize| detect.iter().all(|r| space(r, i));
    let width = detect.iter().map(Vec::len).max().unwrap_or(0);
    let mut starts: Vec<usize> = (0..width)
        .filter(|&i| !all_space(i) && (i == 0 || all_space(i - 1)))
        .collect();
    if starts.is_empty() {
        return Vec::new();
    }
    let filled: std::collections::BTreeSet<usize> = rows
        .iter()
        .flat_map(|r| r.iter().enumerate().filter(|(_, c)| !c.is_whitespace()))
        .map(|(i, _)| i)
        .collect();
    // right-justified headers: data reaching left of a header moves its start
    if has_header && starts.len() > 1 {
        if (0..starts[0]).any(|p| filled.contains(&p)) {
            starts[0] = 0;
        }
        for k in 1..starts.len() {
            let (mut run_end, mut in_run) = (None, false);
            for p in starts[k - 1]..starts[k] {
                if !filled.contains(&p) {
                    in_run = true;
                } else if in_run {
                    run_end = Some(p);
                    in_run = false;
                }
            }
            if in_run {
                run_end = Some(starts[k]);
            }
            if let Some(e) = run_end {
                starts[k] = e;
            }
        }
    }
    starts
        .iter()
        .enumerate()
        .map(|(k, &s)| {
            let next = starts.get(k + 1).copied().unwrap_or(usize::MAX);
            let end = filled.range(s..next).next_back().map_or(s, |&p| p + 1);
            (s, end)
        })
        .collect()
}

/// Fixed-width text (`--format fixed`, `.fixed`): columns where the header
/// row has text; values keep their padding, as VisiData's. `header` rows
/// name the columns (S-43 A).
pub(super) fn fixed(path: &Path, header: usize) -> Result<DataFrame, OpenError> {
    let rows: Vec<Vec<char>> = lines(path)?.iter().map(|l| l.chars().collect()).collect();
    let spans = columnize(&rows[..rows.len().min(FIXED_ROWS)], header > 0);
    let cut = |r: &[char], (s, e): (usize, usize)| -> String {
        r[s.min(r.len())..e.min(r.len())].iter().collect()
    };
    let cells: Vec<Vec<String>> = rows
        .iter()
        .map(|r| spans.iter().map(|&sp| cut(r, sp)).collect())
        .collect();
    let (head, body) = cells.split_at(header.min(cells.len()));
    text_frame(header_names(head, spans.len()), body)
}

/// Unicode-separated values (`.usv`): `␟` between fields, `␞` after each
/// row; the first row is the header.
pub(super) fn usv(path: &Path, header: usize) -> Result<DataFrame, OpenError> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let rows: Vec<Vec<String>> = text
        .split('\u{241e}')
        .map(|r| r.trim_matches(['\r', '\n']))
        .filter(|r| !r.is_empty())
        .map(|r| r.split('\u{241f}').map(String::from).collect::<Vec<_>>())
        .collect();
    let width = rows.iter().map(Vec::len).max().unwrap_or(0);
    let (head, body) = rows.split_at(header.min(rows.len()));
    text_frame(header_names(head, width), body)
}

/// Line-separated values (`.lsv`): `key: value` lines, a blank line between
/// records; a line without `:` continues the previous key. Columns in order
/// of first appearance.
pub(super) fn lsv(path: &Path) -> Result<DataFrame, OpenError> {
    let mut keys: Vec<String> = Vec::new();
    let mut records: Vec<Vec<(String, String)>> = Vec::new();
    let mut rec: Vec<(String, String)> = Vec::new();
    for line in lines(path)? {
        let line = line.trim();
        if line.is_empty() {
            if !rec.is_empty() {
                records.push(std::mem::take(&mut rec));
            }
            continue;
        }
        let (k, v) = match line.split_once(':') {
            Some((k, v)) => (k.trim().to_string(), v.trim()),
            None => (rec.last().map(|(k, _)| k.clone()).unwrap_or_default(), line),
        };
        if !keys.contains(&k) {
            keys.push(k.clone());
        }
        match rec.iter_mut().find(|(n, _)| *n == k) {
            Some((_, old)) => old.push_str(v),
            None => rec.push((k, v.to_string())),
        }
    }
    if !rec.is_empty() {
        records.push(rec);
    }
    let rows: Vec<Vec<String>> = records
        .iter()
        .map(|r| {
            keys.iter()
                .map(|k| {
                    r.iter()
                        .find(|(n, _)| n == k)
                        .map(|(_, v)| v.clone())
                        .unwrap_or_default()
                })
                .collect()
        })
        .collect();
    text_frame(keys, &rows)
}

/// JSON lines of arrays (`.jsonla`): the first is the header, the rest rows;
/// values keep their JSON types.
pub(super) fn jsonla(path: &Path) -> Result<DataFrame, OpenError> {
    use serde_json::Value;
    let bad = |e: serde_json::Error| OpenError::Polars(polars_err!(ComputeError: "jsonla: {e}"));
    let mut it = lines(path)?.into_iter().filter(|l| !l.trim().is_empty());
    let Some(head) = it.next() else {
        return Ok(DataFrame::empty());
    };
    let headers: Vec<String> = serde_json::from_str(&head).map_err(bad)?;
    let headers = unique_headers(headers.into_iter());
    let mut objects = Vec::new();
    for l in it {
        let row: Vec<Value> = serde_json::from_str(&l).map_err(bad)?;
        let obj: serde_json::Map<String, Value> = headers.iter().cloned().zip(row).collect();
        objects.push(Value::Object(obj));
    }
    let json = serde_json::to_vec(&Value::Array(objects)).map_err(bad)?;
    let df = JsonReader::new(std::io::Cursor::new(json))
        .with_json_format(JsonFormat::Json)
        .finish()?;
    // columns in header order, every header even when no row has it
    let cols: Vec<Column> = headers
        .iter()
        .map(|h| {
            df.column(h)
                .cloned()
                .unwrap_or_else(|_| Column::full_null(h.into(), df.height(), &DataType::String))
        })
        .collect();
    DataFrame::new_infer_height(cols).map_err(OpenError::from)
}

/// Rows of JSON objects as a frame, columns typed from the values (nested
/// values become lists and structs), as the JSON reader types them.
pub(crate) fn records(rows: Vec<serde_json::Value>) -> Result<DataFrame, OpenError> {
    let json = serde_json::to_vec(&serde_json::Value::Array(rows))
        .map_err(|e| OpenError::Polars(polars_err!(ComputeError: "{e}")))?;
    if json.len() <= 2 {
        return Ok(DataFrame::empty());
    }
    Ok(JsonReader::new(std::io::Cursor::new(json))
        .with_json_format(JsonFormat::Json)
        .finish()?)
}

/// A value that isn't an object as a row: `{"value": v}`.
fn as_row(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(_) => v,
        v => serde_json::json!({ "value": v }),
    }
}

fn yaml_json(y: &yaml_rust2::Yaml) -> serde_json::Value {
    use serde_json::Value as J;
    use yaml_rust2::Yaml as Y;
    match y {
        Y::Real(r) => r
            .parse::<f64>()
            .map_or_else(|_| J::String(r.clone()), J::from),
        Y::Integer(i) => J::from(*i),
        Y::String(s) => J::String(s.clone()),
        Y::Boolean(b) => J::Bool(*b),
        Y::Array(a) => J::Array(a.iter().map(yaml_json).collect()),
        Y::Hash(h) => J::Object(
            h.iter()
                .map(|(k, v)| {
                    let k = match k {
                        Y::String(s) => s.clone(),
                        k => yaml_json(k).to_string(),
                    };
                    (k, yaml_json(v))
                })
                .collect(),
        ),
        Y::Null | Y::BadValue | Y::Alias(_) => J::Null,
    }
}

/// YAML (`.yaml`, `.yml`), as VisiData: one document that is a list → a row
/// per item; one other document → one row; several documents → a row each.
pub(super) fn yaml(path: &Path) -> Result<DataFrame, OpenError> {
    let text = std::fs::read_to_string(path)?;
    let docs = yaml_rust2::YamlLoader::load_from_str(&text)
        .map_err(|e| OpenError::Polars(polars_err!(ComputeError: "yaml: {e}")))?;
    let rows: Vec<serde_json::Value> = match docs.as_slice() {
        [yaml_rust2::Yaml::Array(items)] => items.iter().map(yaml_json).collect(),
        docs => docs.iter().map(yaml_json).collect(),
    };
    records(rows.into_iter().map(as_row).collect())
}

fn toml_json(v: toml::Value) -> serde_json::Value {
    use serde_json::Value as J;
    match v {
        toml::Value::String(s) => J::String(s),
        toml::Value::Integer(i) => J::from(i),
        toml::Value::Float(f) => J::from(f),
        toml::Value::Boolean(b) => J::Bool(b),
        toml::Value::Datetime(d) => J::String(d.to_string()),
        toml::Value::Array(a) => J::Array(a.into_iter().map(toml_json).collect()),
        toml::Value::Table(t) => J::Object(t.into_iter().map(|(k, v)| (k, toml_json(v))).collect()),
    }
}

/// MessagePack (`.msgpack`), as JSON: a list → a row per item, another value
/// one row (VisiData's `MsgpackSheet`). `.msgpackz` is brotli-compressed.
pub(super) fn msgpack(path: &Path) -> Result<DataFrame, OpenError> {
    use std::io::Read;
    let mut data = std::fs::read(path)?;
    let z = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("msgpackz"));
    if z {
        let mut out = Vec::new();
        brotli::Decompressor::new(data.as_slice(), 4096).read_to_end(&mut out)?;
        data = out;
    }
    let v: serde_json::Value = rmp_serde::from_slice(&data)
        .map_err(|e| OpenError::Polars(polars_err!(ComputeError: "msgpack: {e}")))?;
    let rows = match v {
        serde_json::Value::Array(items) => items,
        v => vec![v],
    };
    records(rows.into_iter().map(as_row).collect())
}

/// TOML (`.toml`): one row, a column per top-level key, as VisiData.
pub(super) fn toml(path: &Path) -> Result<DataFrame, OpenError> {
    let text = std::fs::read_to_string(path)?;
    let table: toml::Table = text
        .parse()
        .map_err(|e| OpenError::Polars(polars_err!(ComputeError: "toml: {e}")))?;
    records(vec![toml_json(toml::Value::Table(table))])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str) -> Vec<(usize, usize)> {
        let rows: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
        columnize(&rows, true)
    }

    #[test]
    fn columnize_follows_the_header_and_widens_to_the_data() {
        assert_eq!(spans("id  name\n1   ann\n22  bobby"), [(0, 2), (4, 9)]);
        // a right-justified header: data left of it moves the start
        assert_eq!(spans("  n x\n100 y"), [(0, 3), (4, 5)]);
    }

    #[test]
    fn msgpack_reads_a_list_of_records_plain_or_brotli() {
        let v = serde_json::json!([{"id": 1, "geo": {"xy": [1.5, 2.0]}}, {"id": 2, "geo": null}]);
        let data = rmp_serde::to_vec_named(&v).unwrap();
        let mut z = Vec::new();
        brotli::BrotliCompress(&mut data.as_slice(), &mut z, &Default::default()).unwrap();
        for (ext, bytes) in [(".msgpack", data), (".msgpackz", z)] {
            let f = tempfile::Builder::new().suffix(ext).tempfile().unwrap();
            std::fs::write(f.path(), bytes).unwrap();
            let df = msgpack(f.path()).unwrap();
            assert_eq!(df.get_column_names(), ["id", "geo"], "{ext}");
            assert_eq!(df.height(), 2, "{ext}");
        }
    }
}
