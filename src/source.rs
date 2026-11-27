//! Live sources (S-39 B): rows that keep coming, from a growing file (`:tail`)
//! or a Lua loader's `poll`. A live sheet holds its source; the app polls the
//! active sheet's source on the tail tick, on the UI thread.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use polars::prelude::*;

use crate::reader::{OpenError, SourceKind};

/// A source whose rows keep coming.
pub trait Source: Send {
    /// Pick up what arrived since the last call; true when the rows changed.
    /// It runs on the UI thread, so it reads only what is new.
    fn poll(&mut self) -> Result<bool, String>;
    /// Every row so far, in arrival order (cheap: columns are shared).
    fn rows(&self) -> PolarsResult<DataFrame>;
    /// What the status line says once after a poll (rows dropped by a cap).
    fn take_note(&mut self) -> Option<String> {
        None
    }
}

/// A source shared by a sheet and its clones. One thread polls it at a time.
#[derive(Clone)]
pub struct Live(Arc<Mutex<dyn Source>>);

impl Live {
    pub fn new(source: impl Source + 'static) -> Self {
        Live(Arc::new(Mutex::new(source)))
    }

    /// The source; a poll that panicked leaves its rows as they were.
    pub fn lock(&self) -> MutexGuard<'_, dyn Source + 'static> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Size and mtime: unchanged means nothing to read.
pub type Stamp = Option<(u64, Option<std::time::SystemTime>)>;

pub fn file_stamp(path: &Path) -> Stamp {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()))
}

/// A CSV/TSV/JSONL file that grows by appended lines: each poll parses only
/// the bytes past `consumed`. Truncation (log rotation) rereads the file.
pub struct FileTail {
    path: PathBuf,
    kind: SourceKind,
    stamp: Stamp,
    consumed: u64,
    df: DataFrame,
    /// Bytes after the last newline, parsed as one more row: the file may end
    /// there for good. Re-read each poll until its newline arrives.
    partial: Option<DataFrame>,
}

impl FileTail {
    pub fn open(path: &Path, kind: SourceKind) -> Result<Self, OpenError> {
        let mut t = FileTail {
            path: path.to_owned(),
            kind,
            stamp: file_stamp(path),
            consumed: 0,
            df: DataFrame::empty(),
            partial: None,
        };
        t.reload()?;
        Ok(t)
    }

    /// Bytes `[from..]`: `(lines, text after the last newline)`.
    fn bytes(&self, from: u64) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
        use std::io::{Read, Seek, SeekFrom};
        let mut f = std::fs::File::open(&self.path)?;
        f.seek(SeekFrom::Start(from))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        let cut = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let rest = buf.split_off(cut);
        Ok((buf, rest))
    }

    /// `partial` bytes as a row of `self.df`'s shape; None when they don't parse yet.
    fn parse_partial(&self, mut partial: Vec<u8>) -> Option<DataFrame> {
        if partial.is_empty() {
            return None;
        }
        partial.push(b'\n');
        let schema = Some(self.df.schema().clone());
        crate::reader::parse_chunk(self.kind, partial, schema, false).ok()
    }

    /// Read the whole file (first load, or after truncation).
    fn reload(&mut self) -> Result<(), OpenError> {
        let (mut lines, mut rest) = self.bytes(0)?;
        // `consumed` 0 = a lone header without its newline: reread it all later.
        self.consumed = lines.len() as u64;
        if lines.is_empty() {
            lines = std::mem::take(&mut rest);
            lines.push(b'\n');
        }
        self.df = crate::reader::parse_chunk(self.kind, lines, None, true)?;
        self.partial = self.parse_partial(rest);
        Ok(())
    }

    fn refresh(&mut self) -> Result<bool, OpenError> {
        let stamp = file_stamp(&self.path);
        if stamp == self.stamp {
            return Ok(false);
        }
        self.stamp = stamp;
        let header_only = self.consumed == 0 || self.df.height() == 0;
        if std::fs::metadata(&self.path)?.len() < self.consumed || header_only {
            self.reload()?; // truncated, rotated, or types still unknown
            return Ok(true);
        }
        let (new, rest) = self.bytes(self.consumed)?;
        let added = new.len() as u64;
        let schema = self.df.schema().clone();
        let more = (!new.is_empty())
            .then(|| crate::reader::parse_chunk(self.kind, new, Some(schema), false))
            .transpose();
        match more {
            Ok(more) => {
                if let Some(more) = more {
                    self.df.vstack_mut(&more)?;
                    self.consumed += added;
                }
                self.partial = self.parse_partial(rest);
            }
            Err(_) => self.reload()?, // new rows don't fit the schema
        }
        Ok(true)
    }
}

impl Source for FileTail {
    fn poll(&mut self) -> Result<bool, String> {
        self.refresh().map_err(|e| e.to_string())
    }

    fn rows(&self) -> PolarsResult<DataFrame> {
        let mut df = self.df.clone();
        if let Some(p) = &self.partial {
            df.vstack_mut(p)?;
        }
        Ok(df)
    }
}

/// Rows so far plus a batch, columns matched by name. A column with no
/// values yet takes the batch's type; one whose types differ takes the type
/// both fit (int and float give float, else text).
pub fn append(rows: &DataFrame, batch: DataFrame) -> PolarsResult<DataFrame> {
    let mut rows = rows.clone();
    for c in batch.columns() {
        let name = c.name().clone();
        let empty = rows
            .column(&name)
            .is_ok_and(|r| r.null_count() == r.len() && r.dtype() != c.dtype());
        if empty {
            let typed = Column::full_null(name.clone(), rows.height(), c.dtype());
            rows.with_column(typed)?;
        }
    }
    if rows.schema() == batch.schema() {
        rows.vstack_mut(&batch)?;
        return Ok(rows);
    }
    let args = UnionArgs {
        diagonal: true,
        to_supertypes: true,
        ..Default::default()
    };
    concat([rows.lazy(), batch.lazy()], args)?.collect()
}

/// The last `max` rows of `df` (0 keeps all), and how many went.
pub fn cap(df: DataFrame, max: usize) -> (DataFrame, usize) {
    let over = df.height().saturating_sub(max);
    if max == 0 || over == 0 {
        return (df, 0);
    }
    (df.slice(over as i64, max), over)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_widens_types_and_matches_names() {
        let a = df!("n" => [1i64, 2], "s" => ["x", "y"]).unwrap();
        let b = df!("s" => ["z"], "n" => [2.5f64]).unwrap();
        let out = append(&a, b).unwrap();
        assert_eq!(out.height(), 3);
        assert_eq!(out.column("n").unwrap().dtype(), &DataType::Float64);
        assert_eq!(out.column("s").unwrap().str().unwrap().get(2), Some("z"));
    }

    #[test]
    fn append_types_a_column_with_no_values_yet() {
        let a = df!("n" => Vec::<Option<String>>::new()).unwrap();
        let b = df!("n" => [2i64]).unwrap();
        let out = append(&a, b).unwrap();
        assert_eq!(out.column("n").unwrap().dtype(), &DataType::Int64);
    }

    #[test]
    fn cap_keeps_the_newest_rows() {
        let d = df!("n" => [1i64, 2, 3, 4]).unwrap();
        let (kept, dropped) = cap(d.clone(), 3);
        assert_eq!(dropped, 1);
        assert_eq!(kept.column("n").unwrap().i64().unwrap().get(0), Some(2));
        assert_eq!(cap(d, 0).1, 0);
    }
}
