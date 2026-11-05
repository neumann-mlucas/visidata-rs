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
    FilterState, Origin, SearchState, Sheet, SheetId, Sheets, SortKey, SortOrder, View,
};
use crate::theme::Theme;
use crate::widget::{SheetWidget, clamp_view};

/// Data rows assumed visible before the first draw measures the real area.
const DEFAULT_VIEWPORT_ROWS: usize = 20;

/// Entry point from main.
pub async fn run(
    initial: Sheet,
    plugins: Option<crate::plugins::Plugins>,
    plugin_warning: Option<String>,
) -> Result<()> {
    install_panic_hook();

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = App::new(initial, plugins, plugin_warning)
        .run(&mut terminal)
        .await;

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
    /// Row count of the filtered view, tagged with the `count_key` it was computed for.
    Count {
        sheet_id: SheetId,
        key: String,
        total: Result<u64, String>,
    },
}

enum InputBuf {
    None,
    Search(String),
    Filter(String),
    Ex(String),
    Select(String),
    Rename(String),
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PlanMode {
    Off,
    Normal,
    Optimized,
}

pub struct App {
    /// Held so the Lua VM lives for the session (even without an active caller).
    /// Loader rules are consulted from main.rs; keymap patches applied in `new`.
    /// Not yet accessed at runtime for aggregator/renderer shapes.
    _plugins: Option<crate::plugins::Plugins>,
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
    theme: Theme,
    /// Data rows visible in the sheet area at the last draw; one PageDown step.
    viewport_rows: usize,
    quit: bool,
    materialize_tx: mpsc::Sender<MaterializeResult>,
    materialize_rx: mpsc::Receiver<MaterializeResult>,
    /// Per-sheet handle so pushing a new sheet doesn't cancel the previous one's work.
    materialize_handles: HashMap<SheetId, JoinHandle<()>>,
}

impl App {
    pub fn new(
        sheet: Sheet,
        plugins: Option<crate::plugins::Plugins>,
        plugin_warning: Option<String>,
    ) -> Self {
        let (tx, rx) = mpsc::channel(8);
        let mut keymap = Keymap::defaults();
        let toast = match (plugins.as_ref(), plugin_warning) {
            (Some(p), _) => {
                p.apply_keymap(&mut keymap);
                None
            }
            (None, Some(w)) => Some(w),
            (None, None) => None,
        };
        Self {
            _plugins: plugins,
            sheets: Sheets::new(sheet),
            keymap,
            pending: Vec::new(),
            pending_candidates: Vec::new(),
            pending_count: None,
            last_action: None,
            toast,
            input: InputBuf::None,
            ex_history: crate::exline::History::load(),
            plan_mode: PlanMode::Off,
            theme: Theme::dark(),
            viewport_rows: DEFAULT_VIEWPORT_ROWS,
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
        let mut tail_tick = tokio::time::interval(Duration::from_secs(2));
        tail_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // first tick fires immediately — burn it so refresh doesn't race the initial materialize
        tail_tick.tick().await;
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
                _ = tail_tick.tick() => {
                    self.on_tail_tick();
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
        self.spawn_count_if_stale();
    }

    /// Recount the active sheet's rows when its `count_key` changed. Not aborted on
    /// supersede (blocking work can't be); stale results are dropped by key.
    fn spawn_count_if_stale(&mut self) {
        let s = self.sheets.active_mut();
        let key = s.count_key();
        if s.view.count_key.as_ref() == Some(&key) {
            return;
        }
        s.view.count_key = Some(key.clone());
        s.view.total_rows = None;
        let (sheet_id, plan) = (s.id, s.count_plan());
        let tx = self.materialize_tx.clone();
        tokio::spawn(async move {
            let total = match tokio::task::spawn_blocking(move || plan.collect()).await {
                Ok(Ok(df)) => df.get_columns()[0]
                    .get(0)
                    .and_then(|v| v.try_extract::<u64>())
                    .map_err(|e| e.to_string()),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("count task crashed: {e}")),
            };
            let _ = tx
                .send(MaterializeResult::Count {
                    sheet_id,
                    key,
                    total,
                })
                .await;
        });
    }

    /// Drive a pending `G` as far as the current window/count allow: slide the
    /// window to the last page once the total is known, then park the cursor on
    /// the last row. Without a total the cursor parks provisionally on the
    /// window's last row and moves again when the count lands.
    fn settle_jump_bottom(&mut self) {
        let s = self.sheets.active_mut();
        if !s.view.jump_bottom {
            return;
        }
        match s.view.total_rows {
            // In-window → done; otherwise the slide's window lands and settles again.
            Some(total) => {
                if total == 0 || self.goto_row(total - 1) {
                    self.sheets.active_mut().view.jump_bottom = false;
                }
            }
            None => {
                if let Some(h) = s.window.as_ref().map(|df| df.height()).filter(|h| *h > 0) {
                    s.view.row_cursor = h - 1;
                }
            }
        }
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
                if sheet_id == self.sheets.active().id {
                    self.settle_jump_bottom();
                }
            }
            MaterializeResult::Count {
                sheet_id,
                key,
                total,
            } => {
                let Some(s) = self.sheets.find_by_id_mut(sheet_id) else {
                    return;
                };
                if s.view.count_key.as_ref() != Some(&key) {
                    return;
                }
                match total {
                    Ok(n) => s.view.total_rows = Some(n),
                    Err(e) => {
                        s.view.jump_bottom = false; // nothing to settle on
                        self.toast = Some(format!("row count: {e}"));
                        return;
                    }
                }
                if sheet_id == self.sheets.active().id {
                    self.settle_jump_bottom();
                }
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
            InputBuf::Search(s)
            | InputBuf::Filter(s)
            | InputBuf::Ex(s)
            | InputBuf::Select(s)
            | InputBuf::Rename(s) => s.push(c),
            InputBuf::None => {}
        }
    }

