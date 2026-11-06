//! Action dispatch: what each keymap / ex-line `Action` does to the app.

use super::*;

impl App {
    pub(super) fn dispatch_sequence(&mut self, actions: Vec<Action>) {
        if actions.iter().any(|a| a.repeatable()) {
            self.last_action = Some(actions.clone());
        }
        for a in actions {
            self.dispatch(a);
        }
    }

    pub(super) fn dispatch(&mut self, action: Action) {
        let before_rev = self.sheets.active().revision;
        let affects = action.affects_data();
        if !matches!(action, Action::GotoBottom) {
            self.sheets.active_mut().view.jump_bottom = false;
        }
        if let Action::SetFilter { regex, .. } = &action
            && let Err(e) = regex::Regex::new(regex)
        {
            self.toast = Some(format!("bad regex: {e}"));
            return;
        }
        if let Action::SetFilter { column, .. } | Action::SetSort { column, .. } = &action
            && !self.sheets.active().has_column(column)
        {
            self.toast = Some(format!("no column: {column}"));
            return;
        }
        if action.undoable() {
            self.sheets.active_mut().checkpoint();
        }
        match action {
            Action::Quit => self.quit = true,
            Action::PopSheet => {
                let popped_id = self.sheets.active().id;
                if self.sheets.pop() {
                    if let Some(h) = self.materialize_handles.remove(&popped_id) {
                        h.abort();
                    }
                } else {
                    self.quit = true;
                }
            }
            Action::Redraw => {}
            Action::Lua(id) => self.run_lua_binding(id),
            Action::RepeatLast => {
                if let Some(last) = self.last_action.clone() {
                    for a in last {
                        self.dispatch(a);
                    }
                }
            }
            Action::CursorDown { count } => self.move_cursor_rows(count.unwrap_or(1) as isize),
            Action::CursorUp { count } => self.move_cursor_rows(-(count.unwrap_or(1) as isize)),
            Action::CursorLeft { count } => self.move_cursor_cols(-(count.unwrap_or(1) as isize)),
            Action::CursorRight { count } => self.move_cursor_cols(count.unwrap_or(1) as isize),
            Action::PageDown => self.page(1),
            Action::PageUp => self.page(-1),
            Action::GotoTop => {
                self.goto_row(0);
            }
            Action::GotoBottom => self.goto_bottom(),
            Action::GotoLeftmost => {
                let s = self.sheets.active_mut();
                s.view.col_cursor = 0;
                s.view.col_offset = 0;
            }
            Action::GotoRightmost => {
                let s = self.sheets.active_mut();
                if let Some(df) = &s.window
                    && df.width() > 0
                {
                    s.view.col_cursor = df.width() - 1;
                }
            }
            Action::FreezeColumn => {
                let s = self.sheets.active_mut();
                let c = s.view.col_cursor;
                if c + 1 > s.view.frozen_left {
                    s.view.frozen_left = c + 1;
                }
            }
            Action::UnfreezeAll => self.sheets.active_mut().view.frozen_left = 0,
            Action::HideCol => {
                if let Some(c) = self.current_column_name() {
                    self.sheets.active_mut().view.hidden.insert(c);
                    self.spawn_materialize();
                }
            }
            Action::UnhideAll => {
                let s = self.sheets.active_mut();
                if !s.view.hidden.is_empty() {
                    s.view.hidden.clear();
                    self.spawn_materialize();
                }
            }
            Action::KeyCol => {
                if let Some(c) = self.current_column_name() {
                    let s = self.sheets.active_mut();
                    let keys = &mut s.view.keys;
                    if let Some(i) = keys.iter().position(|k| k == &c) {
                        keys.remove(i);
                    } else {
                        keys.push(c.clone());
                    }
                    // Keying reorders columns; keep the cursor on the same column.
                    if let Some(i) = s
                        .view_plan()
                        .collect_schema()
                        .ok()
                        .and_then(|sc| sc.index_of(&c))
                    {
                        s.view.col_cursor = i;
                    }
                    self.spawn_materialize();
                }
            }
            Action::UnkeyAll => {
                let s = self.sheets.active_mut();
                if !s.view.keys.is_empty() {
                    s.view.keys.clear();
                    self.spawn_materialize();
                }
            }
            Action::StartRenameColInput => {
                self.start_input(Prompt::Rename);
            }
            Action::RenameCol { new_name } => {
                let Some(old) = self.current_column_name() else {
                    return;
                };
                if new_name != old && self.sheets.active().has_column(&new_name) {
                    self.toast = Some(format!("column already exists: {new_name}"));
                    return;
                }
                let s = self.sheets.active_mut();
                s.checkpoint();
                let step = crate::sheet::PlanStep::Rename {
                    old: old.clone(),
                    new: new_name.clone(),
                };
                if let Err(e) = s.apply_step(step) {
                    self.toast = Some(format!("rename: {e}"));
                    return;
                }
                if s.view.hidden.remove(&old) {
                    s.view.hidden.insert(new_name.clone());
                }
                for k in s.view.keys.iter_mut() {
                    if k == &old {
                        *k = new_name.clone();
                    }
                }
                for (c, _) in s.view.type_casts.iter_mut() {
                    if c == &old {
                        *c = new_name.clone();
                    }
                }
                if let Some(f) = s.view.filter.as_mut().filter(|f| f.column == old) {
                    f.column = new_name.clone();
                }
                for k in s.view.sort.iter_mut().filter(|k| k.column == old) {
                    k.column = new_name.clone();
                }
                self.spawn_materialize();
            }
            Action::TypeCastInt => self.push_type_cast(DataType::Int64),
            Action::TypeCastFloat => self.push_type_cast(DataType::Float64),
            Action::TypeCastString => self.push_type_cast(DataType::String),
            Action::TypeCastDate => self.push_type_cast(DataType::Date),
            Action::TypeCastCurrency => self.push_currency_cast(),
            Action::StartSearchInput => self.start_input(Prompt::Search),
            Action::StartFilterInput => self.start_input(Prompt::Filter),
            Action::StartExInput => self.start_input(Prompt::Ex),
            Action::ExCommand(text) => match crate::exline::parse(&text) {
                Ok(actions) => self.dispatch_sequence(actions),
                Err(e) => self.toast = Some(format!("{e}")),
            },
            Action::SearchNext => self.search_jump(true),
            Action::SearchPrev => self.search_jump(false),
            Action::SetFilter { column, regex } => {
                let s = self.sheets.active_mut();
                s.view.filter = Some(FilterState { column, regex });
                s.view.window_offset = 0;
                s.view.row_cursor = 0;
                s.view.row_offset = 0;
            }
            Action::ClearFilter => {
                let had = self.sheets.active_mut().view.filter.take().is_some();
                if had {
                    self.spawn_materialize();
                }
            }
            Action::SortAsc => self.sort_current_column(SortOrder::Asc),
            Action::SortDesc => self.sort_current_column(SortOrder::Desc),
            Action::SetSort { column, descending } => {
                let s = self.sheets.active_mut();
                let order = if descending {
                    SortOrder::Desc
                } else {
                    SortOrder::Asc
                };
                if let Some(k) = s.view.sort.iter_mut().find(|k| k.column == column) {
                    k.order = order;
                } else {
                    s.view.sort.push(SortKey { column, order });
                }
                s.view.window_offset = 0;
                s.view.row_cursor = 0;
                s.view.row_offset = 0;
            }
            Action::ClearSort => {
                let s = self.sheets.active_mut();
                if !s.view.sort.is_empty() {
                    s.view.sort.clear();
                }
            }
            Action::OpenFreqTable => self.open_freq_table(),
            Action::OpenSheetSwitcher => self.open_sheet_switcher(),
            Action::Pivot {
                index,
                on,
                values,
                agg,
            } => self.open_pivot(&index, &on, &values, &agg),
            Action::Unpivot { on, index } => self.open_unpivot(&on, &index),
            Action::Diff { path, keys } => self.open_diff(&path, &keys),
            Action::Tail { path, follow } => self.open_tail(&path, follow),
            Action::Save { path } => self.save_active(&path),
            Action::SubmitRow => self.submit_row(),
            Action::ToggleSelectRow => self.toggle_select_current(),
            Action::SelectRow => self.set_select_current(true),
            Action::UnselectRow => self.set_select_current(false),
            Action::SelectAll => self.select_all(),
            Action::UnselectAll => {
                self.sheets.active_mut().view.selected = Default::default();
            }
            Action::StartSelectInput => self.start_input(Prompt::Select),
            Action::SelectByRegex { regex } => self.select_by_regex(&regex),
            Action::DupSelected => self.open_dup_selected(),
            Action::TogglePlanPanel => {
                self.plan_mode = match self.plan_mode {
                    PlanMode::Off => PlanMode::Normal,
                    PlanMode::Normal => PlanMode::Optimized,
                    PlanMode::Optimized => PlanMode::Off,
                };
            }
            Action::Join { source, keys, how } => self.open_join(&source, &keys, &how),
            Action::Derive { name, expr } => {
                let s = self.sheets.active_mut();
                match s.derive(&name, &expr) {
                    Ok(()) => self.spawn_materialize(),
                    Err(e) => self.toast = Some(format!("derive: {e}")),
                }
            }
            Action::SetCol { value } => {
                let Some(column) = self.current_column_name() else {
                    return;
                };
                match self.sheets.active_mut().set_col(&column, &value) {
                    Ok(n) => {
                        self.toast = Some(format!("set {column} on {n} rows"));
                        self.spawn_materialize();
                    }
                    Err(e) => self.toast = Some(format!("setcol: {e}")),
                }
            }
            Action::Sql { query } => self.open_sql(&query),
            Action::Export { path } => self.export(&path),
            Action::HideUniform => {
                let sheet = self.sheets.active().clone();
                self.spawn_job("uniform", move || {
                    Ok(JobDone::Hide(
                        sheet.uniform_columns().map_err(|e| e.to_string())?,
                    ))
                });
            }
            Action::DupSheet => {
                let id = self.sheets.alloc_id();
                let copy = Sheet::copy_from(self.sheets.active(), id);
                self.sheets.push(copy);
                self.spawn_materialize();
            }
            Action::Describe => {
                let id = self.sheets.alloc_id();
                let parent = self.sheets.active().clone();
                self.spawn_job("describe", move || {
                    Ok(JobDone::PushSheet(Box::new(Sheet::describe_from(
                        &parent, id,
                    )?)))
                });
            }
            Action::PivotCurrent => {
                let Some(on) = self.current_column_name() else {
                    return;
                };
                let id = self.sheets.alloc_id();
                let parent = self.sheets.active().clone();
                let keys = parent.view.keys.clone();
                self.spawn_job("pivot", move || {
                    let child = Sheet::pivot_current_from(&parent, id, &keys, &on)?;
                    Ok(JobDone::PushSheet(Box::new(child)))
                });
            }
            Action::Undo | Action::Redo => {
                let undo = matches!(action, Action::Undo);
                if self.sheets.active_mut().time_travel(undo) {
                    self.spawn_materialize();
                } else {
                    self.toast = Some(format!("nothing to {}", if undo { "undo" } else { "redo" }));
                }
            }
            Action::SubmitSelected => self.dive_freq(true),
            Action::SelectEqualCell => {
                let s = self.sheets.active();
                let Some(column) = self.current_column_name() else {
                    return;
                };
                let Some(value) = s
                    .window
                    .as_ref()
                    .and_then(|df| df.column(&column).ok()?.get(s.view.row_cursor).ok())
                    .map(|v| v.into_static())
                else {
                    return;
                };
                let sheet = s.clone();
                self.spawn_job("select", move || {
                    let rids = sheet
                        .rids_with_values(&column, std::slice::from_ref(&value))
                        .map_err(|e| e.to_string())?;
                    let text = crate::sheet::any_text(&value);
                    let note = format!("selected {} rows where {column} = {text}", rids.len());
                    Ok(JobDone::Select { rids, note })
                });
            }
            Action::StartDiffInput => self.start_input(Prompt::Diff),
            Action::SubmitInput | Action::CancelInput | Action::InputBackspace => {}
        }
        if affects && self.sheets.active().revision == before_rev {
            self.spawn_materialize();
        }
    }

