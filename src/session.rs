//! `:mksession` (S-29 C): the sheet stack as an ex script `:source` replays.
//!
//! One block per sheet, bottom first. A file sheet starts with `open`; a sheet
//! derived from the one below it starts with the command that derived it, run
//! on that sheet. Then its edits (plan steps), then its view: casts,
//! aggregators, filters, sort, hidden and key columns, frozen columns,
//! selection, cursor. What has no ex form is a `#` comment saying so.

use std::path::{Path, PathBuf};

use polars::prelude::DataType;

use crate::action::Action;
use crate::sheet::{Derivation, Origin, PlanStep, Sheet, SortOrder};

/// Selections bigger than this are saved as a comment with their size.
// ponytail: ids inline in the script; a sidecar file if bigger ones matter.
const MAX_SELECTED: usize = 100_000;

/// Where `:mksession` writes without a path: `$XDG_STATE_HOME/vdrs/session.vdrs`
/// (a temp dir under `cfg!(test)`).
pub fn default_path() -> PathBuf {
    if cfg!(test) {
        let dir = std::env::temp_dir().join(format!("vdrs-test-{}", std::process::id()));
        return dir.join("session.vdrs");
    }
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("vdrs").join("session.vdrs")
}

/// `p` absolute, so a session replays from any directory.
fn abs(p: &Path) -> String {
    std::fs::canonicalize(p)
        .unwrap_or_else(|_| p.to_path_buf())
        .display()
        .to_string()
}

/// A line that starts a sheet: its file, and whether it's a `tail`.
pub fn root(line: &str) -> Option<(String, bool)> {
    let line = line.trim().trim_start_matches(':');
    match crate::exline::parse(line).ok()?.as_slice() {
        [Action::Open { path }] => Some((path.clone(), false)),
        [Action::Tail { path, .. }] => Some((path.clone(), true)),
        _ => None,
    }
}

/// The file `vdrs -S` starts on: the first sheet's, when it's an `open`.
pub fn first_root(text: &str) -> Result<String, &'static str> {
    match text.lines().find_map(root) {
        Some((path, false)) => Ok(path),
        Some((_, true)) => Err("the first sheet is a tail: :source the session instead"),
        None => Err("no :open line, nothing to restore"),
    }
}

/// Write the session for `sheets` (bottom first) to `path`; pasted cells and
/// added rows go to parquet files in `<path>.d/`.
pub fn save<'a>(sheets: impl Iterator<Item = &'a Sheet>, path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let data = PathBuf::from(format!("{}.d", path.display()));
    std::fs::write(path, script(sheets, Some(&data)))
}

/// The ex script for a stack of sheets, bottom first. `data`: where step
/// data goes (`None`: those steps are comments).
pub fn script<'a>(sheets: impl Iterator<Item = &'a Sheet>, data: Option<&Path>) -> String {
    let mut out = String::from("# vdrs session: :source this file, or vdrs -S <file>\n");
    // the previous sheet, when its block was written (derived ones chain on it)
    let mut below: Option<&Sheet> = None;
    for (i, s) in sheets.enumerate() {
        out.push('\n');
        let mut b = Block {
            lines: Vec::new(),
            data: data.map(|d| (d.to_path_buf(), i)),
        };
        let started = match (&s.state.lineage, below) {
            (Some(l), Some(f)) if dived_from(l, f) => b.dive(l),
            (Some(l), Some(p)) if l.parent.id == p.id => {
                if !same_state(&l.parent, p) {
                    b.note(&format!(
                        "{} changed after this sheet was made from it; replay uses it as it is now",
                        p.name
                    ));
                }
                b.derivation(&l.how)
            }
            _ => b.root(s),
        };
        if !started {
            out += &b.lines.join("\n");
            out.push('\n');
            below = None;
            continue;
        }
        b.steps(s);
        b.view(s);
        out += &b.lines.join("\n");
        out.push('\n');
        below = Some(s);
    }
    out
}

