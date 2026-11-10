# S-02 — App loop & Model/Update/View

Status: **draft** (unapproved)
Phase: 0
Depends on: S-01 (`Sheet` shape), direction §9.2 (heavy VisiData user), §9.3 (Polars only)
Research: [`../research/tui-patterns.md`](../research/tui-patterns.md) Q1 synthesis
Blocks: all Phase 0 coding (every reader/widget sits on this loop)

## Problem

A heavy VisiData user keeps multiple sheets open, pipes big CSVs, and expects keystroke-density UX. The loop must (a) stay responsive while Polars materializes a window, (b) never block the UI on I/O, (c) support a sheet stack like VisiData's `vd.sheets`, (d) kill a crashing sheet, not the app.

## Decision

**Television's shape, yazi's layering, csvlens's cancellation.** Single `tokio::select!` main loop over four channels: crossterm events, resolved actions, render ticks, materialization results. One `App` owns a `Sheets` stack. Each sheet mutation produces an `Action`; `Action::affects_data()` decides whether to spawn a re-materialize task. Modal UI (Help/Pick/Input/Confirm/Which) is z-stacked overlays, not separate windows.

## Shape

```rust
pub struct App {
    sheets: Sheets,              // sheet stack + active id
    overlay: Option<Overlay>,    // which-key, help, pick, input, confirm
    event_rx: mpsc::Receiver<Event>,
    action_rx: mpsc::Receiver<Action>,
    render_rx: mpsc::Receiver<RenderTick>,
    materialize_rx: mpsc::Receiver<MaterializeResult>,
    materialize_tx: mpsc::Sender<MaterializeResult>,
    last_action: Option<Action>, // for `.` repeat
    pending_count: Option<usize>,// for `3j` repeat
    dirty: bool,                 // flipped by every state change; cleared on draw
    quit: bool,
}

pub struct Sheets {
    stack: Vec<Sheet>,           // VisiData's `vd.sheets`; last = active
    materializing: HashMap<SheetId, JoinHandle<()>>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    // cosmetic — no re-collect
    CursorDown, CursorUp, ScrollLeft, ScrollRight, FreezeColumn, ToggleHelp,
    // data — triggers materialize
    SortBy(ColRef, SortOrder), FilterRow(FilterExpr), DeriveColumn { name: String, expr: String },
    // sheet-stack
    PushSheet(SheetId), PopSheet, SwitchSheet(SheetId),
    // meta
    Quit, Redraw,
}

impl Action {
    fn affects_data(&self) -> bool { /* exhaustive match */ }
}
```

**Main loop** (sketch):

```rust
loop {
    tokio::select! {
        Some(ev) = self.event_rx.recv() => self.handle_event(ev),
        Some(act) = self.action_rx.recv() => self.handle_action(act),
        Some(res) = self.materialize_rx.recv() => self.handle_materialize(res),
        _ = self.render_rx.recv(), if self.dirty => self.draw()?,
    }
    if self.quit { break }
}
```

**Big CSV handling.** `scan_csv` returns a `LazyFrame`; we never `.collect()` the whole file. Window materialization is `plan.slice(offset, WINDOW).collect()` on a tokio task. Scrolling within the window is cosmetic (no re-collect). Scrolling past it slides the window: new tokio task spawned, old handle cancelled via its `JoinHandle::abort()`, previous window stays rendered with a `~` status indicator until the new one arrives. `WINDOW = max(terminal_height * 4, 1024)`; headroom means micro-scrolls don't thrash.

**Multi-sheet.** `Sheets.stack` is a `Vec<Sheet>`, last = active. `q` pops (VisiData parity). A sheet switcher is a derived meta-sheet listing `stack` — same keymap as any other sheet. Dormant sheets keep their `plan`+`view` but drop `window` on memory pressure (deferred — don't implement until it bites).

**Error surfacing.** Three tiers:

- Fatal (initial open failure): sheet never pushes; caller shows a modal `Confirm` with the error.
- Non-fatal materialize error (bad derive expr, missing column): sheet stays, status-line toast, `window` keeps the previous good materialization.
- Panic in materialize task: `tokio::task::spawn` wrapped with `catch_unwind`; panic kills the sheet (replaced with an `ErrorSheet` carrying the backtrace), not the app.

**Rendering.** Dirty-flag + render tick (16 ms = 60 Hz ceiling, but we only draw when dirty). Resize events debounced 50 ms (television trick). Overlays rendered on top of active sheet in z-order: `Help > Confirm > Input > Pick > Which > Sheet`.

**Panic policy.** `app.rs::main()` installs a global panic hook that restores the terminal (crossterm leave-alt-screen, show-cursor, disable-raw-mode) before `std::process::abort()`. No "crash into unusable terminal" — single most-reported class of VisiData curses bugs.

**Dependency pins** (seed; verify when running `cargo add`):

```toml
ratatui = "0.30"
crossterm = { version = "0.29", features = ["event-stream"] }
tokio = { version = "1.41", features = ["rt-multi-thread", "macros", "sync", "signal", "time", "fs"] }
tokio-stream = "0.1"
signal-hook-tokio = { version = "0.3", features = ["futures-v0_3"] }
polars = { version = "0.44", features = ["lazy", "csv", "parquet", "json", "strings", "temporal"] }
serde = { version = "1", features = ["derive"] }
anyhow = "1"
thiserror = "2"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

mlua lands in Phase 3; not pinned now.

## Why not <alternative>

- **Sync `event::read()` loop (csvlens, gitui, bottom).** Simpler but blocks on Polars collection or forces OS threads. We need tokio for mlua coroutines anyway (Phase 3); introducing it in Phase 3 is a worse disruption than starting with it.
- **Actor model (zellij).** Overkill. Our subsystems don't justify independent event loops.
- **Fixed-tick redraw (60 Hz regardless).** Burns battery; VisiData's #2243 (idle CPU) is literally this. Dirty-flag is cheap.

## Open sub-decisions

- `WINDOW` size tuning: start at `max(terminal_height * 4, 1024)`; benchmark Phase 0 with hyperfine before locking.
- Dormant-sheet eviction policy: skip until it hurts.
- `tokio` runtime shape: multi-thread. Current-thread is a false economy — Polars needs threads to be fast and it manages its own thread pool anyway.
- Testing: `insta` snapshots over `ratatui::backend::TestBackend`. Keep render pure — `fn draw(&self, frame: &mut Frame)` with no I/O. Don't spec more until first test is written.
- Error type policy: `anyhow` at app-flow level, `thiserror` enums at boundaries (`OpenError`, `MaterializeError`). Don't paint the whole app with `thiserror` early.

## Verification before leaving this spec

- Can a sheet be materializing while another is being scrolled? Yes — materialize runs in a tokio task; UI thread never calls `.collect()`.
- Can a sheet crash without taking the app? Yes — `catch_unwind` + `ErrorSheet` fallback.
- Does scroll stay responsive on a 10 GB CSV? Yes if `scan_csv` + `slice` is lazy — which it is per Polars docs. Verify with hyperfine in Phase 0.
- Can S-04 attach a keybind to any `Action` by name? Yes — `#[serde(rename_all = "snake_case")]` makes `Action` round-trip through TOML keymap strings.

Flip any of those answers → this spec is wrong, redraft before S-04 lands.