    fn move_cursor_rows(&mut self, delta: isize) {
        let v = &self.sheets.active().view;
        let cur = v.window_offset + v.row_cursor as u64;
        let target = if delta < 0 {
            cur.saturating_sub(delta.unsigned_abs() as u64)
        } else {
            cur.saturating_add(delta as u64)
        };
        self.goto_row(target);
    }

    /// Put the cursor on absolute view row `target`, clamped to the data. When
    /// the row isn't in the loaded window, re-centre the window on it and load.
    /// Returns true when the target was already inside the window.
    pub(super) fn goto_row(&mut self, target: u64) -> bool {
        let s = self.sheets.active_mut();
        let Some(df) = &s.window else { return false };
        let rows = df.height() as u64;
        let (off, wr) = (s.view.window_offset, s.view.window_rows as u64);
        let last = match s.view.total_rows {
            Some(t) => t.saturating_sub(1),
            None if rows == wr => u64::MAX, // full window: more rows may follow
            None => (off + rows).saturating_sub(1),
        };
        let target = target.min(last);
        if (off..off + rows).contains(&target) {
            s.view.row_cursor = (target - off) as usize;
            return true;
        }
        let mut new_off = target.saturating_sub(wr / 2);
        if let Some(t) = s.view.total_rows {
            new_off = new_off.min(t.saturating_sub(wr));
        }
        if new_off == off {
            return false; // nothing more to load (empty view)
        }
        s.view.window_offset = new_off;
        s.view.row_cursor = (target - new_off) as usize;
        self.spawn_materialize();
        false
    }

