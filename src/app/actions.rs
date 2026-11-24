//! Action dispatch: what each keymap / ex-line `Action` does to the app.

use super::*;

mod cols;
mod edit;
mod files;
mod nav;
mod select;
mod sheets;
mod ui;
mod view;
mod vim;

use std::ops::ControlFlow::{self, Break, Continue};

/// `:scale`'s colors when none are given.
const SCALE_LOW: &str = "#2c7bb6";
const SCALE_HIGH: &str = "#d7191c";

impl App {
    /// `:scale` (`colors`) / `:databar` (`None`): the column's range comes
    /// from a job; the rule lands with it (S-27 B).
    fn scale(&mut self, column: String, colors: Option<(String, String)>) {
        if let Some((lo, hi)) = &colors
            && let Err(e) = crate::theme::parse_color(lo).and(crate::theme::parse_color(hi))
        {
            self.toast = Some(format!("scale: {e}"));
            return;
        }
        let sheet = self.sheets.active().clone();
        self.spawn_job("scale", move || {
            let range = sheet.column_range(&column)?;
            Ok(JobDone::Scale(crate::sheet::ScaleRule {
                column,
                colors,
                range,
            }))
        });
    }

    pub(super) fn dispatch_sequence(&mut self, actions: Vec<Action>) {
        if self.replaying {
            for a in actions {
                self.dispatch(a);
            }
            return;
        }
        if actions.iter().any(|a| a.repeatable()) {
            self.last_action = Some(actions.clone());
        }
        let name = actions.first().and_then(Action::to_ex);
        let name = name.as_deref().and_then(|e| e.split(' ').next());
        let keys = std::mem::take(&mut self.last_keys);
        self.last_cmd = format!("{keys}  {}", name.unwrap_or_default())
            .trim()
            .to_string();
        let refused: Vec<String> = actions.iter().filter_map(|a| self.record(a)).collect();
        for a in actions {
            self.dispatch(a);
        }
        if !refused.is_empty() {
            self.toast = Some(format!("not recorded (no ex form): {}", refused.join(", ")));
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
        if let Action::Color {
            column,
            pred,
            style,
        } = &action
        {
            let s = self.sheets.active();
            let err = match column {
                Some(c) if !s.has_column(c) => Some(format!("no column: {c}")),
                _ => s
                    .check_predicate(pred)
                    .and_then(|()| crate::theme::parse_style(style, false).map(drop))
                    .err(),
            };
            if let Some(e) = err {
                self.toast = Some(format!("color: {e}"));
                return;
            }
        }
        if matches!(self.sheets.active().origin, Origin::Columns { .. })
            && self.columns_dispatch(&action)
        {
            return;
        }
        if matches!(self.sheets.active().origin, Origin::Macros) && self.macros_dispatch(&action) {
            return;
        }
        if matches!(self.sheets.active().origin, Origin::Options) && self.options_dispatch(&action)
        {
            return;
        }
        if matches!(self.sheets.active().origin, Origin::Plot { .. }) && self.plot_dispatch(&action)
        {
            return;
        }
        if self.visual.is_some() && self.visual_dispatch(&action) {
            return;
        }
        if action.undoable() {
            self.sheets.active_mut().checkpoint();
        }
        if action.is_jump() {
            self.record_jump();
        }
        // `"xp`: the paste reads register `x` (`"+p`: the system clipboard)
        let pastes = matches!(
            action,
            Action::PasteCell | Action::PasteCells | Action::PasteAfter | Action::PasteBefore
        );
        if pastes && let Some(c) = self.register_prefix.take().filter(|c| *c != '"') {
            // ponytail: clipboard text has no row schema; `"+p` could parse it
            // into rows like `:patch` if someone needs it
            if c == '+' && matches!(action, Action::PasteAfter | Action::PasteBefore) {
                self.toast =
                    Some("\"+ pastes cells (\"+zp); rows from the clipboard: not yet".into());
                return;
            }
            if c == '+' {
                return self.syspaste(action == Action::PasteCells);
            }
            let Some(named) = self.registers.get(&c).cloned() else {
                self.toast = Some(format!("register \"{c} is empty"));
                return;
            };
            let unnamed = self.register.replace(named);
            self.dispatch(action);
            self.register = unnamed;
            return;
        }
        if self.route(action).is_break() {
            return;
        }
        // `stale`: changed from a columns sheet while under it, now on top
        // again (popped, or raised from the sheet list / undo tree)
        let s = self.sheets.active_mut();
        let stale = std::mem::take(&mut s.state.stale);
        if stale || (affects && s.revision() == before_rev) {
            self.spawn_materialize();
        }
    }

    /// `dispatch` after its checks: what `action` does. `Break`: done, skip
    /// the rematerialize check.
    fn route(&mut self, action: Action) -> ControlFlow<()> {
        match action {
            Action::Quit => self.quit(),
            Action::ForceQuit => self.quit = true,
            Action::ForcePopSheet => self.pop_sheet(),
            Action::CancelJobs => self.cancel_jobs(false),
            Action::CancelAllJobs => self.cancel_jobs(true),
            Action::PopSheet => self.ask_pop_sheet(),
            Action::Redraw => {}
            Action::ToggleMacroRecord => match self.recording {
                None => {
                    self.recording = Some(Recording::default());
                    self.toast = Some("recording macro; m to stop".into());
                }
                Some(_) => self.start_input(Prompt::MacroKey),
            },
            Action::Macro(steps) => self.run_macro(steps),
            Action::Lua(id) => self.run_lua(id, None),
            Action::LuaCommand { id, args } => self.run_lua(id, Some(&args)),
            Action::RepeatLast => {
                if let Some(last) = self.last_action.clone() {
                    for a in last {
                        self.dispatch(a);
                    }
                }
            }
            Action::CursorDown { count } => self.move_cursor_rows(steps(count)),
            Action::CursorUp { count } => self.move_cursor_rows(-steps(count)),
            Action::CursorLeft { count } => self.move_cursor_cols(-steps(count)),
            Action::CursorRight { count } => self.move_cursor_cols(steps(count)),
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
            Action::FreezeCol => self.freeze_col()?,
            Action::AddColAggregate { aggs } => self.addcol_aggregate(&aggs),
            Action::AddColRankSheet => self.addcol_rank_sheet(),
            Action::AddColAgg {
                name,
                agg,
                by,
                over,
                casts,
            } => self.addcol_agg(&name, &agg, (&by, &over), &casts),
            Action::HideCol => {
                if let Some(c) = self.current_column_name() {
                    self.sheets.active_mut().view.hidden.insert(c);
                    self.spawn_materialize();
                }
            }
            Action::HideNamed { column } => {
                let s = self.sheets.active_mut();
                if !s.has_column(&column) && !s.view.hidden.contains(&column) {
                    self.toast = Some(format!("no column: {column}"));
                } else if s.view.hidden.insert(column) {
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
            Action::KeyCol => self.key_col(),
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
            Action::RenameCol { new_name } => self.rename_col(new_name)?,
            Action::RenameColsFromRows { all, overwrite } => {
                self.rename_cols_from_rows(all, overwrite)
            }
            Action::TypeCastInt => self.push_type_cast(DataType::Int64),
            Action::TypeCastFloat => self.push_type_cast(DataType::Float64),
            Action::TypeCastString => self.push_type_cast(DataType::String),
            Action::TypeCastDate => self.type_cast_date()?,
            Action::TypeCastDateAs { time: true } => {
                self.push_type_cast(DataType::Datetime(TimeUnit::Microseconds, None))
            }
            Action::TypeCastDateAs { time: false } => self.push_type_cast(DataType::Date),
            Action::TypeCastCurrency => self.push_currency_cast(),
            Action::TypeCastJson => self.type_cast_json()?,
            Action::TypeCastLen => self.push_type_cast(crate::sheet::LEN_CAST),
            Action::StartSearchInput {
                all_columns,
                backward,
            } => self.start_input(Prompt::Search {
                all_columns,
                backward,
            }),
            Action::StartSearchKeysInput => self.start_input(Prompt::SearchKeys),
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
            Action::SetSort { column, descending } => self.set_sort(column, descending),
            Action::ClearSort => {
                let s = self.sheets.active_mut();
                if !s.view.sort.is_empty() {
                    s.view.sort.clear();
                }
            }
            Action::OpenFreqTable => self.open_freq_table(),
            Action::OpenSheetSwitcher => self.open_sheet_switcher(false),
            Action::OpenSheetsAll => self.open_sheet_switcher(true),
            Action::Pivot {
                index,
                on,
                values,
                agg,
            } => self.open_pivot(&index, &on, &values, &agg),
            Action::Unpivot { on, index } => self.open_unpivot(&on, &index),
            Action::Diff { path, keys } => self.open_diff(&path, &keys),
            Action::Tail { path, follow } => self.open_tail(&path, follow),
            Action::Open { path, format } => self.open_file(&path, format.as_deref()),
            Action::OpenSysPaste { format } => self.open_syspaste(&format),
            Action::Source { path } => self.source(&path, false),
            Action::Patch { column, path } => self.patch(column, path),
            Action::Hits { needle } => self.hits(needle)?,
            Action::StartNoteInput => self.start_note_input()?,
            Action::SetNote { text } => self.set_note(text)?,
            Action::Notes => self.notes()?,
            Action::Refresh => self.refresh(),
            Action::HelpCommands => self.help_commands(),
            Action::Visual { block } => {
                let s = self.sheets.active();
                let row = s.view.window_offset + s.view.row_cursor as u64;
                let col = s.view.col_cursor;
                self.visual = Some(Visual::new(s, row, col, (row, col), block));
            }
            Action::RegisterPrefix => self.awaiting = Some(crate::app::Await::Register),
            Action::SetMark => self.awaiting = Some(crate::app::Await::SetMark),
            Action::GoMark => self.awaiting = Some(crate::app::Await::GoMark),
            Action::PlayMacro => self.awaiting = Some(crate::app::Await::Play),
            Action::RecordMacro => self.record_macro(),
            Action::Operator { op, count } => {
                self.operator = Some(crate::app::Pending {
                    op,
                    count,
                    ..Default::default()
                });
            }
            Action::Registers => self.registers(),
            Action::HalfPage { down } => {
                let count = Some((self.viewport_rows / 2).max(1));
                self.dispatch(match down {
                    true => Action::CursorDown { count },
                    false => Action::CursorUp { count },
                });
            }
            Action::ScreenRow { at } => self.screen_row(at),
            Action::ScrollTo { at } => self.scroll_to(at),
            Action::CellMotion { to } => self.cell_motion(to)?,
            Action::ValueChange { forward } | Action::NullChange { forward } => {
                self.value_change(forward, action)?
            }
            Action::GotoSelected { forward } => self.goto_selected(forward),
            Action::SortKeys { descending } => self.sort_keys(descending)?,
            Action::Transpose => self.transpose(),
            Action::JoinAll { how } => self.open_join_all(&how),
            Action::OpenCell if matches!(self.sheets.active().origin, Origin::Freq { .. }) => {
                self.dive_freq(false, true)
            }
            Action::OpenCell => self.open_cell()?,
            Action::DiveExcept { selected } => self.dive_freq(selected, true),
            Action::FreqKeys => self.freq_keys()?,
            Action::Melt => {
                let keys = self.sheets.active().view.keys.clone();
                let on: Vec<String> = (self.column_names().into_iter())
                    .filter(|c| !keys.contains(c))
                    .collect();
                self.open_unpivot(&on, &keys);
            }
            Action::ExPrompt { cmd } => self.ex_prompt(cmd),
            Action::StartGoColRegexInput => self.start_input(Prompt::GoColRegex),
            Action::SearchCell { backward } => self.search_cell(backward)?,
            Action::Reselect => match self.last_visual {
                Some(v) if v.on(self.sheets.active()) => {
                    self.goto_row(v.end.0);
                    self.sheets.active_mut().view.col_cursor = v.end.1;
                    self.visual = Some(v);
                }
                _ => self.toast = Some("no visual range on this sheet".into()),
            },
            Action::JumpBack => self.jump_back(),
            Action::JumpForward => match self.jump_at {
                Some(i) if i + 1 < self.jumps.len() => self.go_jump(i + 1),
                _ => self.toast = Some("at the newest jump".into()),
            },
            Action::Jumps => self.jumps(),
            Action::JumpPrev => self.jump_prev(),
            Action::Suspend => self.suspend = true,
            Action::Plot { x, ys } => self.plot(x, ys),
            Action::PlotColumn => self.plot_keys(false),
            Action::PlotResize { .. } | Action::ZoomAll | Action::ZoomCursor => {
                self.toast = Some("not a plot (. plots)".into());
            }
            Action::Hist { bins } => self.hist(bins)?,
            Action::Bars => {
                let parent = self.sheets.active().clone();
                let id = self.sheets.alloc_id();
                self.spawn_job("bar", move || {
                    Ok(JobDone::PushSheet(Box::new(Sheet::bars_from(&parent, id)?)))
                });
            }
            Action::PlotNumerics => self.plot_keys(true),
            Action::SplitPane { .. }
            | Action::ClosePane
            | Action::FocusPane
            | Action::SwapPanes => {
                self.pane(&action);
            }
            Action::Link { on } => self.link(on)?,
            Action::CopyStats => self.copy_stats()?,
            Action::StartSubstInput { mode } => match self.sheets.active().editable() {
                Ok(()) => self.start_input(Prompt::Subst { mode }),
                Err(e) => self.toast = Some(e),
            },
            Action::Substitute {
                pattern,
                replacement,
                all,
            } => self.substitute(&pattern, &replacement, all),
            Action::SubstPrompted {
                pattern,
                replacement,
                mode,
            } => self.subst_prompted(&pattern, &replacement, mode),
            Action::SelectIds { ids } => {
                let s = self.sheets.active_mut();
                let mut sel = (*s.view.selected).clone();
                sel.extend(ids);
                s.view.selected = std::sync::Arc::new(sel);
            }
            Action::MkSession { path } => {
                let path = path.map_or_else(crate::session::default_path, Into::into);
                self.toast = Some(match crate::session::save(self.sheets.iter(), &path) {
                    Ok(()) => format!("session saved to {}", path.display()),
                    Err(e) => format!("mksession {}: {e}", path.display()),
                });
            }
            Action::SetCursor { row, column } => self.set_cursor(row, column),
            Action::Save { path } => self.save_active(&path),
            Action::SaveAll { path } => self.save_all(&path),
            Action::SubmitRow => self.submit_row(),
            // VisiData's s / t / u move the cursor down after
            Action::ToggleSelectRow => {
                self.toggle_select_current();
                self.move_cursor_rows(1);
            }
            Action::SelectRow => {
                self.set_select_current(true);
                self.move_cursor_rows(1);
            }
            Action::UnselectRow => {
                self.set_select_current(false);
                self.move_cursor_rows(1);
            }
            Action::SelectAll => self.select_all(),
            Action::UnselectAll => {
                self.sheets.active_mut().view.selected = Default::default();
            }
            Action::StartSelectInput { unselect, all } => {
                self.start_input(Prompt::Select { unselect, all })
            }
            Action::SelectByRegex {
                regex,
                unselect,
                all,
            } => self.select_by_regex(&regex, unselect, all),
            Action::SelectExpr { expr, unselect } => self.select_by_expr(expr, unselect),
            Action::DupSelected => self.open_dup_selected(),
            Action::CopyRow | Action::CopySelected | Action::CopyCell | Action::CopyCells => {
                let selected = matches!(action, Action::CopySelected | Action::CopyCells);
                self.copy(
                    selected,
                    matches!(action, Action::CopyCell | Action::CopyCells),
                    true,
                );
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
            Action::PasteAfter | Action::PasteBefore => self.paste_rows(action)?,
            Action::OpenNew => {
                let id = self.sheets.alloc_id();
                self.sheets.push(Sheet::open_new(id));
                self.spawn_materialize();
            }
            Action::DeleteSelected | Action::CutSelected => {
                self.delete_rows(true, action == Action::CutSelected)
            }
            Action::DedupeRows => self.dedupe_rows(),
            Action::SelectColDuplicates => self.select_col_duplicates()?,
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
                    None => Some(self.sidebar_page.clone()),
                };
            }
            Action::CycleSidebar => self.cycle_sidebar(),
            Action::ShowSidebar { page } => self.show_sidebar(page),
            Action::Join { source, keys, how } => self.open_join(&source, &keys, &how),
            Action::Lookup {
                source,
                key,
                their_key,
                cols,
            } => self.lookup(&source, &key, &their_key, &cols),
            Action::StartAddColInput | Action::StartCaptureInput => {
                self.start_add_col_input(action)
            }
            Action::StartSlideInput { left } => self.start_input(Prompt::Slide { left }),
            Action::SlideCol { by } => self.slide_col(by)?,
            Action::Order { columns } => self.order(columns)?,
            Action::AddCol { name } => self.add_col(name),
            Action::AddColIncr | Action::AddColIncrKey => self.add_col_incr(action),
            Action::SetColIncr | Action::SetColFill | Action::Capture { .. } => {
                self.set_col_fill(action)?
            }
            Action::PrecisionMore | Action::PrecisionLess | Action::SetPrecision { .. } => {
                self.precision(action)?
            }
            Action::MacroSheet => self.open_macros(),
            Action::OptionsSheet => self.open_options(),
            Action::CommitSheet => self.ask_commit_sheet(),
            Action::ToggleFooter => {
                let on = (!self.theme.footer).to_string();
                self.toast = Some(self.set_option("disp_footer", Some(&on)));
            }
            Action::SetOption { name, value } => {
                self.toast = Some(self.set_option(&name, value.as_deref()));
            }
            Action::SetFmtstr { fmt } => {
                let Some(column) = self.current_column_name() else {
                    return Break(());
                };
                let s = self.sheets.active_mut();
                self.toast = Some(s.set_fmtstr(&column, &fmt).unwrap_or_else(|e| e));
            }
            Action::Derive { name, expr } => self.derive(name, expr),
            Action::StartEditInput => self.start_edit_input()?,
            Action::StartSetColInput => match self.sheets.active().editable() {
                Ok(()) => self.start_input(Prompt::SetCol),
                Err(e) => self.toast = Some(e),
            },
            Action::StartSetExprInput => match self.sheets.active().editable() {
                Ok(()) => self.start_input(Prompt::SetExpr),
                Err(e) => self.toast = Some(e),
            },
            Action::SetExpr { expr } => self.set_expr(expr)?,
            Action::Color {
                column,
                pred,
                style,
            } => self.color(column, pred, style),
            Action::Validate { column, pred } => self.validate(column, pred)?,
            Action::Validations => self.toast = Some(self.sheets.active().validations_text()),
            Action::Trust { arg } => self.trust(arg.as_deref()),
            Action::Handlers => self.handlers(),
            Action::Scale { column, colors } => {
                let colors = colors.unwrap_or_else(|| (SCALE_LOW.into(), SCALE_HIGH.into()));
                self.scale(column, Some(colors))
            }
            Action::DataBar { column } => self.scale(column, None),
            Action::NoColor { column } => self.no_color(column),
            Action::SetFilterExpr { expr } => self.set_filter_expr(expr),
            Action::EditCell { value } => self.edit_cell(value)?,
            Action::SetCol { value } => self.set_col(value)?,
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
            Action::SetcolPrecisionInput => {
                self.start_input(Prompt::Ex);
                if let Some((_, line)) = self.input.as_mut() {
                    line.set("precision ");
                }
            }
            Action::KeySelected { .. }
            | Action::TypeSelected { .. }
            | Action::JoinCols
            | Action::AggregateCols { .. }
            | Action::StartAggregateInput { selected: true, .. } => {
                self.toast = Some("only on the columns sheet (C)".into());
            }
            Action::StartAggregateInput { memo, selected } => {
                self.start_input(Prompt::Aggregate { memo, selected })
            }
            Action::AggregateCol { names } => self.aggregate_col(names)?,
            Action::MemoAggregate { names } => {
                let Some(c) = self.current_column_name() else {
                    return Break(());
                };
                self.toast = Some(match self.sheets.active().memo_aggregate(&c, &names) {
                    Ok(t) | Err(t) => t,
                });
            }
            Action::UnfurlCol => self.unfurl_col()?,
            Action::DupSheet => {
                let id = self.sheets.alloc_id();
                let copy = Sheet::copy_from(self.sheets.active(), id);
                self.sheets.push(copy);
                self.spawn_materialize();
            }
            Action::Describe => self.describe(),
            Action::PivotCurrent => self.pivot_current()?,
            Action::TimeWalk { n } => self.time_walk(n),
            Action::ExpandCol { depth } => self.expand(depth, false),
            Action::ExpandCols { depth } => self.expand(depth, true),
            Action::StartExpandDepthInput { all } => self.start_input(Prompt::ExpandDepth { all }),
            Action::ContractCol => self.contract_col()?,
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
            Action::SubmitSelected => self.dive_freq(true, false),
            Action::SelectEqualCell => self.select_equal_cell()?,
            Action::StartDiffInput => self.start_input(Prompt::Diff),
            Action::SelectToSelected { forward } => self.select_to_selected(forward),
            // checkpointed when the rows arrive (`JobDone::SelectIn`)
            Action::SelectBefore | Action::UnselectBefore
                if let Origin::Describe { parent } = self.sheets.active().origin =>
            {
                self.select_described(parent, matches!(action, Action::SelectBefore));
            }
            Action::SelectBefore | Action::UnselectBefore => self.select_before(action),
            Action::StartGoColInput => self.start_input(Prompt::GoCol),
            Action::OpenCellFile => self.open_cell_file(),
            Action::ResizeColMax => self.resize_col_max()?,
            Action::ShowVersion => {
                self.toast = Some(format!("vdrs {}", env!("CARGO_PKG_VERSION")));
            }
            Action::SubmitInput
            | Action::CancelInput
            | Action::InputBackspace
            | Action::InputNormal => {}
        }
        Continue(())
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
        let new = (s.view.col_cursor as isize)
            .saturating_add(delta)
            .clamp(0, cols as isize - 1) as usize;
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
        match self.sheets.active_mut().set_type_cast(name, dt) {
            Ok(()) => self.spawn_materialize(),
            Err(e) => self.toast = Some(format!("type: {e}")),
        }
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

    pub(super) fn sort_current_column(&mut self, order: SortOrder) {
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
        let columns: Option<Vec<String>> = match () {
            _ if st.keys => {
                let visible = self.visible_cols();
                let keys: Vec<String> = (s.view.keys.iter())
                    .filter(|k| visible.contains(k))
                    .cloned()
                    .collect();
                Some(match keys.is_empty() {
                    true => visible.into_iter().take(1).collect(),
                    false => keys,
                })
            }
            _ if st.all_columns || st.expr => None,
            _ => Some(self.current_column_name().into_iter().collect()),
        };
        let sheet = s.clone();
        // ponytail: full scan per n/N; cache hits per (pattern, view) if big files lag.
        self.spawn_job("search", move || {
            let hits: Vec<(u64, usize)> = if st.expr {
                let rows = sheet.expr_hits(&st.pattern)?;
                rows.into_iter().map(|r| (r, cur_col)).collect()
            } else {
                let hits = sheet
                    .search_hits(&st.pattern, columns.as_deref())
                    .map_err(|e| e.to_string())?;
                match st.keys {
                    true => hits.into_iter().map(|(r, _)| (r, cur_col)).collect(),
                    false => hits,
                }
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

    /// An edit's result: its note (if any) as the toast and a fresh window,
    /// or the error after `prefix`.
    pub(super) fn edit_done(&mut self, done: Result<String, String>, prefix: &str) {
        match done {
            Ok(note) => {
                if !note.is_empty() {
                    self.toast = Some(note);
                }
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("{prefix}{e}")),
        }
    }

    /// `:s`: the current column of the selected rows; none selected: of the
    /// view's rows (every row when nothing filters it).
    fn substitute(&mut self, pattern: &str, replacement: &str, all: bool) {
        let Some(c) = self.current_column_name() else {
            return;
        };
        let s = self.sheets.active();
        let (rids, rows) = if !s.view.selected.is_empty() {
            let r: std::sync::Arc<[u64]> = s.view.selected.iter().copied().collect();
            let n = format!("{} selected rows", r.len());
            (Some(r), n)
        } else if s.view.filter.is_some() || s.view.filter_expr.is_some() {
            let (p, r) = (pattern.to_string(), replacement.to_string());
            self.scan_edit(
                "s",
                |s| s.view_rids().map_err(|e| e.to_string()),
                move |app, rids| {
                    let n = format!("{} rows in view", rids.len());
                    app.replace_on(&c, &p, &r, all, Some(rids.into()), n);
                },
            );
            return;
        } else {
            (None, "every row".into())
        };
        self.replace_on(&c, pattern, replacement, all, rids, rows);
    }

    /// `:s` on `rids` (`None`: every row); `rows` names them in the toast.
    fn replace_on(
        &mut self,
        c: &str,
        pattern: &str,
        replacement: &str,
        all: bool,
        rids: Option<std::sync::Arc<[u64]>>,
        rows: String,
    ) {
        let s = self.sheets.active_mut();
        match s.replace(c, pattern, replacement, all, rids, true) {
            Ok(()) => {
                self.toast = Some(format!("replaced /{pattern}/ in {c} on {rows}"));
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("s: {e}")),
        }
    }

    /// `*`: a new `<col>_re` column, replaced; `g*` / `gz*`: in place on the
    /// selected rows, of the current / every visible text column. One undo step.
    fn subst_prompted(&mut self, pattern: &str, replacement: &str, mode: crate::action::SubstMode) {
        use crate::action::SubstMode;
        if let Err(e) = regex::Regex::new(pattern) {
            self.toast = Some(format!("bad regex: {e}"));
            return;
        }
        let Some(c) = self.current_column_name() else {
            return;
        };
        let s = self.sheets.active_mut();
        let selected: std::sync::Arc<[u64]> = s.view.selected.iter().copied().collect();
        let done = match mode {
            SubstMode::NewColumn => {
                let name = format!("{c}_re");
                let q = crate::duckdb::ident(&c);
                s.derive(&name, &format!("CAST({q} AS VARCHAR)"))
                    .and_then(|()| s.replace(&name, pattern, replacement, true, None, false))
                    .map(|()| format!("added {name}"))
            }
            _ if selected.is_empty() => Err("no rows selected".into()),
            SubstMode::Selected => s
                .replace(&c, pattern, replacement, true, Some(selected.clone()), true)
                .map(|()| format!("replaced in {c} on {} rows", selected.len())),
            SubstMode::SelectedAll => {
                // text in the data (a `~` view cast isn't), and visible
                let visible = s.view_plan().collect_schema().ok();
                let text: Vec<String> = s
                    .plan
                    .clone()
                    .collect_schema()
                    .map(|sc| {
                        sc.iter()
                            .filter(|(n, t)| {
                                **t == DataType::String
                                    && visible.as_ref().is_some_and(|v| v.contains(n))
                            })
                            .map(|(n, _)| n.to_string())
                            .collect()
                    })
                    .unwrap_or_default();
                let before = s.snapshot();
                let mut r = Ok(());
                for col in &text {
                    r = r.and_then(|()| {
                        s.replace(
                            col,
                            pattern,
                            replacement,
                            true,
                            Some(selected.clone()),
                            false,
                        )
                    });
                }
                match r {
                    Ok(()) => s.push_undo(before),
                    // all columns or none
                    Err(_) => s.restore(before),
                }
                r.map(|()| {
                    format!(
                        "replaced in {} columns on {} rows",
                        text.len(),
                        selected.len()
                    )
                })
            }
        };
        if done.is_ok() && matches!(mode, SubstMode::NewColumn) {
            self.place_new_column(&format!("{c}_re"), Some(&c));
        }
        match done {
            Ok(msg) => {
                self.toast = Some(msg);
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("subst: {e}")),
        }
    }

    /// New column `name` right after column `at` (VisiData
    /// `addColumnAtCursor`), with the cursor on it.
    pub(super) fn place_new_column(&mut self, name: &str, at: Option<&str>) {
        if let Err(e) = self.sheets.active_mut().place_col(name, at) {
            self.toast = Some(e);
        }
        self.goto_column(name);
    }

    fn submit_row(&mut self) {
        match &self.sheets.active().origin {
            Origin::SheetSwitcher => self.switch_to_row_sheet(),
            Origin::UndoTree { parent } => {
                let parent = *parent;
                let s = self.sheets.active();
                let state = s
                    .cursor_value("state")
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
            Origin::Freq { .. } => self.dive_freq(false, false),
            Origin::Cell => {
                let row = self.cursor_row();
                let id = self.sheets.alloc_id();
                self.push_or_toast(self.sheets.active().open_cell_row(id, row));
            }
            Origin::Hits { parent } => {
                let parent = *parent;
                let s = self.sheets.active();
                let row = s
                    .cursor_value("row")
                    .and_then(|v| v.try_extract::<u64>().ok());
                let column = s.cursor_value("column");
                let cell = row.zip(column.as_ref().and_then(|v| v.get_str()).map(String::from));
                let Some((row, column)) = cell else { return };
                if !self.sheets.switch_to(parent) {
                    self.toast = Some("hits: that sheet was closed".into());
                    return;
                }
                self.goto_column(&column);
                self.goto_row(row - 1);
                self.spawn_materialize();
            }
            Origin::Parts { .. } if !self.sheets.active().source.parts.is_empty() => {
                let s = self.sheets.active();
                let name = s
                    .cursor_value("part")
                    .and_then(|v| v.get_str().map(String::from));
                let part = s
                    .source
                    .parts
                    .iter()
                    .find(|(n, _)| Some(n) == name.as_ref());
                let Some(src) = part.map(|(_, src)| src.clone()) else {
                    return;
                };
                let id = self.sheets.alloc_id();
                let child = Sheet::from_shared(id, src);
                self.sheets.push(child);
                self.spawn_materialize();
            }
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
                        crate::reader::SourceKind::Zip => Some(crate::reader::member_name(
                            &name,
                            &at(df.column("filename").ok()?)?,
                        )),
                        _ => Some(name),
                    }
                });
                let Some(part) = part else { return };
                // the list's own `header`: the one the file was opened with
                let header = self.sheets.active().source.header;
                let id = self.sheets.alloc_id();
                self.spawn_job("open", move || {
                    let loaded = crate::reader::open_part_with(&path, kind, &part, header)
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
        // pop the switcher, then raise the chosen sheet, or open a quit one again
        self.sheets.pop();
        if self.sheets.reopen(id) {
            self.spawn_materialize();
        } else if !self.sheets.switch_to(id) {
            self.toast = Some(format!("no sheet with id {id}"));
        }
    }

    /// `gF` sheet: the parent's rows whose key values are the cursor row's
    /// (`<CR>`) or any selected row's (`g<CR>`); `except`: the other rows.
    fn dive_freq_keys(&mut self, selected: bool, except: bool) {
        let s = self.sheets.active();
        let Origin::Freq { parent, columns } = s.origin.clone() else {
            return;
        };
        // compared as the freq sheet grouped them (the parent's formats then)
        let keys: Vec<_> = (columns.iter())
            .map(|c| {
                let l = s.state.lineage.as_deref();
                (
                    c.clone(),
                    l.map_or(crate::sheet::DiveKey::Value, |l| l.parent.dive_key(c)),
                )
            })
            .collect();
        let rids: Vec<u64> = match selected {
            true => s.view.selected.iter().copied().collect(),
            false => self.rid_at_cursor().into_iter().collect(),
        };
        if rids.is_empty() {
            self.toast = Some("no rows selected".into());
            return;
        }
        let plan = s.plan.clone();
        let Some(parent) = self.sheets.find_any(parent).cloned() else {
            self.toast = Some("dive: parent sheet was closed".into());
            return;
        };
        let id = self.sheets.alloc_id();
        self.spawn_job("dive", move || {
            // row ids are positions in the freq sheet's plan
            use crate::sheet::{ROW_ID, rid_in};
            let wanted = (plan.with_row_index(ROW_ID, None))
                .filter(rid_in(&rids))
                .select(columns.iter().map(|c| col(c.as_str())).collect::<Vec<_>>())
                .collect()
                .map_err(|e| e.to_string())?;
            let name = format!("{}_{}", parent.name, columns.join("_"));
            let child = Sheet::rows_with_keys(&parent, id, &keys, wanted, name, except)?;
            Ok(JobDone::PushSheet(Box::new(child)))
        });
    }

    /// Freq sheet: open the parent's rows whose value is the cursor row's
    /// (`<CR>`) or any selected row's (`g<CR>`) value. `except`
    /// (`zEnter` / `gzEnter`): the parent's other rows.
    fn dive_freq(&mut self, selected: bool, except: bool) {
        let s = self.sheets.active();
        let (parent, column) = match &s.origin {
            Origin::Freq { columns, .. } if columns.len() > 1 => {
                return self.dive_freq_keys(selected, except);
            }
            Origin::Freq { parent, columns } => (*parent, columns.concat()),
            _ => {
                let key = if except { "gz<CR>" } else { "g<CR>" };
                self.toast = Some(format!("{key}: only on a frequency sheet"));
                return;
            }
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
        let key = s.freq_dive_key();
        let own_rids = s.is_freq();
        use crate::sheet::{Binning, DiveKey};
        let Some(parent) = self.sheets.find_any(parent).cloned() else {
            self.toast = Some("dive: parent sheet was closed".into());
            return;
        };
        let id = self.sheets.alloc_id();
        self.spawn_job("dive", move || {
            // Values by row id = position in the freq sheet's base plan; the
            // value column is first even if renamed here. `column` names it in
            // the parent.
            // a bin's row id is its index, labels may repeat (`Binning::Ranges`)
            let picked: Vec<Option<String>> = match key {
                DiveKey::Bin { n, .. } if own_rids => rids
                    .iter()
                    .map(|r| (*r < n as u64).then(|| r.to_string()))
                    .collect(),
                _ => {
                    let values = plan
                        .select([nth(0).as_expr()])
                        .collect()
                        .map_err(|e| e.to_string())?;
                    let vals = &values.columns()[0];
                    let texts = rids
                        .iter()
                        .filter_map(|r| vals.get(*r as usize).ok())
                        .map(|v| (!v.is_null()).then(|| crate::sheet::any_text(&v)));
                    match &key {
                        // ponytail: a repeated label (narrow bins) picks its first bin;
                        // dive by bin index instead of label if narrow bins matter
                        DiveKey::Bin { min, width, n } => {
                            let labels = Binning::labels(*min, *width, *n);
                            texts
                                .map(|t| {
                                    let i = labels.iter().position(|l| Some(l) == t.as_ref());
                                    i.map(|i| i.to_string())
                                })
                                .collect()
                        }
                        _ => texts.collect(),
                    }
                }
            };
            let child = Sheet::rows_with_values(&parent, id, &column, key, picked, except);
            Ok(JobDone::PushSheet(Box::new(child)))
        });
    }

    pub(super) fn pop_sheet(&mut self) {
        let popped_id = self.sheets.active().id;
        if matches!(self.sheets.active().origin, Origin::Macros) {
            self.macro_edits.clear(); // not committed: dropped
        }
        if self.sheets.close() {
            // the pane's last sheet: the other pane fills the screen
            if self
                .split
                .as_ref()
                .is_some_and(|sp| sp.other == self.sheets.active().id)
            {
                self.split = None;
            }
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
    pub(super) fn ask(&mut self, c: Confirm, question: String) {
        self.confirm = Some((c, format!("{question} y/N")));
    }

    /// Cursor onto visible column `name`, if it is one.
    pub(super) fn goto_column(&mut self, name: &str) {
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

    /// `(` `z(` on the cursor column; `g(` `gz(` (`all`): every struct and
    /// list column.
    fn expand(&mut self, depth: usize, all: bool) {
        let done = match all {
            // the cursor stays put: on the first new column, the struct hidden
            false => match self.current_column_name() {
                Some(c) => self.sheets.active_mut().expand(&c, depth),
                None => return,
            },
            true => self.sheets.active_mut().expand_all(depth).map(|n| {
                self.toast = Some(format!(
                    "expanded {n} column{}",
                    if n == 1 { "" } else { "s" }
                ));
            }),
        };
        match done {
            Ok(()) => self.spawn_materialize(),
            Err(e) => self.toast = Some(e),
        }
    }

    /// The active sheet's visible columns, in screen order.
    pub(super) fn visible_cols(&self) -> Vec<String> {
        let s = self.sheets.active();
        s.view_plan()
            .collect_schema()
            .map(|sc| sc.iter_names().map(|n| n.to_string()).collect())
            .unwrap_or_default()
    }

    /// `vd.option("mouse", "true")` (S-26 A9): the wheel moves the cursor 3
    /// rows, a click puts it on a cell, a click on the header on a column.
    pub(super) fn on_mouse(&mut self, m: crossterm::event::MouseEvent) {
        use crossterm::event::{MouseButton, MouseEventKind};
        let area = self.sheet_area;
        let inside = m.column >= area.x
            && m.column < area.x + area.width
            && m.row >= area.y
            && m.row < area.y + area.height;
        match m.kind {
            MouseEventKind::ScrollDown => self.dispatch(Action::CursorDown { count: Some(3) }),
            MouseEventKind::ScrollUp => self.dispatch(Action::CursorUp { count: Some(3) }),
            MouseEventKind::Down(MouseButton::Left) if inside && self.input.is_none() => {
                let s = self.sheets.active_mut();
                let col = crate::widget::column_at(&s.view, &s.state.widths, m.column - area.x);
                if let Some(c) = col {
                    s.view.col_cursor = c;
                }
                let rows = s.window().map_or(0, |df| df.height());
                let row = s.view.row_offset + (m.row - area.y) as usize;
                if m.row > area.y && row <= rows {
                    s.view.row_cursor = row - 1;
                }
            }
            _ => {}
        }
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

    /// `S`; `all`: `gS`, the quit sheets too (S-46 B5).
    fn open_sheet_switcher(&mut self, all: bool) {
        let summaries: Vec<_> = self
            .sheets
            .summaries(all)
            .map(|(id, name, kind)| (id, name.to_string(), kind))
            .collect();
        let id = self.sheets.alloc_id();
        let mut child = Sheet::sheet_switcher(id, summaries);
        if all {
            child.name = "<sheets_all>".into();
        }
        self.sheets.push(child);
        // sheet_switcher ships window pre-materialized; still fire once so revision is nudged
        self.spawn_materialize();
    }

    /// `:set` (S-38 A): the options that change data; the rest live in
    /// `init.lua`. The toast: `name=value`, or why not.
    /// `:set`, `O` `e` (S-37 P0c): option `name` = `value` now (`None`: show
    /// it), checked through the registry and kept with the Lua options.
    pub(super) fn set_option(&mut self, name: &str, value: Option<&str>) -> String {
        let Some(opt) = crate::options::find(name) else {
            let e = crate::options::check(name, "").err().unwrap_or_default();
            return format!(":set: {e}");
        };
        let Some(value) = value else {
            return format!("{name}={}", self.option_value(name));
        };
        let v = match crate::options::check(name, value) {
            Ok(v) => v,
            Err(e) => return format!(":set: {e}"),
        };
        if self.plugins.is_none() {
            self.plugins = crate::plugins::Plugins::load_from_source("").ok();
        }
        if let Some(p) = &self.plugins {
            p.set_option(name, &v);
        }
        let t = &mut self.theme;
        match name {
            "numeric_binning" => t.numeric_binning = v == "true",
            "histogram_bins" => t.histogram_bins = v.parse().unwrap_or(0),
            "incr_base" => t.incr_base = v.parse().unwrap_or(1),
            "unfurl_empty" => t.unfurl_empty = v == "true",
            "header" => t.header = v.parse().unwrap_or(1),
            "disp_footer" => t.footer = v == "true",
            "disp_float_fmt" => {
                t.float_digits = crate::plugins::float_fmt_digits(&v);
                self.sheets.active_mut().state.widths_key = None;
            }
            "duckdb_cmd" => crate::duckdb::set_cmd(Some(v.clone())),
            "csv_skipinitialspace" => crate::reader::set_skipinitialspace(v == "true"),
            "validation" => {
                let ids: Vec<_> = self.sheets.iter().map(|s| s.id).collect();
                for id in ids {
                    if let Some(s) = self.sheets.find_by_id_mut(id) {
                        s.state.validation_warn = v == "warn";
                    }
                }
            }
            "clipboard_copy_cmd" => self.clipboard = clipboard_for(self.plugins.as_ref()),
            "clipboard_paste_cmd" => self.paste_cmd = paste_cmd_for(self.plugins.as_ref()),
            _ => {}
        }
        match opt.live {
            true => format!("{name}={v}"),
            false => format!("{name}={v} (from the next start)"),
        }
    }

    /// Option `name` as it is now: as set, else its default.
    pub(super) fn option_value(&self, name: &str) -> String {
        match &self.plugins {
            Some(p) => p.options().value(name),
            None => crate::options::find(name).map_or("", |o| o.default).into(),
        }
    }

    fn open_freq_table(&mut self) {
        let Some(col_name) = self.current_column_name() else {
            self.toast = Some("no column to freq".into());
            return;
        };
        let id = self.sheets.alloc_id();
        if self.theme.numeric_binning {
            let hist_bins = self.theme.histogram_bins;
            // the bins need the column's min and max
            let parent = self.sheets.active().clone();
            self.spawn_job("freq", move || {
                let bins = Sheet::numeric_bins(&parent, &col_name, hist_bins)?;
                let child = Sheet::freq_binned(&parent, id, &[col_name], bins);
                Ok(JobDone::PushSheet(Box::new(child)))
            });
            return;
        }
        let child = {
            let parent = self.sheets.active();
            Sheet::freq_from(parent, id, &col_name)
        };
        self.sheets.push(child);
        self.spawn_materialize();
    }
}

/// A motion count as signed steps; no count is one.
fn steps(count: Option<usize>) -> isize {
    isize::try_from(count.unwrap_or(1)).unwrap_or(isize::MAX)
}

/// `3 edits`.
fn edits(n: usize) -> String {
    if n == 1 {
        "1 edit".into()
    } else {
        format!("{n} edits")
    }
}
