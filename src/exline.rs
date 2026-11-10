//! Ex-line parser. `:cmd arg1 arg2 …` → one or more Actions.

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
    #[error(":{0} takes no arguments")]
    NoArgs(String),
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
    if let Some(a) = substitute(input) {
        return a.map(|a| vec![a]);
    }
    let (cmd, rest) = match input.split_once(char::is_whitespace) {
        Some((c, r)) => (c, r.trim()),
        None => (input, ""),
    };
    match cmd {
        "q" | "quit" => Ok(vec![Action::PopSheet]),
        "q!" => Ok(vec![Action::ForcePopSheet]),
        "qa" => Ok(vec![Action::Quit]),
        "qa!" | "quit!" => Ok(vec![Action::ForceQuit]),
        // empty = null
        "edit" => Ok(vec![Action::EditCell {
            value: unquote(rest),
        }]),
        "filter" => {
            const USAGE: &str = ":filter <col> <regex>";
            let (col, pat) = first_arg(rest, USAGE)?;
            if col.is_empty() || pat.is_empty() {
                return Err(ExError::Usage(USAGE));
            }
            Ok(vec![Action::SetFilter {
                column: col,
                regex: pat.to_string(),
            }])
        }
        "unfilter" | "clear-filter" => Ok(vec![Action::ClearFilter]),
        "sort" => {
            const USAGE: &str = ":sort [+|-]<col>";
            let rest = one_arg(rest, USAGE)?;
            let rest = rest.as_str();
            let (descending, col) = if let Some(c) = rest.strip_prefix('-') {
                (true, c)
            } else if let Some(c) = rest.strip_prefix('+') {
                (false, c)
            } else {
                (false, rest)
            };
            if col.is_empty() {
                return Err(ExError::Usage(USAGE));
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
            const USAGE: &str = ":pivot <index> <on> <values> [agg]";
            let parts = split_args(rest, USAGE)?;
            let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
            if parts.len() < 3 {
                return Err(ExError::Usage(USAGE));
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
            let parts = split_args(rest, ":tail <path> [--follow]")?;
            let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
            if parts.is_empty() {
                return Err(ExError::Usage(":tail <path> [--follow]"));
            }
            if parts[0] == "-" {
                return Err(ExError::Usage(
                    ":tail needs a file; stdin can't be reopened in the TUI",
                ));
            }
            let path = parts[0].to_string();
            let follow = parts.iter().any(|p| *p == "--follow" || *p == "-f");
            Ok(vec![Action::Tail { path, follow }])
        }
        "diff" => {
            // :diff <path> [key1,key2,...]
            let parts = split_args(rest, ":diff <path> [key1,key2,...]")?;
            let parts: Vec<&str> = parts.iter().map(String::as_str).collect();
            if parts.is_empty() {
                return Err(ExError::Usage(":diff <path> [key1,key2,...]"));
            }
            if parts[0] == "-" {
                return Err(ExError::Usage(
                    ":diff needs a file; stdin can't be reopened in the TUI",
                ));
            }
            let path = parts[0].to_string();
            let keys = parts.get(1).map(|s| comma_list(s)).unwrap_or_default();
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
            let (on, index) = (comma_list(on_part), comma_list(index_part));
            if on.is_empty() {
                return Err(ExError::Usage(
                    ":unpivot <on1,on2,...> [-- <index1,index2,...>]",
                ));
            }
            Ok(vec![Action::Unpivot { on, index }])
        }
        "hide" if rest.is_empty() => Ok(vec![Action::HideCol]),
        "hide" => Ok(vec![Action::HideNamed {
            column: one_arg(rest, ":hide [column]")?,
        }]),
        "unhide" | "unhide-cols" => Ok(vec![Action::UnhideAll]),
        "key" => Ok(vec![Action::KeyCol]),
        "unkey" => Ok(vec![Action::UnkeyAll]),
        "rename" => {
            if rest.is_empty() {
                return Err(ExError::Usage(
                    ":rename <new-name> (operates on current column)",
                ));
            }
            Ok(vec![Action::RenameCol {
                new_name: unquote(rest),
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
                ["json"] => Ok(vec![Action::TypeCastJson]),
                _ => Err(ExError::Usage(":type int|float|string|date|currency|json")),
            }
        }
        // Persist current sheet. Writer picked by extension; `.csv/.tsv/.parquet/
        // .ipc/.arrow/.jsonl/.ndjson` supported. Overwrites existing files.
        "save" | "w" | "write" => {
            if rest.is_empty() {
                return Err(ExError::Usage(":save <path.{csv|tsv|parquet|ipc|jsonl}>"));
            }
            Ok(vec![Action::Save {
                path: unquote(rest),
            }])
        }
        "join" => {
            const USAGE: &str = ":join <file|sheet> <key1,key2,...> [inner|left|full|merge|extend] | :join <file|sheet> [append|concat]";
            let a = split_args(rest, USAGE)?;
            let (source, keys, how) = match a.as_slice() {
                [s, k, h] => (s.clone(), comma_list(k), h.clone()),
                [s, h] if h == "append" || h == "concat" => (s.clone(), Vec::new(), h.clone()),
                [s, k] => (s.clone(), comma_list(k), "inner".to_string()),
                [s] => (s.clone(), Vec::new(), "append".to_string()), // append needs no keys
                _ => return Err(ExError::Usage(USAGE)),
            };
            let keyless = matches!(how.as_str(), "append" | "concat");
            if !matches!(how.as_str(), "inner" | "left" | "full" | "merge" | "extend") && !keyless
                || (keys.is_empty() && !keyless)
            {
                return Err(ExError::Usage(USAGE));
            }
            Ok(vec![Action::Join { source, keys, how }])
        }
        "derive" => {
            const USAGE: &str = ":derive <name> = <sql expression>";
            let (name, expr) = rest.split_once('=').ok_or(ExError::Usage(USAGE))?;
            let (name, expr) = (unquote(name), expr.trim().to_string());
            if name.is_empty() || expr.is_empty() {
                return Err(ExError::Usage(USAGE));
            }
            Ok(vec![Action::Derive { name, expr }])
        }
        "sql" => {
            if rest.is_empty() {
                return Err(ExError::Usage(":sql <SELECT ... FROM self ...>"));
            }
            Ok(vec![Action::Sql {
                query: rest.to_string(),
            }])
        }
        "exec-sql" => {
            if rest.is_empty() {
                return Err(ExError::Usage(":exec-sql <statements> (in a .duckdb file)"));
            }
            Ok(vec![Action::ExecSql {
                sql: rest.to_string(),
            }])
        }
        "export" => {
            if rest.is_empty() {
                return Err(ExError::Usage(":export <path.py|path.sql>"));
            }
            Ok(vec![Action::Export {
                path: unquote(rest),
            }])
        }
        "setcol" => {
            if rest.is_empty() {
                return Err(ExError::Usage(
                    ":setcol <value> (current column, selected rows)",
                ));
            }
            Ok(vec![Action::SetCol {
                value: unquote(rest),
            }])
        }
        "setexpr" => {
            if rest.is_empty() {
                return Err(ExError::Usage(
                    ":setexpr <sql expression> (current column, selected rows)",
                ));
            }
            Ok(vec![Action::SetExpr {
                expr: rest.to_string(),
            }])
        }
        "filter-expr" => Ok(vec![Action::SetFilterExpr {
            expr: (!rest.is_empty()).then(|| rest.to_string()),
        }]),
        "select" | "unselect" => {
            if rest.is_empty() {
                return Err(ExError::Usage(
                    ":select <sql predicate> / :unselect <sql predicate>",
                ));
            }
            Ok(vec![Action::SelectExpr {
                expr: rest.to_string(),
                unselect: cmd == "unselect",
            }])
        }
        "uniform" | "hide-uniform" => Ok(vec![Action::HideUniform]),
        "dup" => Ok(vec![Action::DupSheet]),
        "plan" => Ok(vec![Action::TogglePlanPanel]),
        "sidebar" if rest.is_empty() => Ok(vec![Action::ToggleSidebar]),
        "sidebar" => crate::action::SidebarPage::parse(rest)
            .map(|page| vec![Action::ShowSidebar { page }])
            .ok_or(ExError::Usage("sidebar [column|cell|help|plan|plan-opt]")),
        "describe" => Ok(vec![Action::Describe]),
        "undo" => Ok(vec![Action::Undo]),
        "redo" => Ok(vec![Action::Redo]),
        "earlier" | "later" => {
            let n: i64 = match rest {
                "" => 1,
                n => n
                    .parse()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or(ExError::Usage("earlier [n] / later [n]"))?,
            };
            let n = if cmd == "later" { -n } else { n };
            Ok(vec![Action::TimeWalk { n }])
        }
        "undotree" | "undo-tree" => Ok(vec![Action::OpenUndoTree]),
        "open" | "o" => match split_args(rest, ":open <path>")?.as_slice() {
            [p] if p != "-" => Ok(vec![Action::Open { path: p.clone() }]),
            _ => Err(ExError::Usage(":open <path> (a file; not stdin)")),
        },
        "source" | "so" => match split_args(rest, ":source <path>")?.as_slice() {
            [p] => Ok(vec![Action::Source { path: p.clone() }]),
            _ => Err(ExError::Usage(":source <path>")),
        },
        "select-ids" => {
            const USAGE: &str = ":select-ids <id,id,...>";
            let ids: Result<Vec<u64>, _> = comma_list(rest).iter().map(|i| i.parse()).collect();
            match ids {
                Ok(ids) if !ids.is_empty() => Ok(vec![Action::SelectIds { ids }]),
                _ => Err(ExError::Usage(USAGE)),
            }
        }
        "patch" => {
            const USAGE: &str = ":patch cells <column> <file> | :patch rows <file>";
            match split_args(rest, USAGE)?.as_slice() {
                [k, c, p] if k == "cells" => Ok(vec![Action::Patch {
                    column: Some(c.clone()),
                    path: p.clone(),
                }]),
                [k, p] if k == "rows" => Ok(vec![Action::Patch {
                    column: None,
                    path: p.clone(),
                }]),
                _ => Err(ExError::Usage(USAGE)),
            }
        }
        "expand" => match rest.parse() {
            Ok(depth) => Ok(vec![Action::ExpandCol { depth }]),
            Err(_) if rest.is_empty() => Ok(vec![Action::ExpandCol { depth: 1 }]),
            Err(_) => Err(ExError::Usage(":expand [depth] (0 = all the way)")),
        },
        "mksession" => Ok(vec![Action::MkSession {
            path: (!rest.is_empty()).then(|| unquote(rest)),
        }]),
        "aggregate" if !rest.is_empty() => Ok(vec![Action::AggregateCol {
            names: rest.to_string(),
        }]),
        "aggregate" => Err(ExError::Usage(":aggregate <names> (current column)")),
        "cursor" => {
            const USAGE: &str = ":cursor <row> [column]";
            let a = split_args(rest, USAGE)?;
            let row = a.first().and_then(|r| r.parse().ok()).filter(|r| *r > 0);
            match (row, a.get(1), a.len()) {
                (Some(row), column, 1 | 2) => Ok(vec![Action::SetCursor {
                    row,
                    column: column.cloned(),
                }]),
                _ => Err(ExError::Usage(USAGE)),
            }
        }
        // `:42` → row 42, as in vim.
        n if n.bytes().all(|b| b.is_ascii_digit()) && rest.is_empty() => {
            let row: usize = n.parse().map_err(|_| ExError::Unknown(n.to_string()))?;
            Ok(vec![
                Action::GotoTop,
                Action::CursorDown {
                    count: Some(row.saturating_sub(1)),
                },
            ])
        }
        // Any named action: `:select-all`, `:dedupe_rows`.
        other => match Action::from_name(other).filter(|a| !a.is_input_layer()) {
            // `:cursor-down 3` = `3j` (S-19 macros record counts this way).
            Some(
                Action::CursorDown { .. }
                | Action::CursorUp { .. }
                | Action::CursorLeft { .. }
                | Action::CursorRight { .. },
            ) if rest.parse::<usize>().is_ok() => {
                let a = Action::from_name(other).map(|a| a.with_count(rest.parse().ok()));
                Ok(a.into_iter().collect())
            }
            Some(_) if !rest.is_empty() => Err(ExError::NoArgs(other.to_string())),
            Some(a) => Ok(vec![a]),
            None => Err(ExError::Unknown(other.to_string())),
        },
    }
}

/// `s/pattern/replacement/flags` with any punctuation delimiter (S-28).
/// `\<delim>` is the delimiter itself; `\1` in the replacement is `${1}`.
fn substitute(input: &str) -> Option<Result<Action, ExError>> {
    const USAGE: &str = ":s/pattern/replacement/[g][i]";
    let rest = input.strip_prefix('s')?;
    let d = rest
        .chars()
        .next()
        .filter(|c| c.is_ascii_punctuation() && !"-_\\\"'".contains(*c))?;
    let mut parts = vec![String::new()];
    let mut chars = rest[d.len_utf8()..].chars().peekable();
    while let Some(c) = chars.next() {
        if c == d && parts.len() < 3 {
            parts.push(String::new());
            continue;
        }
        let cur = parts.last_mut()?;
        match c {
            '\\' if chars.peek() == Some(&d) => {
                cur.push(d);
                chars.next();
            }
            '\\' => {
                cur.push(c);
                cur.extend(chars.next());
            }
            c => cur.push(c),
        }
    }
    let (pattern, replacement, flags) = match parts.as_slice() {
        [p] => (p.clone(), String::new(), String::new()),
        [p, r] => (p.clone(), r.clone(), String::new()),
        [p, r, f] => (p.clone(), r.clone(), f.clone()),
        _ => return Some(Err(ExError::Usage(USAGE))),
    };
    if pattern.is_empty() || !flags.chars().all(|f| matches!(f, 'g' | 'i')) {
        return Some(Err(ExError::Usage(USAGE)));
    }
    let pattern = match flags.contains('i') {
        true => format!("(?i){pattern}"),
        false => pattern,
    };
    Some(Ok(Action::Substitute {
        pattern,
        replacement: vim_backrefs(&replacement),
        all: flags.contains('g'),
    }))
}

/// `\1` → `${1}`, `\\` → `\` (vim / VisiData replacements); the rest as is.
pub fn vim_backrefs(r: &str) -> String {
    let mut out = String::new();
    let mut chars = r.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('\\', Some(n)) if n.is_ascii_digit() => {
                out.push_str(&format!("${{{n}}}"));
                chars.next();
            }
            ('\\', Some('\\')) => {
                out.push('\\');
                chars.next();
            }
            (c, _) => out.push(c),
        }
    }
    out
}

/// Built-in ex command names (aliases included), for completion.
pub const COMMANDS: &[&str] = &[
    "clear-filter",
    "clear-sort",
    "aggregate",
    "cursor",
    "expand",
    "mksession",
    "patch",
    "open",
    "select-ids",
    "source",
    "derive",
    "describe",
    "diff",
    "dup",
    "edit",
    "export",
    "filter-expr",
    "filter",
    "hide",
    "hide-uniform",
    "join",
    "key",
    "ls",
    "melt",
    "pivot",
    "q",
    "q!",
    "qa",
    "qa!",
    "quit",
    "quit!",
    "redo",
    "earlier",
    "later",
    "undotree",
    "rename",
    "save",
    "select",
    "setcol",
    "setexpr",
    "sheets",
    "sort",
    "sql",
    "tail",
    "type",
    "undo",
    "unfilter",
    "unhide",
    "unhide-cols",
    "uniform",
    "plan",
    "sidebar",
    "unkey",
    "unpivot",
    "unselect",
    "unsort",
    "w",
    "write",
];

/// Every command name `:` accepts: built-ins, then named actions (kebab-case).
pub fn command_names() -> Vec<String> {
    let actions = Action::NAMED
        .iter()
        .filter(|(_, a)| !a.is_input_layer())
        .map(|(n, _)| n.replace('_', "-"));
    COMMANDS
        .iter()
        .map(|c| c.to_string())
        .chain(actions)
        .collect()
}

/// Lines kept in the history file; older ones are dropped on load.
const HISTORY_MAX: usize = 1000;

/// Shell-like split: whitespace separates, `"..."` / `'...'` group (quotes
/// removed), backslash escapes the next char outside single quotes.
pub(crate) fn split_args(s: &str, usage: &'static str) -> Result<Vec<String>, ExError> {
    let mut out = Vec::new();
    let mut cur: Option<String> = None;
    let mut chars = s.chars();
    let mut quote: Option<char> = None;
    while let Some(c) = chars.next() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some('\''), c) => cur.get_or_insert_default().push(c),
            (_, '\\') => {
                let next = chars.next().ok_or(ExError::Usage(usage))?;
                cur.get_or_insert_default().push(next);
            }
            (Some(_), c) => cur.get_or_insert_default().push(c),
            (None, '"' | '\'') => {
                quote = Some(c);
                cur.get_or_insert_default(); // `""` is an empty argument
            }
            (None, c) if c.is_whitespace() => out.extend(cur.take()),
            (None, c) => cur.get_or_insert_default().push(c),
        }
    }
    if quote.is_some() {
        return Err(ExError::Usage(usage)); // unterminated quote
    }
    out.extend(cur);
    Ok(out)
}

