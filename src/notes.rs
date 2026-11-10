//! Cell notes (S-30 B): annotations kept next to vdrs's state, never next to
//! the data. One TSV per source file: `row, column, note, created, stamp`.

use std::io::Write;
use std::path::{Path, PathBuf};

/// Which row a note is on: the key columns' values when keys are set (stable
/// across file changes), else the row id, valid while the file is unchanged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Row {
    Keys(Vec<String>),
    Id(u64),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub row: Row,
    pub column: String,
    pub text: String,
    /// Unix seconds.
    pub created: u64,
    /// The source's size + mtime when the note was made.
    pub stamp: String,
}

/// The notes of one source file.
#[derive(Clone, Debug, Default)]
pub struct Notes {
    path: PathBuf,
    /// The source's stamp now.
    stamp: String,
    pub notes: Vec<Note>,
}

impl Notes {
    /// The notes of `source` (none yet: empty).
    pub fn load(source: &Path) -> Self {
        let source = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
        let path = store_dir().join(format!(
            "{:016x}.tsv",
            fnv(source.as_os_str().as_encoded_bytes())
        ));
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let mut lines = text.lines();
        // the header names the source: a hash collision reads as no notes
        let notes = match lines.next() {
            Some(h) if h == header(&source) => lines.filter_map(parse_line).collect(),
            _ => Vec::new(),
        };
        Self {
            path,
            stamp: stamp(&source),
            notes,
        }
    }

    /// The note on `column` of a row with id `rid` and `keys` (`None`: no key columns).
    pub fn get(&self, rid: Option<u64>, keys: Option<&[String]>, column: &str) -> Option<&Note> {
        self.notes.iter().find(|n| {
            n.column == column
                && match &n.row {
                    Row::Id(id) => rid == Some(*id),
                    Row::Keys(k) => keys == Some(k.as_slice()),
                }
        })
    }

    /// A row-id note made on another version of the file.
    pub fn stale(&self, n: &Note) -> bool {
        matches!(n.row, Row::Id(_)) && n.stamp != self.stamp
    }

    /// Set the note on (`row`, `column`); empty `text` deletes it. Written at once.
    pub fn set(
        &mut self,
        source: &Path,
        row: Row,
        column: &str,
        text: &str,
    ) -> std::io::Result<()> {
        let mut notes = self.notes.clone();
        notes.retain(|n| !(n.row == row && n.column == column));
        if !text.is_empty() {
            let created = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_secs());
            notes.push(Note {
                row,
                column: column.into(),
                text: text.into(),
                created,
                stamp: self.stamp.clone(),
            });
        }
        let source = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
        let mut out = header(&source) + "\n";
        for n in &notes {
            out += &line(n);
        }
        write_atomic(&self.path, out.as_bytes())?;
        self.notes = notes;
        Ok(())
    }
}

/// `$XDG_STATE_HOME/vdrs/notes`; tests use a temp dir.
fn store_dir() -> PathBuf {
    if cfg!(test) {
        return std::env::temp_dir()
            .join(format!("vdrs-test-{}", std::process::id()))
            .join("notes");
    }
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir)
        .join("vdrs/notes")
}

/// FNV-1a: a stable file name per source path (std's hasher may change).
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x100_0000_01b3)
    })
}

fn header(source: &Path) -> String {
    format!("# vdrs notes\t{}", esc(&source.to_string_lossy()))
}

fn stamp(source: &Path) -> String {
    std::fs::metadata(source).map_or_else(
        |_| String::new(),
        |m| {
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok());
            format!("{}:{}", m.len(), t.map_or(0, |d| d.as_nanos()))
        },
    )
}

/// Temp file + rename: a failed write leaves the old notes whole.
fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
    tmp.write_all(bytes)?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn unesc(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        match (c, c == '\\') {
            (_, true) => match it.next() {
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some(o) => out.push(o),
                None => {}
            },
            (c, false) => out.push(c),
        }
    }
    out
}

