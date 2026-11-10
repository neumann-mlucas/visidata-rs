# TUI Patterns Research — visidata-rs

Sources at `main`/`master`, 2026-10-08. Line numbers approximate (`~`) where
WebFetch summarised rather than quoted. "Skimmed" = inferred from surrounding
files or summary, not read line-by-line. 404 noted where raw URL failed.

Apps covered: **yazi**, **csvlens**, **gitui**, **bottom**, **zellij**,
**atuin**, **television**.

---

## Q1 — App loop & Model/Update/View

### yazi

- Main loop: `yazi-fm/src/app/app.rs`, `App::serve()` ~L28-44. One `select!`
  over an internal event channel (`Event::take()`). `App` struct ~L7-13:
  `core: Core`, `term: Option<Raterm>`, render-pacing (`need_render: u8`,
  `last_render: Instant`, `next_render: Option<Duration>`).
- Events: internal MPSC, not raw `EventStream`. Each tick: one `drain()` then
  `rx.try_recv()` greedy batch, then one render.
- Signals: `yazi-fm/src/signals.rs` L3-31, Tokio task over
  `signal_hook_tokio::Signals`; fed into the same bus.
- Dispatcher: `yazi-fm/src/dispatcher.rs` — pattern-matches
  action/key/mouse/resize/paste/DnD/clipboard/report. `dispatch_seq` fires
  all-but-last immediately, re-emits tail.
- Render: dirty-flag + debounce. `NEED_RENDER` atomic; `<10 ms` since last
  render → schedule, else render now (~L55-68).
- Rendering: `yazi-fm/src/root.rs` L30-55 — base widget, then stacked overlays
  `Preview`, `Modal`, `Tasks`, `Spot`, `Pick`, `Input`, `Confirm`, `Help`,
  `Compare`, `Which`. Z-ordered modal composition.
- State: `Core` → `Mgr` (`yazi-core/src/mgr/mgr.rs` L11-17) owns `tabs`,
  `yanked`, `batcher`, `watcher`, `mimetype`. Each `Tab`
  (`yazi-core/src/tab/tab.rs`) carries own `mode`, `current`, `parent`,
  `backstack`, `history`, `selected`, `preview`, `finder`. Active + dormant.
- Async: Tokio. Background work in `yazi-scheduler`; results posted via the
  event bus and `yazi-dds` pub/sub (`yazi-dds/src/lib.rs` L4-19).

### csvlens

- Main loop: `src/app.rs`, `App::main_loop()`. Sync: poll `InputHandler` →
  `step(control)` → `draw()`. No Tokio.
- Events: `src/input.rs` — three handlers routed by `InputMode`
  (`Default`/`Help`/buffering flavours `Find`/`Filter`/`FilterColumns`/
  `GotoLine`/`FreezeColumns`/`Option`). Keys → `Control` enum variants.
- State: single `App` owns `rows_view`, `csv_table_state`, `finder`, `sorter`,
  `columns_filter`, `transient_message`. Strict TEA: `step(control)` is the
  update fn.
- Render: every loop iter (blocking input means no idle work).
- Async: OS threads, not Tokio. `RowsView` chunk-loads via `do_get_rows`
  (`src/view.rs`). `Finder` runs background regex behind
  `Arc<Mutex<FinderInternalState>>` + `should_terminate` flag (`src/find.rs`).
  `Sorter` is a background sort. UI reads partial results each tick.
- stdin: `src/io.rs` `SeekableFile` — tempfile fallback for non-seekable
  sources; streaming mode spawns a copy thread + header-detect thread,
  coordinated by `AtomicBool`; 1 MiB chunks.
- Multi-sheet: none.

### gitui

- Main loop: `src/gitui.rs` `run_main_loop()` L78-139. First iter returns a
  Notify; then blocks on `crossbeam_channel::Select` over 6 channels
  (`src/main.rs` L202-234): `InputEvent`, `AsyncGitNotification`,
  `AsyncAppNotification`, ticker `Instant`, notify-watcher, spinner. Pure
  crossbeam + threads, no Tokio.
- State: `src/app.rs` `App` struct — ~30 popup fields, 5 tabs
  (Status/Revlog/Files/Stashing/StashList), `PopupStack` for nesting, three
  async-event receivers, `requires_redraw: Cell<bool>`,
  `queue: Queue<InternalEvent>`.
- Render: after every event dispatch (L126-134 draw + re-arm spinner).
- Async: `asyncgit` background threads; results arrive as
  `AsyncGitNotification`. `any_work_pending()` aggregates for UI feedback.

### bottom