    fn move_cursor_cols(&mut self, delta: isize) {
        let s = self.sheets.active_mut();
        let Some(df) = &s.window else { return };
        let cols = df.width();
        if cols == 0 {
            return;
        }
        let new = (s.view.col_cursor as isize + delta).clamp(0, cols as isize - 1) as usize;
        s.view.col_cursor = new;
    }

    /// One screen of data rows. `move_cursor_rows` slides the window when needed.
    fn page(&mut self, dir: isize) {
        self.move_cursor_rows(dir * self.viewport_rows.max(1) as isize);
    }

    fn push_type_cast(&mut self, dt: DataType) {
        let Some(name) = self.current_column_name() else {
            self.toast = Some("no column to type-cast".into());
            return;
        };
        let s = self.sheets.active_mut();
        if let Some(entry) = s.view.type_casts.iter_mut().find(|(c, _)| c == &name) {
            entry.1 = dt;
        } else {
            s.view.type_casts.push((name, dt));
        }
        self.spawn_materialize();
    }

    /// VisiData `$` currency cast: strip `[^0-9.\-]` then cast to float.
    /// Done as a `with_columns` on the base plan so filter/sort see the clean float.
    fn push_currency_cast(&mut self) {
        let Some(name) = self.current_column_name() else {
            self.toast = Some("no column to type-cast".into());
            return;
        };
        let s = self.sheets.active_mut();
        let step = crate::sheet::PlanStep::Currency { column: name };
        if let Err(e) = s.apply_step(step) {
            self.toast = Some(format!("currency: {e}"));
            return;
        }
        self.spawn_materialize();
    }

