//! Commands that open derived sheets or move between sheet states.

use super::*;

impl App {
    pub(super) fn ask_pop_sheet(&mut self) {
        let s = self.sheets.active();
        let pending = match s.origin {
            Origin::Macros => self.macro_edits.len(),
            _ => 0,
        };
        match s.unsaved_edits() + pending {
            0 => self.pop_sheet(),
            n => {
                let q = format!("discard {} to {}?", edits(n), s.name);
                self.ask(Confirm::Pop, q);
            }
        }
    }

    pub(super) fn transpose(&mut self) {
        let parent = self.sheets.active().clone();
        let id = self.sheets.alloc_id();
        self.spawn_job("transpose", move || {
            Ok(JobDone::PushSheet(Box::new(Sheet::transpose_from(
                &parent, id,
            )?)))
        });
    }

    pub(super) fn open_cell(&mut self) -> ControlFlow<()> {
        let Some(column) = self.current_column_name() else {
            return Break(());
        };
        let s = self.sheets.active();
        let Some(v) = s.cursor_value(&column).map(|v| v.into_static()) else {
            return Break(());
        };
        // VisiData's `name[rowkey].column`
        let key = (s.window()).map_or(String::new(), |w| {
            s.row_key(w, s.view.row_cursor, self.cursor_row())
        });
        let name = format!("{}[{key}].{column}", s.name);
        let id = self.sheets.alloc_id();
        self.push_or_toast(Sheet::open_cell(id, name, &v));
        Continue(())
    }

    pub(super) fn freq_keys(&mut self) -> ControlFlow<()> {
        if self.sheets.active().view.keys.is_empty() {
            self.toast = Some("there are no key columns to group by".into());
            return Break(());
        }
        let id = self.sheets.alloc_id();
        let parent = self.sheets.active();
        let freq = Sheet::freq_binned(parent, id, &parent.view.keys, None);
        self.sheets.push(freq);
        self.spawn_materialize();
        Continue(())
    }

    pub(super) fn hist(&mut self, bins: Option<u32>) -> ControlFlow<()> {
        let Some(column) = self.current_column_name() else {
            return Break(());
        };
        let parent = self.sheets.active().clone();
        let id = self.sheets.alloc_id();
        self.spawn_job("hist", move || {
            let s = Sheet::hist_from(&parent, id, &column, bins.unwrap_or(20))?;
            Ok(JobDone::PushSheet(Box::new(s)))
        });
        Continue(())
    }

    pub(super) fn link(&mut self, on: bool) -> ControlFlow<()> {
        let s = self.sheets.active_mut();
        if on && s.state.lineage.is_none() {
            self.toast = Some("link: not a derived sheet".into());
            return Break(());
        }
        s.state.linked = on;
        self.toast = Some(
            if on {
                "linked: refreshes when its parent changes"
            } else {
                "unlinked"
            }
            .into(),
        );
        Continue(())
    }

    pub(super) fn dedupe_rows(&mut self) {
        let id = self.sheets.alloc_id();
        let parent = self.sheets.active().clone();
        self.spawn_job("dedupe", move || {
            Ok(JobDone::PushSheet(Box::new(Sheet::dedupe_from(
                &parent, id,
            )?)))
        });
    }

    pub(super) fn describe(&mut self) {
        let id = self.sheets.alloc_id();
        let parent = self.sheets.active().clone();
        self.spawn_job("describe", move || {
            Ok(JobDone::PushSheet(Box::new(Sheet::describe_from(
                &parent, id,
            )?)))
        });
    }

    pub(super) fn pivot_current(&mut self) -> ControlFlow<()> {
        let Some(on) = self.current_column_name() else {
            return Break(());
        };
        let id = self.sheets.alloc_id();
        let parent = self.sheets.active().clone();
        let keys = parent.view.keys.clone();
        let aggs = crate::sheet::columns::freq_aggregators(&parent);
        self.spawn_job("pivot", move || {
            let child = Sheet::pivot_current_from(&parent, id, &keys, &on, &aggs)?;
            Ok(JobDone::PushSheet(Box::new(child)))
        });
        Continue(())
    }

    pub(super) fn time_walk(&mut self, n: i64) {
        if self.sheets.active_mut().time_walk(n) {
            let at = self.sheets.active().state.undo.current();
            self.toast = Some(format!("at state {at}"));
            self.spawn_materialize();
        } else {
            self.toast = Some(format!(
                "at the {} state",
                if n > 0 { "oldest" } else { "newest" }
            ));
        }
    }
}
