//! Selection, registers and the clipboard, and row and cell edits.

use super::*;

impl App {
    /// Row id under the cursor; None on an empty view.
    pub(in crate::app) fn rid_at_cursor(&self) -> Option<u64> {
        let s = self.sheets.active();
        s.window_rids().get(s.view.row_cursor).copied()
    }

    pub(super) fn toggle_select_current(&mut self) {
        let Some(rid) = self.rid_at_cursor() else {
            return;
        };
        let selected = &mut self.sheets.active_mut().view.selected;
        let selected = std::sync::Arc::make_mut(selected);
        if !selected.remove(&rid) {
            selected.insert(rid);
        }
    }

    pub(super) fn set_select_current(&mut self, select: bool) {
        let Some(rid) = self.rid_at_cursor() else {
            return;
        };
        let selected = &mut self.sheets.active_mut().view.selected;
        let selected = std::sync::Arc::make_mut(selected);
        if select {
            selected.insert(rid);
        } else {
            selected.remove(&rid);
        }
    }

    /// Select every row of the view (not just the loaded window).
    pub(super) fn select_all(&mut self) {
        let sheet = self.sheets.active().clone();
        // ponytail: one set entry per row; swap BTreeSet for a bitmap if
        // multi-million-row selections eat too much memory.
        self.spawn_job("select all", move || {
            let rids = sheet.view_rids().map_err(|e| e.to_string())?;
            let note = format!("selected {} rows", rids.len());
            Ok(JobDone::Select { rids, note })
        });
    }

    /// Select view rows whose current-column text matches `pat` (whole view).
    pub(super) fn select_by_regex(&mut self, pat: &str) {
        if let Err(e) = regex::Regex::new(pat) {
            self.toast = Some(format!("bad regex: {e}"));
            return;
        }
        let Some(column) = self.current_column_name() else {
            return;
        };
        let sheet = self.sheets.active().clone();
        let pat = pat.to_string();
        self.spawn_job("select", move || {
            let rids = sheet
                .rows_matching(&column, &pat)
                .map_err(|e| e.to_string())?;
            let note = format!("selected {} rows matching /{pat}", rids.len());
            Ok(JobDone::Select { rids, note })
        });
    }

    /// `:select` / `:unselect` / `z|` / `z\`: rows where a SQL predicate holds.
    pub(super) fn select_by_expr(&mut self, expr: String, unselect: bool) {
        let sheet = self.sheets.active().clone();
        self.spawn_job("select", move || {
            let rids = sheet.rids_where(&expr)?;
            let n = rids.len();
            Ok(if unselect {
                JobDone::Unselect {
                    rids,
                    note: format!("unselected {n} rows where {expr}"),
                }
            } else {
                JobDone::Select {
                    rids,
                    note: format!("selected {n} rows where {expr}"),
                }
            })
        });
    }

    /// `y` `gy` `zy` `gzy`: the cursor row or the selected rows, every column
    /// or (`cells`) only the cursor column, into the register.
    /// `yank`: a copy, else a cut (registers `0` vs `1`–`9`). Register `+`
    /// (`"+y`) is the system clipboard.
    pub(super) fn copy(&mut self, selected: bool, cells: bool, yank: bool) -> bool {
        match self.register_prefix.take() {
            Some('+') => {
                self.spawn_copy(selected, cells, Some(self.clipboard.clone()), (None, yank))
            }
            to => self.spawn_copy(selected, cells, None, (to.filter(|c| *c != '"'), yank)),
        }
    }

    /// A copy landed: the unnamed register, `0` (yank) or the `1`–`9` ring
    /// (cut), and the named one `"x` asked for.
    pub(in crate::app) fn store_register(&mut self, r: Register, (to, yank): (Option<char>, bool)) {
        let regs = &mut self.registers;
        if yank {
            regs.insert('0', r.clone());
        } else {
            for d in (b'1'..b'9').rev() {
                if let Some(old) = regs.remove(&(d as char)) {
                    regs.insert((d + 1) as char, old);
                }
            }
            regs.insert('1', r.clone());
        }
        if let Some(c) = to {
            regs.insert(c, r.clone());
        }
        self.register = Some(r);
    }

    /// `Y` …: visible columns as TSV (rows) or one value per line (cells).
    pub(super) fn syscopy(&mut self, selected: bool, cells: bool) {
        self.spawn_copy(selected, cells, Some(self.clipboard.clone()), (None, true));
    }