    fn input_push_backspace(&mut self) {
        match &mut self.input {
            InputBuf::Search(s)
            | InputBuf::Filter(s)
            | InputBuf::Ex(s)
            | InputBuf::Select(s)
            | InputBuf::Rename(s) => {
                s.pop();
            }
            InputBuf::None => {}
        }
    }

    fn submit_input(&mut self) {
        match std::mem::replace(&mut self.input, InputBuf::None) {
            InputBuf::Search(needle) if !needle.is_empty() => {
                self.sheets.active_mut().view.search = Some(SearchState { needle });
                self.dispatch(Action::SearchNext);
            }
            InputBuf::Filter(regex) if !regex.is_empty() => {
                if let Some(column) = self.current_column_name() {
                    self.dispatch(Action::SetFilter { column, regex });
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
            InputBuf::Select(regex) if !regex.is_empty() => {
                self.dispatch(Action::SelectByRegex { regex });
            }
            InputBuf::Rename(name) if !name.is_empty() => {
                self.dispatch(Action::RenameCol { new_name: name });
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
        if !matches!(action, Action::GotoBottom) {
            self.sheets.active_mut().view.jump_bottom = false;
        }
        if let Action::SetFilter { regex, .. } | Action::FilterCurrentColumn { regex } = &action {
            if let Err(e) = regex::Regex::new(regex) {
                self.toast = Some(format!("bad regex: {e}"));
                return;
            }
        }
        if let Action::SetFilter { column, .. } | Action::SetSort { column, .. } = &action {
            if !self.sheets.active().has_column(column) {
                self.toast = Some(format!("no column: {column}"));
                return;
            }
        }
        // Selection is positional (row offsets into the view), so any change to
        // row order/membership would silently re-point it at other rows.
        // ponytail: clear instead of tracking row identity; add a row-index
        // column to the base plan if users want selections to survive re-sorts.
        if matches!(
            action,
            Action::FilterCurrentColumn { .. }
                | Action::SetFilter { .. }
                | Action::ClearFilter
                | Action::SortAsc
                | Action::SortDesc
                | Action::SetSort { .. }
                | Action::ClearSort
        ) {
            self.sheets.active_mut().view.selected.clear();
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
                self.input = InputBuf::Rename(String::new());
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
                s.plan = s
                    .plan
                    .clone()
                    .rename([old.as_str()], [new_name.as_str()], true);
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
                if let Some(column) = self.current_column_name() {
                    self.dispatch(Action::SetFilter { column, regex });
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
            Action::Tail { path, follow } => self.open_tail(&path, follow),
            Action::Save { path } => self.save_active(&path),
            Action::SwitchToSheet(id) => {
                if !self.sheets.switch_to(id) {
                    self.toast = Some(format!("no sheet with id {id}"));
                }
            }
            Action::SubmitRow => self.submit_row(),
            Action::ToggleSelectRow => self.toggle_select_current(),
            Action::SelectRow => self.set_select_current(true),
            Action::UnselectRow => self.set_select_current(false),
            Action::SelectAll => self.select_all(),
            Action::UnselectAll => {
                self.sheets.active_mut().view.selected.clear();
            }
            Action::StartSelectInput => self.input = InputBuf::Select(String::new()),
            Action::SelectByRegex { regex } => self.select_by_regex(&regex),
            Action::DupSelected => self.open_dup_selected(),
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
    fn goto_row(&mut self, target: u64) -> bool {
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

    /// Half-window jump. `move_cursor_rows` slides the window when needed.
    /// One screen of data rows.
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
        s.plan = s.plan.clone().with_columns([col(name.as_str())
            .cast(DataType::String)
            .str()
            .replace_all(lit(r"[^0-9.\-]"), lit(""), false)
            .cast(DataType::Float64)
            .alias(name.as_str())]);
        s.view.count_key = None; // a filter on this column may now match differently
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
        // ponytail: synchronous full scan per n/N; move to a background task with
        // cached hits if big files make it lag.
        let hits = match s.search_hits(&needle) {
            Ok(h) => h,
            Err(e) => {
                self.toast = Some(format!("search: {e}"));
                return;
            }
        };
        let next = if forward {
            hits.iter().find(|(r, _)| *r > cur).or(hits.first())
        } else {
            hits.iter().rev().find(|(r, _)| *r < cur).or(hits.last())
        };
        match next.copied() {
            Some((row, c)) => {
                self.goto_row(row);
                self.sheets.active_mut().view.col_cursor = c;
            }
            None => self.toast = Some(format!("no match for /{needle}")),
        }
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

    /// Persist the active sheet to disk. Writer picked by extension; applies
    /// the sheet's `view_plan` (type-casts + filter + sort + visible projection,
    /// all rows — not just the materialized window) so the saved file matches
    /// what the user sees, not raw source data. Overwrites existing files.
    fn save_active(&mut self, path: &str) {
        let plan = self.sheets.active().view_plan();
        let mut df = match plan.collect() {
            Ok(d) => d,
            Err(e) => {
                self.toast = Some(format!("save: collect: {e}"));
                return;
            }
        };
        match crate::sheet::write_df_by_extension(&mut df, std::path::Path::new(path)) {
            Ok(()) => self.toast = Some(format!("saved {} rows → {path}", df.height())),
            Err(e) => self.toast = Some(format!("save: {e}")),
        }
    }

    fn open_tail(&mut self, path: &str, follow: bool) {
        use std::ffi::OsString;
        let loaded = match crate::reader::open(&OsString::from(path), None) {
            Ok(l) => l,
            Err(e) => {
                self.toast = Some(format!("tail load: {e}"));
                return;
            }
        };
        let id = self.sheets.alloc_id();
        match Sheet::tail_from(id, loaded, follow) {
            Ok(s) => {
                self.sheets.push(s);
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("tail: {e}")),
        }
    }

    fn on_tail_tick(&mut self) {
        // Only refresh the active sheet's tail if any. Dormant tail sheets are reactivated
        // via sheet-switcher; refreshing all of them per tick would be wasteful.
        let active = self.sheets.active_mut();
        let is_tail = matches!(active.origin, Origin::Tail { .. });
        if !is_tail {
            return;
        }
        let follow = matches!(active.origin, Origin::Tail { follow: true, .. });
        let v = &active.view;
        let abs = v.window_offset + v.row_cursor as u64;
        let was_at_bottom = match (v.total_rows, &active.window) {
            (Some(total), _) => abs + 1 >= total,
            (None, Some(df)) => v.row_cursor + 1 >= df.height(),
            (None, None) => true,
        };
        if let Err(e) = active.refresh_tail() {
            self.toast = Some(format!("tail refresh: {e}"));
            return;
        }
        // Settles across the window + count results the refresh triggers.
        active.view.jump_bottom |= follow && was_at_bottom;
        self.spawn_materialize();
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

    fn abs_row_at_cursor(&self) -> Option<u64> {
        let s = self.sheets.active();
        s.window.as_ref()?;
        Some(s.view.window_offset + s.view.row_cursor as u64)
    }

    fn toggle_select_current(&mut self) {
        let Some(abs) = self.abs_row_at_cursor() else {
            return;
        };
        let v = &mut self.sheets.active_mut().view;
        if !v.selected.remove(&abs) {
            v.selected.insert(abs);
        }
    }

    fn set_select_current(&mut self, select: bool) {
        let Some(abs) = self.abs_row_at_cursor() else {
            return;
        };
        let v = &mut self.sheets.active_mut().view;
        if select {
            v.selected.insert(abs);
        } else {
            v.selected.remove(&abs);
        }
    }

    /// Select every row of the view (not just the loaded window).
    fn select_all(&mut self) {
        let v = &mut self.sheets.active_mut().view;
        match v.total_rows {
            // ponytail: one set entry per row; swap BTreeSet for a bitmap if
            // multi-million-row selections eat too much memory.
            Some(total) => v.selected = (0..total).collect(),
            None => self.toast = Some("still counting rows; try again".into()),
        }
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
        let s = self.sheets.active_mut();
        match s.rows_matching(&column, pat) {
            Ok(rows) => {
                let matched = rows.len();
                s.view.selected.extend(rows);
                self.toast = Some(format!("selected {matched} rows matching /{pat}"));
            }
            Err(e) => self.toast = Some(format!("select: {e}")),
        }
    }

    fn open_dup_selected(&mut self) {
        let id = self.sheets.alloc_id();
        let built = {
            let parent = self.sheets.active();
            Sheet::dup_selected_from(parent, id)
        };
        match built {
            Ok(s) => {
                self.sheets.push(s);
                self.spawn_materialize();
            }
            Err(e) => self.toast = Some(format!("dup: {e}")),
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
        term.draw(|f| self.render(f))?;
        Ok(())
    }

    /// `&mut` only to clamp the view to the sheet area measured here.
    fn render(&mut self, f: &mut ratatui::Frame<'_>) {
        let area = f.area();
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Min(0),
                Constraint::Length(1),
            ])
            .split(area);
        let main_area = chunks[1];
        let (sheet_area, plan_area) = if self.plan_mode == PlanMode::Off {
            (main_area, None)
        } else {
            let halves = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
                .split(main_area);
            (halves[0], Some(halves[1]))
        };
        self.viewport_rows = (sheet_area.height as usize).saturating_sub(1).max(1);
        let s = self.sheets.active_mut();
        if let Some(df) = &s.window {
            clamp_view(&mut s.view, df, sheet_area);
        }

        self.render_titlebar(f, chunks[0]);
        f.render_widget(
            SheetWidget {
                sheet: self.sheets.active(),
                theme: &self.theme,
            },
            sheet_area,
        );
        if let Some(area) = plan_area {
            self.render_plan_panel(f, area);
        }

        self.render_statusbar(f, chunks[2]);
        if !self.pending_candidates.is_empty() {
            self.render_which_key(f, main_area);
        }
    }

    fn render_plan_panel(&self, f: &mut ratatui::Frame<'_>, area: Rect) {
        let s = self.sheets.active();
        let (title, body) = match self.plan_mode {
            PlanMode::Normal => ("plan", s.window_plan().describe_plan()),
            PlanMode::Optimized => ("plan (opt)", s.window_plan().describe_optimized_plan()),
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
        let abs_row = window_offset as usize + row_cursor + usize::from(rows > 0); // 1-based
        let col = col_cursor + usize::from(cols > 0); // 1-based
        let total = s
            .view
            .total_rows
            .map_or_else(|| "?".to_string(), |n| n.to_string());
        let stack_pos = self.sheets.len();
        let sel = s.view.selected.len();
        let col_name = s
            .window
            .as_ref()
            .and_then(|df| df.get_column_names().get(col_cursor).map(|n| n.to_string()))
            .unwrap_or_default();
        let left = format!(" {name} ", name = s.name);
        let center =
            format!(" row {abs_row}/{total} · col {col}/{cols} ({col_name}) · sel {sel} ",);
        let right = format!(" {kind:?} · sheets {stack_pos} ", kind = s.kind);
        let th = &self.theme;
        let line = Line::from(vec![
            Span::styled(left, th.titlebar_left()),
            Span::styled(center, th.titlebar_mid()),
            Span::styled(right, th.titlebar_right()),
        ]);
        f.render_widget(Paragraph::new(line), area);
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
            (InputBuf::Select(s), _, _, _) => Line::from(vec![
                Span::styled("| ", Style::default().fg(Color::Magenta)),
                Span::raw(s.clone()),
            ]),
            (InputBuf::Rename(s), _, _, _) => Line::from(vec![
                Span::styled("rename: ", Style::default().fg(Color::Yellow)),
                Span::raw(s.clone()),
            ]),
            // Polars errors span lines (plan dumps); the bar shows the first.
            (_, Some(t), _, _) => Line::from(Span::styled(
                t.lines().next().unwrap_or_default().to_string(),
                Style::default().fg(Color::Red),
            )),
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
                " hjkl · gg/G · s/u sel · gs/gu · | sel-re · \" dup · F freq · [/] sort · zs freeze · P plan · : ex · . · q ",
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// App over `name,n / alice,3 / bob,1 / carol,2` with the first window materialized.
    fn test_app() -> (App, tempfile::NamedTempFile) {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "name,n\nalice,3\nbob,1\ncarol,2").unwrap();
        tmp.flush().unwrap();
        let csv = Some(crate::reader::SourceKind::Csv);
        let loaded = crate::reader::open(tmp.path().as_os_str(), csv).unwrap();
        let mut app = App::new(Sheet::from_source(0, loaded), None, None);
        let s = app.sheets.active_mut();
        s.window = Some(s.window_plan().collect().unwrap());
        (app, tmp)
    }

    /// `test_app` with a 2-row window over the 3-row data, count already known.
    fn paged_app() -> (App, tempfile::NamedTempFile) {
        let (mut app, tmp) = test_app();
        let s = app.sheets.active_mut();
        s.view.window_rows = 2;
        s.window = Some(s.window_plan().collect().unwrap());
        s.view.total_rows = Some(3);
        s.view.count_key = Some(s.count_key());
        (app, tmp)
    }

    /// Feed the result the materialize task would send for the active sheet.
    fn land_window(app: &mut App) {
        let s = app.sheets.active();
        let msg = MaterializeResult::Ok {
            sheet_id: s.id,
            revision: s.revision,
            df: s.window_plan().collect().unwrap(),
        };
        app.on_materialize(msg);
    }

    fn titlebar(app: &mut App) -> String {
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(80, 6)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let buf = term.backend().buffer();
        (0..80).map(|x| buf[(x, 0)].symbol()).collect()
    }

    #[tokio::test]
    async fn goto_bottom_slides_to_last_data_row() {
        let (mut app, _tmp) = paged_app();
        app.dispatch(Action::GotoBottom);
        assert_eq!(app.sheets.active().view.window_offset, 1);
        land_window(&mut app);
        let v = &app.sheets.active().view;
        assert_eq!((v.row_cursor, v.jump_bottom), (1, false));
        assert!(
            titlebar(&mut app).contains("row 3/3"),
            "{}",
            titlebar(&mut app)
        );
    }

    #[tokio::test]
    async fn goto_bottom_before_count_settles_when_count_lands() {
        let (mut app, _tmp) = paged_app();
        let s = app.sheets.active_mut();
        s.view.total_rows = None;
        let key = s.count_key();
        app.dispatch(Action::GotoBottom);
        let v = &app.sheets.active().view;
        assert_eq!((v.window_offset, v.row_cursor, v.jump_bottom), (0, 1, true));
        assert!(
            titlebar(&mut app).contains("row 2/?"),
            "{}",
            titlebar(&mut app)
        );
        // stale count is ignored
        app.on_materialize(MaterializeResult::Count {
            sheet_id: 0,
            key: "stale".into(),
            total: Ok(99),
        });
        assert_eq!(app.sheets.active().view.total_rows, None);
        app.on_materialize(MaterializeResult::Count {
            sheet_id: 0,
            key,
            total: Ok(3),
        });
        assert_eq!(app.sheets.active().view.window_offset, 1);
        land_window(&mut app);
        let v = &app.sheets.active().view;
        assert_eq!((v.row_cursor, v.jump_bottom), (1, false));
    }

    #[tokio::test]
    async fn cursor_move_cancels_pending_goto_bottom() {
        let (mut app, _tmp) = paged_app();
        app.sheets.active_mut().view.total_rows = None;
        app.dispatch(Action::GotoBottom);
        app.dispatch(Action::CursorUp { count: None });
        assert!(!app.sheets.active().view.jump_bottom);
    }

    #[tokio::test]
    async fn materialize_keeps_toast() {
        let (mut app, _tmp) = test_app();
        app.toast = Some("saved 3 rows".into());
        land_window(&mut app);
        assert_eq!(app.toast.as_deref(), Some("saved 3 rows"));
    }

    #[tokio::test]
    async fn rename_keeps_filter_and_sort_on_column() {
        let (mut app, _tmp) = test_app();
        let v = &mut app.sheets.active_mut().view;
        v.filter = Some(FilterState {
            column: "name".into(),
            regex: "a".into(),
        });
        v.sort.push(SortKey {
            column: "name".into(),
            order: SortOrder::Asc,
        });
        app.dispatch(Action::RenameCol {
            new_name: "who".into(),
        });
        let s = app.sheets.active();
        assert_eq!(s.view.filter.as_ref().unwrap().column, "who");
        assert_eq!(s.view.sort[0].column, "who");
        assert!(s.window_plan().collect().is_ok());
    }

    #[tokio::test]
    async fn rename_prompt_is_rendered() {
        let (mut app, _tmp) = test_app();
        app.input = InputBuf::Rename("foo".into());
        let mut term = Terminal::new(ratatui::backend::TestBackend::new(40, 6)).unwrap();
        term.draw(|f| app.render(f)).unwrap();
        let buf = term.backend().buffer();
        let last: String = (0..40).map(|x| buf[(x, 5)].symbol()).collect();
        assert!(last.starts_with("rename: foo"), "status bar: {last:?}");
    }

    #[tokio::test]
    async fn page_down_on_last_window_stays_in_bounds() {
        let (mut app, _tmp) = test_app();
        // Window of 4 holding 3 rows = last (partial) window; nothing beyond it.
        app.sheets.active_mut().view.window_rows = 4;
        for _ in 0..3 {
            app.dispatch(Action::PageDown);
        }
        let v = &app.sheets.active().view;
        assert_eq!(v.window_offset, 0);
        assert_eq!(v.row_cursor, 2);
    }

    #[tokio::test]
    async fn bad_filter_regex_is_rejected_with_toast() {
        let (mut app, _tmp) = test_app();
        app.dispatch(Action::SetFilter {
            column: "name".into(),
            regex: "(".into(),
        });
        assert!(app.sheets.active().view.filter.is_none());
        assert!(app.toast.as_deref().unwrap().starts_with("bad regex"));
    }

    #[tokio::test]
    async fn sort_and_filter_clear_positional_selection() {
        let (mut app, _tmp) = test_app();
        for action in [
            Action::SetSort {
                column: "n".into(),
                descending: false,
            },
            Action::SetFilter {
                column: "name".into(),
                regex: "a".into(),
            },
        ] {
            app.sheets.active_mut().view.selected.insert(0);
            app.dispatch(action);
            assert!(app.sheets.active().view.selected.is_empty());
        }
    }
}

#[cfg(test)]
mod scenario {
    //! End-to-end: real keymap + dispatch + background Polars tasks + render.
    use super::*;
    use std::io::Write;

    pub struct Harness {
        pub app: App,
        pub term: Terminal<ratatui::backend::TestBackend>,
        _tmp: tempfile::NamedTempFile,
    }

    impl Harness {
        /// `rows`-row CSV: id,name,grp,amt with id = 1..=rows.
        pub async fn new(rows: usize) -> Self {
            let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
            writeln!(tmp, "id,name,grp,amt").unwrap();
            for i in 1..=rows {
                writeln!(tmp, "{i},name_{i},{},{}", i % 7, i as f64 * 1.5).unwrap();
            }
            tmp.flush().unwrap();
            let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
            let mut app = App::new(Sheet::from_source(0, loaded), None, None);
            app.spawn_materialize();
            let term = Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
            let mut h = Self {
                app,
                term,
                _tmp: tmp,
            };
            h.pump().await;
            h
        }

        /// Apply background results until none arrive for a while, then draw.
        pub async fn pump(&mut self) {
            while let Ok(Some(res)) =
                tokio::time::timeout(Duration::from_millis(500), self.app.materialize_rx.recv())
                    .await
            {
                self.app.on_materialize(res);
            }
            self.app.draw(&mut self.term).unwrap();
        }

        /// Space-separated key tokens, e.g. "g g", "<C-d>", or literal text via `type_`.
        pub async fn keys(&mut self, ks: &str) {
            for k in ks.split(' ').filter(|k| !k.is_empty()) {
                self.app.on_key(k.parse().unwrap());
                self.app.draw(&mut self.term).unwrap();
            }
            self.pump().await;
        }

        pub async fn type_(&mut self, text: &str) {
            for c in text.chars() {
                self.app
                    .on_key(Key::from(crossterm::event::KeyEvent::from(KeyCode::Char(
                        c,
                    ))));
            }
            self.keys("<CR>").await;
        }

        pub fn screen(&self) -> String {
            let buf = self.term.backend().buffer();
            (0..buf.area.height)
                .map(|y| {
                    (0..buf.area.width)
                        .map(|x| buf[(x, y)].symbol())
                        .collect::<String>()
                        .trim_end()
                        .to_string()
                })
                .collect::<Vec<_>>()
                .join("\n")
        }
    }

    fn title(h: &Harness) -> String {
        h.screen().lines().next().unwrap_or_default().to_string()
    }

    /// First data row (below the header), first cell.
    fn first_cell(h: &Harness) -> String {
        let screen = h.screen();
        let line = screen.lines().nth(2).unwrap_or_default();
        line.split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn navigate_search_filter_save_across_windows() {
        let mut h = Harness::new(5000).await; // 12 rows tall → 9 data rows per screen
        assert!(title(&h).contains("row 1/5000"), "{}", title(&h));

        h.keys("G").await;
        assert!(title(&h).contains("row 5000/5000"), "{}", title(&h));
        assert!(h.screen().contains("5000  name_5000"), "{}", h.screen());

        // :save writes every row of the view, not just the loaded window
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("all.csv");
        h.keys(":").await;
        h.type_(&format!("w {}", out.display())).await;
        let saved = crate::reader::open(out.as_os_str(), None).unwrap();
        assert_eq!(saved.lazy.collect().unwrap().height(), 5000);

        h.keys("g g").await;
        assert!(title(&h).contains("row 1/5000"), "{}", title(&h));
        assert_eq!(first_cell(&h), "1");

        h.keys(&"<C-d> ".repeat(12)).await;
        assert!(title(&h).contains("row 109/5000"), "{}", title(&h));

        h.keys("]").await; // sort desc on id
        assert_eq!(first_cell(&h), "5000");
        h.keys("z o g g /").await;
        h.type_("name_4999").await;
        assert!(title(&h).contains("row 4999/5000"), "{}", title(&h));

        h.keys("0 l l z |").await; // filter grp
        h.type_("^3$").await;
        let want = (1..=5000).filter(|i| i % 7 == 3).count();
        assert!(
            title(&h).contains(&format!("row 1/{want}")),
            "{}",
            title(&h)
        );
        h.keys("F").await;
        assert!(
            title(&h).contains("freq(grp)") && title(&h).contains("/1 "),
            "{}",
            title(&h)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn selection_covers_whole_view() {
        let mut h = Harness::new(5000).await;
        h.keys("g s").await;
        assert!(title(&h).contains("sel 5000"), "{}", title(&h));
        h.keys("g u l |").await;
        h.type_("^name_49").await; // 49, 490-499, 4900-4999
        assert!(title(&h).contains("sel 111"), "{}", title(&h));
        h.keys("\"").await;
        assert!(title(&h).contains("row 1/111"), "{}", title(&h));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_col_keeps_cursor_on_column() {
        let mut h = Harness::new(10).await;
        h.keys("l l !").await; // key grp → moves to the front
        assert!(title(&h).contains("col 1/4 (grp)"), "{}", title(&h));
        h.keys("!").await; // unkey → back to its place
        assert!(title(&h).contains("col 3/4 (grp)"), "{}", title(&h));
    }

    fn status(h: &Harness) -> String {
        h.screen().lines().last().unwrap_or_default().to_string()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_user_input_is_rejected_without_breaking_the_sheet() {
        let mut h = Harness::new(30).await;
        h.keys(":").await;
        h.type_("sort nosuch").await;
        assert_eq!(status(&h), "no column: nosuch");
        h.keys(":").await;
        h.type_("filter nosuch x").await;
        assert_eq!(status(&h), "no column: nosuch");
        h.keys("^").await;
        h.type_("grp").await; // id → grp would duplicate
        assert_eq!(status(&h), "column already exists: grp");
        // sheet still healthy: derived sheets build
        h.keys("F").await;
        assert!(
            title(&h).contains("freq(id)") && title(&h).contains("/30 "),
            "{}",
            title(&h)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn multiline_errors_show_first_line_only() {
        let mut h = Harness::new(3).await;
        h.app.toast = Some("materialize: boom\n\nResolved plan: …".into());
        h.pump().await;
        assert_eq!(status(&h), "materialize: boom");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tail_follow_and_row_diff() {
        use std::io::Write;
        let mut h = Harness::new(3).await;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.jsonl");
        std::fs::write(&p, "{\"a\":1}\n{\"a\":2}\n").unwrap();
        h.keys(":").await;
        h.type_(&format!("tail {} -f", p.display())).await;
        h.keys("G").await;
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "{{\"a\":3}}\n{{\"a\":4}}").unwrap();
        drop(f);
        h.app.on_tail_tick();
        h.pump().await;
        assert!(title(&h).contains("row 4/4"), "{}", title(&h)); // followed to the new bottom

        h.keys("q").await;
        let other = dir.path().join("o.csv");
        std::fs::write(
            &other,
            "id,name,grp,amt\n1,name_1,1,1.5\n2,CHANGED,2,3.0\n99,x,0,0\n",
        )
        .unwrap();
        h.keys(":").await;
        h.type_(&format!("diff {} id", other.display())).await;
        // id 2 changed, id 3 only_a, id 99 only_b; id 1 identical → hidden
        assert!(
            title(&h).contains("rowdiff(id)") && title(&h).contains("/3 "),
            "{}",
            h.screen()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn cursor_column_scrolls_into_view() {
        let mut h = Harness::new(10).await;
        h.term = Terminal::new(ratatui::backend::TestBackend::new(16, 5)).unwrap();
        h.keys("$").await;
        assert!(
            h.screen().lines().nth(1).unwrap().contains("amt"),
            "{}",
            h.screen()
        );
        h.keys("0").await;
        assert!(
            h.screen().lines().nth(1).unwrap().starts_with("id"),
            "{}",
            h.screen()
        );
    }
}
