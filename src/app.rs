//! App loop. tokio::select! over crossterm events + materialize task results. See S-02.

use std::collections::HashMap;
use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, Event as CtEvent, EventStream, KeyCode,
    KeyEventKind,
};
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

use crate::action::{Action, SidebarPage};
use crate::key::Key;
use crate::keymap::{Chord, Dispatch, Keymap, Layer};
use crate::lineedit::LineEdit;
use crate::sheet::{
    FilterState, Origin, SearchState, Sheet, SheetId, Sheets, SortKey, SortOrder, View,
};
use crate::theme::Theme;
use crate::widget::{SheetWidget, clamp_view, refresh_widths};

mod actions;
mod columns;
#[cfg(test)]
mod golden;
mod hooks;
mod input;
mod jobs;
#[cfg(test)]
mod monkey;
mod render;

/// Data rows assumed visible before the first draw measures the real area.
const DEFAULT_VIEWPORT_ROWS: usize = 20;

/// Entry point from main.
/// `session`: a `.vdrs` file `initial` is the first sheet of (`vdrs -S`).
pub async fn run(
    initial: Sheet,
    plugins: Option<crate::plugins::Plugins>,
    plugin_warning: Option<String>,
    session: Option<String>,
) -> Result<()> {
    install_panic_hook();

    enable_raw_mode().context("no usable terminal (try --headless)")?;
    let mut stdout = io::stdout();
    crossterm::execute!(stdout, EnterAlternateScreen, EnableBracketedPaste)?;
    push_key_flags();
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(initial, plugins, plugin_warning);
    if app.plugins.as_ref().is_some_and(|p| p.mouse()) {
        crossterm::execute!(io::stdout(), crossterm::event::EnableMouseCapture)?;
    }
    app.color = crate::theme::ColorDepth::from_env(|k| std::env::var(k).ok());
    if let Some(s) = session {
        app.source(&s, true);
    }
    let result = app.run(&mut terminal).await;

    pop_key_flags();
    disable_raw_mode().ok();
    crossterm::execute!(
        io::stdout(),
        crossterm::event::DisableMouseCapture,
        DisableBracketedPaste,
        LeaveAlternateScreen
    )
    .ok();
    terminal.show_cursor().ok();
    result
}

/// Kitty keyboard protocol, disambiguate only (S-26 A6): `Esc` comes at once,
/// `<C-i>` ≠ `<Tab>`, `<C-h>` ≠ `<BS>`. Terminals without it ignore the
/// sequence (no support query: it would stall on terminals that never answer).
fn push_key_flags() {
    use crossterm::event::{KeyboardEnhancementFlags, PushKeyboardEnhancementFlags};
    let flags = KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
    crossterm::execute!(io::stdout(), PushKeyboardEnhancementFlags(flags)).ok();
}

fn pop_key_flags() {
    crossterm::execute!(io::stdout(), crossterm::event::PopKeyboardEnhancementFlags).ok();
}

/// `<C-z>`: restore the terminal, stop until `fg` (SIGCONT), take it back.
fn suspend<B: ratatui::backend::Backend>(term: &mut Terminal<B>, mouse: bool) -> Result<()>
where
    B::Error: Send + Sync + 'static,
{
    #[cfg(unix)]
    {
        pop_key_flags();
        disable_raw_mode()?;
        crossterm::execute!(
            io::stdout(),
            crossterm::event::DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        )?;
        // the stop lands before `kill` is reaped; `fg` continues from here
        let stopped = std::process::Command::new("kill")
            .args(["-TSTP", &std::process::id().to_string()])
            .status();
        enable_raw_mode()?;
        crossterm::execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            crossterm::terminal::Clear(crossterm::terminal::ClearType::All)
        )?;
        push_key_flags();
        if mouse {
            crossterm::execute!(io::stdout(), crossterm::event::EnableMouseCapture)?;
        }
        // `term.clear()` would ask the terminal for the cursor, and the
        // event stream eats the reply: reset both buffers for a full redraw
        term.swap_buffers();
        term.swap_buffers();
        stopped.context("kill -TSTP")?;
    }
    #[cfg(not(unix))]
    let _ = (term, mouse);
    Ok(())
}

fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        pop_key_flags();
        let _ = disable_raw_mode();
        let _ = crossterm::execute!(
            io::stdout(),
            crossterm::event::DisableMouseCapture,
            DisableBracketedPaste,
            LeaveAlternateScreen
        );
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
        id: u64,
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
    s.set_window(df);
    s
}