    fn sort_current_column(&mut self, order: SortOrder) {
        let Some(name) = self.current_column_name() else {
            self.toast = Some("no column to sort".into());
            return;
        };
        let s = self.sheets.active_mut();
        if let Some(k) = s.view.sort.iter_mut().find(|k| k.column == name) {
            k.order = order;
        } else {
            s.view.sort.push(SortKey {
                column: name,
                order,
            });
        }
        s.view.window_offset = 0;
        s.view.row_cursor = 0;
        s.view.row_offset = 0;
    }

    fn goto_bottom(&mut self) {
        self.sheets.active_mut().view.jump_bottom = true;
        self.settle_jump_bottom();
    }

    fn search_jump(&mut self, forward: bool) {
        let s = self.sheets.active();
        let Some(needle) = s.view.search.as_ref().map(|st| st.needle.clone()) else {
            self.toast = Some("no search; press / first".into());
            return;
        };
        let cur = s.view.window_offset + s.view.row_cursor as u64;
        let sheet = s.clone();
        // ponytail: full scan per n/N; cache hits per (needle, view) if big files lag.
        self.spawn_job("search", move || {
            let hits = sheet.search_hits(&needle).map_err(|e| e.to_string())?;
            let next = if forward {
                hits.iter().find(|(r, _)| *r > cur).or(hits.first())
            } else {
                hits.iter().rev().find(|(r, _)| *r < cur).or(hits.last())
            };
            Ok(match next.copied() {
                Some((row, col)) => JobDone::Jump { row, col },
                None => JobDone::Toast(format!("no match for /{needle}")),
            })
        });
    }

