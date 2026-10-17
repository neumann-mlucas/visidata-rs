//! Ex-line parser. `:cmd arg1 arg2 …` → one or more Actions.
//!
//! Phase 1 subset: `:q`/`:quit`, `:qa`/`:quit!`, `:filter <col> <regex>`,
//! `:unfilter`, `:sort [+|-]<col>`, `:unsort`, `:ls` (sheet switcher),
//! `:pivot <index> <on> <values> [agg]`, `:unpivot <on1,on2> -- <idx1,idx2>`.
//! `:derive` is deferred — needs a real expression parser; comes with S-06.

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use crate::action::Action;

#[derive(Debug, thiserror::Error)]
pub enum ExError {
    #[error("usage: {0}")]
    Usage(&'static str),
    #[error("unknown command: :{0}")]
    Unknown(String),
    #[error("empty command")]
    Empty,
}

pub fn parse(input: &str) -> Result<Vec<Action>, ExError> {
    let input = input
        .trim()
        .strip_prefix(':')
        .unwrap_or(input.trim())
        .trim();
    if input.is_empty() {
        return Err(ExError::Empty);
    }
    let (cmd, rest) = match input.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (input, ""),
    };
    match cmd {
        "q" | "quit" => Ok(vec![Action::PopSheet]),
        "qa" | "quit!" => Ok(vec![Action::Quit]),
        "filter" => {
            let (col, pat) = rest
                .split_once(char::is_whitespace)
                .ok_or(ExError::Usage(":filter <col> <regex>"))?;
            if col.is_empty() || pat.trim().is_empty() {
                return Err(ExError::Usage(":filter <col> <regex>"));
            }
            Ok(vec![Action::SetFilter {
                column: col.to_string(),
                regex: pat.trim().to_string(),
            }])
        }
        "unfilter" | "clear-filter" => Ok(vec![Action::ClearFilter]),
        "sort" => {
            if rest.is_empty() {
                return Err(ExError::Usage(":sort [+|-]<col>"));
            }
            let (descending, col) = if let Some(c) = rest.strip_prefix('-') {
                (true, c)
            } else if let Some(c) = rest.strip_prefix('+') {
                (false, c)
            } else {
                (false, rest)
            };
            if col.is_empty() {
                return Err(ExError::Usage(":sort [+|-]<col>"));
            }
            Ok(vec![Action::SetSort {
                column: col.to_string(),
                descending,
            }])
        }
        "unsort" | "clear-sort" => Ok(vec![Action::ClearSort]),
        "ls" | "sheets" => Ok(vec![Action::OpenSheetSwitcher]),
        "pivot" => {
            // :pivot <index> <on> <values> [agg]
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.len() < 3 {
                return Err(ExError::Usage(":pivot <index> <on> <values> [agg]"));
            }
            let agg = parts.get(3).copied().unwrap_or("sum").to_string();
            Ok(vec![Action::Pivot {
                index: parts[0].to_string(),
                on: parts[1].to_string(),
                values: parts[2].to_string(),
                agg,
            }])
        }
        "tail" => {
            // :tail <path> [--follow]
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.is_empty() {
                return Err(ExError::Usage(":tail <path> [--follow]"));
            }
            let path = parts[0].to_string();
            let follow = parts.iter().any(|p| *p == "--follow" || *p == "-f");
            Ok(vec![Action::Tail { path, follow }])
        }
        "diff" => {
            // :diff <path> [key1,key2,...]
            let parts: Vec<&str> = rest.split_whitespace().collect();
            if parts.is_empty() {
                return Err(ExError::Usage(":diff <path> [key1,key2,...]"));
            }
            let path = parts[0].to_string();
            let keys = parts
                .get(1)
                .map(|s| {
                    s.split(',')
                        .map(|k| k.trim().to_string())
                        .filter(|k| !k.is_empty())
                        .collect()
                })
                .unwrap_or_default();
            Ok(vec![Action::Diff { path, keys }])
        }
        "unpivot" | "melt" => {
            // :unpivot <on-cols> -- <index-cols>  (comma-separated; `--` optional)
            if rest.is_empty() {
                return Err(ExError::Usage(
                    ":unpivot <on1,on2,...> [-- <index1,index2,...>]",
                ));
            }
            let (on_part, index_part) = match rest.split_once(" -- ") {
                Some((a, b)) => (a, b),
                None => (rest, ""),
            };
            let on: Vec<String> = on_part
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            let index: Vec<String> = index_part
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if on.is_empty() {
                return Err(ExError::Usage(
                    ":unpivot <on1,on2,...> [-- <index1,index2,...>]",
                ));
            }
            Ok(vec![Action::Unpivot { on, index }])
        }
        "hide" => Ok(vec![Action::HideCol]),
        "unhide" | "unhide-cols" => Ok(vec![Action::UnhideAll]),
        "key" => Ok(vec![Action::KeyCol]),
        "unkey" => Ok(vec![Action::UnkeyAll]),
        "rename" => {
            if rest.is_empty() {
                return Err(ExError::Usage(":rename <new-name> (operates on current column)"));
            }
            Ok(vec![Action::RenameCol {
                new_name: rest.trim().to_string(),
            }])
        }
        "type" => {
            let parts: Vec<&str> = rest.split_whitespace().collect();
            match parts.as_slice() {
                ["int"] => Ok(vec![Action::TypeCastInt]),
                ["float"] => Ok(vec![Action::TypeCastFloat]),
                ["string" | "str"] => Ok(vec![Action::TypeCastString]),
                ["date"] => Ok(vec![Action::TypeCastDate]),
                ["currency"] => Ok(vec![Action::TypeCastCurrency]),
                _ => Err(ExError::Usage(":type int|float|string|date|currency")),
            }
        }
        // Persist current sheet. Writer picked by extension; `.csv/.tsv/.parquet/
        // .ipc/.arrow/.jsonl/.ndjson` supported. Overwrites existing files.
        "save" | "w" | "write" => {
            if rest.is_empty() {
                return Err(ExError::Usage(":save <path.{csv|tsv|parquet|ipc|jsonl}>"));
            }
            Ok(vec![Action::Save {
                path: rest.to_string(),
            }])
        }
        other => Err(ExError::Unknown(other.to_string())),
    }
}