/// The internal clipboard (S-18 §2.6): one per app, so a copy on one sheet
/// pastes on another. Rows keep every column (hidden too), typed.
#[derive(Debug, Clone)]
pub(crate) struct Register {
    pub kind: RegisterKind,
    pub df: DataFrame,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegisterKind {
    Rows,
    Cells,
}

/// A background job in flight.
struct Job {
    id: u64,
    sheet_id: SheetId,
    label: &'static str,
    /// Drops the job's result; the blocking work itself can't be interrupted.
    abort: tokio::task::AbortHandle,
}

/// The UI-thread half of an edit whose scan ran in a job (S-33).
type EditApply = Box<dyn FnOnce(&mut App) + Send>;

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
    /// Fill the register (`y` …): the named one, and whether it's a yank.
    Copy(Box<Register>, (Option<char>, bool)),
    /// `zP` `gzP`: the clipboard's text, read by the paste command.
    SysPaste {
        text: String,
        selected: bool,
    },
    /// Text sent to the system clipboard (`Y` …); OSC 52 bytes still to write.
    SysCopy {
        note: String,
        osc52: Option<String>,
    },
    /// Remove row ids from the job's sheet selection.
    Unselect {
        rids: Vec<u64>,
        note: String,
    },
    Toast(String),
    /// `:exec-sql` done: the file's fresh index, and the last query's rows.
    ExecSql {
        db: std::path::PathBuf,
        index: DataFrame,
        rows: Option<Box<Sheet>>,
    },
    /// `:save` done: these edits of the sheet are on disk.
    Saved {
        note: String,
        edits: Vec<crate::sheet::PlanStep>,
        path: String,
        rows: usize,
    },
    /// Selection stats for the status line (S-30 C).
    SelStats {
        key: StatsKey,
        stats: Vec<(&'static str, String)>,
    },
    /// A new sheet, and something to say about it.
    PushSheetToast(Box<Sheet>, String),
    /// A plot binned again (zoom): its rows, and whether its view goes too (`_`).
    Rebinned(Box<Sheet>, bool),
    /// A plot's `s` / `u` (S-23 C4): rows of sheet `sheet` to (un)select.
    SelectIn {
        sheet: SheetId,
        select: bool,
        rids: Vec<u64>,
    },
    /// A ranged ex command's rows (S-26 B): run `cmd` with them selected.
    Ranged {
        rids: Vec<u64>,
        cmd: String,
    },
    /// `:scale` / `:databar` with its column's range (S-27 B).
    Scale(crate::sheet::ScaleRule),
    /// `:refresh`: sheets to replace by id (S-30 A).
    Refreshed(crate::sheet::Refreshed),
    /// The job's source, read into memory.
    Loaded(DataFrame),
    /// A tail that can't append, reopened.
    TailSource(Box<crate::reader::LoadedSource>),
    /// The sidebar's column stats, for the key they were computed for.
    SidebarStats {
        key: String,
        stats: Result<DataFrame, String>,
    },
    /// A scan for an edit (S-33): `apply` runs if the sheet still matches `stamp`.
    Edit {
        stamp: crate::sheet::ScanStamp,
        apply: EditApply,
    },
    /// `:pivot` preview for the ex-line text it was computed from.
    PivotPreview {
        text: String,
        result: Result<Box<Sheet>, String>,
    },
}

/// Which prompt the input line belongs to; decides what `<CR>` does with the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Prompt {
    /// `/` `?` `g/` `g?`.
    Search {
        all_columns: bool,
        backward: bool,
    },
    /// `z/` `z?`.
    SearchExpr {
        backward: bool,
    },
    /// `=`.
    Derive,
    /// `z|` `z\`.
    SelectExpr {
        unselect: bool,
    },
    Filter,
    Ex,
    Select,
    Rename,
    Diff,
    /// `e`, prefilled with the cell.
    Edit,
    /// `ge`.
    SetCol,
    /// `g=`.
    SetExpr,
    /// `ga`: how many rows.
    AddRows,
    /// `m` again (S-19): which key the recorded macro goes on.
    MacroKey,
    /// `zn` (S-30 B): the cursor cell's note.
    Note,
    /// `za` (S-31).
    AddCol,
    /// `zH` / `zL` (S-34): how many places to slide.
    Slide {
        left: bool,
    },
    /// `;` (S-31).
    Capture,
    /// `*` `g*` `gz*` (S-28): `search/replace`.
    Subst {
        mode: crate::action::SubstMode,
    },
    /// `z(` `gz(` (`all`): how many levels.
    ExpandDepth {
        all: bool,
    },
    /// `+` `z+` `g+`: aggregator names.
    Aggregate {
        memo: bool,
        selected: bool,
    },
}

