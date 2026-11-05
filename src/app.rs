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

use unicode_width::UnicodeWidthStr;

use crate::action::Action;
use crate::key::Key;
use crate::keymap::{Chord, Dispatch, Keymap, Layer};
use crate::lineedit::LineEdit;
use crate::sheet::{
    FilterState, Origin, ROW_ID, SearchState, Sheet, SheetId, Sheets, SortKey, SortOrder, View,
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

    enable_raw_mode().context("no usable terminal (try --headless)")?;
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
    /// A background job finished (see `App::spawn_job`).
    Job {
        sheet_id: SheetId,
        label: &'static str,
        result: Result<JobDone, String>,
    },
    /// Row count of the filtered view, tagged with the `count_key` it was computed for.
    Count {
        sheet_id: SheetId,
        key: String,
        total: Result<u64, String>,
    },
}

const PIVOT_USAGE: &str = ":pivot <rows> <columns> <values> [agg]";
const PIVOT_AGGS: &str = "sum (default), mean, count, min, max, first, last, or a Lua aggregator";
const PIVOT_PREVIEW_ROWS: u32 = 10_000;

struct PivotPreview {
    /// Ex-line text this preview belongs to.
    text: String,
    /// The pivoted sample, or hint / error text.
    body: Result<Box<Sheet>, String>,
}

/// An in-memory sheet showing `df` (preview panes).
fn preview_sheet(parent: &Sheet, df: DataFrame) -> Sheet {
    let mut s = Sheet::child(
        parent,
        0,
        String::new(),
        df.clone().lazy(),
        Origin::Copy { parent: parent.id },
    );
    s.window = Some(df);
    s
}

/// What a background job hands back to the UI thread.
enum JobDone {
    /// Push a derived sheet (pivot, dup, describe, sql, join, …).
    PushSheet(Box<Sheet>),
    /// Move the cursor to view row / column (search).
    Jump {
        row: u64,
        col: usize,
    },
    /// Hide these columns on the job's sheet (`:uniform`).
    Hide(Vec<String>),
    /// Add row ids to the job's sheet selection.
    Select {
        rids: Vec<u64>,
        note: String,
    },
    Toast(String),
    /// `:pivot` preview for the ex-line text it was computed from.
    PivotPreview {
        text: String,
        result: Result<Box<Sheet>, String>,
    },
}

/// Which prompt the input line belongs to; decides what `<CR>` does with the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prompt {
    Search,
    Filter,
    Ex,
    Select,
    Rename,
    Diff,
}

