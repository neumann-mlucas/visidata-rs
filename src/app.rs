//! App loop. tokio::select! over crossterm events + materialize task results. See S-02.

use std::collections::HashMap;
use std::io;
use std::time::Duration;

use anyhow::{Context, Result};
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use polars::prelude::*;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_stream::StreamExt;

use crate::action::Action;
use crate::key::Key;
use crate::keymap::{Chord, Dispatch, Keymap, Layer};
use crate::sheet::{
    DEFAULT_WINDOW_ROWS, FilterState, Origin, SearchState, Sheet, SheetId, Sheets, SortKey,
    SortOrder, View,
};
use crate::widget::{SheetWidget, clamp_view};

/// Entry point from main.
pub async fn run(initial: Sheet) -> Result<()> {
    install_panic_hook();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = App::new(initial).run(&mut terminal).await;

    disable_raw_mode().ok();
    crossterm::execute!(io::stdout(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();
    result
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(io::stdout(), LeaveAlternateScreen);
        default(info);
    }));
}

enum MaterializeResult {
    Ok {
        sheet_id: SheetId,
        revision: u64,
        df: DataFrame,
    },
    Err {
        sheet_id: SheetId,
        revision: u64,
        err: String,
    },
}

enum InputBuf {
    None,
    Search(String),
    Filter(String),
    Ex(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PlanMode {
    Off,
    Normal,
    Optimized,
}

pub struct App {
    sheets: Sheets,
    keymap: Keymap,
    pending: Vec<Key>,
    pending_candidates: Vec<Chord>,
    pending_count: Option<usize>,
    last_action: Option<Vec<Action>>,
    toast: Option<String>,
    input: InputBuf,
    ex_history: crate::exline::History,
    plan_mode: PlanMode,
    quit: bool,
    materialize_tx: mpsc::Sender<MaterializeResult>,
    materialize_rx: mpsc::Receiver<MaterializeResult>,
    /// Per-sheet handle so pushing a new sheet doesn't cancel the previous one's work.
    materialize_handles: HashMap<SheetId, JoinHandle<()>>,
}

impl App {
    pub fn new(sheet: Sheet) -> Self {
        let (tx, rx) = mpsc::channel(8);
        Self {
            sheets: Sheets::new(sheet),
            keymap: Keymap::defaults(),
            pending: Vec::new(),
            pending_candidates: Vec::new(),
            pending_count: None,
            last_action: None,
            toast: None,
            input: InputBuf::None,
            ex_history: crate::exline::History::load(),
            plan_mode: PlanMode::Off,
            quit: false,
            materialize_tx: tx,
            materialize_rx: rx,
            materialize_handles: HashMap::new(),
        }
    }

    async fn run<B: ratatui::backend::Backend>(mut self, term: &mut Terminal<B>) -> Result<()> {
        self.spawn_materialize();
        let mut events = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(16));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.draw(term)?;

        loop {
            tokio::select! {
                biased;
                maybe_ev = events.next() => match maybe_ev {
                    Some(Ok(CtEvent::Key(ke))) if ke.kind != KeyEventKind::Release => {
                        let key: Key = ke.into();
                        self.on_key(key);
                        self.draw(term)?;
                    }
                    Some(Ok(CtEvent::Resize(_, _))) => { self.draw(term)?; }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e).context("event stream"),
                    None => break,
                },
                Some(res) = self.materialize_rx.recv() => {
                    self.on_materialize(res);
                    self.draw(term)?;
                }
                _ = tick.tick() => {}
            }
            if self.quit {
                break;
            }
        }
        Ok(())
    }