/// First quote-aware argument plus the raw, trimmed remainder (kept verbatim so
/// regexes keep their backslashes).
fn first_arg<'a>(s: &'a str, usage: &'static str) -> Result<(String, &'a str), ExError> {
    let s = s.trim_start();
    let end = match s.chars().next() {
        Some(q @ ('"' | '\'')) => s[1..].find(q).map(|i| i + 2).ok_or(ExError::Usage(usage))?,
        _ => s.find(char::is_whitespace).unwrap_or(s.len()),
    };
    let first = split_args(&s[..end], usage)?.pop().unwrap_or_default();
    Ok((first, s[end..].trim()))
}

/// Exactly one (possibly quoted) argument.
fn one_arg(s: &str, usage: &'static str) -> Result<String, ExError> {
    match split_args(s, usage)?.as_slice() {
        [one] => Ok(one.clone()),
        _ => Err(ExError::Usage(usage)),
    }
}

/// Whole remainder as one value: quotes stripped when it's a single quoted arg.
fn unquote(s: &str) -> String {
    match split_args(s, "") {
        Ok(a) if a.len() == 1 => a[0].clone(),
        _ => s.trim().to_string(),
    }
}

fn comma_list(s: &str) -> Vec<String> {
    s.split(',')
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty())
        .collect()
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
        let mut lines = fs::File::open(&path)
            .ok()
            .map(|f| {
                BufReader::new(f)
                    .lines()
                    .map_while(Result::ok)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if lines.len() > HISTORY_MAX {
            lines.drain(..lines.len() - HISTORY_MAX);
            let _ = fs::write(&path, lines.join("\n") + "\n"); // best-effort, like push
        }
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
        self.cursor = None;
        if self.lines.last().is_some_and(|l| l == line) {
            return;
        }
        self.lines.push(line.to_string());
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
    // Unit tests drive the real App; never touch the user's history file.
    if cfg!(test) {
        let dir = std::env::temp_dir().join(format!("vdrs-test-{}", std::process::id()));
        return dir.join("exline_history");
    }
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
    fn substitute_parses_delimiters_flags_and_backrefs() {
        let sub = |p: &str, r: &str, all: bool| {
            vec![Action::Substitute {
                pattern: p.into(),
                replacement: r.into(),
                all,
            }]
        };
        assert_eq!(parse(":s/ +$//").unwrap(), sub(" +$", "", false));
        assert_eq!(parse("s/ +$/").unwrap(), sub(" +$", "", false));
        assert_eq!(parse("s#a/b#c#gi").unwrap(), sub("(?i)a/b", "c", true));
        assert_eq!(
            parse(r"s/(\w)x/\1y\\/g").unwrap(),
            sub(r"(\w)x", r"${1}y\", true)
        );
        assert_eq!(parse(r"s/\/x/y/").unwrap(), sub("/x", "y", false));
        assert_eq!(parse("s/a b/c d/").unwrap(), sub("a b", "c d", false));
        for bad in ["s//x/", "s/a/b/q"] {
            assert!(matches!(parse(bad), Err(ExError::Usage(_))), "{bad}");
        }
        // other commands starting with s still parse as themselves
        assert_eq!(
            parse("sort -x").unwrap()[0],
            Action::SetSort {
                column: "x".into(),
                descending: true
            }
        );
        assert!(matches!(
            parse("select-ids 1").unwrap()[0],
            Action::SelectIds { .. }
        ));
    }

    /// `COMMANDS` (what Tab completes) names only commands `parse` knows.
    #[test]
    fn listed_commands_exist() {
        for c in COMMANDS {
            assert!(!matches!(parse(c), Err(ExError::Unknown(_))), ":{c}");
        }
        for n in command_names() {
            assert!(!matches!(parse(&n), Err(ExError::Unknown(_))), ":{n}");
        }
    }

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
    fn tail_and_diff_refuse_stdin() {
        // reading stdin inside the TUI would block on the terminal forever
        assert!(matches!(parse("tail -"), Err(ExError::Usage(_))));
        assert!(matches!(parse("diff - id"), Err(ExError::Usage(_))));
    }

    #[test]
    fn quoted_args_allow_spaces() {
        let a = parse(r#"filter "unit price" ^1\d+$"#).unwrap();
        assert!(matches!(&a[0], Action::SetFilter { column, regex }
            if column == "unit price" && regex == r"^1\d+$"));
        let a = parse(r#"sort -"my col""#).unwrap();
        assert!(
            matches!(&a[0], Action::SetSort { column, descending: true } if column == "my col")
        );
        let a = parse(r#"tail '/tmp/a b.jsonl' -f"#).unwrap();
        assert!(matches!(&a[0], Action::Tail { path, follow: true } if path == "/tmp/a b.jsonl"));
        let a = parse(r#"rename "new name""#).unwrap();
        assert!(matches!(&a[0], Action::RenameCol { new_name } if new_name == "new name"));
        assert!(matches!(parse(r#"sort "open"#), Err(ExError::Usage(_))));
        assert_eq!(
            split_args(r#"a\ b "" c"#, "").unwrap(),
            vec!["a b", "", "c"]
        );
    }

    #[test]
    fn parses_new_commands() {
        let a = parse("join other.csv id,day left").unwrap();
        assert!(matches!(&a[0], Action::Join { source, keys, how }
            if source == "other.csv" && keys == &["id", "day"] && how == "left"));
        let a = parse("join more.csv").unwrap();
        assert!(matches!(&a[0], Action::Join { how, .. } if how == "append"));
        assert!(matches!(
            parse("join x.csv id sideways"),
            Err(ExError::Usage(_))
        ));
        let a = parse("derive total = qty * price").unwrap();
        assert!(
            matches!(&a[0], Action::Derive { name, expr } if name == "total" && expr == "qty * price")
        );
        assert!(matches!(
            &parse("sql SELECT * FROM self").unwrap()[0],
            Action::Sql { .. }
        ));
        assert!(matches!(
            &parse("exec-sql DROP TABLE t; SELECT 1").unwrap()[0],
            Action::ExecSql { sql } if sql == "DROP TABLE t; SELECT 1"
        ));
        assert!(matches!(
            &parse("export out.py").unwrap()[0],
            Action::Export { .. }
        ));
        assert!(matches!(&parse("setcol 0").unwrap()[0], Action::SetCol { value } if value == "0"));
        for (cmd, want) in [
            ("uniform", Action::HideUniform),
            ("dup", Action::DupSheet),
            ("describe", Action::Describe),
            ("undo", Action::Undo),
            ("redo", Action::Redo),
        ] {
            assert_eq!(parse(cmd).unwrap(), vec![want]);
        }
    }

    /// Every command docs/index.html documents (`data-ex="…"`) exists.
    #[test]
    fn user_guide_commands_exist() {
        let html = include_str!("../docs/index.html");
        let documented: Vec<&str> = html
            .split("data-ex=\"")
            .skip(1)
            .filter_map(|s| s.split('"').next())
            .collect();
        assert!(documented.len() >= 25, "{documented:?}");
        for cmd in documented {
            assert!(
                !matches!(parse(cmd), Err(ExError::Unknown(_))),
                "documented :{cmd} doesn't exist"
            );
        }
    }

    #[test]
    fn rejects_bad_usage() {
        assert!(matches!(parse("filter justone"), Err(ExError::Usage(_))));
        assert!(matches!(parse("sort"), Err(ExError::Usage(_))));
    }

    #[test]
    fn join_append_needs_no_key_argument() {
        let [Action::Join { keys, how, .. }] = &parse("join b.csv append").unwrap()[..] else {
            panic!("not a join");
        };
        assert!(keys.is_empty());
        assert_eq!(how, "append");
    }
}
