//! Column commands: rename, cast, key, add, fill, slide, aggregate, resize.

use super::*;

impl App {
    pub(super) fn freeze_col(&mut self) -> ControlFlow<()> {
        let Some(c) = self.current_column_name() else {
            return Break(());
        };
        let done = self.sheets.active_mut().freeze_col(&c).map(|name| {
            self.place_new_column(&name, Some(&c));
            format!("added {name}")
        });
        self.edit_done(done, "freeze-col: ");
        Continue(())
    }

    pub(super) fn key_col(&mut self) {
        if let Some(c) = self.current_column_name() {
            let s = self.sheets.active_mut();
            let keys = &mut s.view.keys;
            if let Some(i) = keys.iter().position(|k| k == &c) {
                keys.remove(i);
            } else {
                keys.push(c.clone());
            }
            // Keying reorders columns; keep the cursor on the same column.
            self.goto_column(&c);
            self.spawn_materialize();
        }
    }

    pub(super) fn rename_col(&mut self, new_name: String) -> ControlFlow<()> {
        let Some(old) = self.current_column_name() else {
            return Break(());
        };
        let s = self.sheets.active_mut();
        let before = s.snapshot();
        match s.rename_col(&old, &new_name) {
            Ok(()) => {
                s.push_undo(before);
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(e),
        }
        Continue(())
    }

    pub(super) fn rename_cols_from_rows(&mut self, all: bool, overwrite: bool) {
        let columns = match all {
            true => self.visible_cols(),
            false => self.current_column_name().into_iter().collect(),
        };
        let selected: Vec<u64> = self.sheets.active().view.selected.iter().copied().collect();
        let rids = match selected.is_empty() {
            true => self.rid_at_cursor().into_iter().collect(),
            false => selected,
        };
        self.scan_edit(
            "rename",
            move |s| s.names_from_rows(&rids, &columns, overwrite),
            |app, names| app.rename_columns(names),
        );
    }

    pub(super) fn type_cast_date(&mut self) -> ControlFlow<()> {
        let Some(c) = self.current_column_name() else {
            {
                self.push_type_cast(DataType::Date);
                return Break(());
            }
        };
        let s = self.sheets.active();
        let dt = crate::sheet::date_cast_type(s, &c);
        let all = s
            .row_count()
            .zip(s.window())
            .is_some_and(|(n, w)| n <= w.height() as u64);
        self.push_type_cast(dt.clone());
        if dt == DataType::Date && !all {
            self.toast = Some(format!(
                "{c}: no time in the loaded rows, cast to date; `:type datetime` keeps later times"
            ));
        }
        Continue(())
    }

    pub(super) fn type_cast_json(&mut self) -> ControlFlow<()> {
        let Some(c) = self.current_column_name() else {
            return Break(());
        };
        let c2 = c.clone();
        self.scan_edit(
            "json",
            move |s| s.json_dtype(&c2),
            move |app, dt| match app.sheets.active_mut().json_decode_as(&c, dt) {
                Ok(()) => app.spawn_materialize(),
                Err(e) => app.toast = Some(e),
            },
        );
        Continue(())
    }

    pub(super) fn start_add_col_input(&mut self, action: Action) {
        match self.sheets.active().editable() {
            Err(e) => self.toast = Some(e),
            Ok(()) if matches!(action, Action::StartAddColInput) => {
                self.start_input(Prompt::AddCol)
            }
            Ok(()) => self.start_input(Prompt::Capture),
        }
    }

    pub(super) fn slide_col(&mut self, by: i64) -> ControlFlow<()> {
        let Some(column) = self.current_column_name() else {
            return Break(());
        };
        let s = self.sheets.active_mut();
        let snap = s.snapshot();
        match s.slide_col(&column, by) {
            Ok(Some(at)) => {
                s.push_undo(snap);
                s.view.col_cursor = at;
                self.spawn_materialize();
            }
            Ok(None) => {}
            Err(e) => self.toast = Some(e),
        }
        Continue(())
    }

    pub(super) fn order(&mut self, columns: Vec<String>) -> ControlFlow<()> {
        let s = self.sheets.active_mut();
        if let Some(c) = columns.iter().find(|c| !s.has_column(c)) {
            self.toast = Some(format!("no column: {c}"));
            return Break(());
        }
        let snap = s.snapshot();
        // bare `:order`: back to plan order
        if s.set_order(&columns) {
            s.push_undo(snap);
            self.spawn_materialize();
        }
        Continue(())
    }

    pub(super) fn add_col(&mut self, name: String) {
        let at = self.current_column_name();
        match self.sheets.active_mut().add_empty_col(&name) {
            Ok(()) => {
                self.place_new_column(&name, at.as_deref());
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("addcol: {e}")),
        }
    }

    pub(super) fn add_col_incr(&mut self, action: Action) {
        let at = self.current_column_name();
        let base = self.theme.incr_base;
        let by_key = action == Action::AddColIncrKey;
        self.scan_edit(
            "addcol-incr",
            move |s| match by_key {
                true => s.key_run_numbers(base),
                false => s.view_order_rids().map(|r| {
                    let n = r.len() as i64;
                    (r, (0..n).map(|i| base as i64 + i).collect())
                }),
            },
            move |app, (rids, nums)| match app.sheets.active_mut().add_incr_col_with(rids, nums) {
                Ok(name) => {
                    app.place_new_column(&name, at.as_deref());
                    app.spawn_materialize();
                }
                Err(e) => app.toast = Some(format!("addcol-incr: {e}")),
            },
        );
    }

    pub(super) fn set_col_fill(&mut self, action: Action) -> ControlFlow<()> {
        let Some(column) = self.current_column_name() else {
            return Break(());
        };
        let base = self.theme.incr_base;
        match &action {
            Action::SetColIncr if self.sheets.active().view.selected.is_empty() => {
                self.toast = Some("no rows selected".into());
            }
            Action::SetColIncr => self.scan_edit(
                "setcol-incr",
                |s| s.selected_in_view_order().map_err(|e| e.to_string()),
                move |app, rids| {
                    let s = app.sheets.active_mut();
                    let done = s
                        .set_incr_with(&column, rids, base)
                        .map(|n| format!("set {column} to {base}…{}", base + n as u64 - 1));
                    app.edit_done(done, "");
                },
            ),
            Action::SetColFill => {
                let c = column.clone();
                self.scan_edit(
                    "fill",
                    move |s| s.fill_scan(&c),
                    move |app, (ids, vals)| {
                        let s = app.sheets.active_mut();
                        let done = s
                            .fill_apply(&column, ids, vals)
                            .map(|n| format!("filled {n} values"));
                        app.edit_done(done, "");
                    },
                );
            }
            Action::Capture { pattern } => {
                let done = self.sheets.active_mut().capture(&column, pattern);
                let done = done.map(|name| {
                    self.place_new_column(&name, Some(&column));
                    format!("added {name}; ( expands it")
                });
                self.edit_done(done, "");
            }
            _ => {}
        }
        Continue(())
    }

    pub(super) fn derive(&mut self, name: String, expr: String) {
        let at = self.current_column_name();
        let s = self.sheets.active_mut();
        let new = !s.has_column(&name);
        match s.derive(&name, &expr) {
            Ok(()) => {
                if new {
                    self.place_new_column(&name, at.as_deref());
                }
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("derive: {e}")),
        }
    }

    pub(super) fn set_expr(&mut self, expr: String) -> ControlFlow<()> {
        let Some(column) = self.current_column_name() else {
            return Break(());
        };
        match self.sheets.active_mut().set_expr(&column, &expr) {
            Ok(n) => {
                self.toast = Some(format!("set {column} on {n} rows"));
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("setexpr: {e}")),
        }
        Continue(())
    }

    pub(super) fn set_col(&mut self, value: String) -> ControlFlow<()> {
        let Some(column) = self.current_column_name() else {
            return Break(());
        };
        match self.sheets.active_mut().set_col(&column, &value) {
            Ok(n) => {
                self.toast = Some(format!("set {column} on {n} rows"));
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("setcol: {e}")),
        }
        Continue(())
    }

    pub(super) fn aggregate_col(&mut self, names: String) -> ControlFlow<()> {
        let Some(c) = self.current_column_name() else {
            return Break(());
        };
        // undo step only once the names are checked
        let s = self.sheets.active_mut();
        let before = s.snapshot();
        match s.add_aggregators(&c, &names) {
            Ok(()) => s.push_undo(before),
            Err(e) => self.toast = Some(e),
        }
        Continue(())
    }

    pub(super) fn unfurl_col(&mut self) -> ControlFlow<()> {
        let Some(c) = self.current_column_name() else {
            return Break(());
        };
        let id = self.sheets.alloc_id();
        let empty = self.theme.unfurl_empty;
        match Sheet::unfurl_from(self.sheets.active(), id, &c, empty) {
            Ok(s) => {
                self.sheets.push(s);
                self.spawn_materialize();
                self.goto_column(&format!("{c}_value"));
            }
            Err(e) => self.toast = Some(e),
        }
        Continue(())
    }

    pub(super) fn contract_col(&mut self) -> ControlFlow<()> {
        let Some(c) = self.current_column_name() else {
            return Break(());
        };
        match self.sheets.active_mut().contract(&c) {
            Ok(parent) => {
                self.spawn_materialize();
                self.goto_column(&parent);
            }
            Err(e) => self.toast = Some(e),
        }
        Continue(())
    }

    pub(super) fn resize_col_max(&mut self) -> ControlFlow<()> {
        let Some(c) = self.current_column_name() else {
            return Break(());
        };
        let lua_render = self.plugins.as_ref().filter(|p| p.has_renderers());
        let render_cell = |c: &str, d: &polars::prelude::DataType, v: &AnyValue| {
            lua_render.and_then(|p| p.render(c, d, v))
        };
        let render = lua_render.map(|_| &render_cell as crate::widget::CellRenderer);
        let s = self.sheets.active();
        let Some((at, full)) = s.window().filter(|_| s.is_loaded()).and_then(|df| {
            let at = df.get_column_index(&c)?;
            let fmt = crate::widget::cell_fmt(&s.view, &self.theme, &df[at]);
            let w = crate::widget::SheetWidget::full_width(df.height(), &df[at], render, fmt);
            Some((at, w))
        }) else {
            self.toast = Some("_: still loading".into());
            return Break(());
        };
        // VisiData's toggleWidth: full width, or back to the default
        let s = self.sheets.active_mut();
        if s.state.widths.get(at) == Some(&full) {
            s.state.width_floor.remove(&c);
        } else {
            s.state.width_floor.insert(c, full);
        }
        s.reset_widths();
        Continue(())
    }
}