- Main loop: `src/lib.rs` `start_bottom()` L434-569; inner loop L517-565,
  blocks on `receiver.recv()` of `BottomEvent` (`Terminate`/`Resize`/
  `KeyInput`/`MouseInput`/`PasteEvent`/`Update(data)`/`Clean`).
- Events: 3 native threads feed the mpsc — collection L393-430, input L344-391
  (crossterm sync poll), clean L505-514. No Tokio.
- State: `src/app.rs` `App` — `AppWidgetStates`, `DataStore`, dialogs,
  `AppConfigFields` (~40 opts), `widget_map`, multi-tap state (`gg`, `dd`).
- Render: on event.

### zellij

- Actor model, not one loop. `zellij-server/src/lib.rs` — PTY / Screen /
  Plugin / PTY-writer / background-jobs threads over
  `ChannelWithContext`/`SenderWithContext` (L55-62).
- Message type: `ServerInstruction` enum L92-161 (`FirstClientConnected`,
  `Render`, `ChangeMode`, `Reconfigure`, …).
- State: distributed. `SessionMetaData` L1030-1048, `SessionState` L1074-1199.
- Render: Screen actor on `Render` message, bytes pushed to client socket.
- Plugins: dedicated thread (`plugin_thread_main`) running Wasmi; lifecycle
  via `PluginInstruction` enum.

### atuin

- Main loop: `crates/atuin/src/command/client/search/interactive.rs`
  `history()`. `tokio::select!` over user input (`event::poll` +
  `event::read`), background tasks (update check, history count), async query
  futures.
- State: `State` struct — `keymap_mode` (Emacs/VimNormal/VimInsert/Inspector),
  `pending_key` for multi-key, tab index, cursors.
- Update: `InputAction` enum (`Accept`, `Copy`, `Delete`,
  `DeleteAllMatching`, `ReturnOriginal`, `ReturnQuery`, `Continue`, `Redraw`,
  `SwitchContext`). Two-phase: resolve under immutable borrow, then mutate —
  dodges borrowck conflicts with pending-key consumption.
- Render: `draw_inner()`, layout varies by `Compactness` + `invert`.

### television

- Main loop: `television/app.rs` `App::run()` async L178-293.
  `tokio::select!` L256 over event/action/render/UI-state channels.
- Events: `television/event.rs` `EventLoop` as Tokio task. Internal
  `tokio::select!` prioritises control (pause/resume/abort), Ctrl-C, tick,
  then crossterm poll. **Resize debounce** via `flush_resize_events()`
  (50 ms poll, keep first + last). Release events → `NULL_KEY` discarded.
  Case normalised.
- State: `App` L29-50 — `television: Television`, `should_quit`, 4 channels,
  task handles, history + frecency.
- Model/Update/View: strict TEA. Event → `Action` → `Television` update →
  render task. `Action::affects_results()` short-circuits matcher work on
  cosmetic actions.
- Modal: `Television::mode` ∈ `{Channel, RemoteControl, ActionPicker}`.
  `handle_action()` orchestrates transitions ~L920-1050.
- Render: `should_render()` L681-715 — tick-based with faster interval while
  a channel executes.

### Synthesis — Q1

Take **television's shape with yazi's layering**:

- One async `tokio::select!` main loop. Event → `Action` → state → dirty-flag
  → draw. Strict TEA (television/atuin). Rules out gitui/bottom's
  threads-only — Polars async + mlua coroutines want Tokio.
- `crossterm::EventStream` behind an `EventLoop` with three TV tricks: resize
  debounce 50 ms, release-key strip, tick dirty-flag redraw (yazi's `<10 ms`
  suppression equally fine).
- One `App` owning a `Sheets` stack (yazi `Tabs`), each sheet with its own
  `LazyFrame`-view, cursor, selection, finder, sorter. Active + dormant.
  Modal overlays (yazi z-stack) for `Help`/`Pick`/`Input`/`Confirm`/`Which` —
  not separate windows.
- `Action` enum serde-renamed snake_case → config keybinds bind by name
  (television). Add `affects_data()` predicate to skip Polars re-collect on
  cosmetic actions.
- Background work (scan, filter, sort) as `tokio::spawn` + mpsc back.
  csvlens' terminate-flag for cancellable long runs.

---

## Q2 — Readers / data sources / plugin shapes

### yazi plugin shape

- Preset Lua plugins: `yazi-plugin/preset/plugins/*.lua` — 34 files
  (`code.lua`, `json.lua`, `pdf.lua`, `svg.lua`, `image.lua`, `archive.lua`,
  `fzf.lua`, `rg.lua`, `fd.lua`, `file.lua`, `folder.lua`, …).
