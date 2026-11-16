//! Text formats read eagerly into text columns, as VisiData reads them (S-42 B):
//! fixed width, `.usv`, `.lsv`, `.jsonla`.

use std::path::Path;

use polars::prelude::*;

use super::{OpenError, unique_headers};

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
/// with a header row (#2265, #2255, #3029).
fn columnize(rows: &[Vec<char>]) -> Vec<(usize, usize)> {
    let Some(head) = rows.first() else {
        return Vec::new();
    };
    let space = |r: &[char], i: usize| r.get(i).is_none_or(|c| c.is_whitespace());
    let mut starts: Vec<usize> = (0..head.len())
        .filter(|&i| !space(head, i) && (i == 0 || space(head, i - 1)))
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
    if starts.len() > 1 {
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
/// row has text; values keep their padding, as VisiData's.
pub(super) fn fixed(path: &Path) -> Result<DataFrame, OpenError> {
    let rows: Vec<Vec<char>> = lines(path)?.iter().map(|l| l.chars().collect()).collect();
    let spans = columnize(&rows[..rows.len().min(FIXED_ROWS)]);
    let cut = |r: &[char], (s, e): (usize, usize)| -> String {
        r[s.min(r.len())..e.min(r.len())].iter().collect()
    };
    let Some((head, body)) = rows.split_first() else {
        return Ok(DataFrame::empty());
    };
    let headers = spans
        .iter()
        .map(|&sp| cut(head, sp).trim().to_string())
        .collect();
    let body: Vec<Vec<String>> = body
        .iter()
        .map(|r| spans.iter().map(|&sp| cut(r, sp)).collect())
        .collect();
    text_frame(headers, &body)
}

/// Unicode-separated values (`.usv`): `␟` between fields, `␞` after each
/// row; the first row is the header.
pub(super) fn usv(path: &Path) -> Result<DataFrame, OpenError> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let mut rows = text
        .split('\u{241e}')
        .map(|r| r.trim_matches(['\r', '\n']))
        .filter(|r| !r.is_empty())
        .map(|r| r.split('\u{241f}').map(String::from).collect::<Vec<_>>());
    let headers = rows.next().unwrap_or_default();
    text_frame(headers, &rows.collect::<Vec<_>>())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn spans(text: &str) -> Vec<(usize, usize)> {
        let rows: Vec<Vec<char>> = text.lines().map(|l| l.chars().collect()).collect();
        columnize(&rows)
    }

    #[test]
    fn columnize_follows_the_header_and_widens_to_the_data() {
        assert_eq!(spans("id  name\n1   ann\n22  bobby"), [(0, 2), (4, 9)]);
        // a right-justified header: data left of it moves the start
        assert_eq!(spans("  n x\n100 y"), [(0, 3), (4, 5)]);
    }
}