/// Key values are joined by U+001F (escaped like any text).
fn line(n: &Note) -> String {
    let row = match &n.row {
        Row::Id(id) => format!("id:{id}"),
        Row::Keys(k) => format!("keys:{}", k.join("\u{1f}")),
    };
    let f = [
        row,
        n.column.clone(),
        n.text.clone(),
        n.created.to_string(),
        n.stamp.clone(),
    ];
    f.iter().map(|s| esc(s)).collect::<Vec<_>>().join("\t") + "\n"
}

fn parse_line(l: &str) -> Option<Note> {
    let f: Vec<String> = l.split('\t').map(unesc).collect();
    let [row, column, text, created, stamp] = f.as_slice() else {
        return None;
    };
    let row = match row.split_once(':')? {
        ("id", id) => Row::Id(id.parse().ok()?),
        ("keys", k) => Row::Keys(k.split('\u{1f}').map(String::from).collect()),
        _ => return None,
    };
    Some(Note {
        row,
        column: column.clone(),
        text: text.clone(),
        created: created.parse().ok()?,
        stamp: stamp.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(text: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        f.write_all(text.as_bytes()).unwrap();
        f
    }

    #[test]
    fn notes_survive_a_reload_by_id_and_by_keys() {
        let src = source("id,v\n1,a\n");
        let mut n = Notes::load(src.path());
        n.set(src.path(), Row::Id(0), "v", "tab\there\nand a line")
            .unwrap();
        n.set(
            src.path(),
            Row::Keys(vec!["1".into(), "x".into()]),
            "v",
            "by key",
        )
        .unwrap();
        let again = Notes::load(src.path());
        let got = again.get(Some(0), None, "v").unwrap();
        assert_eq!(got.text, "tab\there\nand a line");
        assert!(!again.stale(got));
        let keys = ["1".to_string(), "x".to_string()];
        assert_eq!(again.get(None, Some(&keys), "v").unwrap().text, "by key");
        assert!(again.get(Some(1), None, "v").is_none());
        let mut again = again;
        again.set(src.path(), Row::Id(0), "v", "").unwrap();
        assert!(Notes::load(src.path()).get(Some(0), None, "v").is_none());
    }

    #[test]
    fn an_id_note_on_a_changed_file_is_stale_a_key_note_is_not() {
        let mut src = source("id,v\n1,a\n");
        let mut n = Notes::load(src.path());
        n.set(src.path(), Row::Id(0), "v", "old").unwrap();
        n.set(src.path(), Row::Keys(vec!["1".into()]), "v", "kept")
            .unwrap();
        src.write_all(b"2,b\n").unwrap();
        src.flush().unwrap();
        let again = Notes::load(src.path());
        let by_id = again.get(Some(0), None, "v").unwrap();
        assert!(again.stale(by_id));
        let by_key = again.get(None, Some(&["1".to_string()]), "v").unwrap();
        assert!(!again.stale(by_key));
    }

    #[test]
    fn a_failed_write_keeps_the_old_notes() {
        let src = source("id\n1\n");
        let mut n = Notes::load(src.path());
        n.set(src.path(), Row::Id(0), "id", "first").unwrap();
        // the store path is a directory now: the rename fails
        std::fs::remove_file(&n.path).unwrap();
        std::fs::create_dir(&n.path).unwrap();
        assert!(n.set(src.path(), Row::Id(0), "id", "second").is_err());
        assert_eq!(n.get(Some(0), None, "id").unwrap().text, "first");
        let dir = n.path.parent().unwrap();
        let leftovers = std::fs::read_dir(dir)
            .unwrap()
            .filter(|e| e.as_ref().unwrap().path().extension().is_none())
            .count();
        std::fs::remove_dir(&n.path).unwrap();
        assert_eq!(leftovers, 0, "no temp file left behind");
    }
}