- Module `M` exposing:
  - `peek(job)` — `ya.preview_code(job)` or `ya.preview_widget(...)`.
  - `seek(job)` — scroll; computes step from `job.area.h`/`job.units`,
    emits `ya.emit("peek", {...})`.
  - `spot(job)` — metadata card (file.lua).
  - `fetch(job)` — iterate `job.files`, `ya.co(...)` coroutine, yield
    `(file, result)`.
  - `preload()` → bool.
- `job`: `file.{url,path,stat}`, `mime`, `area`, `units`, `skip`.
- Runtime registry: `rt.plugin.{spotters,previewers,fetchers,preloaders}` —
  lists of `{ mime, run }` rules matched top-down. Config lives in
  `yazi-config/preset/yazi-default.toml` `[plugin]` section.
- Loader/isolate: `yazi-plugin/src/loader/` and `isolate/` — raw URLs 404'd
  (**skimmed**). Shared Lua VM; isolate wraps per-call sandboxing.

### csvlens source abstraction

- **No `trait Source`.** Two structs: `SeekableFile` (`src/io.rs`) ensures
  readable-from-start (tempfile fallback for pipes); `CsvConfig` +
  `CsvLensReader` (`src/csv.rs`) layer on top. Streaming via separate
  `StreamingCsvReader` wrapping `csv_core`, sleep-on-EOF for tail.
- Random access: background-built position table (byte offsets per record);
  seeks nearest offset, parses forward.
- Dispatch: `CsvlensRecordIterator` **enum**, variants for streaming vs
  standard.
- Errors: `anyhow::Result` on `run_csvlens_with_options`; initial-open
  failure prints + exits. Search returns partial matches —
  "load-what-you-can" for queries, "fail-the-sheet" for open.

### others (data sources)

- gitui: no format abstraction — git via `asyncgit`.
- bottom: OS; one `DataStore`; no plugins.
- zellij: WASM plugins can act as sources via pipe system (`CliPipe`,
  `MessageFromPlugin`, `pipe_to_all_plugins()`). Overkill for us.
- atuin: `SearchEngine` **trait** (`search/engines.rs` L63-72):

  ```rust
  async fn full_query(&mut self, ...) -> Result<Vec<History>>;
  async fn query(&mut self, ...)      -> Result<Vec<History>>;
  fn get_highlight_indices(&self, cmd: &str, input: &str) -> Vec<usize>;
  ```

  Static dispatch via `#[enum_dispatch(SearchEngine)]` on `AnySearchEngine`
  (L91-96, variants `Db`, `Daemon`). No vtable.
- television: multiple built-in "cable channels"; preview is an async task
  over a channel (`update_preview_state()` ~L791-820).

### Format sniffing crates

| Crate | Version | Mechanism | Notes |
|---|---|---|---|
| `infer` | 0.22.0 (Sep 2026) | Magic bytes. `get()` / `get_from_path()`, category checks, custom matchers. | Lightweight, no system deps. **Best fit.** |
| `file-format` | 0.29.0 (Sep 2026) | Signatures + specialised readers (XML/CFB/ZIP introspection); can use extension too. | Richer; needs per-format features; more deps. |
| `tree_magic` | 0.2.3 (Sep 2026) | FreeDesktop MIME tree, system DB on Linux. ~150 ns / 5-100 µs. | Portability pain (system DB). |

For us the **extension decides** most cases (`.csv/.tsv/.parquet/.jsonl`);
magic bytes a fallback. `infer` fits.

### Error surfacing

- csvlens: fatal on initial load, non-fatal on partial scan.
- yazi: `error.lua` preview; sheet stays visible with overlay. Non-fatal.
- gitui/bottom: typed error channels + toast.

### Synthesis — Q2

- `trait Source` with Polars-shaped async methods:

  ```rust
  trait Source {
      async fn schema(&mut self) -> Result<Schema>;
      async fn slice(&mut self, offset: usize, len: usize) -> Result<DataFrame>;
      fn lazy(&self) -> Result<LazyFrame>;        // optional fast path
      fn total_rows_hint(&self) -> Option<usize>;
  }
  ```

  Static dispatch via `#[enum_dispatch]` (atuin): one
  `AnySource { Csv, Tsv, Parquet, Jsonl, Stdin, Lua(LuaSource) }`. `Lua`
  variant lets mlua plugins register sources without a Box.
- Steal csvlens' `SeekableFile` — tempfile fallback unblocks
  `cat x.csv | visidata-rs`.
- Loader errors: typed `OpenError` for initial-open fatals; non-fatal errors
  → `transient_message` toasts (csvlens).