impl Prompt {
    fn label(self) -> &'static str {
        match self {
            Prompt::Search => "/",
            Prompt::Filter => "filter: ",
            Prompt::Ex => ":",
            Prompt::Select => "select: ",
            Prompt::Rename => "rename: ",
            Prompt::Diff => "diff with: ",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PlanMode {
    Off,
    Normal,
    Optimized,
}

pub struct App {
    /// Lua VM for the session: keymap patches applied in `new`; loader rules
    /// consulted when `:diff` opens a file.
    plugins: Option<crate::plugins::Plugins>,
    sheets: Sheets,
    keymap: Keymap,
    pending: Vec<Key>,
    pending_candidates: Vec<Chord>,
    pending_count: Option<usize>,
    last_action: Option<Vec<Action>>,
    toast: Option<String>,
    input: Option<(Prompt, LineEdit)>,
    ex_history: crate::exline::History,
    plan_mode: PlanMode,
    /// While typing `:pivot …`: hints, then a pivot of the first rows.
    pivot_preview: Option<PivotPreview>,
    theme: Theme,
    /// Data rows visible in the sheet area at the last draw; one PageDown step.
    viewport_rows: usize,
    /// Labels of background jobs in flight, shown in the status bar.
    busy: Vec<&'static str>,
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
        let (keymap, theme) = keymap_and_theme(plugins.as_ref());
        Self {
            plugins,
            sheets: Sheets::new(sheet),
            keymap,
            pending: Vec::new(),
            pending_candidates: Vec::new(),
            pending_count: None,
            last_action: None,
            toast: plugin_warning,
            input: None,
            ex_history: crate::exline::History::load(),
            plan_mode: PlanMode::Off,
            pivot_preview: None,
            theme,
            viewport_rows: DEFAULT_VIEWPORT_ROWS,
            busy: Vec::new(),
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
                    self.reload_plugins_if_changed();
                    self.on_tail_tick();
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

    /// No background work outstanding for the active sheet (tests wait on this).
    #[cfg(test)]
    fn settled(&self) -> bool {
        let s = self.sheets.active();
        s.state.loaded_rev == s.revision && !s.state.counting && self.busy.is_empty()
    }

    /// Run `work` on the blocking pool; its result lands in `on_job_done`.
    /// Polars collects can't be interrupted, so there's no cancel: a result
    /// for a sheet that's gone is dropped.
    fn spawn_job(
        &mut self,
        label: &'static str,
        work: impl FnOnce() -> Result<JobDone, String> + Send + 'static,
    ) {
        self.busy.push(label);
        let sheet_id = self.sheets.active().id;
        let tx = self.materialize_tx.clone();
        tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(work)
                .await
                .unwrap_or_else(|e| Err(format!("job crashed: {e}")));
            let _ = tx
                .send(MaterializeResult::Job {
                    sheet_id,
                    label,
                    result,
                })
                .await;
        });
    }

    fn on_job_done(
        &mut self,
        sheet_id: SheetId,
        label: &'static str,
        result: Result<JobDone, String>,
    ) {
        if let Some(i) = self.busy.iter().position(|l| *l == label) {
            self.busy.remove(i);
        }
        match result {
            Err(e) => self.toast = Some(format!("{label}: {e}")),
            Ok(JobDone::Toast(t)) => self.toast = Some(t),
            Ok(JobDone::PivotPreview { text, result }) => {
                if let Some(p) = self.pivot_preview.as_mut().filter(|p| p.text == text) {
                    p.body = result.map_err(|e| format!("{e}\n\n{}", PIVOT_USAGE));
                }
            }
            Ok(JobDone::PushSheet(sheet)) => {
                self.sheets.push(*sheet);
                self.spawn_materialize();
            }
            Ok(JobDone::Jump { row, col }) => {
                if self.sheets.active().id == sheet_id {
                    self.goto_row(row);
                    self.sheets.active_mut().view.col_cursor = col;
                }
            }
            Ok(JobDone::Hide(cols)) => {
                if let Some(s) = self.sheets.find_by_id_mut(sheet_id) {
                    self.toast = Some(format!("hid {} uniform columns", cols.len()));
                    s.view.hidden.extend(cols);
                    if s.id == self.sheets.active().id {
                        self.spawn_materialize();
                    }
                }
            }
            Ok(JobDone::Select { rids, note }) => {
                if let Some(s) = self.sheets.find_by_id_mut(sheet_id) {
                    s.view.selected.extend(rids);
                    self.toast = Some(note);
                }
            }
        }
    }

    fn spawn_materialize(&mut self) {
        let s = self.sheets.active_mut();
        s.revision = s.revision.wrapping_add(1);
        let sheet_id = s.id;
        let rev = s.revision;
        let plan = s.window_plan_with_rids();
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
        s.state.counting = true;
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
                s.state.loaded_rev = revision;
                let (rids, df) = match crate::sheet::rid_values(&df) {
                    Ok(r) => (r, df.drop(ROW_ID).unwrap_or(df)),
                    Err(_) => (Vec::new(), df), // pre-built windows carry no ids
                };
                s.state.window_rids = rids;
                s.window = Some(df);
                if sheet_id == self.sheets.active().id {
                    self.settle_jump_bottom();
                }
            }
            MaterializeResult::Job {
                sheet_id,
                label,
                result,
            } => self.on_job_done(sheet_id, label, result),
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
                s.state.counting = false;
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
                s.state.loaded_rev = revision;
                self.toast = Some(format!("materialize: {err}"));
            }
        }
    }

    fn on_key(&mut self, key: Key) {
        // Toast lives until the next key — "clear on next action" without a timer.
        self.toast = None;
        if self.input.is_some() {
            self.on_input_key(key);
            self.refresh_pivot_preview();
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
        let Some((prompt, line)) = self.input.as_mut() else {
            return;
        };
        // Ex-mode history navigation: Up/Down walk history, not bound in keymap to keep layer clean.
        if *prompt == Prompt::Ex {
            let recalled = match key.code {
                KeyCode::Up => Some(self.ex_history.prev()),
                KeyCode::Down => Some(self.ex_history.next()),
                _ => None,
            };
            if let Some(r) = recalled {
                if let Some(text) = r {
                    line.set(text);
                }
                return;
            }
        }
        if line.edit(key) {
            return;
        }
        match self.keymap.resolve(Layer::Input, &[], key) {
            Dispatch::Fire(actions) => {
                for a in actions {
                    match a {
                        Action::SubmitInput => self.submit_input(),
                        Action::CancelInput => self.cancel_input(),
                        Action::InputBackspace => {
                            if let Some((_, l)) = self.input.as_mut() {
                                l.backspace();
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {
                if let (KeyCode::Char(c), false) = (key.code, key.ctrl) {
                    line.insert(c);
                }
            }
        }
    }

    /// Keep the `:pivot` preview in step with the ex-line text.
    fn refresh_pivot_preview(&mut self) {
        let text = match &self.input {
            Some((Prompt::Ex, line)) => line.text().to_string(),
            _ => {
                self.pivot_preview = None;
                return;
            }
        };
        let rest = match text.trim_start().strip_prefix("pivot") {
            Some(r) if r.is_empty() || r.starts_with(' ') => r.to_string(),
            _ => {
                self.pivot_preview = None;
                return;
            }
        };
        if self.pivot_preview.as_ref().is_some_and(|p| p.text == text) {
            return;
        }
        let s = self.sheets.active();
        let columns: Vec<String> = s.window.as_ref().map_or_else(Vec::new, |df| {
            df.get_column_names()
                .iter()
                .map(|n| n.to_string())
                .collect()
        });
        let args = crate::exline::split_args(&rest, PIVOT_USAGE).unwrap_or_default();
        let hint = |next: &str| {
            format!(
                "{PIVOT_USAGE}\n\nnext: {next}\n\ncolumns: {}\naggs: {}",
                columns.join(", "),
                PIVOT_AGGS
            )
        };
        // A half-typed column name (prefix of one, no space yet) isn't an error.
        let typing = !rest.ends_with(' ')
            && args.len() <= 3
            && args.last().is_some_and(|a| {
                !columns.contains(a) && columns.iter().any(|c| c.starts_with(a.as_str()))
            });
        let done = args.len() - usize::from(typing);
        let unknown = args.iter().take(done.min(3)).find(|a| !columns.contains(a));
        let body = match (done, unknown) {
            (_, Some(c)) => Err(format!("unknown column: {c}\n\n{}", hint("a column name"))),
            (0, _) => Err(hint("<rows>: column whose values label the rows")),
            (1, _) => Err(hint("<columns>: column whose values become new columns")),
            (2, _) => Err(hint("<values>: column to aggregate in each cell")),
            _ => {
                let (index, on, values) = (args[0].clone(), args[1].clone(), args[2].clone());
                let agg = args.get(3).cloned().unwrap_or_else(|| "sum".into());
                let mut sample = s.clone();
                sample.plan = s.view_plan().slice(0, PIVOT_PREVIEW_ROWS);
                sample.view = Default::default();
                let lua = self.plugins.as_ref().and_then(|p| p.aggregator(&agg));
                let previous = self.pivot_preview.take().map(|p| p.body);
                let job_text = text.clone();
                self.spawn_job("preview", move || {
                    let result = match &lua {
                        Some(a) => lua_pivot(a, &sample, &index, &on, &values)
                            .map(|df| Box::new(preview_sheet(&sample, df))),
                        None => {
                            Sheet::pivot_from(&sample, 0, &index, &on, &values, &agg).map(Box::new)
                        }
                    };
                    Ok(JobDone::PivotPreview {
                        text: job_text,
                        result,
                    })
                });
                // Keep showing the last preview until this one lands.
                previous.unwrap_or_else(|| Err("computing…".into()))
            }
        };
        self.pivot_preview = Some(PivotPreview { text, body });
    }

    fn start_input(&mut self, prompt: Prompt) {
        self.input = Some((prompt, LineEdit::default()));
    }

    fn submit_input(&mut self) {
        let Some((prompt, line)) = self.input.take() else {
            return;
        };
        let text = line.text().to_string();
        if text.is_empty() && prompt != Prompt::Ex {
            return;
        }
        match prompt {
            Prompt::Search => {
                self.sheets.active_mut().view.search = Some(SearchState { needle: text });
                self.dispatch(Action::SearchNext);
            }
            Prompt::Filter => {
                if let Some(column) = self.current_column_name() {
                    self.dispatch(Action::SetFilter {
                        column,
                        regex: text,
                    });
                } else {
                    self.toast = Some("no column to filter".into());
                }
            }
            Prompt::Ex => {
                self.ex_history.push(&text);
                match crate::exline::parse(&text) {
                    Ok(actions) => self.dispatch_sequence(actions),
                    Err(e) => self.toast = Some(format!("{e}")),
                }
            }
            Prompt::Select => self.dispatch(Action::SelectByRegex { regex: text }),
            Prompt::Rename => self.dispatch(Action::RenameCol { new_name: text }),
            Prompt::Diff => {
                let keys = self.sheets.active().view.keys.clone();
                self.dispatch(Action::Diff { path: text, keys });
            }
        }
    }

    fn cancel_input(&mut self) {
        self.input = None;
        self.ex_history.reset_cursor();
    }

    /// A `vd.bind` function: `vd.sheet()` describes the active sheet's cursor.
    fn run_lua_binding(&mut self, id: usize) {
        let Some(p) = self.plugins.as_ref() else {
            return;
        };
        let s = self.sheets.active();
        let v = &s.view;
        let df = s.window.as_ref();
        let ctx = crate::plugins::SheetCtx {
            name: &s.name,
            row: v.window_offset + v.row_cursor as u64 + 1,
            col: v.col_cursor + 1,
            column_names: df.map_or_else(Vec::new, |df| {
                df.get_column_names()
                    .iter()
                    .map(|n| n.to_string())
                    .collect()
            }),
            value: df
                .and_then(|df| df.get_columns().get(v.col_cursor)?.get(v.row_cursor).ok())
                .unwrap_or(AnyValue::Null),
        };
        self.toast = Some(match p.call(id, &ctx) {
            Ok(toasts) if toasts.is_empty() => return,
            Ok(toasts) => toasts.join(" · "),
            Err(e) => format!("{e}"),
        });
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
                s.plan = s
                    .plan
                    .clone()
                    .rename([old.as_str()], [new_name.as_str()], true);
                s.state.steps.push(crate::sheet::PlanStep::Rename {
                    old: old.clone(),
                    new: new_name.clone(),
                });
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
                    let note = format!("selected {} rows where {column} = {value}", rids.len());
                    Ok(JobDone::Select { rids, note })
                });
            }
            Action::StartDiffInput => self.start_input(Prompt::Diff),
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
        s.state
            .steps
            .push(crate::sheet::PlanStep::Currency { column: name });
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
        if let Some(p) = &self.plugins {
            if p.resolve_loader(std::path::Path::new(path)).is_some() {
                self.toast = Some(format!(
                    "tail: {path} needs a plugin loader; can't follow it"
                ));
                return;
            }
        }
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

    /// Hot reload: `init.lua` edited/created/deleted → fresh VM, keymap, theme.
    fn reload_plugins_if_changed(&mut self) {
        let Some((fresh, warning)) = self.plugins.as_ref().and_then(|p| p.reload_if_changed())
        else {
            return;
        };
        self.plugins = Some(fresh);
        (self.keymap, self.theme) = keymap_and_theme(self.plugins.as_ref());
        self.toast = Some(warning.unwrap_or_else(|| "init.lua reloaded".into()));
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
        match active.refresh_tail() {
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

    fn open_diff(&mut self, path: &str, keys: &[String]) {
        use std::ffi::OsString;
        let loaded = match crate::plugins::open(self.plugins.as_ref(), &OsString::from(path), None)
        {
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
                Sheet::row_diff_from(parent, id, &Sheet::from_source(0, loaded), keys)
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
        let right = match open_sheet {
            Some(id) => self
                .sheets
                .find_by_id_mut(id)
                .expect("id from summaries")
                .clone(),
            None => match crate::plugins::open(self.plugins.as_ref(), source.as_ref(), None) {
                Ok(l) => Sheet::from_source(0, l),
                Err(e) => {
                    self.toast = Some(format!("join: {e}"));
                    return;
                }
            },
        };
        let id = self.sheets.alloc_id();
        match Sheet::join_from(self.sheets.active(), id, &right, keys, how) {
            Ok(s) => self.push_collected("join", s),
            Err(e) => self.toast = Some(format!("join: {e}")),
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
                match crate::reader::open_part(&path, kind, &part) {
                    Ok(loaded) => {
                        let id = self.sheets.alloc_id();
                        self.sheets.push(Sheet::from_source(id, loaded));
                        self.spawn_materialize();
                    }
                    Err(e) => self.toast = Some(format!("open {part}: {e}")),
                }
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
        // Values by row id = position in the freq sheet's base plan.
        let values = match s.plan.clone().select([col(column.as_str())]).collect() {
            Ok(df) => df,
            Err(e) => {
                self.toast = Some(format!("dive: {e}"));
                return;
            }
        };
        let vals = values.column(&column).expect("selected column");
        let picked: Vec<AnyValue> = rids
            .iter()
            .filter_map(|r| vals.get(*r as usize).ok().map(|v| v.into_static()))
            .collect();
        let Some(parent_sheet) = self.sheets.find_by_id_mut(parent) else {
            self.toast = Some("dive: parent sheet was closed".into());
            return;
        };
        let parent_sheet = parent_sheet.clone();
        let id = self.sheets.alloc_id();
        let child = Sheet::rows_with_values(&parent_sheet, id, &column, &picked);
        self.sheets.push(child);
        self.spawn_materialize();
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
        let v = &mut self.sheets.active_mut().view;
        if !v.selected.remove(&rid) {
            v.selected.insert(rid);
        }
    }

    fn set_select_current(&mut self, select: bool) {
        let Some(rid) = self.rid_at_cursor() else {
            return;
        };
        let v = &mut self.sheets.active_mut().view;
        if select {
            v.selected.insert(rid);
        } else {
            v.selected.remove(&rid);
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
        let preview = self
            .input
            .is_some()
            .then_some(())
            .and(self.pivot_preview.as_ref());
        let (sheet_area, plan_area) = if self.plan_mode == PlanMode::Off && preview.is_none() {
            (main_area, None)
        } else {
            let halves = Layout::default()
                .direction(Direction::Horizontal)
                .constraints(if preview.is_some() {
                    [Constraint::Percentage(50), Constraint::Percentage(50)]
                } else {
                    [Constraint::Percentage(65), Constraint::Percentage(35)]
                })
                .split(main_area);
            (halves[0], Some(halves[1]))
        };
        self.viewport_rows = (sheet_area.height as usize).saturating_sub(1).max(1);
        let lua_render = self.plugins.as_ref().filter(|p| p.has_renderers());
        let render_cell =
            |c: &str, d: &DataType, v: &AnyValue| lua_render.and_then(|p| p.render(c, d, v));
        let render = lua_render.map(|_| &render_cell as crate::widget::CellRenderer);
        let s = self.sheets.active_mut();
        if let Some(df) = &s.window {
            clamp_view(&mut s.view, df, sheet_area, render);
        }

        self.render_titlebar(f, chunks[0]);
        f.render_widget(
            SheetWidget {
                sheet: self.sheets.active(),
                theme: &self.theme,
                render,
            },
            sheet_area,
        );
        if let Some(area) = plan_area {
            match self.pivot_preview.as_ref().filter(|_| self.input.is_some()) {
                Some(p) => self.render_pivot_preview(f, area, p),
                None => self.render_plan_panel(f, area),
            }
        }

        self.render_statusbar(f, chunks[2]);
        if !self.pending_candidates.is_empty() {
            self.render_which_key(f, main_area);
        }
    }

    fn render_pivot_preview(&self, f: &mut ratatui::Frame<'_>, area: Rect, p: &PivotPreview) {
        let title = match &p.body {
            Ok(_) => format!(" pivot preview: first {PIVOT_PREVIEW_ROWS} rows (Enter = all) "),
            Err(_) => " pivot ".to_string(),
        };
        let block = ratatui::widgets::Block::default()
            .title(title)
            .borders(ratatui::widgets::Borders::ALL)
            .style(Style::default().fg(self.theme.header_fg));
        let inner = block.inner(area);
        f.render_widget(block, area);
        match &p.body {
            Ok(sheet) => f.render_widget(
                SheetWidget {
                    sheet,
                    theme: &self.theme,
                    render: None,
                },
                inner,
            ),
            Err(hint) => f.render_widget(
                Paragraph::new(hint.as_str()).wrap(ratatui::widgets::Wrap { trim: false }),
                inner,
            ),
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
            .filter_map(|c| c.on.get(pending_len).map(|k| (k, c.desc.as_ref())))
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
        let center =
            format!(" row {abs_row}/{total} · col {col}/{cols} ({col_name}) · sel {sel} ",);
        let right = format!(" {kind:?} · sheets {stack_pos} ", kind = s.kind);
        // Position info wins over the sheet name: the name gives up its head.
        let room = (area.width as usize).saturating_sub(center.width() + right.width() + 2);
        let left = format!(" {} ", truncate_head(&s.name, room));
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
            (Some((prompt, line)), _, _, _) => {
                // Cursor = reversed cell (a space at the end of the line).
                let (before, after) = line.split();
                let mut rest = after.chars();
                let at = rest.next().map_or(" ".to_string(), String::from);
                Line::from(vec![
                    Span::styled(prompt.label(), Style::default().fg(Color::Yellow)),
                    Span::raw(before.to_string()),
                    Span::styled(at, Style::default().add_modifier(Modifier::REVERSED)),
                    Span::raw(rest.as_str().to_string()),
                ])
            }
            // Polars errors span lines (plan dumps); the bar shows the first.
            (_, Some(t), _, _) => Line::from(Span::styled(
                t.lines().next().unwrap_or_default().to_string(),
                self.theme.toast(),
            )),
            (_, _, Some(c), _) => Line::from(format!("count: {c}")),
            _ if !self.busy.is_empty() => Line::from(format!("working: {}…", self.busy.join(", "))),
            (_, _, _, true) => Line::from(format!(
                "pending: {}",
                self.pending
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join(" ")
            )),
            _ => Line::from(
                " hjkl · gg/G · / search · [ ] sort · z| filter · s sel · F freq · I describe · U undo · : ex · q ",
            ),
        };
        f.render_widget(Paragraph::new(text).style(self.theme.statusbar()), area);
    }
}

/// `:pivot` with a Lua aggregator: Polars builds each cell's value list, Lua
/// reduces it. Runs on a background job; the VM lock is held per cell, so
/// renderers on the UI thread interleave.
/// ponytail: per-cell Lua calls; fine for pivot-sized outputs, slow for millions of cells.
fn lua_pivot(
    a: &crate::plugins::Aggregator,
    parent: &Sheet,
    index: &str,
    on: &str,
    values: &str,
) -> Result<DataFrame, String> {
    use crate::plugins::AggValue;
    use polars_ops::pivot::{PivotAgg, pivot_stable};
    let groups = parent
        .view_plan()
        .group_by_stable([col(index), col(on)])
        .agg([col(values)])
        .collect()
        .map_err(|e| e.to_string())?;
    let lists = groups
        .column(values)
        .and_then(|c| c.list().cloned())
        .map_err(|e| e.to_string())?;
    let mut out = Vec::with_capacity(lists.len());
    for cell in lists.into_iter() {
        let cell = cell.unwrap_or_else(|| Series::new_empty("v".into(), &DataType::Null));
        out.push(a.call(&cell).map_err(|e| e.to_string())?);
    }
    let agg_col = if out.iter().all(|v| !matches!(v, AggValue::Text(_))) {
        let nums: Vec<Option<f64>> = out
            .iter()
            .map(|v| match v {
                AggValue::Num(n) => Some(*n),
                _ => None,
            })
            .collect();
        Column::new(values.into(), nums)
    } else {
        let texts: Vec<Option<String>> = out
            .iter()
            .map(|v| match v {
                AggValue::Num(n) => Some(n.to_string()),
                AggValue::Text(t) => Some(t.clone()),
                AggValue::Null => None,
            })
            .collect();
        Column::new(values.into(), texts)
    };
    let long = DataFrame::new(vec![
        groups.column(index).map_err(|e| e.to_string())?.clone(),
        groups.column(on).map_err(|e| e.to_string())?.clone(),
        agg_col,
    ])
    .map_err(|e| e.to_string())?;
    pivot_stable(
        &long,
        [on],
        Some([index]),
        Some([values]),
        false,
        Some(PivotAgg::First),
        None,
    )
    .map_err(|e| e.to_string())
}

/// Default keymap plus Lua patches, and the theme `init.lua` picked.
fn keymap_and_theme(plugins: Option<&crate::plugins::Plugins>) -> (Keymap, Theme) {
    let mut keymap = Keymap::defaults();
    let mut theme = Theme::dark();
    if let Some(p) = plugins {
        p.apply_keymap(&mut keymap);
        theme = p.theme().unwrap_or(theme);
    }
    (keymap, theme)
}

/// Keep the tail of `s` within `max` display cells, marking the cut with `…`.
fn truncate_head(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    if s.width() <= max {
        return s.to_string();
    }
    let mut used = 1; // the ellipsis
    let tail: Vec<char> = s
        .chars()
        .rev()
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= max
        })
        .collect();
    std::iter::once('…').chain(tail.into_iter().rev()).collect()
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
            df: s.window_plan_with_rids().collect().unwrap(),
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
        let mut line = LineEdit::default();
        line.set("foo");
        app.input = Some((Prompt::Rename, line));
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
    async fn selection_follows_its_row_across_sort_and_filter() {
        let (mut app, _tmp) = test_app(); // alice,3 bob,1 carol,2
        land_window(&mut app);
        app.dispatch(Action::ToggleSelectRow); // alice
        app.dispatch(Action::SetSort {
            column: "n".into(),
            descending: false,
        });
        land_window(&mut app); // bob, carol, alice
        let s = app.sheets.active();
        assert_eq!(s.state.window_rids, vec![1, 2, 0]);
        assert_eq!(s.view.selected.iter().copied().collect::<Vec<_>>(), vec![0]);
        app.dispatch(Action::SetFilter {
            column: "name".into(),
            regex: "a".into(),
        }); // alice + carol remain, selection kept
        app.dispatch(Action::DupSelected);
        while !app.busy.is_empty() {
            let res = app.materialize_rx.recv().await.unwrap();
            app.on_materialize(res);
        }
        assert_eq!(app.sheets.len(), 2, "dup sheet pushed");
        let dup = app.sheets.active().plan.clone().collect().unwrap();
        assert_eq!(
            dup.column("name").unwrap().get(0).unwrap().get_str(),
            Some("alice")
        );
    }
}

#[cfg(test)]
mod scenario {
    //! End-to-end: real keymap + dispatch + background Polars tasks + render.
    use super::*;
    use std::io::Write;
    use unicode_width::UnicodeWidthStr;

    pub struct Harness {
        pub app: App,
        pub term: Terminal<ratatui::backend::TestBackend>,
        _tmp: Option<tempfile::NamedTempFile>,
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
            Self::with_source(loaded, tmp).await
        }

        pub async fn with_source(
            loaded: crate::reader::LoadedSource,
            tmp: tempfile::NamedTempFile,
        ) -> Self {
            let mut app = App::new(Sheet::from_source(0, loaded), None, None);
            app.spawn_materialize();
            let mut h = Self::from_app(app).await;
            h._tmp = Some(tmp);
            h
        }

        pub async fn from_app(app: App) -> Self {
            let term = Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
            let mut h = Self {
                app,
                term,
                _tmp: None,
            };
            h.pump().await;
            h
        }

        /// Apply background results until the app is idle, then draw.
        pub async fn pump(&mut self) {
            // Generous timeout: parallel tests share CPUs; settled() ends it early.
            while !self.app.settled() {
                let next = self.app.materialize_rx.recv();
                match tokio::time::timeout(Duration::from_secs(20), next).await {
                    Ok(Some(res)) => self.app.on_materialize(res),
                    _ => break,
                }
            }
            // stale results (superseded counts) may still be queued; apply them too
            while let Ok(res) = self.app.materialize_rx.try_recv() {
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
            self.type_no_enter(text).await;
            self.keys("<CR>").await;
        }

        /// Type into the open prompt without submitting; waits for jobs.
        pub async fn type_no_enter(&mut self, text: &str) {
            for c in text.chars() {
                self.app
                    .on_key(Key::from(crossterm::event::KeyEvent::from(KeyCode::Char(
                        c,
                    ))));
            }
            self.pump().await;
            self.app.draw(&mut self.term).unwrap();
        }

        pub fn screen(&self) -> String {
            let buf = self.term.backend().buffer();
            (0..buf.area.height)
                .map(|y| {
                    // Like a terminal: a wide symbol covers the next cell too.
                    let mut line = String::new();
                    let mut x = 0;
                    while x < buf.area.width {
                        let sym = buf[(x, y)].symbol();
                        line.push_str(sym);
                        x += sym.width().max(1) as u16;
                    }
                    line.trim_end().to_string()
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
            title(&h).contains("o.csv") && title(&h).contains("row 1/3"),
            "{}",
            h.screen()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wide_and_control_chars_render_on_one_line() {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        tmp.write_all("k,v,z\n名前,\"a\nb\",end\nx\ty,q,end\n".as_bytes())
            .unwrap();
        tmp.flush().unwrap();
        let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
        let mut h = Harness::with_source(loaded, tmp).await;
        let screen = h.screen();
        let rows: Vec<&str> = screen.lines().collect();
        assert!(
            rows[2].starts_with("名前") && rows[2].contains("a↵b"),
            "{screen}"
        );
        assert!(rows[3].contains("x→y"), "{screen}");
        // `z` column starts at the same terminal column on both rows
        let col = |r: &str| r[..r.find("end").unwrap()].width();
        assert_eq!(col(rows[2]), col(rows[3]), "{screen}");
        h.keys("j").await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn long_sheet_name_never_hides_position() {
        let mut h = Harness::new(3).await;
        h.app.sheets.active_mut().name = format!("/{}/data.csv", "very/long/dir".repeat(20));
        h.pump().await;
        let t = title(&h);
        assert!(
            t.contains("…") && t.contains("data.csv") && t.contains("row 1/3"),
            "{t}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn diff_uses_plugin_loader_and_tail_refuses_it() {
        let dir = tempfile::tempdir().unwrap();
        let ml = dir.path().join("other.ml");
        std::fs::write(&ml, "opaque").unwrap();
        std::fs::write(
            dir.path().join("other.csv"),
            "id,name,grp,amt\n1,name_1,1,1.5\n",
        )
        .unwrap();
        // loader "converts" x.ml by pointing at its sibling x.csv
        let lua = r#"vd.register_loader { extensions = "ml",
            open = function(p) return (p:gsub("%.ml$", ".csv")) end }"#;
        let mut h = Harness::new(2).await;
        h.app.plugins = Some(crate::plugins::Plugins::load_from_source(lua).unwrap());
        h.keys(":").await;
        h.type_(&format!("diff {} id", ml.display())).await;
        assert!(
            title(&h).contains("other.ml") && title(&h).contains("sheets 2"),
            "{}",
            h.screen()
        );
        h.keys("q :").await;
        h.type_(&format!("tail {}", ml.display())).await;
        assert!(status(&h).contains("can't follow"), "{}", status(&h));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn empty_sheet_survives_every_basic_key() {
        let mut h = Harness::new(0).await;
        for k in [
            "G", "j", "k", "<C-d>", "<C-u>", "g g", "s", "t", "u", "g s", "n", "$", "F", "q",
        ] {
            h.keys(k).await;
            assert!(
                title(&h).contains("row 0/0") && title(&h).contains("sel 0"),
                "{k}: {}",
                title(&h)
            );
        }
        h.keys("\"").await;
        assert_eq!(status(&h), "dup: no rows selected");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prompt_line_editing_and_quoted_args() {
        let mut h = Harness::new(5).await;
        // type "ort -id", jump home, fix the typo, submit
        h.keys(":").await;
        for c in "ort -id".chars() {
            h.app
                .on_key(Key::from(crossterm::event::KeyEvent::from(KeyCode::Char(
                    c,
                ))));
        }
        h.keys("<Home>").await;
        h.type_("s").await;
        assert_eq!(first_cell(&h), "5");
        h.keys(":").await;
        h.type_(r#"filter "name" ^name_[12]$"#).await;
        assert!(title(&h).contains("row 1/2"), "{}", title(&h));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn heavy_commands_run_in_background_and_report() {
        let mut h = Harness::new(3000).await;
        h.keys(":").await;
        h.type_("pivot grp grp id count").await; // job → pushes pivot sheet
        assert!(
            title(&h).contains("pivot(") && title(&h).contains("row 1/7"),
            "{}",
            title(&h)
        );
        h.keys("q").await;
        // while a job runs the status bar says so
        h.app.on_key("g".parse().unwrap());
        h.app.on_key("s".parse().unwrap());
        h.app.draw(&mut h.term).unwrap();
        assert!(
            status(&h).starts_with("working: select all"),
            "{}",
            status(&h)
        );
        h.pump().await;
        assert!(title(&h).contains("sel 3000"), "{}", title(&h));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn join_types_against_file_and_open_sheet() {
        let mut h = Harness::new(5).await; // id 1..5
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("o.csv");
        std::fs::write(&other, "id,name,extra\n2,TWO,x\n3,,y\n9,NINE,z\n").unwrap();
        let o = other.display();
        for (how, rows) in [("inner", 2), ("left", 5), ("full", 6), ("append", 8)] {
            h.keys(":").await;
            h.type_(&format!("join {o} id {how}")).await;
            assert!(
                title(&h).contains(&format!("row 1/{rows}")),
                "{how}: {}",
                h.screen()
            );
            h.keys("q").await;
        }
        // merge: right's non-null name wins, left kept where right is null
        h.keys(":").await;
        h.type_(&format!("join {o} id merge")).await;
        let s = h
            .app
            .sheets
            .active()
            .view_plan()
            .sort(["id"], Default::default());
        let df = s.collect().unwrap();
        let names: Vec<_> = df
            .column("name")
            .unwrap()
            .str()
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(names[1..3], [Some("TWO"), Some("name_3")]);
        assert!(df.column("name_right").is_err());
        // an open sheet as the right side, by #id: freq(name) joined back on name
        h.keys("q l F").await; // freq sheet on `name`
        let freq_id = h.app.sheets.active().id;
        h.keys("S <CR>").await; // switcher row 1 = base sheet → back on top
        assert!(title(&h).contains("row 1/5"), "{}", title(&h));
        h.keys(":").await;
        h.type_(&format!("join #{freq_id} name")).await;
        let df = h.app.sheets.active().view_plan().collect().unwrap();
        assert_eq!(df.height(), 5);
        assert!(df.column("count").is_ok(), "{:?}", df.get_column_names());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn freq_dive_opens_matching_rows() {
        let mut h = Harness::new(5000).await; // grp = id % 7: 1..=5 → 715 rows, 0 and 6 → 714
        h.keys("l l F").await;
        assert_eq!(first_cell(&h), "1"); // count tie broken by value
        h.keys("<CR>").await;
        assert!(
            title(&h).contains("grp=1") && title(&h).contains("row 1/715"),
            "{}",
            title(&h)
        );
        let df = h.app.sheets.active().view_plan().collect().unwrap();
        assert!(
            df.column("grp")
                .unwrap()
                .i64()
                .unwrap()
                .into_iter()
                .all(|v| v == Some(1))
        );
        h.keys("q s j s g <CR>").await; // two freq rows selected → union
        assert!(title(&h).contains("row 1/1430"), "{}", title(&h));
        h.keys("q q <CR>").await; // <CR> on a data sheet explains itself
        assert_eq!(status(&h), "<CR>: nothing to open here");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn derive_setcol_and_undo_redo() {
        let mut h = Harness::new(5).await;
        h.keys(":").await;
        h.type_("derive dbl = amt * 2").await;
        let names = |h: &Harness| h.screen().lines().nth(1).unwrap().to_string();
        assert!(names(&h).contains("dbl"), "{}", h.screen());
        h.keys("U").await;
        assert!(!names(&h).contains("dbl"), "{}", h.screen());
        h.keys("R").await;
        assert!(names(&h).contains("dbl"), "{}", h.screen());
        h.keys(":").await;
        h.type_("derive bad = nosuch + 1").await;
        assert!(status(&h).starts_with("derive:"), "{}", status(&h));
        // setcol on selected rows, type-checked
        h.keys("s j s 0 :").await;
        h.type_("setcol 42").await;
        assert_eq!(status(&h), "set id on 2 rows");
        let ids: Vec<_> = h
            .app
            .sheets
            .active()
            .view_plan()
            .collect()
            .unwrap()
            .column("id")
            .unwrap()
            .i64()
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(ids, [Some(42), Some(42), Some(3), Some(4), Some(5)]);
        h.keys(":").await;
        h.type_("setcol abc").await;
        assert!(status(&h).contains("doesn't fit id"), "{}", status(&h));
        h.keys("U U").await; // undo setcol, then the second selection
        assert!(title(&h).contains("sel 1"), "{}", title(&h));
        assert_eq!(first_cell(&h), "1");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sql_describe_pivot_uniform_copy() {
        let mut h = Harness::new(5000).await;
        h.keys(":").await;
        h.type_("sql SELECT grp, count(*) AS n FROM self GROUP BY grp ORDER BY grp")
            .await;
        assert!(
            title(&h).contains("sql:") || title(&h).contains("row 1/7"),
            "{}",
            title(&h)
        );
        assert!(title(&h).contains("row 1/7"), "{}", title(&h));
        // GROUP BY order isn't fixed by Polars: collected once so row ids hold
        let plan = h.app.sheets.active().plan.describe_plan().unwrap();
        assert!(plan.starts_with("DF "), "{plan}");
        h.keys("q I").await;
        assert!(
            title(&h).contains("describe") && title(&h).contains("row 1/4"),
            "{}",
            title(&h)
        );
        let d = h.app.sheets.active().view_plan().collect().unwrap();
        assert_eq!(
            d.column("distinct")
                .unwrap()
                .get(2)
                .unwrap()
                .extract::<u64>(),
            Some(7)
        ); // grp
        assert_eq!(
            d.column("max").unwrap().get(0).unwrap().get_str(),
            Some("5000")
        ); // id
        h.keys("q l l W").await; // no key cols → one row, a column per grp value
        let p = h.app.sheets.active().view_plan().collect().unwrap();
        assert_eq!(p.shape(), (1, 7));
        h.keys("q h !").await; // key on name… then W on grp → 5000 rows
        h.keys("l W").await;
        assert!(title(&h).contains("row 1/5000"), "{}", title(&h));
        h.keys("q :").await;
        h.type_("derive k = 1").await;
        h.keys(":").await;
        h.type_("uniform").await;
        assert_eq!(status(&h), "hid 1 uniform columns");
        h.keys("g \"").await;
        assert!(title(&h).contains("copy of"), "{}", title(&h));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn diff_prompt_uses_key_columns_and_export_writes_scripts() {
        let mut h = Harness::new(3).await;
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("o.csv");
        std::fs::write(&other, "id,name,grp,amt\n1,name_1,1,1.5\n2,X,2,3.0\n").unwrap();
        h.keys("! D").await; // key = id
        h.type_(&other.display().to_string()).await;
        assert!(title(&h).contains("row 1/2"), "{}", h.screen()); // id 2 changed, id 3 only_a
        h.keys("q").await;
        for ext in ["py", "sql"] {
            let out = dir.path().join(format!("view.{ext}"));
            h.keys(":").await;
            h.type_(&format!("export {}", out.display())).await;
            let script = std::fs::read_to_string(&out).unwrap();
            assert!(script.contains(".csv"), "{script}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn parts_list_opens_each_table() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("two.db");
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("create table a(x); insert into a values (1); create table b(y); insert into b values (8),(9);")
            .unwrap();
        drop(c);
        let loaded = crate::reader::open(db.as_os_str(), None).unwrap();
        let parts = crate::reader::list_parts(&db, loaded.kind).unwrap();
        let mut app = App::new(Sheet::parts_list(0, loaded, parts), None, None);
        app.spawn_materialize();
        let mut h = Harness::from_app(app).await;
        assert!(
            title(&h).contains("tables") && title(&h).contains("row 1/2"),
            "{}",
            title(&h)
        );
        h.keys("j <CR>").await;
        assert!(
            title(&h).contains(":b") && title(&h).contains("row 1/2"),
            "{}",
            title(&h)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tail_on_parquet_directory_picks_up_new_files() {
        let dir = tempfile::tempdir().unwrap();
        let write = |i: i64| {
            let mut df = df!("v" => [i, i]).unwrap();
            let f = std::fs::File::create(dir.path().join(format!("{i}.parquet"))).unwrap();
            ParquetWriter::new(f).finish(&mut df).unwrap();
        };
        write(1);
        let mut h = Harness::new(1).await;
        h.keys(":").await;
        h.type_(&format!("tail {}", dir.path().display())).await;
        assert!(title(&h).contains("row 1/2"), "{}", title(&h));
        write(2);
        h.app.on_tail_tick();
        h.pump().await;
        assert!(title(&h).contains("/4 "), "{}", title(&h));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lua_aggregator_renderer_and_select_equal() {
        let mut h = Harness::new(20).await;
        h.app.plugins = Some(
            crate::plugins::Plugins::load_from_source(
                r#"vd.register_aggregator("n", function(vs) return #vs end)
                   vd.register_renderer { column = "name", render = function(v) return string.upper(v) end }"#,
            )
            .unwrap(),
        );
        h.pump().await;
        assert!(h.screen().contains("NAME_1 "), "{}", h.screen()); // renderer applied
        h.keys(":").await;
        h.type_("pivot grp name id n").await; // custom aggregator: rows per cell
        let df = h.app.sheets.active().view_plan().collect().unwrap();
        assert_eq!(df.shape(), (7, 21)); // grp + one column per name
        let total: f64 = df.get_columns()[1..]
            .iter()
            .map(|c| c.f64().unwrap().sum().unwrap_or(0.0))
            .sum();
        assert_eq!(total, 20.0);
        h.keys("q l l ,").await; // grp of row 1 is 1 → ids 1, 8, 15
        assert!(title(&h).contains("sel 3"), "{}", title(&h));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn pivot_prompt_previews_live() {
        let mut h = Harness::new(20).await;
        h.keys(":").await;
        h.type_no_enter("pivot grp").await;
        assert!(h.screen().contains("next: <columns>"), "{}", h.screen());
        h.type_no_enter(" nope").await;
        assert!(
            h.screen().contains("unknown column: nope"),
            "{}",
            h.screen()
        );
        for _ in 0..4 {
            h.keys("<BS>").await;
        }
        h.type_no_enter("na").await; // half-typed: still a hint, not an error
        assert!(h.screen().contains("next: <columns>"), "{}", h.screen());
        h.type_no_enter("me id").await;
        let screen = h.screen();
        assert!(screen.contains("pivot preview"), "{screen}");
        eprintln!("{screen}");
        assert!(screen.contains("name_1"), "{screen}"); // pivoted column header
        h.type_no_enter(" bogus").await; // bad agg: error in the panel, no toast
        assert!(h.screen().contains("unknown agg: bogus"), "{}", h.screen());
        assert!(h.app.toast.is_none());
        h.keys("<Esc>").await;
        assert!(!h.screen().contains("pivot"), "{}", h.screen());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lua_function_binding_sees_the_cursor_and_toasts() {
        let mut h = Harness::new(5).await;
        h.app.plugins = Some(
            crate::plugins::Plugins::load_from_source(
                r#"vd.bind("sheet", "X", function()
                     local s = vd.sheet()
                     vd.toast(s.column .. "=" .. s.value .. " @" .. s.cursor.row .. "/" .. #s.column_names)
                   end, "show cell")
                   vd.bind("sheet", "E", function() error("boom") end)"#,
            )
            .unwrap(),
        );
        (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
        h.keys("j l X").await;
        assert!(status(&h).contains("name=name_2 @2/"), "{}", status(&h));
        h.keys("E").await;
        assert!(status(&h).contains("boom"), "{}", status(&h));
        // vd.toast outside a binding is a load error, not a silent queue
        let err = crate::plugins::Plugins::load_from_source(r#"vd.toast("hi")"#);
        assert!(err.is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn renderer_output_sets_width_and_runs_once_per_value() {
        let mut h = Harness::new(3).await;
        // `calls` grows on every Lua call: memoized output never changes
        h.app.plugins = Some(
            crate::plugins::Plugins::load_from_source(
                r#"calls = 0
                   vd.register_renderer { column = "id", render = function(v)
                     calls = calls + 1
                     return "id-" .. v .. "-rendered-wide#" .. calls end }"#,
            )
            .unwrap(),
        );
        h.pump().await;
        let first = h.screen();
        assert!(first.contains("id-1-rendered-wide#1 "), "{first}"); // not cut to the i64 width
        h.keys("j k").await;
        assert_eq!(h.screen(), first);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn long_values_further_down_widen_their_column() {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        let mut body = "k,v\n".to_string();
        for i in 0..200 {
            body += &format!(
                "{i},{}\n",
                if i == 150 { "a-much-longer-value" } else { "x" }
            );
        }
        tmp.write_all(body.as_bytes()).unwrap();
        let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
        let mut h = Harness::with_source(loaded, tmp).await;
        h.keys("1 5 0 j").await; // row 151 holds the long value; far past the first-64 sample
        assert!(h.screen().contains("a-much-longer-value"), "{}", h.screen());
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