    fn spawn_materialize(&mut self) {
        let s = self.sheets.active_mut();
        s.revision = s.revision.wrapping_add(1);
        let sheet_id = s.id;
        let rev = s.revision;
        let plan = s.window_plan();
        if let Some(h) = self.materialize_handles.remove(&sheet_id) {
            h.abort();
        }
        let tx = self.materialize_tx.clone();
        self.materialize_handles.insert(
            sheet_id,
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || plan.collect()).await;
                let msg = match result {
                    Ok(Ok(df)) => MaterializeResult::Ok {
                        sheet_id,
                        revision: rev,
                        df,
                    },
                    Ok(Err(e)) => MaterializeResult::Err {
                        sheet_id,
                        revision: rev,
                        err: e.to_string(),
                    },
                    Err(join_err) => MaterializeResult::Err {
                        sheet_id,
                        revision: rev,
                        err: format!("materialize task crashed: {join_err}"),
                    },
                };
                let _ = tx.send(msg).await;
            }),
        );
    }

    fn on_materialize(&mut self, res: MaterializeResult) {
        match res {
            MaterializeResult::Ok {
                sheet_id,
                revision,
                df,
            } => {
                let Some(s) = self.sheets.find_by_id_mut(sheet_id) else {
                    return;
                };
                if revision != s.revision {
                    return;
                }
                s.window = Some(df);
                self.toast = None;
            }
            MaterializeResult::Err {
                sheet_id,
                revision,
                err,
            } => {
                let Some(s) = self.sheets.find_by_id_mut(sheet_id) else {
                    return;
                };
                if revision != s.revision {
                    return;
                }
                self.toast = Some(format!("materialize: {err}"));
            }
        }
    }

    fn on_key(&mut self, key: Key) {
        // Toast lives until the next key — "clear on next action" without a timer.
        self.toast = None;
        if !matches!(self.input, InputBuf::None) {
            self.on_input_key(key);
            return;
        }
        // Count prefix: digits accumulate when no pending chord AND the digit
        // isn't the start of a bound chord. "0" is bound to goto_leftmost so
        // only accumulates when a non-zero count is already building.
        if self.pending.is_empty() {
            if let KeyCode::Char(c) = key.code {
                if c.is_ascii_digit() {
                    let d = c.to_digit(10).unwrap() as usize;
                    let is_zero = c == '0';
                    let already = self.pending_count.is_some();
                    let digit_is_bound_leaf = matches!(
                        self.keymap.resolve(Layer::Sheet, &[], key),
                        Dispatch::Fire(_)
                    );
                    let accumulate = (!digit_is_bound_leaf) || (is_zero && already) || (!is_zero);
                    if accumulate {
                        let cur = self.pending_count.unwrap_or(0);
                        self.pending_count = Some(cur.saturating_mul(10).saturating_add(d));
                        return;
                    }
                }
            }
        }
        match self.keymap.resolve(Layer::Sheet, &self.pending, key) {
            Dispatch::Fire(actions) => {
                self.pending.clear();
                self.pending_candidates.clear();
                let count = self.pending_count.take();
                let actions = inject_count(actions, count);
                self.dispatch_sequence(actions);
            }
            Dispatch::Pending(cands) => {
                self.pending.push(key);
                self.pending_candidates = cands;
            }
            Dispatch::Miss => {
                self.pending.clear();
                self.pending_candidates.clear();
                self.pending_count = None;
                self.toast = Some(format!("no binding: {key}"));
            }
        }
    }

    fn on_input_key(&mut self, key: Key) {
        // Ex-mode history navigation: Up/Down walk history, not bound in keymap to keep layer clean.
        if matches!(self.input, InputBuf::Ex(_)) {
            match key.code {
                KeyCode::Up => {
                    if let Some(line) = self.ex_history.prev() {
                        if let InputBuf::Ex(buf) = &mut self.input {
                            *buf = line.to_string();
                        }
                    }
                    return;
                }
                KeyCode::Down => {
                    if let Some(line) = self.ex_history.next() {
                        if let InputBuf::Ex(buf) = &mut self.input {
                            *buf = line.to_string();
                        }
                    }
                    return;
                }
                _ => {}
            }
        }
        match self.keymap.resolve(Layer::Input, &[], key) {
            Dispatch::Fire(actions) => {
                for a in actions {
                    match a {
                        Action::SubmitInput => self.submit_input(),
                        Action::CancelInput => self.cancel_input(),
                        Action::InputBackspace => self.input_push_backspace(),
                        _ => {}
                    }
                }
            }
            _ => {
                if let KeyCode::Char(c) = key.code {
                    self.input_push_char(c);
                }
            }
        }
    }

    fn input_push_char(&mut self, c: char) {
        match &mut self.input {
            InputBuf::Search(s) | InputBuf::Filter(s) | InputBuf::Ex(s) => s.push(c),
            InputBuf::None => {}
        }
    }

    fn input_push_backspace(&mut self) {
        match &mut self.input {
            InputBuf::Search(s) | InputBuf::Filter(s) | InputBuf::Ex(s) => {
                s.pop();
            }
            InputBuf::None => {}
        }
    }

    fn submit_input(&mut self) {
        match std::mem::replace(&mut self.input, InputBuf::None) {
            InputBuf::Search(needle) if !needle.is_empty() => {
                self.sheets.active_mut().view.search = Some(SearchState {
                    needle,
                    current: None,
                });
                self.dispatch(Action::SearchNext);
            }
            InputBuf::Filter(regex) if !regex.is_empty() => {
                if let Some(col) = self.current_column_name() {
                    let s = self.sheets.active_mut();
                    s.view.filter = Some(FilterState { column: col, regex });
                    s.view.window_offset = 0;
                    s.view.row_cursor = 0;
                    s.view.row_offset = 0;
                    self.spawn_materialize();
                } else {
                    self.toast = Some("no column to filter".into());
                }
            }
            InputBuf::Ex(text) => {
                self.ex_history.push(&text);
                match crate::exline::parse(&text) {
                    Ok(actions) => self.dispatch_sequence(actions),
                    Err(e) => self.toast = Some(format!("{e}")),
                }
            }
            _ => {}
        }
    }

    fn cancel_input(&mut self) {
        self.input = InputBuf::None;
        self.ex_history.reset_cursor();
    }

    fn current_column_name(&self) -> Option<String> {
        let s = self.sheets.active();
        s.window.as_ref().and_then(|df| {
            df.get_column_names()
                .get(s.view.col_cursor)
                .map(|n| n.to_string())
        })
    }

    fn dispatch_sequence(&mut self, actions: Vec<Action>) {
        if actions.iter().any(|a| a.repeatable()) {
            self.last_action = Some(actions.clone());
        }
        for a in actions {
            self.dispatch(a);
        }
    }

    fn dispatch(&mut self, action: Action) {
        let before_rev = self.sheets.active().revision;
        let affects = action.affects_data();
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
                let s = self.sheets.active_mut();
                s.view.row_cursor = 0;
                s.view.row_offset = 0;
            }
            Action::GotoBottom => self.goto_bottom(),
            Action::GotoLeftmost => {
                let s = self.sheets.active_mut();
                s.view.col_cursor = 0;
                s.view.col_offset = 0;
            }
            Action::GotoRightmost => {
                let s = self.sheets.active_mut();
                if let Some(df) = &s.window {
                    if df.width() > 0 {
                        s.view.col_cursor = df.width() - 1;
                    }
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
            Action::StartSearchInput => self.input = InputBuf::Search(String::new()),
            Action::StartFilterInput => self.input = InputBuf::Filter(String::new()),
            Action::StartExInput => self.input = InputBuf::Ex(String::new()),
            Action::ExCommand(text) => match crate::exline::parse(&text) {
                Ok(actions) => self.dispatch_sequence(actions),
                Err(e) => self.toast = Some(format!("{e}")),
            },
            Action::SearchNext => self.search_jump(true),
            Action::SearchPrev => self.search_jump(false),
            Action::FilterCurrentColumn { regex } => {
                if let Some(col) = self.current_column_name() {
                    self.sheets.active_mut().view.filter = Some(FilterState { column: col, regex });
                    self.spawn_materialize();
                }
            }
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
            Action::SwitchToSheet(id) => {
                if !self.sheets.switch_to(id) {
                    self.toast = Some(format!("no sheet with id {id}"));
                }
            }
            Action::SubmitRow => self.submit_row(),
            Action::TogglePlanPanel => {
                self.plan_mode = match self.plan_mode {
                    PlanMode::Off => PlanMode::Normal,
                    PlanMode::Normal => PlanMode::Optimized,
                    PlanMode::Optimized => PlanMode::Off,
                };
            }
            Action::SubmitInput
            | Action::CancelInput
            | Action::InputChar(_)
            | Action::InputBackspace => {}
        }
        if affects && self.sheets.active().revision == before_rev {
            self.spawn_materialize();
        }
    }

    fn move_cursor_rows(&mut self, delta: isize) {
        let s = self.sheets.active_mut();
        let Some(df) = &s.window else { return };
        let rows = df.height() as isize;
        if rows == 0 {
            return;
        }
        let new_cursor = s.view.row_cursor as isize + delta;
        let window_full = df.height() as u32 == s.view.window_rows;
        if new_cursor >= rows && window_full {
            let step = ((new_cursor - rows + 1) as u64)
                .max(1)
                .min(s.view.window_rows as u64 / 2);
            s.view.window_offset = s.view.window_offset.saturating_add(step);
            s.view.row_cursor = (rows as usize).saturating_sub(1);
            self.spawn_materialize();
        } else if new_cursor < 0 && s.view.window_offset > 0 {
            let overflow = (-new_cursor) as u64;
            let step = overflow.max(1).min(s.view.window_rows as u64 / 2);
            s.view.window_offset = s.view.window_offset.saturating_sub(step);
            s.view.row_cursor = 0;
            self.spawn_materialize();
        } else {
            s.view.row_cursor = new_cursor.clamp(0, rows - 1) as usize;
        }
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

    fn page(&mut self, dir: isize) {
        let (rows, step) = {
            let s = self.sheets.active();
            let Some(df) = &s.window else { return };
            (
                df.height() as isize,
                (s.view.window_rows as isize / 2).max(1),
            )
        };
        self.move_cursor_rows(dir * step);
        let s = self.sheets.active_mut();
        let near_top = s.view.row_cursor < 2;
        let near_bot = s.view.row_cursor + 2 >= rows as usize;
        let offset = s.view.window_offset as i64;
        if dir < 0 && near_top && offset > 0 {
            s.view.window_offset = offset.saturating_sub(step as i64) as u64;
            self.spawn_materialize();
        } else if dir > 0 && near_bot {
            s.view.window_offset = (offset + step as i64) as u64;
            self.spawn_materialize();
        }
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
        let s = self.sheets.active_mut();
        if let Some(df) = &s.window {
            if df.height() > 0 {
                s.view.row_cursor = df.height() - 1;
            }
        }
    }

    fn search_jump(&mut self, forward: bool) {
        let s = self.sheets.active_mut();
        let Some(df) = &s.window else { return };
        let Some(state) = s.view.search.as_mut() else {
            self.toast = Some("no search; press / first".into());
            return;
        };
        let needle = state.needle.to_lowercase();
        let start = state.current.map(|(r, _)| r).unwrap_or(s.view.row_cursor);
        let n = df.height();
        if n == 0 {
            return;
        }
        let indices: Box<dyn Iterator<Item = usize>> = if forward {
            Box::new(((start + 1)..n).chain(0..=start))
        } else {
            let before: Vec<usize> = (0..start).rev().collect();
            let after: Vec<usize> = ((start + 1)..n).rev().collect();
            Box::new(before.into_iter().chain(after))
        };
        let cols = df.get_columns();
        for ri in indices {
            for (ci, c) in cols.iter().enumerate() {
                if let Ok(av) = c.get(ri) {
                    let cell = match av {
                        AnyValue::String(s) => s.to_string(),
                        AnyValue::StringOwned(s) => s.as_str().to_string(),
                        other => other.to_string(),
                    };
                    if cell.to_lowercase().contains(&needle) {
                        s.view.row_cursor = ri;
                        s.view.col_cursor = ci;
                        state.current = Some((ri, ci));
                        return;
                    }
                }
            }
        }
        self.toast = Some(format!("no match for /{needle}"));
    }

    fn open_pivot(&mut self, index: &str, on: &str, values: &str, agg: &str) {
        let id = self.sheets.alloc_id();
        let child = {
            let parent = self.sheets.active();
            Sheet::pivot_from(parent, id, index, on, values, agg)
        };
        match child {
            Ok(c) => {
                self.sheets.push(c);
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("pivot: {e}")),
        }
    }

    fn open_diff(&mut self, path: &str, keys: &[String]) {
        use std::ffi::OsString;
        let loaded = match crate::reader::open(&OsString::from(path), None) {
            Ok(l) => l,
            Err(e) => {
                self.toast = Some(format!("diff load: {e}"));
                return;
            }
        };
        let id = self.sheets.alloc_id();
        let built = {
            let parent = self.sheets.active();
            if keys.is_empty() {
                Sheet::schema_diff_from(parent, id, &loaded)
            } else {
                Sheet::row_diff_from(parent, id, &loaded, keys)
            }
        };
        match built {
            Ok(s) => {
                self.sheets.push(s);
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("diff: {e}")),
        }
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
        // Only meaningful on the sheet-switcher: pull the row's `id` column and switch.
        let s = self.sheets.active();
        if !matches!(s.origin, Origin::SheetSwitcher) {
            self.toast = Some("<CR>: nothing to submit here".into());
            return;
        }
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

    fn draw<B: ratatui::backend::Backend>(&mut self, term: &mut Terminal<B>) -> Result<()> {
        let viewport_rows = term
            .size()
            .map(|s| s.height.saturating_sub(2) as usize)
            .unwrap_or(10);
        {
            let s = self.sheets.active_mut();
            if let Some(df) = &s.window {
                let (h, w) = (df.height(), df.width());
                clamp_view(&mut s.view, h, w, viewport_rows);
            }
        }
        term.draw(|f| self.render(f))?;
        Ok(())
    }

    fn render(&self, f: &mut ratatui::Frame<'_>) {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
        self.render_titlebar(f, chunks[0]);

        let main_area = chunks[1];
        if self.plan_mode == PlanMode::Off {
            f.render_widget(
                SheetWidget {
                    sheet: self.sheets.active(),
                },
                main_area,
            );
        } else {
            let halves = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
                .split(main_area);
            f.render_widget(
                SheetWidget {
                    sheet: self.sheets.active(),
                },
                halves[0],
            );
            self.render_plan_panel(f, halves[1]);
        }

        self.render_statusbar(f, chunks[2]);
        if !self.pending_candidates.is_empty() {
            self.render_which_key(f, main_area);
        }
    }

    fn render_plan_panel(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let s = self.sheets.active();
        let (title, body) = match self.plan_mode {
            PlanMode::Normal => ("plan", s.plan.describe_plan()),
            PlanMode::Optimized => ("plan (opt)", s.plan.describe_optimized_plan()),
            PlanMode::Off => return,
        };
        let text = body.unwrap_or_else(|e| format!("<error>: {e}"));
        let block = ratatui::widgets::Block::default()
            .title(format!(" {title} "))
            .borders(ratatui::widgets::Borders::ALL)
            .style(Style::default().fg(Color::Green));
        let p = Paragraph::new(text)
            .block(block)
            .wrap(ratatui::widgets::Wrap { trim: false });
        f.render_widget(p, area);
    }

    fn render_which_key(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let pending_len = self.pending.len();
        let lines: Vec<Line> = self
            .pending_candidates
            .iter()
            .filter_map(|c| c.on.get(pending_len).map(|k| (k, c.desc)))
            .map(|(k, desc)| {
                Line::from(vec![
                    Span::styled(
                        format!(" {k:>6} "),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("→ "),
                    Span::raw(desc.to_string()),
                ])
            })
            .collect();
        let h = (lines.len() as u16 + 2).min(area.height);
        let w = 44.min(area.width);
        let x = area.x + area.width.saturating_sub(w);
        let y = area.y + area.height.saturating_sub(h);
        let rect = Rect {
            x,
            y,
            width: w,
            height: h,
        };
        let block = ratatui::widgets::Block::default()
            .title(format!(
                " {} ",
                self.pending
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join("")
            ))
            .borders(ratatui::widgets::Borders::ALL)
            .style(Style::default().bg(Color::Black).fg(Color::White));
        let p = Paragraph::new(lines).block(block);
        f.render_widget(ratatui::widgets::Clear, rect);
        f.render_widget(p, rect);
    }

    fn render_titlebar(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let s = self.sheets.active();
        let View {
            row_cursor,
            col_cursor,
            window_offset,
            ..
        } = s.view;
        let (rows, cols) = s
            .window
            .as_ref()
            .map(|df| (df.height(), df.width()))
            .unwrap_or((0, 0));
        let abs_row = window_offset as usize + row_cursor;
        let stack_pos = self.sheets.len();
        let title = format!(
            " {name} [{kind:?}] | {abs_row}/{rows}·{col_cursor}/{cols} | win+{window_offset} | sheets:{stack_pos} ",
            name = s.name,
            kind = s.kind
        );
        f.render_widget(
            Paragraph::new(title).style(
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            area,
        );
    }

    fn render_statusbar(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let text = match (
            &self.input,
            &self.toast,
            self.pending_count,
            !self.pending.is_empty(),
        ) {
            (InputBuf::Search(s), _, _, _) => Line::from(vec![
                Span::styled("/", Style::default().fg(Color::Yellow)),
                Span::raw(s.clone()),
            ]),
            (InputBuf::Filter(s), _, _, _) => Line::from(vec![
                Span::styled("|", Style::default().fg(Color::Yellow)),
                Span::raw(s.clone()),
            ]),
            (InputBuf::Ex(s), _, _, _) => Line::from(vec![
                Span::styled(":", Style::default().fg(Color::Yellow)),
                Span::raw(s.clone()),
            ]),
            (_, Some(t), _, _) => {
                Line::from(Span::styled(t.clone(), Style::default().fg(Color::Red)))
            }
            (_, _, Some(c), _) => Line::from(format!("count: {c}")),
            (_, _, _, true) => Line::from(format!(
                "pending: {}",
                self.pending
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )),
            _ => Line::from(
                " j/k/h/l · gg/G · 0/$ · /n|zf · [/]zo sort · zs/zc freeze · F freq · S sheets · : ex · . repeat · q pop/quit ",
            ),
        };
        f.render_widget(
            Paragraph::new(text).style(Style::default().bg(Color::DarkGray).fg(Color::White)),
            area,
        );
    }
}

fn inject_count(actions: Vec<Action>, count: Option<usize>) -> Vec<Action> {
    if count.is_none() {
        return actions;
    }
    actions
        .into_iter()
        .enumerate()
        .map(|(i, a)| if i == 0 { with_count(a, count) } else { a })
        .collect()
}

fn with_count(a: Action, count: Option<usize>) -> Action {
    match a {
        Action::CursorDown { .. } => Action::CursorDown { count },
        Action::CursorUp { .. } => Action::CursorUp { count },
        Action::CursorLeft { .. } => Action::CursorLeft { count },
        Action::CursorRight { .. } => Action::CursorRight { count },
        other => other,
    }
}

#[allow(dead_code)]
const _: u32 = DEFAULT_WINDOW_ROWS;