/// File-persisted command history. Append-only, best-effort I/O.
pub struct History {
    path: PathBuf,
    lines: Vec<String>,
    cursor: Option<usize>,
}

impl History {
    pub fn load() -> Self {
        let path = history_path();
        let lines = fs::File::open(&path)
            .ok()
            .map(|f| {
                BufReader::new(f)
                    .lines()
                    .map_while(Result::ok)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        Self {
            path,
            lines,
            cursor: None,
        }
    }

    /// Append a line (dedup against last), persist best-effort.
    pub fn push(&mut self, line: &str) {
        let line = line.trim();
        if line.is_empty() {
            return;
        }
        if self.lines.last().is_some_and(|l| l == line) {
            return;
        }
        self.lines.push(line.to_string());
        self.cursor = None;
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        if let Ok(mut f) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            let _ = writeln!(f, "{line}");
        }
    }

    #[allow(clippy::should_implement_trait)]
    pub fn prev(&mut self) -> Option<&str> {
        if self.lines.is_empty() {
            return None;
        }
        let next = match self.cursor {
            None => self.lines.len() - 1,
            Some(0) => 0,
            Some(i) => i - 1,
        };
        self.cursor = Some(next);
        Some(&self.lines[next])
    }

    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<&str> {
        let i = self.cursor?;
        if i + 1 >= self.lines.len() {
            self.cursor = None;
            return Some("");
        }
        let next = i + 1;
        self.cursor = Some(next);
        Some(&self.lines[next])
    }

    pub fn reset_cursor(&mut self) {
        self.cursor = None;
    }
}

fn history_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("vdrs").join("exline_history")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quit() {
        assert!(matches!(
            parse(":q").unwrap_or_default().as_slice(),
            [Action::PopSheet]
        ));
        assert!(matches!(
            parse("quit").unwrap_or_default().as_slice(),
            [Action::PopSheet]
        ));
        assert!(matches!(
            parse("qa").unwrap_or_default().as_slice(),
            [Action::Quit]
        ));
    }

    #[test]
    fn parses_filter() {
        let a = parse("filter name ^a.*").unwrap();
        match &a[0] {
            Action::SetFilter { column, regex } => {
                assert_eq!(column, "name");
                assert_eq!(regex, "^a.*");
            }
            other => panic!("wrong action: {other:?}"),
        }
    }

    #[test]
    fn parses_sort_signed() {
        let a = parse("sort -age").unwrap();
        assert!(matches!(&a[0], Action::SetSort { column, descending: true } if column == "age"));
        let b = parse("sort +age").unwrap();
        assert!(matches!(&b[0], Action::SetSort { column, descending: false } if column == "age"));
        let c = parse("sort age").unwrap();
        assert!(matches!(&c[0], Action::SetSort { column, descending: false } if column == "age"));
    }

    #[test]
    fn rejects_unknown() {
        assert!(matches!(parse("blargh"), Err(ExError::Unknown(_))));
    }

    #[test]
    fn parses_save() {
        let a = parse("save out.parquet").unwrap();
        assert!(matches!(&a[0], Action::Save { path } if path == "out.parquet"));
        let b = parse(":w /tmp/x.tsv").unwrap();
        assert!(matches!(&b[0], Action::Save { path } if path == "/tmp/x.tsv"));
        assert!(matches!(parse("save"), Err(ExError::Usage(_))));
    }

    #[test]
    fn rejects_bad_usage() {
        assert!(matches!(parse("filter justone"), Err(ExError::Usage(_))));
        assert!(matches!(parse("sort"), Err(ExError::Usage(_))));
    }
}