/// The parent's plan and view as the child saw them.
fn same_state(then: &Sheet, now: &Sheet) -> bool {
    let (a, b) = (&then.view, &now.view);
    then.state.steps == now.state.steps
        && a.sort == b.sort
        && a.filter == b.filter
        && a.filter_expr == b.filter_expr
        && a.type_casts == b.type_casts
        && a.hidden == b.hidden
        && a.keys == b.keys
}

/// A freq dive (`<CR>` on a frequency row) whose freq sheet is `below`.
fn dived_from(l: &crate::sheet::Lineage, below: &Sheet) -> bool {
    let (Derivation::Values { column, .. }, Some(fl)) = (&l.how, &below.state.lineage) else {
        return false;
    };
    matches!(&fl.how, Derivation::Freq { column: c } if c == column) && fl.parent.id == l.parent.id
}

struct Block {
    lines: Vec<String>,
    /// Folder for step data, and this sheet's index in the stack.
    data: Option<(PathBuf, usize)>,
}

impl Block {
    /// The freq sheet's rows holding `values` selected, then dived into.
    fn dive(&mut self, l: &crate::sheet::Lineage) -> bool {
        let Derivation::Values { column, values } = &l.how else {
            return false;
        };
        let c = crate::duckdb::ident(column);
        let texts: Vec<String> = values
            .iter()
            .flatten()
            .map(|v| crate::duckdb::lit(v))
            .collect();
        let mut terms = Vec::new();
        if !texts.is_empty() {
            terms.push(format!("CAST({c} AS VARCHAR) IN ({})", texts.join(", ")));
        }
        if values.iter().any(Option::is_none) {
            terms.push(format!("{c} IS NULL"));
        }
        self.ex(Action::UnselectAll);
        self.ex(Action::SelectExpr {
            expr: terms.join(" OR "),
            unselect: false,
        });
        self.ex(Action::SubmitSelected);
        true
    }

    /// `df` to `<data>/<sheet>-<n>.parquet`; its path, or why not.
    fn sidecar(&mut self, n: usize, df: &polars::prelude::DataFrame) -> Result<String, String> {
        let Some((dir, sheet)) = &self.data else {
            return Err("no session file".into());
        };
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let path = dir.join(format!("{sheet}-{n}.parquet"));
        crate::sheet::write_df_by_extension(&mut df.clone(), &path).map_err(|e| e.to_string())?;
        Ok(path.display().to_string())
    }

    fn ex(&mut self, a: Action) {
        match a.to_ex() {
            Some(ex) => self.lines.push(ex),
            None => self.note(&format!("{a:?} has no ex form")),
        }
    }

    fn note(&mut self, why: &str) {
        self.lines.push(format!("# not saved: {why}"));
    }

    fn col(&mut self, name: &str) {
        self.ex(Action::SetCursor {
            row: 1,
            column: Some(name.to_string()),
        });
    }

    fn select(&mut self, ids: impl Iterator<Item = u64>) {
        self.ex(Action::UnselectAll);
        let ids: Vec<u64> = ids.collect();
        if !ids.is_empty() {
            self.ex(Action::SelectIds { ids });
        }
    }

    /// A sheet read from a file; false: it can't be reopened.
    fn root(&mut self, s: &Sheet) -> bool {
        let path = s.source.path.as_deref().map(abs);
        match (&s.origin, path) {
            (Origin::File | Origin::Parts { .. }, Some(path)) => {
                self.ex(Action::Open { path });
                true
            }
            (Origin::Tail { path, follow }, _) => {
                self.ex(Action::Tail {
                    path: abs(path),
                    follow: *follow,
                });
                true
            }
            (o, _) => {
                self.note(&format!(
                    "{} ({} sheet, not made from the sheet above)",
                    s.name,
                    o.name()
                ));
                false
            }
        }
    }