- Plugin shape: mirror yazi's module-table. A loader plugin:
  `{ open(path, opts) -> Source, matches(path, mime) -> bool }`. Registry in
  TOML, top-down `mime`/`ext` match (yazi `[plugin]`). mlua coroutines for
  non-blocking I/O.
- Detection: `PathBuf::extension()` first, `infer` 0.22 fallback. One
  `detect(path) -> SourceKind`.

---

## Q3 — Keybinding systems & extensibility

### yazi

- `yazi-config/preset/keymap-default.toml`. One `[mgr]` with
  `keymap = [{ on, run, desc }, ...]`. Layers (`mgr`/`tasks`/`spot`/`pick`/
  `input`/`confirm`/`help`/`cmp`) each own a section.
- `on`: single key (`"q"`, `"<Esc>"`, `"<C-d>"`) or **array** for chords
  (`on = ["g", "g"]`, `on = ["z", ...]`, `on = ["c", "c"]`,
  `on = ["m", "s"]`). `run` is a command string parsed into `Actions`.
- Keymap: `yazi-config/src/keymap/keymap.rs` — per-layer `KeymapSection`.
  `Keymap::chords(layer)` → `Arc<Vec<ChordArc>>`.
- `Chord` (`yazi-config/src/keymap/chord.rs` L13-20): `id`, `on: Vec<Key>`,
  `run: Actions`, `desc`, `r#for` (platform). Equality by `on` alone
  (L45-51) → config overrides work by shadow.
- Router: `yazi-fm/src/router.rs` — iterates chords for active layer,
  matches first key; prefix match → "which-key" overlay (multi-key pending),
  else fire. **Linear scan**, no trie.
- Key: `yazi-config/src/keymap/key.rs` — `code`, `shift`, `ctrl`, `alt`,
  `super_`. Parses `<C-A>`, `<S-a>`, `<F1>`, `<Enter>`.
  `TryFrom<KeyEvent>` normalises shift (e.g. `Shift+1` → `!`).
- Runtime extension: `prepend_keymap`/`append_keymap` arrays; Lua plugins
  push entries at load.
- **No count prefix** (vim `3j`); uses `arrow 50%` and absolute moves.
- **No `.` repeat** built-in.

### gitui

- `src/keys/key_list.rs` — `KeysList` with **89 fixed fields**, one per
  action; `GituiKeyEvent { code, modifiers }`. Loaded from `.ron` via
  `struct_patch::Patch` (L186-194) — sparse patch over defaults
  (L117-184).
- No chords, no count, no runtime extension.

### bottom

- Hardcoded. Multi-tap (`gg`, `dd`) via timestamp state on `App`. No config
  file for keybinds (404'd on `options/config/key_bindings.rs` — may have
  moved; **skimmed**).

### atuin

- `KeymapConfig` (`atuin-client/src/settings.rs` L1054-1071) split by mode:
  `emacs`, `vim_normal`, `vim_insert`, `inspector`, `prefix`. Each
  `HashMap<String, KeyBindingConfig>`.
- `KeyBindingConfig` = `Simple(String) | Rules(Vec<KeyRuleConfig>)`
  (L1045-1052). `KeyRuleConfig { when: Option<String>, action: String }` —
  **conditional bindings** (`when = "cursor-at-start"` etc).
- Chords: `pending_key` on `State`, next keypress resolves. Separate
  `prefix` mode map. Supports `gg`.
- No count prefix.

### television

- `television/config/keybindings.rs` — flat TOML:
  `"key" = "action"` or `["a1", "a2"]` sequence.
- Syntax: `ctrl-a`, `shift-enter`, `<ctrl-a>`, case-insensitive modifiers,
  uppercase ⇒ shift. Unbind via `= "no_op"`. Merges onto defaults.
- No chord, no count.

### zellij

- `zellij-utils/src/input/keybinds.rs`:
  `HashMap<InputMode, HashMap<KeyWithModifier, Vec<Action>>>`.
- Chords via **mode switching** (`Action::SwitchToMode(Prefix)` + follow-up).
  `shortcut_for_action()` BFS depth 4 for help display.
- Config in KDL (`zellij-utils/src/kdl/mod.rs`), `keybinds { ... }` block
  parsed through `kdl` crate + macros (`parse_kdl_action_arguments!`, etc).
- No count prefix.

### Count prefix (`3j`)

**None** of the surveyed apps support it natively. Model as
`App.pending_count: Option<usize>` consumed by the next non-digit action —
nobody else did it so there's no prior art to copy; small state addition.

### Chord/prefix structures