    /// Register copy, or with `clip` the same rows as text to the system clipboard.
    pub(super) fn spawn_copy(
        &mut self,
        selected: bool,
        cells: bool,
        clip: Option<crate::clipboard::Clipboard>,
        to: (Option<char>, bool),
    ) -> bool {
        // ponytail: the register holds the copy in memory; refuse past 10M cells, raise on demand.
        const MAX_CELLS: usize = 10_000_000;
        let s = self.sheets.active();
        let Ok(schema) = s.view_plan().collect_schema() else {
            return false;
        };
        let rows = if selected { s.view.selected.len() } else { 1 };
        if rows == 0 {
            self.toast = Some("no rows selected".into());
            return false;
        }
        let width = if cells { 1 } else { schema.len() };
        if rows * width > MAX_CELLS {
            self.toast = Some(format!(
                "copy: {} cells is over the {} limit",
                grouped((rows * width) as u64),
                grouped(MAX_CELLS as u64)
            ));
            return false;
        }
        let column = match self.current_column_name() {
            Some(c) => c,
            None if cells => return false,
            None => String::new(),
        };
        let pos = (!selected).then(|| s.view.window_offset + s.view.row_cursor as u64);
        let sheet = s.clone();
        let visible: Vec<PlSmallStr> = schema.iter_names().cloned().collect();
        self.spawn_job("copy", move || {
            let mut df = sheet
                .copy_rows(pos, clip.is_none())
                .map_err(|e| e.to_string())?;
            if let Some(clip) = clip {
                let (text, n, what) = if cells {
                    let c = df.column(&column).map_err(|e| e.to_string())?;
                    (crate::clipboard::lines(c), c.len(), "cell")
                } else {
                    let df = df.select(visible).map_err(|e| e.to_string())?;
                    (crate::clipboard::tsv(&df), df.height(), "row")
                };
                if n == 0 {
                    return Ok(JobDone::Toast("nothing to copy".into()));
                }
                let osc52 = clip.send(&text).map_err(|e| format!("clipboard: {e}"))?;
                let s = if n == 1 { "" } else { "s" };
                let note = format!("copied {} {what}{s} to clipboard", grouped(n as u64));
                return Ok(JobDone::SysCopy { note, osc52 });
            }
            let kind = if cells {
                df = df.select([column.as_str()]).map_err(|e| e.to_string())?;
                RegisterKind::Cells
            } else {
                RegisterKind::Rows
            };
            Ok(JobDone::Copy(Box::new(Register { kind, df }), to))
        });
        true
    }

    /// `zp` / `gzp`: the copied cells (`zy` `gzy`) into the current column.
    /// `zP` `gzP`: read the clipboard in the background, or take the last
    /// terminal paste when there's no paste command (SSH).
    pub(super) fn syspaste(&mut self, selected: bool) {
        if let Some(argv) = self.paste_cmd.clone() {
            self.spawn_job("paste", move || {
                let text = crate::clipboard::read(&argv)?;
                Ok(JobDone::SysPaste { text, selected })
            });
        } else if let Some(text) = self.pasted.clone() {
            self.paste_text(&text, selected);
        } else {
            self.toast = Some("nothing pasted: paste with the terminal first".into());
        }
    }

    /// Lines of `text` into the active sheet: from the cursor row down, or
    /// into the selected rows; fields from the cursor column rightwards.
    pub(in crate::app) fn paste_text(&mut self, text: &str, selected: bool) {
        let lines = crate::clipboard::fields(text);
        if lines.is_empty() {
            self.toast = Some("nothing to paste from the clipboard".into());
            return;
        }
        let s = self.sheets.active();
        let cols: Vec<String> = self
            .visible_cols()
            .into_iter()
            .skip(s.view.col_cursor)
            .collect();
        let rids = if selected {
            match s.selected_in_view_order() {
                Ok(r) if r.is_empty() => {
                    self.toast = Some("no rows selected".into());
                    return;
                }
                Ok(r) => r,
                Err(e) => {
                    self.toast = Some(format!("paste: {e}"));
                    return;
                }
            }
        } else {
            let at = s.view.window_offset + s.view.row_cursor as u64;
            s.view_rids_at(at, lines.len()).unwrap_or_default()
        };
        match self.sheets.active_mut().paste_region(&cols, rids, &lines) {
            Ok(n) => {
                let s = if n == 1 { "" } else { "s" };
                self.toast = Some(format!("pasted {} cell{s}", grouped(n as u64)));
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("paste: {e}")),
        }
    }