/// A visual range: from the anchor to the cursor (view rows from 0, and
/// columns for a block).
// ponytail: view rows, not row ids: a sort while it's open moves the range
// with the positions, not the rows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Visual {
    sheet: SheetId,
    row: u64,
    col: usize,
    /// The other corner; the live range takes the cursor instead.
    end: (u64, usize),
    block: bool,
}

impl Visual {
    fn rows(&self) -> (u64, u64) {
        (self.row.min(self.end.0), self.row.max(self.end.0))
    }
    fn cols(&self) -> Option<(usize, usize)> {
        self.block
            .then(|| (self.col.min(self.end.1), self.col.max(self.end.1)))
    }
}

/// An operator waiting for its motion: `op`, the counts typed before it and
/// before the motion, and whether `i` (a text object) came.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Pending {
    op: char,
    count: Option<usize>,
    motion_count: Option<usize>,
    object: bool,
}

/// A jumplist place: sheet, view row (from 0), column.
#[derive(Debug, Clone, PartialEq)]
struct Jump {
    sheet: SheetId,
    row: u64,
    column: Option<String>,
}

/// A y/N question asked in the status line; any other key keeps things as they are.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Confirm {
    Pop,
    Quit,
}

impl Prompt {
    fn label(self) -> &'static str {
        match self {
            Prompt::Search {
                all_columns,
                backward,
            } => match (all_columns, backward) {
                (false, false) => "/",
                (false, true) => "?",
                (true, false) => "g/",
                (true, true) => "g?",
            },
            Prompt::SearchExpr { backward: false } => "search by expr: ",
            Prompt::SearchExpr { backward: true } => "searchr by expr: ",
            Prompt::Derive => "new column expr= ",
            Prompt::SelectExpr { unselect: false } => "select by expr: ",
            Prompt::SelectExpr { unselect: true } => "unselect by expr: ",
            Prompt::Filter => "filter: ",
            Prompt::Ex => ":",
            Prompt::Select => "select: ",
            Prompt::Rename => "rename: ",
            Prompt::Diff => "diff with: ",
            Prompt::Edit => "edit: ",
            Prompt::SetCol => "set selected rows to: ",
            Prompt::SetExpr => "set selected rows to expr= ",
            Prompt::AddRows => "add rows: ",
            Prompt::ExpandDepth { .. } => "expand depth (0 = all the way): ",
            Prompt::Aggregate { .. } => "choose aggregators: ",
            Prompt::MacroKey => "macro key (e.g. <F5> or g x; empty discards): ",
            Prompt::Subst { .. } => "search/replace: ",
            Prompt::Note => "note (empty deletes): ",
            Prompt::AddCol => "column name: ",
            Prompt::Slide { left: true } => "slide col left n=",
            Prompt::Slide { left: false } => "slide col right n=",
            Prompt::Capture => "capture regex: ",
        }
    }
}

/// `Z` / `:vsplit` (S-23 B): the pane not focused shows `other`; the focused
/// one shows the active sheet, so every command acts on it unchanged.
struct Split {
    other: SheetId,
    /// Side by side (`:vsplit`), else top and bottom.
    vertical: bool,
    /// The focused pane is the top (left) one.
    focus_first: bool,
}

/// What selection stats were computed for: sheet, selection, column, sheet
/// revision. Holding the selection's `Arc` makes the next `make_mut` copy it,
/// so a changed selection is always a different `Arc`.
// ponytail: a toggle copies the whole set once while stats are held; add a
// selection generation counter if that shows on huge selections.
#[derive(Clone, Debug)]
struct StatsKey(SheetId, Arc<std::collections::BTreeSet<u64>>, String, u64);

impl PartialEq for StatsKey {
    fn eq(&self, o: &Self) -> bool {
        self.0 == o.0 && Arc::ptr_eq(&self.1, &o.1) && self.2 == o.2 && self.3 == o.3
    }
}

/// A macro being recorded.
#[derive(Default)]
struct Recording {
    steps: Vec<String>,
    refused: Vec<String>,
    /// `q{x}` (S-26 D4): stops into register `x` instead of asking for a key.
    into: Option<char>,
}

