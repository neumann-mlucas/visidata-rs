//! Cursor motion, scrolling, jumps and search hits.

use super::*;

impl App {
    pub(super) fn hits(&mut self, needle: String) -> ControlFlow<()> {
        if let Err(e) = regex::Regex::new(&needle) {
            self.toast = Some(format!("bad regex: {e}"));
            return Break(());
        }
        let parent = self.sheets.active().clone();
        let id = self.sheets.alloc_id();
        self.spawn_job("hits", move || {
            // ponytail: fixed cap; raise when someone needs more
            const CAP: usize = 100_000;
            let (s, capped) = Sheet::hits_from(&parent, id, &needle, CAP)?;
            if capped {
                return Ok(JobDone::PushSheetToast(
                    Box::new(s),
                    format!("{CAP}+ hits (capped)"),
                ));
            }
            Ok(JobDone::PushSheet(Box::new(s)))
        });
        Continue(())
    }

    pub(super) fn screen_row(&mut self, at: u8) {
        let rows = self.viewport_rows;
        let s = self.sheets.active_mut();
        let loaded = s.window().map_or(0, |w| w.height());
        let last = (s.view.row_offset + rows).min(loaded).saturating_sub(1);
        s.view.row_cursor = match at {
            0 => s.view.row_offset,
            1 => (s.view.row_offset + last) / 2,
            _ => last,
        };
    }

    pub(super) fn scroll_to(&mut self, at: u8) {
        let rows = self.viewport_rows;
        let v = &mut self.sheets.active_mut().view;
        let back = match at {
            0 => 0,
            1 => rows / 2,
            _ => rows.saturating_sub(1),
        };
        v.row_offset = v.row_cursor.saturating_sub(back);
    }

    pub(super) fn cell_motion(&mut self, to: i8) -> ControlFlow<()> {
        let s = self.sheets.active_mut();
        let Some(df) = s.window() else {
            return Break(());
        };
        let (r, c) = (s.view.row_cursor, s.view.col_cursor);
        let full = |i: &usize| df.columns()[*i].get(r).is_ok_and(|v| !v.is_null());
        let n = df.width();
        let hit = match to {
            1 => (c + 1..n).find(full),
            -1 => (0..c).rev().find(full),
            _ => (0..n).rev().find(full),
        };
        match hit {
            Some(i) => s.view.col_cursor = i,
            None => self.toast = Some("no other non-empty cell in this row".into()),
        }
        Continue(())
    }

    pub(super) fn value_change(&mut self, forward: bool, action: Action) -> ControlFlow<()> {
        // ponytail: the loaded rows only; a job over the view if it matters
        let null = matches!(action, Action::NullChange { .. });
        let s = self.sheets.active_mut();
        let Some(df) = s.window() else {
            return Break(());
        };
        let (r, c) = (s.view.row_cursor, s.view.col_cursor);
        let Some(col) = df.columns().get(c) else {
            return Break(());
        };
        let here = col.get(r).map(|v| v.into_static()).ok();
        let hit = |i: &usize| match null {
            true => col.get(*i).is_ok_and(|v| v.is_null()),
            false => col.get(*i).map(|v| v.into_static()).ok() != here,
        };
        let hit = match forward {
            true => (r + 1..df.height()).find(hit),
            false => (0..r).rev().find(hit),
        };
        match hit {
            Some(i) => s.view.row_cursor = i,
            None if null => self.toast = Some("no null in the loaded rows".into()),
            None => self.toast = Some("no other value in the loaded rows".into()),
        }
        Continue(())
    }

    pub(super) fn goto_selected(&mut self, forward: bool) {
        let at = self.cursor_row();
        let col = self.sheets.active().view.col_cursor;
        let sheet = self.sheets.active().clone();
        self.spawn_job("go to selected", move || {
            let (row, _) = sheet
                .rids_to_selected(at, forward)
                .map_err(|e| e.to_string())?
                .ok_or("no selected row")?;
            Ok(JobDone::Jump { row, col })
        });
    }

    pub(super) fn search_cell(&mut self, backward: bool) -> ControlFlow<()> {
        let s = self.sheets.active();
        let Some(v) = self.current_column_name().and_then(|c| s.cursor_value(&c)) else {
            return Break(());
        };
        let text = crate::sheet::any_text(&v);
        self.start_search(SearchState {
            pattern: format!("^{}$", regex::escape(&text)),
            all_columns: false,
            backward,
            expr: false,
            keys: false,
        });
        Continue(())
    }

    pub(super) fn jump_back(&mut self) {
        let cur = match self.jump_at {
            Some(i) => i,
            None => {
                self.jumps.push(self.here());
                self.jumps.len() - 1
            }
        };
        self.jump_at = Some(cur);
        match cur.checked_sub(1) {
            Some(to) => self.go_jump(to),
            None => self.toast = Some("at the oldest jump".into()),
        }
    }

    pub(super) fn jumps(&mut self) {
        let name = |id| {
            self.sheets
                .iter()
                .find(|s| s.id == id)
                .map(|s| s.name.clone())
        };
        let rows: Vec<Vec<String>> = self
            .jumps
            .iter()
            .enumerate()
            .map(|(i, j)| {
                vec![
                    if self.jump_at == Some(i) { ">" } else { "" }.into(),
                    name(j.sheet).unwrap_or_else(|| "(closed)".into()),
                    (j.row + 1).to_string(),
                    j.column.clone().unwrap_or_default(),
                ]
            })
            .collect();
        let id = self.sheets.alloc_id();
        let names = ["at", "sheet", "row", "column"];
        self.sheets.push(Sheet::listing(id, "jumps", &names, &rows));
        self.spawn_materialize();
    }

    pub(super) fn jump_prev(&mut self) {
        match self.sheets.len() {
            0 | 1 => self.toast = Some("no previous sheet".into()),
            n => {
                let id = self.sheets.iter().nth(n - 2).map(|s| s.id);
                if let Some(id) = id {
                    self.sheets.switch_to(id);
                    self.spawn_materialize();
                }
            }
        }
    }

    pub(super) fn set_cursor(&mut self, row: u64, column: Option<String>) {
        self.goto_row(row - 1);
        if let Some(c) = column {
            if self.sheets.active().has_column(&c) {
                self.goto_column(&c);
            } else {
                self.fail(format!("no column: {c}"));
            }
        }
    }
}