- **Linear vec, first-key match + which-key**: yazi.
- **Nested hashmap by mode**: zellij.
- **Flat hashmap per mode + separate prefix map**: atuin.
- **Fixed struct of fields**: gitui.
- **Flat hashmap, no chords**: television, bottom.

Trie correct in theory, overkill in practice. Yazi's "linear vec +
which-key on prefix" is simplest and most-used.

### Runtime extension (plugins)

- yazi: `prepend_keymap`/`append_keymap`; Lua pushes entries.
- zellij: WASM `KeybindPipe` (`PluginInstruction::KeybindPipe`) rewrites
  the keymap tree.
- others: config-file or hardcoded, no runtime mutation.

### `.` repeat

- None native. Yazi's action-stream model makes it cheap to add — capture
  **resolved `Action`** post-chord/post-count, not keystrokes
  (keymap-change resilient).

### Synthesis — Q3

- Steal yazi's config: TOML, `keymap = [...]` per layer, entries
  `{ on, run, desc, for? }`. Chord via `on = ["g", "g"]`. Lua
  `prepend_keymap`/`append_keymap`.
- Key struct: adopt yazi's `Key` + `<C-x>`/`<S-F1>`/`<Esc>` parser
  wholesale.
- Resolution: linear-vec + first-key match + which-key overlay. Trie later
  only if a layer exceeds ~500 entries (unlikely).
- Count prefix: add. `pending_count: Option<usize>`, cleared by non-digit.
- Modes: fold into layers rather than a separate axis.
- `.` repeat: `last_action: Option<Action>` on `App`, action-resolved.
- Lua API: `vd.bind("sheet", "<C-f>", "find", desc = "...")`, pushes to
  layer chord vec.
- Skip atuin's `when` conditionals in Phase 0; add if needed.

---

## Crate recommendations

Versions as seen in surveyed manifests + docs.rs (2026-10-08). MIT or
MIT/Apache dual unless noted.

**UI / terminal**

- `ratatui` 0.30 (bottom, csvlens).
- `crossterm` 0.29 with `event-stream`.
- `tui-input` optional — if a text widget is needed before writing one.

**Async**

- `tokio` 1.41+ with `rt-multi-thread`, `macros`, `sync`, `signal`, `time`,
  `fs`.
- `tokio-stream` for `EventStream`.
- `signal-hook-tokio` 0.3 (yazi) for OS signals into the bus.

**Data**

- `polars` latest with `lazy`, `csv`, `parquet`, `json`, `strings`,
  `temporal`.
- `csv` 1.4 — low-level streaming fallback (csvlens).
- `arrow` 59 — transitive via polars; direct for `arrow-csv` streaming.

**Config / serde**

- `serde` 1.0 derive.
- `toml` 0.8.
- `struct_patch` — gitui's sparse override trick (optional).
- `enum_dispatch` 0.3 — atuin pattern for `AnySource`.

**Scripting**

- `mlua` 0.12 with `lua54` + `async` + `vendored` + `send`. Matches yazi.
  (`luau` is a sandboxed alt; defer.)

**Format detection**

- `infer` 0.22. Skip `tree_magic` (system DB) and `file-format` (heavier)
  unless we need deep introspection.

**Keymap parsing**

- No crate. Roll `Key` parsing (~50 lines, mirror yazi `key.rs`). Pulling
  `kdl` or similar would be over-engineering.

**Errors / logging**

- `anyhow` for app flow, `thiserror` for boundary errors (`OpenError`,
  `QueryError`).
- `tracing` + `tracing-subscriber`.

**Testing**

- `insta` for frame snapshots.
- `rstest` for parameterised cases.
- No Lua test framework at Phase 0 — assert-based `demo()` per module.

---

## Honesty log — skimmed, not fully read

- yazi `yazi-plugin/src/loader/`, `isolate/` — raw URLs 404'd; plugin
  loading inferred from `lib.rs`, preset Lua, and `yazi-dds`.
- csvlens `src/app.rs` — top-level flow summarised, not every state
  transition.
- gitui `src/app.rs` — ~30 popup fields not individually catalogued.
- bottom `src/options/config/key_bindings.rs` — 404'd; keybind config
  summary inferred from `src/app.rs` multi-tap handling.
- bottom `src/app.rs` on `main` branch — initial 404; derived from
  `src/lib.rs` + later re-fetch.
- zellij KDL — macro surface described, not every action-argument parser.
- zellij plugins `mod.rs` — summary only; `WasmBridge` not read.
- television `television.rs` — large; action handling + preview update
  inspected, full matcher pipeline skimmed.

All other refs read directly from raw URLs.