/// What the next key names (vim profile, S-26 D2/D4).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Await {
    Register,
    SetMark,
    GoMark,
    Record,
    Play,
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
    /// While `m` records (S-19): each step as a `vd.bind` list item, and
    /// what couldn't be recorded.
    recording: Option<Recording>,
    /// Macro steps still to play; each waits for the previous one's jobs.
    /// `true`: a `vd.on` handler's `vd.exec`, whose new sheets fire no events.
    macro_queue: std::collections::VecDeque<(Action, bool)>,
    /// A replayed step failed: `pump_macro` drops the steps after it.
    failed: bool,
    /// `vd.on` (S-29): the newest sheet `open` / `push` fired for, and the
    /// ids of sheets hook commands made (they fire nothing: no loops).
    hooked: Option<SheetId>,
    hook_born: Vec<std::ops::Range<SheetId>>,
    /// Status-line stats of the selection (S-30 C), and what was last asked.
    sel_stats: Option<(StatsKey, Vec<(&'static str, String)>)>,
    sel_stats_asked: Option<StatsKey>,
    /// `Z` (S-23 B): the second pane, if split.
    split: Option<Split>,
    /// Cell notes by source file, read when first shown (S-30 B).
    notes: std::collections::HashMap<std::path::PathBuf, crate::notes::Notes>,
    /// The active sheet `follow_link` last looked at (S-30 A).
    link_seen: Option<SheetId>,
    /// A macro step is dispatching: its actions are neither recorded nor `.`.
    replaying: bool,
    toast: Option<String>,
    input: Option<(Prompt, LineEdit)>,
    ex_history: crate::exline::History,
    /// The open sidebar page (S-23 A); `sidebar_page` is the one `b` reopens.
    sidebar: Option<SidebarPage>,
    sidebar_page: SidebarPage,
    /// The column page's stats: (what they were computed for, one `I` row).
    sidebar_stats: Option<(String, Result<DataFrame, String>)>,
    /// While typing `:pivot …`: hints, then a pivot of the first rows.
    pivot_preview: Option<PivotPreview>,
    theme: Theme,
    /// What the terminal shows (S-26 A4); tests: true color.
    color: crate::theme::ColorDepth,
    /// Data rows visible in the sheet area at the last draw; one PageDown step.
    viewport_rows: usize,
    /// Columns of the table area: a plot bins x to twice this (braille).
    viewport_cols: u16,
    /// `V` / `<C-v>` in progress (S-26 C), and the last one (`'<,'>`).
    visual: Option<Visual>,
    last_visual: Option<Visual>,
    /// Places before big jumps (S-26 A7); `jump_at`: where `jump_back` is.
    jumps: Vec<Jump>,
    jump_at: Option<usize>,
    /// Where the active sheet was last drawn (mouse clicks).
    sheet_area: ratatui::layout::Rect,
    register: Option<Register>,
    /// `"0` last yank, `"1`–`"9` last cuts, `"a`–`"z` named (S-26 D2); the
    /// unnamed `""` is `register`.
    registers: std::collections::BTreeMap<char, Register>,
    /// `d` `y` `c` waiting for a motion (vim profile, S-26 D3).
    operator: Option<Pending>,
    /// `"x` typed: the next yank / cut / paste uses register `x`.
    register_prefix: Option<char>,
    /// `"` `m` `'` `q` `@` typed, the key naming the register / mark not yet.
    awaiting: Option<Await>,
    /// `m{a-z}` marks: view positions, like the jumplist (S-26 D4).
    marks: std::collections::BTreeMap<char, Jump>,
    /// `q{a-z}` macros: ex steps, this session only; `last_macro` for `@@`.
    macros: std::collections::BTreeMap<char, Vec<String>>,
    last_macro: Option<char>,
    /// Asked in the status line: unsaved edits would be lost (S-26 A1).
    confirm: Option<(Confirm, String)>,
    /// Where `Y` … send text; from `vd.option("clipboard_copy_cmd")` or the platform.
    clipboard: crate::clipboard::Clipboard,
    /// What `zP` `gzP` run to read the clipboard; `None`: use `pasted`.
    paste_cmd: Option<Vec<String>>,
    /// The terminal's last paste (bracketed paste) into the table.
    pasted: Option<String>,
    /// Background jobs in flight; labels shown in the status bar, `<C-c>` aborts.
    busy: Vec<Job>,
    /// Id of the last spawned job.
    next_job: u64,
    /// When the current stretch of background work started.
    working_since: Option<Instant>,
    /// Spinner frame on screen, so ticks redraw only when it changes.
    drawn_spin: Option<usize>,
    quit: bool,
    /// `<C-z>`: the run loop hands the terminal back and stops the process.
    suspend: bool,
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
        let clipboard = clipboard_for(plugins.as_ref());
        let paste_cmd = paste_cmd_for(plugins.as_ref());
        Self {
            plugins,
            sheets: Sheets::new(sheet),
            keymap,
            pending: Vec::new(),
            pending_candidates: Vec::new(),
            pending_count: None,
            last_action: None,
            recording: None,
            macro_queue: Default::default(),
            failed: false,
            hooked: None,
            hook_born: Vec::new(),
            sel_stats: None,
            sel_stats_asked: None,
            link_seen: None,
            notes: Default::default(),
            split: None,
            replaying: false,
            toast: plugin_warning,
            input: None,
            ex_history: crate::exline::History::load(),
            sidebar: None, // L1: starts closed, as VisiData
            sidebar_page: SidebarPage::Column,
            sidebar_stats: None,
            pivot_preview: None,
            theme,
            viewport_rows: DEFAULT_VIEWPORT_ROWS,
            viewport_cols: 100,
            sheet_area: ratatui::layout::Rect::default(),
            jumps: Vec::new(),
            jump_at: None,
            visual: None,
            last_visual: None,
            register: None,
            registers: Default::default(),
            register_prefix: None,
            operator: None,
            awaiting: None,
            marks: Default::default(),
            macros: Default::default(),
            last_macro: None,
            confirm: None,
            clipboard,
            paste_cmd,
            pasted: None,
            busy: Vec::new(),
            next_job: 0,
            working_since: None,
            drawn_spin: None,
            quit: false,
            suspend: false,
            color: crate::theme::ColorDepth::True,
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
                    Some(Ok(CtEvent::Mouse(m))) => {
                        self.on_mouse(m);
                        dirty = true;
                    }
                    Some(Ok(CtEvent::Paste(text))) => {
                        self.on_paste(text);
                        dirty = true;
                    }
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
            dirty |= self.fire_push_hooks();
            self.ask_sel_stats();
            self.follow_link();
            self.feed_other_pane();
            if !self.macro_queue.is_empty() && self.idle() {
                self.pump_macro();
                dirty = true;
            }
            if std::mem::take(&mut self.suspend) {
                let mouse = self.plugins.as_ref().is_some_and(|p| p.mouse());
                if let Err(e) = suspend(term, mouse) {
                    self.toast = Some(format!("suspend: {e}"));
                }
                dirty = true;
            }
            if self.quit {
                break;
            }
        }
        self.fire_event("quit", &[]);
        if self.plugins.as_ref().is_none_or(|p| p.session_autosave()) {
            // best effort: a failed save mustn't keep vdrs from exiting
            let _ = crate::session::save(self.sheets.iter(), &crate::session::default_path());
        }
        Ok(())
    }

    /// No background work outstanding for the active sheet (tests wait on
    /// this); a macro waiting on a prompt counts as settled.
    #[cfg(test)]
    fn settled(&self) -> bool {
        let waiting = self.input.is_some() || self.confirm.is_some();
        let other = self.split.as_ref().map(|sp| sp.other);
        let other_loaded = other
            .and_then(|id| self.sheets.iter().find(|s| s.id == id))
            .is_none_or(Sheet::is_loaded);
        other_loaded
            && self.idle()
            && !self.sheets.active().is_counting()
            && (self.macro_queue.is_empty() || waiting)
    }

    /// The active sheet's window is current and no job runs: the next macro
    /// step may go.
    fn idle(&self) -> bool {
        self.sheets.active().is_loaded() && self.busy.is_empty()
    }

    /// Play queued macro steps while nothing is in flight; a step that opens
    /// a prompt or a question pauses playback until it's answered.
    fn pump_macro(&mut self) {
        while self.idle() && self.input.is_none() && self.confirm.is_none() {
            if std::mem::take(&mut self.failed) {
                let n = self.macro_queue.len();
                self.macro_queue.clear();
                let why = self.toast.take().unwrap_or_default();
                self.toast = Some(format!("{why}; stopped, {n} steps not run"));
                return;
            }
            let Some((a, hook)) = self.macro_queue.pop_front() else {
                return;
            };
            let lo = self.sheets.next_id();
            self.replaying = true;
            self.dispatch(a);
            self.replaying = false;
            if hook && self.sheets.next_id() > lo {
                self.hook_born.push(lo..self.sheets.next_id());
            }
        }
    }

    /// An error toast; during a replay it stops the steps after this one.
    pub(super) fn fail(&mut self, msg: String) {
        self.failed |= self.replaying || !self.macro_queue.is_empty();
        self.toast = Some(msg);
    }

    /// The stats key for what's on screen; `None`: nothing selected.
    fn sel_stats_key(&self) -> Option<StatsKey> {
        let s = self.sheets.active();
        if s.view.selected.is_empty() {
            return None;
        }
        Some(StatsKey(
            s.id,
            s.view.selected.clone(),
            self.current_column_name()?,
            s.revision(),
        ))
    }

    /// The selection's stats for the status line, once per selection and
    /// column, in the background.
    // ponytail: no debounce; each `s` on a huge selection starts a job and
    // the stale results are dropped. Debounce if it shows in a profile.
    fn ask_sel_stats(&mut self) {
        let Some(key) = self.sel_stats_key() else {
            return;
        };
        let have = self.sel_stats.as_ref().map(|(k, _)| k);
        if have == Some(&key) || self.sel_stats_asked.as_ref() == Some(&key) {
            return;
        }
        self.sel_stats_asked = Some(key.clone());
        let sheet = self.sheets.active().clone();
        self.spawn_job("stats", move || {
            let stats = sheet.selection_stats(&key.2).map_err(|e| e.to_string())?;
            Ok(JobDone::SelStats { key, stats })
        });
    }

    /// The file a note on the active sheet goes to: sheets read from a file only.
    fn notes_path(&self) -> Option<std::path::PathBuf> {
        let s = self.sheets.active();
        s.source
            .path
            .clone()
            .filter(|_| matches!(s.origin, Origin::File))
    }

    /// The active sheet's notes, read on first use.
    fn notes_mut(&mut self) -> Option<(std::path::PathBuf, &mut crate::notes::Notes)> {
        let path = self.notes_path()?;
        let n = self
            .notes
            .entry(path.clone())
            .or_insert_with(|| crate::notes::Notes::load(&path));
        Some((path, n))
    }

    /// The cursor cell's row (by keys when there are key columns) and column.
    fn note_cell(&self) -> Option<(crate::notes::Row, String)> {
        let s = self.sheets.active();
        let column = self.current_column_name()?;
        let r = s.view.row_cursor;
        let row = match s.view.keys.is_empty() {
            true => crate::notes::Row::Id(*s.window_rids().get(r)?),
            false => crate::notes::Row::Keys(crate::sheet::key_text(s.window()?, &s.view.keys, r)),
        };
        Some((row, column))
    }

    /// The note on the cursor cell.
    fn cursor_note(&self) -> Option<&crate::notes::Note> {
        let notes = self.notes.get(&self.notes_path()?)?;
        let s = self.sheets.active();
        let rid = s.window_rids().get(s.view.row_cursor).copied();
        let keys = (!s.view.keys.is_empty())
            .then(|| {
                Some(crate::sheet::key_text(
                    s.window()?,
                    &s.view.keys,
                    s.view.row_cursor,
                ))
            })
            .flatten();
        notes.get(rid, keys.as_deref(), &self.current_column_name()?)
    }

    /// `.` / `g.`: x is the first key column holding numbers or dates (VisiData's rule).
    fn plot_keys(&mut self, all: bool) {
        let s = self.sheets.active();
        let Some(df) = s.window() else { return };
        let plottable = |c: &str| {
            df.column(c).is_ok_and(|c| {
                let t = c.dtype();
                t.is_primitive_numeric() || matches!(t, DataType::Date | DataType::Datetime(..))
            })
        };
        let x = match s.view.keys.iter().find(|k| plottable(k)) {
            Some(x) => x.clone(),
            None if s.view.keys.is_empty() => {
                self.toast =
                    Some("no key columns; press `!` on a numeric column to set x-axis".into());
                return;
            }
            None => {
                self.toast = Some("no numeric key column; set type with `#` `%` or `@`".into());
                return;
            }
        };
        let ys: Vec<String> = match all {
            false => self.current_column_name().into_iter().collect(),
            true => df
                .columns()
                .iter()
                .filter(|c| {
                    c.dtype().is_primitive_numeric() && !s.view.keys.contains(&c.name().to_string())
                })
                .map(|c| c.name().to_string())
                .collect(),
        };
        self.plot(x, ys);
    }

    /// Keys on a plot sheet (S-23 C3/C4, VisiData's canvas): `hjkl` move the
    /// cursor box by its size, `HJKL` resize it, `+` `-` zoom around it, `_`
    /// fits, `zz` zooms to it; `s` `u` (un)select the parent's rows in it,
    /// `Enter` opens them. `false`: not a plot key.
    fn plot_dispatch(&mut self, a: &Action) -> bool {
        let (cols, rows) = (
            f64::from(self.viewport_cols.max(1)),
            self.viewport_rows.max(1) as f64,
        );
        let bins = u32::from(self.viewport_cols.max(10)) * 2;
        let s = self.sheets.active_mut();
        let id = s.id;
        let (Origin::Plot { parent, x, ys }, Some(v)) = (s.origin.clone(), s.state.plot.as_mut())
        else {
            return false;
        };
        let n = |c: &Option<usize>| c.unwrap_or(1) as f64;
        let mut range = None; // Some: bin again, `None` inside: fit all
        match a {
            Action::CursorLeft { count } => v.step(-n(count), 0.0),
            Action::CursorRight { count } => v.step(n(count), 0.0),
            Action::CursorUp { count } => v.step(0.0, n(count)),
            Action::CursorDown { count } => v.step(0.0, -n(count)),
            Action::PlotResize { .. } | Action::SlideCol { by: -1 | 1 } => {
                let (wider, taller) = match a {
                    Action::PlotResize { wider, taller } => (*wider, *taller),
                    Action::SlideCol { by } => (*by as i8, 0), // H L, as VisiData's canvas
                    _ => return false,
                };
                let (cw, ch) = ((v.x[1] - v.x[0]) / cols, (v.y[1] - v.y[0]) / rows);
                let c = &mut v.cursor;
                c[2] = (c[2] + f64::from(wider) * cw).max(cw);
                c[3] = (c[3] + f64::from(taller) * ch).max(ch);
            }
            Action::StartAggregateInput { .. } => {
                v.zoom(0.5);
                range = Some(Some(v.x));
            }
            Action::HideCol => {
                v.zoom(2.0);
                range = Some(Some(v.x));
            }
            // `zz` scrolls a sheet; on a plot it zooms (VisiData Canvas)
            Action::ZoomCursor | Action::ScrollTo { at: 1 } => {
                v.zoom_to_cursor();
                range = Some(Some(v.x));
            }
            Action::ZoomAll => range = Some(None),
            Action::ToggleSelectRow | Action::UnselectRow | Action::SubmitRow => {
                let cursor = v.cursor;
                let Some(p) = self.sheets.iter().find(|p| p.id == parent).cloned() else {
                    self.toast = Some("plot: its sheet was closed".into());
                    return true;
                };
                let a = a.clone();
                let new = self.sheets.alloc_id();
                self.spawn_job("plot", move || {
                    let rids = Sheet::rids_in_box(&p, &x, &ys, cursor)?;
                    Ok(match a {
                        Action::SubmitRow => {
                            JobDone::PushSheet(Box::new(Sheet::box_from(&p, new, rids)?))
                        }
                        a => JobDone::SelectIn {
                            sheet: p.id,
                            select: matches!(a, Action::ToggleSelectRow),
                            rids,
                        },
                    })
                });
                return true;
            }
            _ => return false,
        }
        if let Some(range) = range {
            let snapshot = (*v.parent).clone();
            let p = self.sheets.iter().find(|p| p.id == parent).cloned();
            let p = p.unwrap_or(snapshot);
            self.spawn_job("plot", move || {
                let fresh = Sheet::plot_from(&p, id, &x, &ys, bins, range)?;
                Ok(JobDone::Rebinned(Box::new(fresh), range.is_none()))
            });
        }
        true
    }

    /// `:plot`: bin the view in the background, push the plot sheet.
    fn plot(&mut self, x: String, ys: Vec<String>) {
        let parent = self.sheets.active().clone();
        let id = self.sheets.alloc_id();
        let bins = u32::from(self.viewport_cols.max(10)) * 2;
        self.spawn_job("plot", move || {
            Ok(JobDone::PushSheet(Box::new(Sheet::plot_from(
                &parent, id, &x, &ys, bins, None,
            )?)))
        });
    }

    /// `:refresh` the active sheet (and its changed ancestors) in the background.
    fn refresh(&mut self) {
        let id = self.sheets.active().id;
        let stack: Vec<Sheet> = self.sheets.iter().cloned().collect();
        self.spawn_job("refresh", move || {
            crate::sheet::refresh(id, &stack).map(JobDone::Refreshed)
        });
    }

    /// A linked sheet that just became active catches up with its parent.
    fn follow_link(&mut self) {
        let s = self.sheets.active();
        if self.link_seen == Some(s.id) {
            return;
        }
        self.link_seen = Some(s.id);
        if !s.state.linked {
            return;
        }
        let stack: Vec<Sheet> = self.sheets.iter().cloned().collect();
        if s.parent_changed(&stack) {
            self.refresh();
        }
    }

    /// The stats on screen now, if they're up to date.
    fn current_sel_stats(&self) -> Option<&[(&'static str, String)]> {
        let key = self.sel_stats_key()?;
        self.sel_stats
            .as_ref()
            .filter(|(k, _)| *k == key)
            .map(|(_, s)| s.as_slice())
    }

    /// The column page's stats for the cursor column, recomputed in the
    /// background when the column or the sheet's data changed.
    fn refresh_sidebar_stats(&mut self) {
        if self.sidebar != Some(SidebarPage::Column) {
            return;
        }
        let Some(column) = self.current_column_name() else {
            return;
        };
        let s = self.sheets.active();
        // ponytail: edits/undo show up as step or undo counts; a cheap proxy
        // for "the data changed", recomputes on selection changes too.
        let key = format!(
            "{}|{column}|{}|{}|{}",
            s.id,
            s.count_key(),
            s.state.steps.len(),
            s.state.undo.current()
        );
        if self.sidebar_stats.as_ref().is_some_and(|(k, _)| *k == key) {
            return;
        }
        let mut one = s.clone();
        one.view.sort.clear();
        one.view.keys.clear();
        one.view.hidden = one
            .view_plan()
            .collect_schema()
            .map(|sc| {
                sc.iter_names()
                    .filter(|n| n.as_str() != column)
                    .map(|n| n.to_string())
                    .collect()
            })
            .unwrap_or_default();
        self.sidebar_stats = Some((key.clone(), Err("computing…".into())));
        self.spawn_job("stats", move || {
            let stats = Sheet::describe_from(&one, 0)
                .and_then(|d| d.plan.collect().map_err(|e| e.to_string()));
            Ok(JobDone::SidebarStats { key, stats })
        });
    }

    /// Column under the cursor. While a new window is on its way the loaded one
    /// is out of date (a key hit right after `-`), so ask the plan instead.
    fn current_column_name(&self) -> Option<String> {
        let s = self.sheets.active();
        let names: Vec<String> = match s.window() {
            Some(df) if s.is_loaded() => df
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

/// Default keymap plus Lua patches, and the theme `init.lua` picked.
fn keymap_and_theme(plugins: Option<&crate::plugins::Plugins>) -> (Keymap, Theme) {
    let mut keymap = Keymap::defaults();
    let mut theme = Theme::dark();
    if let Some(p) = plugins {
        p.apply_keymap(&mut keymap);
        theme = p.theme().unwrap_or(theme);
        theme.float_digits = p.float_digits();
        (theme.numeric_binning, theme.histogram_bins) = p.numeric_binning();
    }
    (keymap, theme)
}

/// Unit tests always get an in-memory sink: no test touches the real clipboard.
fn clipboard_for(plugins: Option<&crate::plugins::Plugins>) -> crate::clipboard::Clipboard {
    if cfg!(test) {
        #[cfg(test)]
        return crate::clipboard::Clipboard::Memory(Default::default());
    }
    let option = plugins.and_then(|p| p.clipboard_copy_cmd());
    crate::clipboard::Clipboard::pick(option.as_deref(), |k| std::env::var(k).ok())
}

/// Unit tests never read the real clipboard: they paste with the terminal.
fn paste_cmd_for(plugins: Option<&crate::plugins::Plugins>) -> Option<Vec<String>> {
    if cfg!(test) {
        return None;
    }
    let option = plugins.and_then(|p| p.clipboard_paste_cmd());
    crate::clipboard::paste_cmd(option.as_deref(), |k| std::env::var(k).ok())
}

const SPINNER: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const SPIN_AFTER_MS: u128 = 150;

use crate::sheet::grouped;

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

#[cfg(test)]
mod tests;

#[cfg(test)]
pub(crate) mod scenario;

/// Recorded ex steps as the actions they run (S-19).
fn macro_actions(steps: &[String]) -> Vec<Action> {
    steps
        .iter()
        .map(|s| match s.strip_prefix(':') {
            Some(cmd) => Action::ExCommand(cmd.to_string()),
            None => Action::from_name(s).unwrap_or(Action::Redraw),
        })
        .collect()
}