    /// The command that made a sheet from the one above; false: none.
    fn derivation(&mut self, how: &Derivation) -> bool {
        let file = |s: &Sheet| match (&s.origin, &s.source.path) {
            (Origin::File, Some(p)) if s.state.steps.is_empty() => Some(abs(p)),
            _ => None,
        };
        match how {
            Derivation::Copy => self.ex(Action::DupSheet),
            Derivation::Freq { column } => {
                self.col(column);
                self.ex(Action::OpenFreqTable);
            }
            Derivation::Rids(ids) => {
                self.select(ids.iter().copied());
                self.ex(Action::DupSelected);
            }
            Derivation::Sql { query, others } if others.is_empty() => self.ex(Action::Sql {
                query: query.clone(),
            }),
            Derivation::Join { right, keys, how } if file(right).is_some() => {
                self.ex(Action::Join {
                    source: file(right).unwrap_or_default(),
                    keys: keys.clone(),
                    how: how.clone(),
                })
            }
            Derivation::Unpivot { on, index } => self.ex(Action::Unpivot {
                on: on.clone(),
                index: index.clone(),
            }),
            Derivation::Pivot {
                index,
                on,
                values,
                agg,
            } => self.ex(Action::Pivot {
                index: index.clone(),
                on: on.clone(),
                values: values.clone(),
                agg: agg.clone(),
            }),
            Derivation::PivotCount { on, .. } => {
                self.col(on);
                self.ex(Action::PivotCurrent);
            }
            Derivation::Describe => self.ex(Action::Describe),
            Derivation::Unfurl { column } => {
                self.col(column);
                self.ex(Action::UnfurlCol);
            }
            Derivation::Diff { other, keys } if file(other).is_some() => self.ex(Action::Diff {
                path: file(other).unwrap_or_default(),
                keys: keys.clone(),
            }),
            Derivation::Values { .. } => {
                // a dive right above its freq sheet is saved (`Block::dive`);
                // ponytail: one whose freq sheet was closed isn't
                self.note("a sheet opened from a frequency row (<CR>)");
                return false;
            }
            _ => {
                self.note("a sheet made from another sheet or an edited file");
                return false;
            }
        }
        true
    }

    fn steps(&mut self, s: &Sheet) {
        for (n, step) in s.state.steps.iter().enumerate() {
            match step {
                PlanStep::Rename { old, new } => {
                    self.col(old);
                    self.ex(Action::RenameCol {
                        new_name: new.clone(),
                    });
                }
                PlanStep::Currency { column } => {
                    self.col(column);
                    self.ex(Action::TypeCastCurrency);
                }
                PlanStep::Derive { name, expr, .. } => self.ex(Action::Derive {
                    name: name.clone(),
                    expr: expr.clone(),
                }),
                PlanStep::Json { column, .. } => {
                    self.col(column);
                    self.ex(Action::TypeCastJson);
                }
                PlanStep::SetCol {
                    column,
                    value,
                    rids,
                    ..
                } => {
                    self.select(rids.iter().copied());
                    self.col(column);
                    self.ex(Action::SetCol {
                        value: value.clone().unwrap_or_default(),
                    });
                }
                PlanStep::SetExpr {
                    column, expr, rids, ..
                } => {
                    self.select(rids.iter().copied());
                    self.col(column);
                    self.ex(Action::SetExpr { expr: expr.clone() });
                }
                PlanStep::Replace {
                    column,
                    pattern,
                    replacement,
                    all,
                    rids,
                } => {
                    self.select(rids.iter().flat_map(|r| r.iter().copied()));
                    self.col(column);
                    self.ex(Action::Substitute {
                        pattern: pattern.clone(),
                        replacement: replacement.clone(),
                        all: *all,
                    });
                }
                PlanStep::DeleteRows { rids } => {
                    self.select(rids.iter().copied());
                    self.ex(Action::DeleteSelected);
                }
                PlanStep::SetCells { column, patch } => match self.sidecar(n, patch) {
                    Ok(path) => self.ex(Action::Patch {
                        column: Some(column.clone()),
                        path,
                    }),
                    Err(e) => self.note(&format!("{} cells in {column} ({e})", patch.height())),
                },
                PlanStep::AddRows { rows } => match self.sidecar(n, rows) {
                    Ok(path) => self.ex(Action::Patch { column: None, path }),
                    Err(e) => self.note(&format!("{} added rows ({e})", rows.height())),
                },
                PlanStep::Capture {
                    column, pattern, ..
                } => {
                    self.col(column);
                    self.ex(Action::Capture {
                        pattern: pattern.clone(),
                    });
                }
                PlanStep::Expand { column, fields, .. } => {
                    self.col(column);
                    let depth = fields.iter().map(|(p, _)| p.len()).max().unwrap_or(1);
                    self.ex(Action::ExpandCol { depth });
                }
            }
        }
    }