    fn open_pivot(&mut self, index: &str, on: &str, values: &str, agg: &str) {
        let id = self.sheets.alloc_id();
        let parent = self.sheets.active().clone();
        let (index, on, values, agg) = (
            index.to_string(),
            on.to_string(),
            values.to_string(),
            agg.to_string(),
        );
        let lua = self.plugins.as_ref().and_then(|p| p.aggregator(&agg));
        self.spawn_job("pivot", move || {
            let child = match &lua {
                Some(a) => {
                    let df = lua_pivot(a, &parent, &index, &on, &values)?;
                    let name = format!("pivot({index}×{on}→{agg}({values})): {}", parent.name);
                    let origin = Origin::Pivot { parent: parent.id };
                    Sheet::child(&parent, id, name, df.lazy(), origin)
                }
                None => Sheet::pivot_from(&parent, id, &index, &on, &values, &agg)?,
            };
            Ok(JobDone::PushSheet(Box::new(child)))
        });
    }

    /// Persist the active sheet to disk. Writer picked by extension; applies
    /// the sheet's `view_plan` (type-casts + filter + sort + visible projection,
    /// all rows — not just the materialized window) so the saved file matches
    /// what the user sees, not raw source data. Overwrites existing files.
    fn save_active(&mut self, path: &str) {
        let plan = self.sheets.active().view_plan();
        let path = path.to_string();
        self.spawn_job("save", move || {
            let mut df = plan.collect().map_err(|e| format!("collect: {e}"))?;
            crate::sheet::write_df_by_extension(&mut df, std::path::Path::new(&path))
                .map_err(|e| e.to_string())?;
            Ok(JobDone::Toast(format!(
                "saved {} rows → {path}",
                df.height()
            )))
        });
    }

    fn open_tail(&mut self, path: &str, follow: bool) {
        use std::ffi::OsString;
        // A loader converts once; refreshes would re-scan that stale snapshot.
        if let Some(p) = &self.plugins
            && p.resolve_loader(std::path::Path::new(path)).is_some()
        {
            self.toast = Some(format!(
                "tail: {path} needs a plugin loader; can't follow it"
            ));
            return;
        }
        let id = self.sheets.alloc_id();
        let path = path.to_string();
        self.spawn_job("tail", move || {
            let loaded =
                crate::reader::open(&OsString::from(path), None).map_err(|e| e.to_string())?;
            let s = Sheet::tail_from(id, loaded, follow).map_err(|e| e.to_string())?;
            Ok(JobDone::PushSheet(Box::new(s)))
        });
    }

    /// Hot reload: `init.lua` edited/created/deleted → fresh VM, keymap, theme.
    pub(super) fn reload_plugins_if_changed(&mut self) {
        let Some((fresh, warning)) = self.plugins.as_ref().and_then(|p| p.reload_if_changed())
        else {
            return;
        };
        self.plugins = Some(fresh);
        (self.keymap, self.theme) = keymap_and_theme(self.plugins.as_ref());
        self.sheets.active_mut().state.widths_key = None; // renderers may have changed
        self.toast = Some(warning.unwrap_or_else(|| "init.lua reloaded".into()));
    }

