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
        let before_rev = self.sheets.active().revision();
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
        if let Action::SelectExpr { expr, .. }
        | Action::SetFilterExpr {
            expr: Some(expr), ..
        } = &action
            && let Err(e) = self.sheets.active().check_predicate(expr)
        {
            self.toast = Some(e);
            return;
        }
        if matches!(self.sheets.active().origin, Origin::Columns { .. })
            && self.columns_dispatch(&action)
        {
            return;
        }
        if action.undoable() {
            self.sheets.active_mut().checkpoint();
        }
        match action {
            Action::Quit => {
                let n: usize = self.sheets.iter().map(Sheet::unsaved_edits).sum();
                if n == 0 {
                    self.quit = true;
                } else {
                    self.ask(Confirm::Quit, format!("quit and discard {}?", edits(n)));
                }
            }
            Action::ForceQuit => self.quit = true,
            Action::ForcePopSheet => self.pop_sheet(),
            Action::CancelJobs => self.cancel_jobs(false),
            Action::CancelAllJobs => self.cancel_jobs(true),
            Action::PopSheet => {
                let s = self.sheets.active();
                match s.unsaved_edits() {
                    0 => self.pop_sheet(),
                    n => {
                        let q = format!("discard {} to {}?", edits(n), s.name);
                        self.ask(Confirm::Pop, q);
                    }
                }
            }
            Action::Redraw => {}
            Action::Lua(id) => self.run_lua(id, None),
            Action::LuaCommand { id, args } => self.run_lua(id, Some(&args)),
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
                if let Some(df) = s.window()
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
                    self.goto_column(&c);
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
                let s = self.sheets.active_mut();
                let before = s.snapshot();
                match s.rename_col(&old, &new_name) {
                    Ok(()) => {
                        s.push_undo(before);
                        self.spawn_materialize();
                    }
                    Err(e) => self.toast = Some(e),
                }
            }
            Action::TypeCastInt => self.push_type_cast(DataType::Int64),
            Action::TypeCastFloat => self.push_type_cast(DataType::Float64),
            Action::TypeCastString => self.push_type_cast(DataType::String),
            Action::TypeCastDate => self.push_type_cast(DataType::Date),
            Action::TypeCastCurrency => self.push_currency_cast(),
            Action::TypeCastJson => {
                let Some(c) = self.current_column_name() else {
                    return;
                };
                match self.sheets.active_mut().json_decode(&c) {
                    Ok(()) => self.spawn_materialize(),
                    Err(e) => self.toast = Some(e),
                }
            }
            Action::StartSearchInput {
                all_columns,
                backward,
            } => self.start_input(Prompt::Search {
                all_columns,
                backward,
            }),
            Action::StartSearchExprInput { backward } => {
                self.start_input(Prompt::SearchExpr { backward })
            }
            Action::StartDeriveInput => self.start_input(Prompt::Derive),
            Action::StartSelectExprInput { unselect } => {
                self.start_input(Prompt::SelectExpr { unselect })
            }
            Action::StartFilterInput => self.start_input(Prompt::Filter),
            Action::StartExInput => self.start_input(Prompt::Ex),
            Action::ExCommand(text) => self.run_ex(&text),
            Action::SearchNext => self.search_jump(false),
            Action::SearchPrev => self.search_jump(true),
            Action::SetFilter { column, regex } => {
                let s = self.sheets.active_mut();
                s.view.filter = Some(FilterState { column, regex });
                s.view.window_offset = 0;
                s.view.row_cursor = 0;
                s.view.row_offset = 0;
            }
            Action::ClearFilter => {
                let s = self.sheets.active_mut();
                let had = s.view.filter.take().is_some() | s.view.filter_expr.take().is_some();
                s.invalidate(crate::sheet::Reason::Rows);
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
            Action::SelectExpr { expr, unselect } => self.select_by_expr(expr, unselect),
            Action::DupSelected => self.open_dup_selected(),
            Action::CopyRow => {
                self.copy(false, false);
            }
            Action::CopySelected => {
                self.copy(true, false);
            }
            Action::CopyCell => {
                self.copy(false, true);
            }
            Action::CopyCells => {
                self.copy(true, true);
            }
            Action::SysCopyRow => self.syscopy(false, false),
            Action::SysCopySelected => self.syscopy(true, false),
            Action::SysCopyCell => self.syscopy(false, true),
            Action::SysCopyCells => self.syscopy(true, true),
            Action::SysPasteCells => self.syspaste(false),
            Action::SysPasteCellsSelected => self.syspaste(true),
            Action::PasteCell | Action::PasteCells => {
                self.paste_cells(action == Action::PasteCells)
            }
            Action::CutCell | Action::CutCells => {
                self.clear_cells(action == Action::CutCells, true)
            }
            Action::DeleteCell | Action::DeleteCells => {
                self.clear_cells(action == Action::DeleteCells, false)
            }
            Action::DeleteRow | Action::CutRow => self.delete_rows(false, action == Action::CutRow),
            Action::AddRow => self.add_rows(1, None, true),
            Action::AddRows { count } => self.add_rows(count, None, true),
            Action::StartAddRowsInput => match self.sheets.active().editable() {
                Ok(()) => self.start_input(Prompt::AddRows),
                Err(e) => self.toast = Some(e),
            },
            Action::PasteAfter | Action::PasteBefore => {
                let rows = match &self.register {
                    Some(Register {
                        kind: RegisterKind::Rows,
                        df,
                    }) => df.clone(),
                    Some(_) => {
                        self.toast = Some("p pastes rows: copy with y or gy".into());
                        return;
                    }
                    None => {
                        self.toast = Some("nothing copied".into());
                        return;
                    }
                };
                self.add_rows(0, Some(rows), action == Action::PasteAfter);
            }
            Action::OpenNew => {
                let id = self.sheets.alloc_id();
                let df = DataFrame::new_infer_height(vec![Column::new_empty(
                    "A".into(),
                    &DataType::String,
                )])
                .expect("one empty column");
                let src = crate::reader::LoadedSource::from_df(
                    crate::reader::SourceKind::Csv,
                    "unnamed".into(),
                    None,
                    df,
                );
                self.sheets.push(Sheet::from_source(id, src));
                self.spawn_materialize();
            }
            Action::DeleteSelected | Action::CutSelected => {
                self.delete_rows(true, action == Action::CutSelected)
            }
            Action::DedupeRows => {
                let id = self.sheets.alloc_id();
                let parent = self.sheets.active().clone();
                self.spawn_job("dedupe", move || {
                    Ok(JobDone::PushSheet(Box::new(Sheet::dedupe_from(
                        &parent, id,
                    )?)))
                });
            }
            Action::SelectColDuplicates => {
                let Some(column) = self.current_column_name() else {
                    return;
                };
                let sheet = self.sheets.active().clone();
                self.spawn_job("select", move || {
                    let rids = sheet
                        .rids_col_duplicates(&column)
                        .map_err(|e| e.to_string())?;
                    let note = format!("selected {} repeated {column} values", rids.len());
                    Ok(JobDone::Select { rids, note })
                });
            }
            Action::TogglePlanPanel => {
                self.sidebar = match self.sidebar {
                    Some(SidebarPage::Plan) => Some(SidebarPage::PlanOpt),
                    Some(SidebarPage::PlanOpt) => None,
                    _ => Some(SidebarPage::Plan),
                };
            }
            Action::ToggleSidebar => {
                self.sidebar = match self.sidebar {
                    Some(_) => None,
                    None => Some(self.sidebar_page),
                };
            }
            Action::CycleSidebar => {
                let page = self.sidebar.map_or(self.sidebar_page, SidebarPage::next);
                (self.sidebar, self.sidebar_page) = (Some(page), page);
            }
            Action::ShowSidebar { page } => (self.sidebar, self.sidebar_page) = (Some(page), page),
            Action::Join { source, keys, how } => self.open_join(&source, &keys, &how),
            Action::Derive { name, expr } => {
                let s = self.sheets.active_mut();
                match s.derive(&name, &expr) {
                    Ok(()) => self.spawn_materialize(),
                    Err(e) => self.toast = Some(format!("derive: {e}")),
                }
            }
            Action::StartEditInput => {
                let s = self.sheets.active();
                if let Err(e) = s.editable() {
                    self.toast = Some(e);
                    return;
                }
                let (Some(column), Some(df)) = (self.current_column_name(), s.window()) else {
                    return;
                };
                let text = df
                    .column(&column)
                    .and_then(|c| c.get(s.view.row_cursor))
                    .map(|v| {
                        if v.is_null() {
                            String::new()
                        } else {
                            crate::sheet::any_text(&v)
                        }
                    });
                let Ok(text) = text else {
                    return; // empty view
                };
                self.start_input(Prompt::Edit);
                if let Some((_, line)) = self.input.as_mut() {
                    line.set(&text);
                }
            }
            Action::StartSetColInput => match self.sheets.active().editable() {
                Ok(()) => self.start_input(Prompt::SetCol),
                Err(e) => self.toast = Some(e),
            },
            Action::StartSetExprInput => match self.sheets.active().editable() {
                Ok(()) => self.start_input(Prompt::SetExpr),
                Err(e) => self.toast = Some(e),
            },
            Action::SetExpr { expr } => {
                let Some(column) = self.current_column_name() else {
                    return;
                };
                match self.sheets.active_mut().set_expr(&column, &expr) {
                    Ok(n) => {
                        self.toast = Some(format!("set {column} on {n} rows"));
                        self.spawn_materialize();
                    }
                    Err(e) => self.toast = Some(format!("setexpr: {e}")),
                }
            }
            Action::SetFilterExpr { expr } => {
                let s = self.sheets.active_mut();
                match s.set_filter_expr(expr.as_deref()) {
                    // `affects_data`: dispatch re-materializes.
                    Ok(()) => {
                        (s.view.window_offset, s.view.row_cursor, s.view.row_offset) = (0, 0, 0)
                    }
                    Err(e) => self.toast = Some(format!("filter-expr: {e}")),
                }
            }
            Action::EditCell { value } => {
                let (Some(column), Some(rid)) = (self.current_column_name(), self.rid_at_cursor())
                else {
                    return;
                };
                match self.sheets.active_mut().edit_cell(&column, rid, &value) {
                    Ok(()) => self.spawn_materialize(),
                    Err(e) => self.toast = Some(e),
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
            Action::ExecSql { sql } => self.exec_sql(sql),
            Action::Export { path } => self.export(&path),
            Action::HideUniform => {
                let sheet = self.sheets.active().clone();
                self.spawn_job("uniform", move || {
                    Ok(JobDone::Hide(
                        sheet.uniform_columns().map_err(|e| e.to_string())?,
                    ))
                });
            }
            Action::OpenColumns => self.open_columns(false),
            Action::OpenColumnsAll => self.open_columns(true),
            Action::KeySelected { .. }
            | Action::TypeSelected { .. }
            | Action::AggregateCols { .. }
            | Action::StartAggregateInput { selected: true, .. } => {
                self.toast = Some("only on the columns sheet (C)".into());
            }
            Action::StartAggregateInput { memo, selected } => {
                self.start_input(Prompt::Aggregate { memo, selected })
            }
            Action::AggregateCol { names } => {
                let Some(c) = self.current_column_name() else {
                    return;
                };
                if let Err(e) = self.sheets.active_mut().add_aggregators(&c, &names) {
                    self.toast = Some(e);
                }
            }
            Action::MemoAggregate { names } => {
                let Some(c) = self.current_column_name() else {
                    return;
                };
                self.toast = Some(match self.sheets.active().memo_aggregate(&c, &names) {
                    Ok(t) | Err(t) => t,
                });
            }
            Action::UnfurlCol => {
                let Some(c) = self.current_column_name() else {
                    return;
                };
                let id = self.sheets.alloc_id();
                match Sheet::unfurl_from(self.sheets.active(), id, &c) {
                    Ok(s) => {
                        self.sheets.push(s);
                        self.spawn_materialize();
                        self.goto_column(&format!("{c}_value"));
                    }
                    Err(e) => self.toast = Some(e),
                }
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
            Action::TimeWalk { n } => {
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
            Action::ExpandCol { depth } => self.expand(depth, false),
            Action::ExpandCols { depth } => self.expand(depth, true),
            Action::StartExpandDepthInput { all } => self.start_input(Prompt::ExpandDepth { all }),
            Action::ContractCol => {
                let Some(c) = self.current_column_name() else {
                    return;
                };
                match self.sheets.active_mut().contract(&c) {
                    Ok(parent) => {
                        self.spawn_materialize();
                        self.goto_column(&parent);
                    }
                    Err(e) => self.toast = Some(e),
                }
            }
            Action::OpenUndoTree => {
                let id = self.sheets.alloc_id();
                let tree = Sheet::undo_tree_of(self.sheets.active(), id);
                self.sheets.push(tree);
                self.spawn_materialize();
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
                    .window()
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
        if affects && self.sheets.active().revision() == before_rev {
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
        let Some(df) = s.window() else { return false };
        let rows = df.height() as u64;
        let (off, wr) = (s.view.window_offset, s.view.window_rows as u64);
        let last = match s.row_count() {
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
        if let Some(t) = s.row_count() {
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
        let Some(df) = s.window() else { return };
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
        self.sheets.active_mut().set_type_cast(name, dt);
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

    /// A new `/` `?` `g/` `g?` `z/` `z?` search: remember it, jump to the first hit.
    pub(super) fn start_search(&mut self, st: SearchState) {
        self.sheets.active_mut().view.search = Some(st);
        self.dispatch_sequence(vec![Action::SearchNext]);
    }

    /// `n` (`reverse` false) / `N`: next hit in the search's direction, or
    /// against it, wrapping. A column search looks in the cursor column.
    fn search_jump(&mut self, reverse: bool) {
        let s = self.sheets.active();
        let Some(st) = s.view.search.clone() else {
            self.toast = Some("no search; press / first".into());
            return;
        };
        let backward = st.backward != reverse;
        let cur = s.view.window_offset + s.view.row_cursor as u64;
        let cur_col = s.view.col_cursor;
        let column = (!st.all_columns && !st.expr)
            .then(|| self.current_column_name())
            .flatten();
        let sheet = s.clone();
        // ponytail: full scan per n/N; cache hits per (pattern, view) if big files lag.
        self.spawn_job("search", move || {
            let hits: Vec<(u64, usize)> = if st.expr {
                let rows = sheet.expr_hits(&st.pattern)?;
                rows.into_iter().map(|r| (r, cur_col)).collect()
            } else {
                sheet
                    .search_hits(&st.pattern, column.as_deref())
                    .map_err(|e| e.to_string())?
            };
            let next = if backward {
                hits.iter().rev().find(|(r, _)| *r < cur).or(hits.last())
            } else {
                hits.iter().find(|(r, _)| *r > cur).or(hits.first())
            };
            Ok(match next.copied() {
                Some((row, col)) => JobDone::Jump { row, col },
                None if st.expr => JobDone::Toast(format!("no rows where {}", st.pattern)),
                None => JobDone::Toast(format!("no match for /{}", st.pattern)),
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
        let s = self.sheets.active();
        let plan = s.view_plan();
        let edits = s.edits();
        let table = s.table_name();
        let path = path.to_string();
        self.spawn_job("save", move || {
            let mut df = plan.collect().map_err(|e| format!("collect: {e}"))?;
            let p = std::path::Path::new(&path);
            if crate::duckdb::is_db(p) {
                crate::duckdb::save(&mut df, p, &table)?;
            } else {
                crate::sheet::write_df_by_extension(&mut df, p).map_err(|e| e.to_string())?;
            }
            let note = format!("saved {} rows → {path}", df.height());
            Ok(JobDone::Saved { note, edits })
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
        self.clipboard = super::clipboard_for(self.plugins.as_ref());
        self.paste_cmd = super::paste_cmd_for(self.plugins.as_ref());
        crate::duckdb::set_cmd(self.plugins.as_ref().and_then(|p| p.duckdb_cmd()));
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
        let reopening = self.busy.iter().any(|j| j.label == "tail reopen");
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
        let was_at_bottom = match (active.row_count(), active.window()) {
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
            Origin::UndoTree { parent } => {
                let parent = *parent;
                let s = self.sheets.active();
                let state = s
                    .window()
                    .and_then(|df| df.column("state").ok()?.get(s.view.row_cursor).ok())
                    .and_then(|v| v.try_extract::<u64>().ok());
                let Some(state) = state else { return };
                self.sheets.pop();
                if !self.sheets.switch_to(parent) {
                    self.toast = Some("undotree: that sheet was closed".into());
                    return;
                }
                if self.sheets.active_mut().goto_state(state) {
                    self.toast = Some(format!("at state {state}"));
                    self.spawn_materialize();
                }
            }
            Origin::Freq { .. } => self.dive_freq(false),
            Origin::Parts { path, kind } => {
                let (path, kind) = (path.clone(), *kind);
                let s = self.sheets.active();
                let part = s.window().and_then(|df| {
                    let at =
                        |c: &Column| c.get(s.view.row_cursor).ok()?.get_str().map(String::from);
                    let name = at(df.columns().first()?)?;
                    match kind {
                        crate::reader::SourceKind::Duckdb => {
                            Some(format!("{}.{name}", at(df.column("schema_name").ok()?)?))
                        }
                        _ => Some(name),
                    }
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
        let Some(df) = s.window() else { return };
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
            let values = plan
                .select([nth(0).as_expr()])
                .collect()
                .map_err(|e| e.to_string())?;
            let vals = &values.columns()[0];
            let picked: Vec<AnyValue> = rids
                .iter()
                .filter_map(|r| vals.get(*r as usize).ok().map(|v| v.into_static()))
                .collect();
            let child = Sheet::rows_with_values(&parent, id, &column, &picked);
            Ok(JobDone::PushSheet(Box::new(child)))
        });
    }

    /// `:exec-sql`: run statements in the `.duckdb` file the active sheet came
    /// from (its index or one of its tables), as VisiData.
    fn exec_sql(&mut self, sql: String) {
        use crate::reader::{LoadedSource, SourceKind};
        let s = self.sheets.active();
        let db = match (&s.origin, &s.source.path) {
            (
                Origin::Parts {
                    path,
                    kind: SourceKind::Duckdb,
                },
                _,
            ) => path.clone(),
            (_, Some(p)) if s.kind == SourceKind::Duckdb => p.clone(),
            _ => {
                self.toast = Some("exec-sql: not a .duckdb sheet".into());
                return;
            }
        };
        let id = self.sheets.alloc_id();
        self.spawn_job("exec-sql", move || {
            let rows = crate::duckdb::exec(&db, &sql)?;
            let index = crate::duckdb::index(&db)?;
            let rows = rows.map(|df| {
                let src = LoadedSource::from_df(
                    SourceKind::Duckdb,
                    "exec-sql".into(),
                    Some(db.clone()),
                    df,
                );
                Box::new(Sheet::from_source(id, src))
            });
            Ok(JobDone::ExecSql { db, index, rows })
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

    pub(super) fn pop_sheet(&mut self) {
        let popped_id = self.sheets.active().id;
        if self.sheets.pop() {
            if let Some(h) = self.materialize_handles.remove(&popped_id) {
                h.abort();
            }
            let s = self.sheets.active_mut();
            if std::mem::take(&mut s.state.stale) {
                self.spawn_materialize();
            }
        } else {
            self.quit = true;
        }
    }

    /// The question stays in the status line until a key answers it.
    fn ask(&mut self, c: Confirm, question: String) {
        self.confirm = Some((c, format!("{question} y/N")));
    }

    /// Row id under the cursor; None on an empty view.
    pub(super) fn rid_at_cursor(&self) -> Option<u64> {
        let s = self.sheets.active();
        s.window_rids().get(s.view.row_cursor).copied()
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

    /// `:select` / `:unselect` / `z|` / `z\`: rows where a SQL predicate holds.
    fn select_by_expr(&mut self, expr: String, unselect: bool) {
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
    fn copy(&mut self, selected: bool, cells: bool) -> bool {
        self.spawn_copy(selected, cells, None)
    }

    /// `Y` …: visible columns as TSV (rows) or one value per line (cells).
    fn syscopy(&mut self, selected: bool, cells: bool) {
        self.spawn_copy(selected, cells, Some(self.clipboard.clone()));
    }

    /// Register copy, or with `clip` the same rows as text to the system clipboard.
    fn spawn_copy(
        &mut self,
        selected: bool,
        cells: bool,
        clip: Option<crate::clipboard::Clipboard>,
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
            let mut df = sheet.copy_rows(pos).map_err(|e| e.to_string())?;
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
            Ok(JobDone::Copy(Box::new(Register { kind, df })))
        });
        true
    }

    /// `zp` / `gzp`: the copied cells (`zy` `gzy`) into the current column.
    /// `zP` `gzP`: read the clipboard in the background, or take the last
    /// terminal paste when there's no paste command (SSH).
    fn syspaste(&mut self, selected: bool) {
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
    pub(super) fn paste_text(&mut self, text: &str, selected: bool) {
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

    /// The active sheet's visible columns, in screen order.
    /// Cursor onto visible column `name`, if it is one.
    fn goto_column(&mut self, name: &str) {
        let s = self.sheets.active_mut();
        if let Some(i) = s
            .view_plan()
            .collect_schema()
            .ok()
            .and_then(|sc| sc.index_of(name))
        {
            s.view.col_cursor = i;
        }
    }

    /// `(` `z(` on the cursor column; `g(` `gz(` (`all`): every struct column.
    fn expand(&mut self, depth: usize, all: bool) {
        let done = match all {
            // the cursor stays put: on the first new column, the struct hidden
            false => match self.current_column_name() {
                Some(c) => self.sheets.active_mut().expand(&c, depth),
                None => return,
            },
            true => self.sheets.active_mut().expand_all(depth).map(|n| {
                self.toast = Some(format!(
                    "expanded {n} struct column{}",
                    if n == 1 { "" } else { "s" }
                ));
            }),
        };
        match done {
            Ok(()) => self.spawn_materialize(),
            Err(e) => self.toast = Some(e),
        }
    }

    fn visible_cols(&self) -> Vec<String> {
        let s = self.sheets.active();
        s.view_plan()
            .collect_schema()
            .map(|sc| sc.iter_names().map(|n| n.to_string()).collect())
            .unwrap_or_default()
    }

    /// The terminal pasted `text` (bracketed paste). In a prompt it's typed
    /// in (first line); in the table it fills the register, as TSV or CSV,
    /// for `p` `zp` `gzp`, and is what `zP` `gzP` paste without a command.
    pub(super) fn on_paste(&mut self, text: String) {
        self.toast = None;
        if let Some((_, line)) = self.input.as_mut() {
            for c in text.lines().next().unwrap_or_default().chars() {
                line.insert(c);
            }
            self.refresh_pivot_preview();
            return;
        }
        let table = crate::clipboard::table(&text);
        let Some(width) = table.iter().map(Vec::len).max() else {
            return;
        };
        self.pasted = Some(text);
        let all = self.visible_cols();
        // A first line naming the sheet's columns is a header (`Y` copies one).
        let header = table.len() > 1 && table[0].iter().all(|f| all.contains(f));
        let (names, rows) = if header {
            (table[0].clone(), &table[1..])
        } else {
            let mut names = all;
            names.truncate(width);
            (names, &table[..])
        };
        let cols: Vec<Column> = names
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let vals: Vec<Option<&str>> = rows
                    .iter()
                    .map(|r| r.get(i).map(String::as_str).filter(|v| !v.is_empty()))
                    .collect();
                Column::new(n.as_str().into(), vals)
            })
            .collect();
        let kind = if cols.len() == 1 {
            RegisterKind::Cells
        } else {
            RegisterKind::Rows
        };
        let (h, w) = (rows.len(), cols.len());
        match DataFrame::new_infer_height(cols) {
            Ok(df) if w > 0 => {
                self.register = Some(Register { kind, df });
                self.toast = Some(format!(
                    "clipboard: {} row{} × {w} col{}",
                    grouped(h as u64),
                    if h == 1 { "" } else { "s" },
                    if w == 1 { "" } else { "s" }
                ));
            }
            Ok(_) => self.toast = Some("pasted: the sheet has no columns".into()),
            Err(e) => self.toast = Some(format!("paste: {e}")),
        }
    }

    fn paste_cells(&mut self, selected: bool) {
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
        let s = self.sheets.active_mut();
        let done = if selected {
            s.paste_cells(&column, &values)
                .map(|n| format!("pasted into {column} on {} rows", grouped(n as u64)))
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
        match done {
            Ok(note) => {
                if !note.is_empty() {
                    self.toast = Some(note);
                }
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("paste: {e}")),
        }
    }

    /// `zx` / `gzx` (`copy`): copy the cell(s) to the register, then null
    /// them; `zd` / `gzd`: just null them.
    fn clear_cells(&mut self, selected: bool, copy: bool) {
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
        if copy && !self.copy(selected, true) {
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
    fn delete_rows(&mut self, selected: bool, cut: bool) {
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
        if cut && !self.copy(selected, false) {
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
    fn add_rows(&mut self, n: usize, values: Option<DataFrame>, after: bool) {
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
        if let Ok(Some(at)) = s.view_index_of(ids[0]) {
            let half = s.view.window_rows as u64 / 2;
            s.view.window_offset = at.saturating_sub(half);
            s.view.row_cursor = (at - s.view.window_offset) as usize;
        }
        self.spawn_materialize();
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

/// `3 edits`.
fn edits(n: usize) -> String {
    if n == 1 {
        "1 edit".into()
    } else {
        format!("{n} edits")
    }
}
