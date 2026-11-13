//! Action dispatch: what each keymap / ex-line `Action` does to the app.

use super::*;

mod edit;
mod files;
mod vim;

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
            Action::ToggleMacroRecord => match self.recording {
                None => {
                    self.recording = Some(Recording::default());
                    self.toast = Some("recording macro; m to stop".into());
                }
                Some(_) => self.start_input(Prompt::MacroKey),
            },
            Action::Macro(steps) => {
                // a macro a replayed step calls runs next; a key pressed while
                // a replay still waits on a job queues behind it
                if self.replaying {
                    for a in steps.into_iter().rev() {
                        self.macro_queue.push_front((a, false));
                    }
                } else {
                    self.macro_queue
                        .extend(steps.into_iter().map(|a| (a, false)));
                }
                self.pump_macro();
            }
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
                let c2 = c.clone();
                self.scan_edit(
                    "json",
                    move |s| s.json_dtype(&c2),
                    move |app, dt| match app.sheets.active_mut().json_decode_as(&c, dt) {
                        Ok(()) => app.spawn_materialize(),
                        Err(e) => app.toast = Some(e),
                    },
                );
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
            Action::Open { path } => self.open_file(&path),
            Action::Source { path } => self.source(&path, false),
            Action::Patch { column, path } => {
                let read = polars::prelude::LazyFrame::scan_parquet(
                    path.as_str().into(),
                    Default::default(),
                )
                .and_then(|lf| lf.collect());
                let done = read.map_err(|e| e.to_string()).and_then(|df| {
                    self.sheets
                        .active_mut()
                        .restore_patch(column.as_deref(), df)
                });
                match done {
                    Ok(()) => self.spawn_materialize(),
                    Err(e) => self.toast = Some(format!("patch {path}: {e}")),
                }
            }
            Action::Hits { needle } => {
                if let Err(e) = regex::Regex::new(&needle) {
                    self.toast = Some(format!("bad regex: {e}"));
                    return;
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
            }
            Action::StartNoteInput => {
                if self.notes_mut().is_none() {
                    self.toast = Some("notes: only on sheets read from a file".into());
                    return;
                }
                let old = self.cursor_note().map(|n| n.text.clone());
                self.start_input(Prompt::Note);
                if let (Some((_, line)), Some(old)) = (self.input.as_mut(), old) {
                    line.set(&old);
                }
            }
            Action::SetNote { text } => {
                let Some((row, column)) = self.note_cell() else {
                    self.toast = Some("note: no cell here".into());
                    return;
                };
                let Some((path, notes)) = self.notes_mut() else {
                    self.toast = Some("notes: only on sheets read from a file".into());
                    return;
                };
                self.toast = Some(match notes.set(&path, row, &column, text.trim()) {
                    Err(e) => format!("note: {e}"),
                    Ok(()) if text.trim().is_empty() => "note deleted".into(),
                    Ok(()) => "note saved".into(),
                });
            }
            Action::Notes => {
                let Some((_, notes)) = self.notes_mut() else {
                    self.toast = Some("notes: only on sheets read from a file".into());
                    return;
                };
                let notes = notes.clone();
                let parent = self.sheets.active().clone();
                let id = self.sheets.alloc_id();
                self.spawn_job("notes", move || {
                    Ok(JobDone::PushSheet(Box::new(Sheet::notes_from(
                        &parent, id, &notes,
                    )?)))
                });
            }
            Action::Refresh => self.refresh(),
            Action::HelpCommands => {
                let rows: Vec<_> = self
                    .keymap
                    .chords(crate::keymap::Layer::Sheet)
                    .iter()
                    .map(|c| {
                        let keys: Vec<String> = c.on.iter().map(|k| k.to_string()).collect();
                        let run: Vec<String> = c.run.iter().filter_map(Action::to_ex).collect();
                        vec![keys.join(" "), run.join(" ; "), c.desc.to_string()]
                    })
                    .collect();
                let id = self.sheets.alloc_id();
                let names = ["key", "action", "description"];
                self.sheets.push(Sheet::listing(id, "help", &names, &rows));
                self.spawn_materialize();
            }
            Action::Visual { block } => {
                let s = self.sheets.active();
                let row = s.view.window_offset + s.view.row_cursor as u64;
                let col = s.view.col_cursor;
                self.visual = Some(Visual {
                    sheet: s.id,
                    row,
                    col,
                    end: (row, col),
                    block,
                });
            }
            Action::RegisterPrefix => self.awaiting = Some(crate::app::Await::Register),
            Action::SetMark => self.awaiting = Some(crate::app::Await::SetMark),
            Action::GoMark => self.awaiting = Some(crate::app::Await::GoMark),
            Action::PlayMacro => self.awaiting = Some(crate::app::Await::Play),
            Action::RecordMacro => match self.recording.as_ref().map(|r| r.into) {
                None => self.awaiting = Some(crate::app::Await::Record),
                Some(None) => self.start_input(Prompt::MacroKey),
                Some(Some(c)) => {
                    let steps = self.recording.take().map(|r| r.steps).unwrap_or_default();
                    self.toast = Some(format!("macro @{c}: {} steps", steps.len()));
                    self.macros.insert(c, steps);
                }
            },
            Action::Operator { op, count } => {
                self.operator = Some(crate::app::Pending {
                    op,
                    count,
                    ..Default::default()
                });
            }
            Action::Registers => {
                let mut all: Vec<(char, &Register)> =
                    self.registers.iter().map(|(c, r)| (*c, r)).collect();
                if let Some(r) = &self.register {
                    all.insert(0, ('"', r));
                }
                let rows: Vec<Vec<String>> = all
                    .iter()
                    .map(|(c, r)| {
                        let kind = match r.kind {
                            RegisterKind::Rows => "rows",
                            RegisterKind::Cells => "cells",
                        };
                        let first =
                            r.df.get_row(0)
                                .map(|row| {
                                    row.0
                                        .iter()
                                        .map(crate::sheet::any_text)
                                        .collect::<Vec<_>>()
                                        .join(" · ")
                                })
                                .unwrap_or_default();
                        vec![
                            format!("\"{c}"),
                            kind.into(),
                            r.df.height().to_string(),
                            first,
                        ]
                    })
                    .collect();
                let id = self.sheets.alloc_id();
                let names = ["register", "kind", "rows", "first row"];
                self.sheets
                    .push(Sheet::listing(id, "registers", &names, &rows));
                self.spawn_materialize();
            }
            Action::HalfPage { down } => {
                let count = Some((self.viewport_rows / 2).max(1));
                self.dispatch(match down {
                    true => Action::CursorDown { count },
                    false => Action::CursorUp { count },
                });
            }
            Action::ScreenRow { at } => {
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
            Action::ScrollTo { at } => {
                let rows = self.viewport_rows;
                let v = &mut self.sheets.active_mut().view;
                let back = match at {
                    0 => 0,
                    1 => rows / 2,
                    _ => rows.saturating_sub(1),
                };
                v.row_offset = v.row_cursor.saturating_sub(back);
            }
            Action::CellMotion { to } => {
                let s = self.sheets.active_mut();
                let Some(df) = s.window() else { return };
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
            }
            Action::ValueChange { forward } => {
                // ponytail: the loaded rows only; a job over the view if it matters
                let s = self.sheets.active_mut();
                let Some(df) = s.window() else { return };
                let (r, c) = (s.view.row_cursor, s.view.col_cursor);
                let Some(col) = df.columns().get(c) else {
                    return;
                };
                let here = col.get(r).map(|v| v.into_static()).ok();
                let differs = |i: &usize| col.get(*i).map(|v| v.into_static()).ok() != here;
                let hit = match forward {
                    true => (r + 1..df.height()).find(differs),
                    false => (0..r).rev().find(differs),
                };
                match hit {
                    Some(i) => s.view.row_cursor = i,
                    None => self.toast = Some("no other value in the loaded rows".into()),
                }
            }
            Action::SearchCell { backward } => {
                let s = self.sheets.active();
                let (Some(column), Some(df)) = (self.current_column_name(), s.window()) else {
                    return;
                };
                let Ok(v) = df.column(&column).and_then(|c| c.get(s.view.row_cursor)) else {
                    return;
                };
                let text = crate::sheet::any_text(&v);
                self.start_search(SearchState {
                    pattern: format!("^{}$", regex::escape(&text)),
                    all_columns: false,
                    backward,
                    expr: false,
                });
            }
            Action::Reselect => match self.last_visual {
                Some(v) if v.sheet == self.sheets.active().id => {
                    self.goto_row(v.end.0);
                    self.sheets.active_mut().view.col_cursor = v.end.1;
                    self.visual = Some(v);
                }
                _ => self.toast = Some("no visual range on this sheet".into()),
            },
            Action::JumpBack => {
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
            Action::JumpForward => match self.jump_at {
                Some(i) if i + 1 < self.jumps.len() => self.go_jump(i + 1),
                _ => self.toast = Some("at the newest jump".into()),
            },
            Action::Jumps => {
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
            Action::JumpPrev => match self.sheets.len() {
                0 | 1 => self.toast = Some("no previous sheet".into()),
                n => {
                    let id = self.sheets.iter().nth(n - 2).map(|s| s.id);
                    if let Some(id) = id {
                        self.sheets.switch_to(id);
                        self.spawn_materialize();
                    }
                }
            },
            Action::Suspend => self.suspend = true,
            Action::Plot { x, ys } => self.plot(x, ys),
            Action::PlotColumn => self.plot_keys(false),
            Action::PlotResize { .. } | Action::ZoomAll | Action::ZoomCursor => {
                self.toast = Some("not a plot (. plots)".into());
            }
            Action::Hist { bins } => {
                let Some(column) = self.current_column_name() else {
                    return;
                };
                let parent = self.sheets.active().clone();
                let id = self.sheets.alloc_id();
                self.spawn_job("hist", move || {
                    let s = Sheet::hist_from(&parent, id, &column, bins.unwrap_or(20))?;
                    Ok(JobDone::PushSheet(Box::new(s)))
                });
            }
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
            Action::Link { on } => {
                let s = self.sheets.active_mut();
                if on && s.state.lineage.is_none() {
                    self.toast = Some("link: not a derived sheet".into());
                    return;
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
            }
            Action::CopyStats => {
                let Some(stats) = self.current_sel_stats() else {
                    self.toast = Some("stats: select rows first (and wait for the stats)".into());
                    return;
                };
                let (h, v): (Vec<&str>, Vec<&str>) =
                    stats.iter().map(|(k, v)| (*k, v.as_str())).unzip();
                let text = format!("{}\n{}\n", h.join("\t"), v.join("\t"));
                let clip = self.clipboard.clone();
                self.spawn_job("copy", move || {
                    let osc52 = clip.send(&text).map_err(|e| format!("clipboard: {e}"))?;
                    Ok(JobDone::SysCopy {
                        note: "copied the stats to clipboard".into(),
                        osc52,
                    })
                });
            }
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
            Action::SetCursor { row, column } => {
                self.goto_row(row - 1);
                if let Some(c) = column {
                    if self.sheets.active().has_column(&c) {
                        self.goto_column(&c);
                    } else {
                        self.fail(format!("no column: {c}"));
                    }
                }
            }
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
                self.sheets.push(Sheet::open_new(id));
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
            Action::StartAddColInput | Action::StartCaptureInput => {
                match self.sheets.active().editable() {
                    Err(e) => self.toast = Some(e),
                    Ok(()) if matches!(action, Action::StartAddColInput) => {
                        self.start_input(Prompt::AddCol)
                    }
                    Ok(()) => self.start_input(Prompt::Capture),
                }
            }
            Action::StartSlideInput { left } => self.start_input(Prompt::Slide { left }),
            Action::SlideCol { by } => {
                let Some(column) = self.current_column_name() else {
                    return;
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
            }
            Action::Order { columns } => {
                let s = self.sheets.active_mut();
                if let Some(c) = columns.iter().find(|c| !s.has_column(c)) {
                    self.toast = Some(format!("no column: {c}"));
                    return;
                }
                let snap = s.snapshot();
                // bare `:order`: back to plan order
                let mut order = columns.clone();
                if !columns.is_empty() {
                    order.extend(
                        s.view
                            .order
                            .iter()
                            .filter(|c| !columns.contains(c))
                            .cloned(),
                    );
                }
                if order != s.view.order {
                    s.view.order = order;
                    s.state.widths_key = None;
                    s.push_undo(snap);
                    self.spawn_materialize();
                }
            }
            Action::AddCol { name } => {
                let at = self.current_column_name();
                match self.sheets.active_mut().add_empty_col(&name) {
                    Ok(()) => {
                        self.place_new_column(&name, at.as_deref());
                        self.spawn_materialize();
                    }
                    Err(e) => self.toast = Some(format!("addcol: {e}")),
                }
            }
            Action::AddColIncr => {
                let at = self.current_column_name();
                self.scan_edit(
                    "addcol-incr",
                    |s| s.view_order_rids(),
                    move |app, rids| match app.sheets.active_mut().add_incr_col_with(rids) {
                        Ok(name) => {
                            app.place_new_column(&name, at.as_deref());
                            app.spawn_materialize();
                        }
                        Err(e) => app.toast = Some(format!("addcol-incr: {e}")),
                    },
                );
            }
            Action::SetColIncr | Action::SetColFill | Action::Capture { .. } => {
                let Some(column) = self.current_column_name() else {
                    return;
                };
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
                                .set_incr_with(&column, rids)
                                .map(|n| format!("set {column} to 1…{n}"));
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
            }
            Action::PrecisionMore | Action::PrecisionLess | Action::SetPrecision { .. } => {
                let Some(column) = self.current_column_name() else {
                    return;
                };
                let (set, delta) = match action {
                    Action::PrecisionMore => (None, 1),
                    Action::PrecisionLess => (None, -1),
                    Action::SetPrecision { digits } => (digits, 0),
                    _ => return,
                };
                // VisiData starts from its default `{:.2f}`
                let start = self.theme.float_digits.unwrap_or(2);
                let s = self.sheets.active_mut();
                self.toast = Some(match s.set_precision(&column, set, delta, start) {
                    Ok(Some(1)) => format!("{column}: 1 decimal"),
                    Ok(Some(d)) => format!("{column}: {d} decimals"),
                    Ok(None) => format!("{column}: default precision"),
                    Err(e) => e,
                });
            }
            Action::SetFmtstr { fmt } => {
                let Some(column) = self.current_column_name() else {
                    return;
                };
                let s = self.sheets.active_mut();
                self.toast = Some(s.set_fmtstr(&column, &fmt).unwrap_or_else(|e| e));
            }
            Action::Derive { name, expr } => {
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
            Action::Color {
                column,
                pred,
                style,
            } => self
                .sheets
                .active_mut()
                .view
                .formats
                .push(crate::sheet::FormatRule {
                    column,
                    pred,
                    style,
                }),
            Action::Scale { column, colors } => {
                let colors = colors.unwrap_or_else(|| (SCALE_LOW.into(), SCALE_HIGH.into()));
                self.scale(column, Some(colors))
            }
            Action::DataBar { column } => self.scale(column, None),
            Action::NoColor { column } => {
                let s = self.sheets.active_mut();
                let v = &mut s.view;
                let before = v.formats.len() + v.scales.len();
                v.formats.retain(|r| match column.as_deref() {
                    None => false,
                    Some("*") => r.column.is_some(),
                    Some(c) => r.column.as_deref() != Some(c),
                });
                v.scales
                    .retain(|r| column.as_deref().is_some_and(|c| r.column != c));
                let n = before - v.formats.len() - v.scales.len();
                self.toast = Some(format!(
                    "{n} color rule{} cleared",
                    if n == 1 { "" } else { "s" }
                ));
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
                // undo step only once the names are checked
                let s = self.sheets.active_mut();
                let before = s.snapshot();
                match s.add_aggregators(&c, &names) {
                    Ok(()) => s.push_undo(before),
                    Err(e) => self.toast = Some(e),
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
                let aggs = crate::sheet::columns::freq_aggregators(&parent);
                self.spawn_job("pivot", move || {
                    let child = Sheet::pivot_current_from(&parent, id, &keys, &on, &aggs)?;
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
            Action::SubmitInput
            | Action::CancelInput
            | Action::InputBackspace
            | Action::InputNormal => {}
        }
        // `stale`: changed from a columns sheet while under it, now on top
        // again (popped, or raised from the sheet list / undo tree)
        let s = self.sheets.active_mut();
        let stale = std::mem::take(&mut s.state.stale);
        if stale || (affects && s.revision() == before_rev) {
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
            Origin::Hits { parent } => {
                let parent = *parent;
                let s = self.sheets.active();
                let cell = s.window().and_then(|df| {
                    let r = df
                        .column("row")
                        .ok()?
                        .get(s.view.row_cursor)
                        .ok()?
                        .try_extract::<u64>()
                        .ok()?;
                    let c = df.column("column").ok()?.get(s.view.row_cursor).ok()?;
                    Some((r, c.get_str()?.to_string()))
                });
                let Some((row, column)) = cell else { return };
                if !self.sheets.switch_to(parent) {
                    self.toast = Some("hits: that sheet was closed".into());
                    return;
                }
                self.goto_column(&column);
                self.goto_row(row - 1);
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
        let ranges = match s.state.lineage.as_deref().map(|l| &l.how) {
            Some(crate::sheet::Derivation::Freq {
                bins: Some(crate::sheet::Binning::Ranges { min, width, n }),
                ..
            }) => Some((*min, *width, *n)),
            _ => None,
        };
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
            let texts = || -> Vec<Option<String>> {
                picked
                    .iter()
                    .map(|v| (!v.is_null()).then(|| crate::sheet::any_text(v)))
                    .collect()
            };
            let child = match (ranges, parent.date_fmt(&column)) {
                (Some(r), _) => Sheet::rows_in_bins(&parent, id, &column, r, &texts())?,
                (None, Some(f)) => Sheet::rows_formatted(&parent, id, &column, f, &texts())?,
                (None, None) => Sheet::rows_with_values(&parent, id, &column, &picked),
            };
            Ok(JobDone::PushSheet(Box::new(child)))
        });
    }

    pub(super) fn pop_sheet(&mut self) {
        let popped_id = self.sheets.active().id;
        if self.sheets.pop() {
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
    fn ask(&mut self, c: Confirm, question: String) {
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

    /// The active sheet's visible columns, in screen order.
    fn visible_cols(&self) -> Vec<String> {
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
        if let Some(hist_bins) = self.theme.numeric_binning {
            // the bins need the column's min and max
            let parent = self.sheets.active().clone();
            self.spawn_job("freq", move || {
                let bins = Sheet::numeric_bins(&parent, &col_name, hist_bins)?;
                let child = Sheet::freq_binned(&parent, id, &col_name, bins);
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

/// `3 edits`.
fn edits(n: usize) -> String {
    if n == 1 {
        "1 edit".into()
    } else {
        format!("{n} edits")
    }
}