    pub(super) fn on_tail_tick(&mut self) {
        // Only refresh the active sheet's tail if any. Dormant tail sheets are reactivated
        // via sheet-switcher; refreshing all of them per tick would be wasteful.
        let active = self.sheets.active_mut();
        if !matches!(active.origin, Origin::Tail { .. }) {
            return;
        }
        // One reopen at a time: a change meanwhile is seen on a later tick.
        let reopening = self.busy.contains(&"tail reopen");
        let active = self.sheets.active_mut();
        if !reopening && let Some(path) = active.tail_needs_reopen() {
            let kind = active.kind;
            self.spawn_job("tail reopen", move || {
                let loaded = crate::reader::open(path.as_os_str(), Some(kind));
                let loaded = loaded.map_err(|e| e.to_string())?;
                Ok(JobDone::TailSource(Box::new(loaded)))
            });
            return;
        }
        // Reads only appended bytes, so the UI thread can afford it; rotation
        // rereads the whole file.
        // ponytail: move to a job if a rotated giant log ever stalls a tick.
        self.after_tail_change(Sheet::refresh_tail);
    }

    /// Run `change` on the active tail; keep following the bottom if the cursor was there.
    fn after_tail_change(
        &mut self,
        change: impl FnOnce(&mut Sheet) -> Result<bool, crate::reader::OpenError>,
    ) {
        let active = self.sheets.active_mut();
        let follow = matches!(active.origin, Origin::Tail { follow: true, .. });
        let v = &active.view;
        let abs = v.window_offset + v.row_cursor as u64;
        let was_at_bottom = match (v.total_rows, &active.window) {
            (Some(total), _) => abs + 1 >= total,
            (None, Some(df)) => v.row_cursor + 1 >= df.height(),
            (None, None) => true,
        };
        match change(active) {
            Ok(true) => {}
            Ok(false) => return, // nothing new
            Err(e) => {
                self.toast = Some(format!("tail refresh: {e}"));
                return;
            }
        }
        // Settles across the window + count results the refresh triggers.
        active.view.jump_bottom |= follow && was_at_bottom;
        self.spawn_materialize();
    }

    pub(super) fn apply_tail_source(
        &mut self,
        sheet_id: SheetId,
        loaded: crate::reader::LoadedSource,
    ) {
        if self.sheets.active().id == sheet_id {
            self.after_tail_change(|s| {
                s.set_tail_source(loaded)?;
                Ok(true)
            });
        } else if let Some(s) = self.sheets.find_by_id_mut(sheet_id) {
            // Shown next time it's raised and something re-materializes.
            let _ = s.set_tail_source(loaded);
        }
    }

    /// Whether a Lua loader claims `path`: those run here, on the VM's thread.
    fn lua_loader_for(&self, path: &str) -> bool {
        let p = self.plugins.as_ref();
        p.is_some_and(|p| p.resolve_loader(std::path::Path::new(path)).is_some())
    }

    /// Open `path` now through a Lua loader, or return None for `job` to read it.
    fn open_with_loader(
        &mut self,
        label: &str,
        path: &str,
    ) -> Result<Option<crate::reader::LoadedSource>, ()> {
        if !self.lua_loader_for(path) {
            return Ok(None);
        }
        crate::plugins::open(self.plugins.as_ref(), path.as_ref(), None)
            .map(Some)
            .map_err(|e| self.toast = Some(format!("{label}: {e}")))
    }

    fn open_diff(&mut self, path: &str, keys: &[String]) {
        let Ok(opened) = self.open_with_loader("diff", path) else {
            return;
        };
        let id = self.sheets.alloc_id();
        let parent = self.sheets.active().clone();
        let (path, keys) = (path.to_string(), keys.to_vec());
        self.spawn_job("diff", move || {
            let loaded = match opened {
                Some(l) => l,
                None => crate::reader::open(path.as_ref(), None).map_err(|e| e.to_string())?,
            };
            let mut s = if keys.is_empty() {
                Sheet::schema_diff_from(&parent, id, &loaded)?
            } else {
                Sheet::row_diff_from(&parent, id, &Sheet::from_source(0, loaded), &keys)?
            };
            // A full join has no fixed row order: collect once, like `:join`.
            if !keys.is_empty() {
                s.plan = s.plan.clone().collect().map_err(|e| e.to_string())?.lazy();
            }
            Ok(JobDone::PushSheet(Box::new(s)))
        });
    }

