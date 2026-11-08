//! App loop. tokio::select! over crossterm events + materialize task results. See S-02.

use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{Event as CtEvent, EventStream, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use polars::prelude::*;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
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
use crate::widget::{SheetWidget, clamp_view, refresh_widths};

mod actions;
#[cfg(test)]
mod golden;
#[cfg(test)]
mod monkey;
mod render;

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
    /// The job's source, read into memory.
    Loaded(DataFrame),
    /// A tail that can't append, reopened.
    TailSource(Box<crate::reader::LoadedSource>),
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
    /// When the current stretch of background work started.
    working_since: Option<Instant>,
    /// Spinner frame on screen, so ticks redraw only when it changes.
    drawn_spin: Option<usize>,
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
            working_since: None,
            drawn_spin: None,
            quit: false,
            materialize_tx: tx,
            materialize_rx: rx,
            materialize_handles: HashMap::new(),
        }
    }

    async fn run<B: ratatui::backend::Backend>(mut self, term: &mut Terminal<B>) -> Result<()>
    where
        B::Error: Send + Sync + 'static,
    {
        self.spawn_materialize();
        let mut events = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(16));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut tail_tick = tokio::time::interval(Duration::from_secs(2));
        tail_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // first tick fires immediately — burn it so refresh doesn't race the initial materialize
        tail_tick.tick().await;
        self.draw(term)?;

        // Biased: everything queued is handled before the draw arm runs, so a
        // held key costs one draw per burst, not one per repeat.
        let mut dirty = false;
        loop {
            tokio::select! {
                biased;
                maybe_ev = events.next() => match maybe_ev {
                    Some(Ok(CtEvent::Key(ke))) if ke.kind != KeyEventKind::Release => {
                        let key: Key = ke.into();
                        self.on_key(key);
                        dirty = true;
                    }
                    Some(Ok(CtEvent::Resize(_, _))) => dirty = true,
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e).context("event stream"),
                    None => break,
                },
                Some(res) = self.materialize_rx.recv() => {
                    self.on_materialize(res);
                    dirty = true;
                }
                _ = tail_tick.tick() => {
                    self.reload_plugins_if_changed();
                    self.on_tail_tick();
                    dirty = true;
                }
                _ = std::future::ready(()), if dirty && !self.quit => {
                    self.draw(term)?;
                    dirty = false;
                }
                _ = tick.tick() => dirty = self.spin() != self.drawn_spin,
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
    /// Polars collects can't be interrupted, so there's no cancel: a selection
    /// or jump for a sheet that's gone is dropped, while a finished `PushSheet`
    /// still opens on top of whatever is active.
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
            Ok(JobDone::Loaded(df)) => {
                if let Some(s) = self.sheets.find_by_id_mut(sheet_id)
                    && s.state.steps.is_empty()
                {
                    s.plan = df.lazy();
                }
            }
            Ok(JobDone::TailSource(loaded)) => self.apply_tail_source(sheet_id, *loaded),
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
                    std::sync::Arc::make_mut(&mut s.view.selected).extend(rids);
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
                Ok(Ok(df)) => df.columns()[0]
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

    /// First window is up: read the source into RAM behind the user's back.
    /// Undo snapshots keep their scan plans; they stay correct, just slower.
    fn load_into_memory(&mut self) {
        let s = self.sheets.active_mut();
        if std::mem::replace(&mut s.state.load_tried, true) {
            return;
        }
        if let Some(plan) = s.memory_plan() {
            self.spawn_job("load into memory", move || {
                let df = plan.collect().map_err(|e| e.to_string())?;
                Ok(JobDone::Loaded(df))
            });
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
                // Polars 0.44 returns every scan column when only the row index
                // is selected, i.e. when all columns are hidden.
                let df = df.drop_many(s.view.hidden.iter().map(String::as_str));
                s.state.window_rids = rids;
                s.window = Some(df);
                if sheet_id == self.sheets.active().id {
                    self.settle_jump_bottom();
                    self.load_into_memory();
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
        // Count prefix: digits accumulate when no chord is pending and the digit
        // isn't bound. `0` is bound to goto_leftmost, so it only extends a count.
        if self.pending.is_empty()
            && let KeyCode::Char(c) = key.code
            && c.is_ascii_digit()
        {
            let d = c.to_digit(10).unwrap() as usize;
            // A bound digit fires (`0`, `vd.bind`) unless a count is underway.
            let unbound = matches!(self.keymap.resolve(Layer::Sheet, &[], key), Dispatch::Miss);
            let accumulate = unbound || self.pending_count.is_some();
            if accumulate {
                let cur = self.pending_count.unwrap_or(0);
                self.pending_count = Some(cur.saturating_mul(10).saturating_add(d));
                return;
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
        self.toast = None;
        if *prompt == Prompt::Ex && key.code == KeyCode::Tab {
            let lua = self.plugins.as_ref().map(|p| p.command_names());
            self.toast = complete_command(line, lua.unwrap_or_default());
            return;
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
                        other @ (Action::Lua(_) | Action::ExCommand(_)) => self.dispatch(other),
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
                self.dispatch_sequence(vec![Action::SearchNext]);
            }
            Prompt::Filter => {
                if let Some(column) = self.current_column_name() {
                    self.dispatch_sequence(vec![Action::SetFilter {
                        column,
                        regex: text,
                    }]);
                } else {
                    self.toast = Some("no column to filter".into());
                }
            }
            Prompt::Ex => {
                self.ex_history.push(&text);
                self.run_ex(&text);
            }
            Prompt::Select => self.dispatch_sequence(vec![Action::SelectByRegex { regex: text }]),
            Prompt::Rename => self.dispatch_sequence(vec![Action::RenameCol { new_name: text }]),
            Prompt::Diff => {
                let keys = self.sheets.active().view.keys.clone();
                self.dispatch_sequence(vec![Action::Diff { path: text, keys }]);
            }
        }
    }

    fn cancel_input(&mut self) {
        self.input = None;
        self.ex_history.reset_cursor();
    }

    /// Run an ex command: a built-in or named action, else a `vd.command`.
    fn run_ex(&mut self, text: &str) {
        match crate::exline::parse(text) {
            Ok(actions) => self.dispatch_sequence(actions),
            Err(crate::exline::ExError::Unknown(name)) => {
                match self.plugins.as_ref().and_then(|p| p.command(&name)) {
                    Some(id) => {
                        let line = text.trim().trim_start_matches(':').trim_start();
                        let args = line[name.len()..].trim().to_string();
                        self.dispatch(Action::LuaCommand { id, args });
                    }
                    None => self.toast = Some(format!("unknown command: :{name}")),
                }
            }
            Err(e) => self.toast = Some(format!("{e}")),
        }
    }

    /// A Lua callback (`vd.bind`, or `vd.command` with `args`): `vd.sheet()`
    /// describes the active sheet's cursor.
    fn run_lua(&mut self, id: usize, args: Option<&str>) {
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
                .and_then(|df| df.columns().get(v.col_cursor)?.get(v.row_cursor).ok())
                .unwrap_or(AnyValue::Null),
        };
        let out = match p.call(id, &ctx, args) {
            Ok(out) => out,
            Err(e) => {
                self.toast = Some(format!("{e}"));
                return;
            }
        };
        let parent = s.id;
        for (name, df) in out.sheets {
            let id = self.sheets.alloc_id();
            let origin = Origin::Lua { parent };
            let child = Sheet::child(self.sheets.active(), id, name, df.lazy(), origin);
            self.sheets.push(child);
            self.spawn_materialize();
        }
        if !out.toasts.is_empty() {
            self.toast = Some(out.toasts.join(" · "));
        }
    }

    /// Column under the cursor. While a new window is on its way the loaded one
    /// is out of date (a key hit right after `-`), so ask the plan instead.
    fn current_column_name(&self) -> Option<String> {
        let s = self.sheets.active();
        let names: Vec<String> = match &s.window {
            Some(df) if s.state.loaded_rev == s.revision => df
                .get_column_names()
                .iter()
                .map(|n| n.to_string())
                .collect(),
            _ => s
                .view_plan()
                .collect_schema()
                .ok()?
                .iter_names()
                .map(|n| n.to_string())
                .collect(),
        };
        names
            .get(s.view.col_cursor.min(names.len().saturating_sub(1)))
            .cloned()
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
    for cell in lists.series_iter() {
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
    let long = DataFrame::new_infer_height(vec![
        groups.column(index).map_err(|e| e.to_string())?.clone(),
        groups.column(on).map_err(|e| e.to_string())?.clone(),
        agg_col,
    ])
    .map_err(|e| e.to_string())?;
    crate::sheet::pivot_df(&long, on, &[index.to_string()], values, element().first())
        .map_err(|e| e.to_string())
}

/// Tab in the `:` prompt: complete the command name under the cursor, vim
/// style. One match → the name and a space; several → their common prefix,
/// with the candidates as the returned toast. Arguments aren't completed.
fn complete_command(line: &mut LineEdit, lua: Vec<String>) -> Option<String> {
    let (before, after) = line.split();
    if !after.is_empty() || before.contains(char::is_whitespace) {
        return None;
    }
    let mut names: Vec<String> = crate::exline::command_names()
        .into_iter()
        .chain(lua)
        .filter(|n| n.starts_with(before))
        .collect();
    names.sort();
    names.dedup();
    match names.as_slice() {
        [] => Some(format!("no command starts with {before:?}")),
        [one] => {
            line.set(&format!("{one} "));
            None
        }
        many => {
            let first = &many[0];
            let common = many.iter().fold(first.len(), |n, s| {
                first
                    .bytes()
                    .zip(s.bytes())
                    .take(n)
                    .take_while(|(a, b)| a == b)
                    .count()
            });
            line.set(&first[..common]);
            const SHOWN: usize = 12;
            let more = if many.len() > SHOWN { " …" } else { "" };
            Some(many[..many.len().min(SHOWN)].join(" ") + more)
        }
    }
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

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPIN_AFTER_MS: u128 = 150;

/// `1234567` as `1,234,567`.
fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Exactly `width` display cells of `s`: cut at the end, or padded.
fn fit(s: &str, width: usize) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut used = 0;
    let mut out: String = s
        .chars()
        .take_while(|c| {
            used += c.width().unwrap_or(0);
            used <= width
        })
        .collect();
    let pad = width.saturating_sub(out.width());
    out.extend(std::iter::repeat_n(' ', pad));
    out
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
mod tests;

#[cfg(test)]
pub(crate) mod scenario;