    fn view(&mut self, s: &Sheet) {
        let v = &s.view;
        for (column, dtype) in &v.type_casts {
            let cast = match dtype {
                DataType::Int64 => Action::TypeCastInt,
                DataType::Float64 => Action::TypeCastFloat,
                DataType::String => Action::TypeCastString,
                DataType::Date => Action::TypeCastDate,
                other => {
                    self.note(&format!("{column} as {other}"));
                    continue;
                }
            };
            self.col(column);
            self.ex(cast);
        }
        for (column, agg) in &v.aggregators {
            self.col(column);
            self.ex(Action::AggregateCol { names: agg.clone() });
        }
        for (column, d) in &v.precision {
            self.col(column);
            self.ex(Action::SetPrecision { digits: Some(*d) });
        }
        if let Some(f) = &v.filter {
            self.ex(Action::SetFilter {
                column: f.column.clone(),
                regex: f.regex.clone(),
            });
        }
        if let Some(e) = &v.filter_expr {
            self.ex(Action::SetFilterExpr {
                expr: Some(e.clone()),
            });
        }
        for f in &v.formats {
            self.ex(Action::Color {
                column: f.column.clone(),
                pred: f.pred.clone(),
                style: f.style.clone(),
            });
        }
        // the range is computed again when the session loads
        for r in &v.scales {
            let column = r.column.clone();
            self.ex(match &r.colors {
                Some(c) => Action::Scale {
                    column,
                    colors: Some(c.clone()),
                },
                None => Action::DataBar { column },
            });
        }
        for k in &v.sort {
            self.ex(Action::SetSort {
                column: k.column.clone(),
                descending: k.order == SortOrder::Desc,
            });
        }
        for k in &v.keys {
            self.col(k);
            self.ex(Action::KeyCol);
        }
        for h in &v.hidden {
            self.ex(Action::HideNamed { column: h.clone() });
        }
        let visible: Vec<String> = s
            .view_plan()
            .collect_schema()
            .map(|sc| sc.iter_names().map(|n| n.to_string()).collect())
            .unwrap_or_default();
        if let Some(c) = v.frozen_left.checked_sub(1).and_then(|i| visible.get(i)) {
            self.col(c);
            self.ex(Action::FreezeColumn);
        }
        if v.selected.len() > MAX_SELECTED {
            self.note(&format!(
                "{} selected rows (over {MAX_SELECTED})",
                v.selected.len()
            ));
        } else if !v.selected.is_empty() || !s.state.steps.is_empty() {
            self.select(v.selected.iter().copied());
        }
        self.ex(Action::SetCursor {
            row: v.window_offset + v.row_cursor as u64 + 1,
            column: visible.get(v.col_cursor).cloned(),
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn first_root_is_the_first_sheet_line() {
        use super::first_root;
        assert_eq!(
            first_root("# s\n:open /a.csv\n:tail /b.log\n"),
            Ok("/a.csv".into())
        );
        assert!(first_root(":tail /b.log\n:open /a.csv\n").is_err());
        assert!(first_root(":sort -x\n").is_err());
    }
}