    /// `:join` source: `#<id>` or the exact name of an open sheet, else a file.
    fn open_join(&mut self, source: &str, keys: &[String], how: &str) {
        let active = self.sheets.active().id;
        let open_sheet = self
            .sheets
            .summaries()
            .find(|(id, name, _)| {
                *id != active
                    && (*name == source || source.strip_prefix('#') == Some(&id.to_string()))
            })
            .map(|(id, _, _)| id);
        let right = open_sheet.and_then(|id| Some(self.sheets.find_by_id_mut(id)?.clone()));
        let opened = match right {
            Some(_) => Ok(None), // already open: no file to read
            None => self.open_with_loader("join", source),
        };
        let Ok(opened) = opened else { return };
        let id = self.sheets.alloc_id();
        let left = self.sheets.active().clone();
        let (source, keys, how) = (source.to_string(), keys.to_vec(), how.to_string());
        self.spawn_job("join", move || {
            let right = match (right, opened) {
                (Some(sheet), _) => sheet,
                (None, Some(l)) => Sheet::from_source(0, l),
                (None, None) => {
                    let l = crate::reader::open(source.as_ref(), None);
                    Sheet::from_source(0, l.map_err(|e| e.to_string())?)
                }
            };
            let mut s = Sheet::join_from(&left, id, &right, &keys, &how)?;
            s.plan = s.plan.clone().collect().map_err(|e| e.to_string())?.lazy();
            Ok(JobDone::PushSheet(Box::new(s)))
        });
    }

    fn open_unpivot(&mut self, on: &[String], index: &[String]) {
        let id = self.sheets.alloc_id();
        let child = {
            let parent = self.sheets.active();
            Sheet::unpivot_from(parent, id, on, index)
        };
        self.sheets.push(child);
        self.spawn_materialize();
    }

    fn submit_row(&mut self) {
        match &self.sheets.active().origin {
            Origin::SheetSwitcher => self.switch_to_row_sheet(),
            Origin::Freq { .. } => self.dive_freq(false),
            Origin::Parts { path, kind } => {
                let (path, kind) = (path.clone(), *kind);
                let s = self.sheets.active();
                let part = s.window.as_ref().and_then(|df| {
                    let v = df.get_columns().first()?.get(s.view.row_cursor).ok()?;
                    v.get_str().map(String::from)
                });
                let Some(part) = part else { return };
                let id = self.sheets.alloc_id();
                self.spawn_job("open", move || {
                    let loaded = crate::reader::open_part(&path, kind, &part)
                        .map_err(|e| format!("{part}: {e}"))?;
                    Ok(JobDone::PushSheet(Box::new(Sheet::from_source(id, loaded))))
                });
            }
            _ => self.toast = Some("<CR>: nothing to open here".into()),
        }
    }

    /// Sheet list: pull the row's `id` and raise that sheet.
    fn switch_to_row_sheet(&mut self) {
        let s = self.sheets.active();
        let Some(df) = &s.window else { return };
        let ri = s.view.row_cursor;
        let Ok(av) = df.column("id").and_then(|c| c.get(ri)) else {
            return;
        };
        let id = match av.try_extract::<i64>() {
            Ok(x) => x as u32,
            Err(_) => return,
        };
        // pop the switcher, then raise the chosen sheet
        self.sheets.pop();
        if !self.sheets.switch_to(id) {
            self.toast = Some(format!("no sheet with id {id}"));
        }
    }