    pub(super) fn paste_cells(&mut self, selected: bool) {
        let Some(column) = self.current_column_name() else {
            return;
        };
        let values = match &self.register {
            Some(Register {
                kind: RegisterKind::Cells,
                df,
            }) if df.height() > 0 => df.columns()[0].clone(),
            Some(Register {
                kind: RegisterKind::Rows,
                ..
            }) => {
                self.toast = Some("zp pastes cells: copy with zy or gzy".into());
                return;
            }
            _ => {
                self.toast = Some("nothing copied".into());
                return;
            }
        };
        let done = if selected {
            if self.sheets.active().view.selected.is_empty() {
                self.toast = Some("paste: no rows selected".into());
                return;
            }
            self.scan_edit(
                "paste",
                |s| s.selected_in_view_order().map_err(|e| e.to_string()),
                move |app, rids| {
                    let s = app.sheets.active_mut();
                    let done = s
                        .paste_cells_with(&column, &values, rids)
                        .map(|n| format!("pasted into {column} on {} rows", grouped(n as u64)));
                    app.edit_done(done, "paste: ");
                },
            );
            return;
        } else {
            let Some(rid) = self.rid_at_cursor() else {
                return;
            };
            let text = match values.get(0) {
                Ok(v) if !v.is_null() => crate::sheet::any_text(&v),
                _ => String::new(),
            };
            self.sheets
                .active_mut()
                .edit_cell(&column, rid, &text)
                .map(|()| String::new())
        };
        self.edit_done(done, "paste: ");
    }

    /// `zx` / `gzx` (`copy`): copy the cell(s) to the register, then null
    /// them; `zd` / `gzd`: just null them.
    pub(super) fn clear_cells(&mut self, selected: bool, copy: bool) {
        let s = self.sheets.active();
        if let Err(e) = s.editable() {
            self.toast = Some(e);
            return;
        }
        let Some(column) = self.current_column_name() else {
            return;
        };
        if selected && s.view.selected.is_empty() {
            self.toast = Some("no rows selected".into());
            return;
        }
        let rid = self.rid_at_cursor();
        // The copy job clones the sheet now, before the edit below; a refused
        // copy (toast says why) cuts nothing.
        if copy && !self.copy(selected, true, false) {
            return;
        }
        let s = self.sheets.active_mut();
        let done = if selected {
            s.set_col(&column, "").map(drop)
        } else {
            let Some(rid) = rid else { return };
            s.edit_cell(&column, rid, "")
        };
        match done {
            Ok(()) => self.spawn_materialize(),
            Err(e) => self.toast = Some(e),
        }
    }

    /// `d` `gd` `x` `gx`: the cursor row stays put, so the next row moves up
    /// under it. `cut` copies the rows first; a refused copy deletes nothing.
    pub(super) fn delete_rows(&mut self, selected: bool, cut: bool) {
        let s = self.sheets.active();
        if let Err(e) = s.editable() {
            self.toast = Some(e);
            return;
        }
        let rids: Vec<u64> = if selected {
            s.view.selected.iter().copied().collect()
        } else {
            self.rid_at_cursor().into_iter().collect()
        };
        if rids.is_empty() {
            self.toast = Some(
                if selected {
                    "no rows selected"
                } else {
                    "no rows"
                }
                .into(),
            );
            return;
        }
        // The copy job clones the sheet now, before the delete.
        if cut && !self.copy(selected, false, false) {
            return;
        }
        match self.sheets.active_mut().delete_rows(rids) {
            Ok(n) => {
                if !cut {
                    let s = if n == 1 { "" } else { "s" };
                    self.toast = Some(format!("deleted {} row{s}", grouped(n as u64)));
                }
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(e),
        }
    }

    /// `a` `ga` `p` `P`: rows after (or before) the cursor row; the cursor
    /// moves to the first new row.
    pub(super) fn add_rows(&mut self, n: usize, values: Option<DataFrame>, after: bool) {
        let anchor = self.rid_at_cursor();
        let s = self.sheets.active_mut();
        let ids = match s.add_rows(anchor, after, values.as_ref(), n) {
            Ok(ids) => ids,
            Err(e) => {
                self.toast = Some(e);
                return;
            }
        };
        let k = ids.len();
        if values.is_some() {
            let s = if k == 1 { "" } else { "s" };
            self.toast = Some(format!("pasted {} row{s}", grouped(k as u64)));
        }
        self.spawn_materialize();
        let first = ids[0];
        self.scan_edit(
            "cursor to new row",
            move |s| s.view_index_of(first).map_err(|e| e.to_string()),
            |app, at| {
                if let Some(at) = at {
                    let s = app.sheets.active_mut();
                    let half = s.view.window_rows as u64 / 2;
                    s.view.window_offset = at.saturating_sub(half);
                    s.view.row_cursor = (at - s.view.window_offset) as usize;
                    app.spawn_materialize();
                }
            },
        );
    }

    pub(super) fn open_dup_selected(&mut self) {
        if self.sheets.active().view.selected.is_empty() {
            self.toast = Some("dup: no rows selected".into());
            return;
        }
        let id = self.sheets.alloc_id();
        let parent = self.sheets.active().clone();
        self.spawn_job("dup", move || {
            Ok(JobDone::PushSheet(Box::new(Sheet::dup_selected_from(
                &parent, id,
            )?)))
        });
    }
}