    /// Freq sheet: open the parent's rows whose value is the cursor row's
    /// (`<CR>`) or any selected row's (`g<CR>`) value.
    fn dive_freq(&mut self, selected: bool) {
        let s = self.sheets.active();
        let Origin::Freq { parent, column } = s.origin.clone() else {
            self.toast = Some("g<CR>: only on a frequency sheet".into());
            return;
        };
        let rids: Vec<u64> = if selected {
            s.view.selected.iter().copied().collect()
        } else {
            self.rid_at_cursor().into_iter().collect()
        };
        if rids.is_empty() {
            self.toast = Some("no rows selected".into());
            return;
        }
        let plan = s.plan.clone();
        let Some(parent) = self.sheets.find_by_id_mut(parent).map(|p| p.clone()) else {
            self.toast = Some("dive: parent sheet was closed".into());
            return;
        };
        let id = self.sheets.alloc_id();
        self.spawn_job("dive", move || {
            // Values by row id = position in the freq sheet's base plan; the
            // value column is first even if renamed here. `column` names it in
            // the parent.
            let values = plan.select([nth(0)]).collect().map_err(|e| e.to_string())?;
            let vals = &values.get_columns()[0];
            let picked: Vec<AnyValue> = rids
                .iter()
                .filter_map(|r| vals.get(*r as usize).ok().map(|v| v.into_static()))
                .collect();
            let child = Sheet::rows_with_values(&parent, id, &column, &picked);
            Ok(JobDone::PushSheet(Box::new(child)))
        });
    }

    /// `:sql`: `self` = this view; every open sheet is also `s<id>` (see `S`).
    fn open_sql(&mut self, query: &str) {
        let active = self.sheets.active().id;
        let ids: Vec<SheetId> = self.sheets.summaries().map(|(id, _, _)| id).collect();
        let others: Vec<Sheet> = ids
            .into_iter()
            .filter(|id| *id != active)
            .filter_map(|id| Some(self.sheets.find_by_id_mut(id)?.clone()))
            .collect();
        let id = self.sheets.alloc_id();
        match Sheet::sql_from(self.sheets.active(), id, query, others) {
            Ok(s) => self.push_collected("sql", s),
            Err(e) => self.toast = Some(format!("sql: {e}")),
        }
    }

    /// Push `sheet` with its plan collected once, in the background. For
    /// results whose row order Polars doesn't fix (joins, GROUP BY): row ids
    /// are positions, so a re-collect in another order would move selections.
    // ponytail: holds the whole result in memory; fine for joins/aggregates.
    fn push_collected(&mut self, label: &'static str, mut sheet: Sheet) {
        self.spawn_job(label, move || {
            sheet.plan = sheet
                .plan
                .clone()
                .collect()
                .map_err(|e| e.to_string())?
                .lazy();
            Ok(JobDone::PushSheet(Box::new(sheet)))
        });
    }

    fn export(&mut self, path: &str) {
        let p = std::path::Path::new(path);
        let result = crate::export::script_for(self.sheets.active(), p)
            .and_then(|script| std::fs::write(p, script).map_err(|e| format!("export: {e}")));
        self.toast = Some(match result {
            Ok(()) => format!("exported → {path}"),
            Err(e) => e,
        });
    }

    /// Row id under the cursor; None on an empty view.
    fn rid_at_cursor(&self) -> Option<u64> {
        let s = self.sheets.active();
        s.state.window_rids.get(s.view.row_cursor).copied()
    }

    fn toggle_select_current(&mut self) {
        let Some(rid) = self.rid_at_cursor() else {
            return;
        };
        let selected = &mut self.sheets.active_mut().view.selected;
        let selected = std::sync::Arc::make_mut(selected);
        if !selected.remove(&rid) {
            selected.insert(rid);
        }
    }

    fn set_select_current(&mut self, select: bool) {
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
    fn select_all(&mut self) {
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
    fn select_by_regex(&mut self, pat: &str) {
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

    fn open_dup_selected(&mut self) {
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

    fn open_sheet_switcher(&mut self) {
        let summaries: Vec<_> = self
            .sheets
            .summaries()
            .map(|(id, name, kind)| (id, name.to_string(), kind))
            .collect();
        let id = self.sheets.alloc_id();
        let child = Sheet::sheet_switcher(id, summaries);
        self.sheets.push(child);
        // sheet_switcher ships window pre-materialized; still fire once so revision is nudged
        self.spawn_materialize();
    }

    fn open_freq_table(&mut self) {
        let Some(col_name) = self.current_column_name() else {
            self.toast = Some("no column to freq".into());
            return;
        };
        let id = self.sheets.alloc_id();
        let child = {
            let parent = self.sheets.active();
            Sheet::freq_from(parent, id, &col_name)
        };
        self.sheets.push(child);
        self.spawn_materialize();
    }
}
