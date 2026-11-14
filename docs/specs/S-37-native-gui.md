# S-37 — Native GUI frontend (egui), with a toolbar, settings and real plots

Status: **drafted 2026-11-14**, rough.

- Part P0 is worth doing for the TUI alone.
- P1 onward is gated: start only once a GUI is decided.

Depends on: S-02 (app loop), S-23 (layout, plots), S-26 A4 (colour depth), S-27 (styles), S-10
(Lua API), S-36 (number formats feed both frontends).

## 1. Why, and what not

The TUI stays the main frontend and the source of truth for tests. The GUI is a second frontend
over the same core, in the same process. There is no client-server split: no caller needs one.
If one shows up, a transport goes over the P1 boundary.

Gains only a GUI can give, in order:

1. **Plots:**
   - Pixel resolution, zoom and pan with mouse and trackpad, hover with the exact row.
   - Brushing: drag over points to select their rows.
   - Alpha for overplotting.
   - Braille stays in the TUI.
2. **Toolbar and menus:**
   - Common actions one click away.
   - A menubar with every command and its key: the GUI form of VisiData's menu
     (`visidata/menu.py`, `Alt+f` …, which vdrs lacks).
   - A command palette.
3. **Settings:** a settings window built from typed options (checkboxes, choices, numbers), each
   with its help text. Today options are `vd.option(...)` lines in `init.lua`, checked by a
   `match` in `src/plugins.rs`.
4. **OS integration:** native open/save dialogs, drag a file onto the window, copy a selection
   as TSV and HTML for spreadsheets.
5. **Later:** proportional fonts (more columns on screen), drag to resize or reorder columns,
   several windows, image cells.

Not doing: web/wasm (mlua links C Lua, and Polars and multi-thread tokio are limited there), a
GUI-first redesign, a widget per cell.

## 2. Crates (checked on crates.io 2026-11-14)

| Need | Crate | Why |
|---|---|---|
| Window, widgets | `eframe` / `egui` 0.37 | Immediate mode, like ratatui: rebuild from `App` state each frame, so `render(&mut self, …)` keeps its shape. Pure Rust, no UI language, fast releases. |
| Plots | `egui_plot` 0.38 | Zoom, pan, hover, box select. |
| GUI tests | `egui_kittest` 0.37 | Drives an egui app headless (AccessKit queries, snapshots). |
| File dialogs | `rfd` | Native dialogs. 0.18 is a week old, so pin the release before it under the dependency cooldown. |
| Clipboard | through egui / `arboard` 3.7 | egui's clipboard covers text; HTML copy needs arboard. |

Rejected:

- `iced`: Elm-style messages would mean restructuring `App`.
- `slint`: `.slint` language; check its licence first.
- `gpui`: tied to Zed, unstable API.
- `ratatui-wgpu`: a TUI in a window. It gives no toolbar or settings widgets, so it doesn't
  deliver the gains above.

All GUI crates sit behind a cargo feature `gui`, off by default, so the TUI build and `just check`
stay as they are. CI adds one `--features gui` clippy + test job.

## 3. P0: TUI-side groundwork (5–7 d, ships alone)

Each part helps the TUI on its own and keeps ratatui and crossterm out of the core.

- **P0a. Own input types (1 d):**
  - `key.rs` gets its own `KeyCode` enum instead of `crossterm::event::KeyCode`; crossterm
    appears 47 times in `key.rs`, plus `lineedit`, `app/input`, `hooks`, `actions/vim`.
  - A vdrs `Mouse` event replaces `crossterm::event::MouseEvent` in `App::on_mouse`.
  - `From<crossterm::KeyEvent>` stays in `key.rs`, the only crossterm edge apart from the app loop.
- **P0b. Own style type (1 d):**
  - `theme.rs` and the Lua renderer (`plugins.rs`, `Rendered.style`) use a vdrs `Style` and
    `Color`, converted to ratatui's at paint time.
  - The Lua API doesn't change: Lua already passes strings and tables.
- **P0c. Option registry + options sheet `O` (2–3 d):**
  - One table of options: name, type (bool / int / enum / string / fmtstr), default, help, and
    whether it applies to the sheet or globally.
  - `vd.option` validates through it, replacing the `match` in `plugins.rs`.
  - `O` (VisiData `options-global`, `visidata/optionssheet.py`) pushes a sheet of options; `e`
    edits one, checked against its type.
  - `z<C-s>` (`commit-sheet` there) writes changes to `options.lua` next to `init.lua`, loaded
    after it (as `macros.lua`, S-19). Hand edits to other lines stay.
  - `zO` (`options-sheet`, per-sheet overrides): later.
  - The GUI settings window reads the same table.
- **P0d. Command catalog (1–2 d):**
  - Per action: longname (`Action::NAMED` already has it), one-line help, menu path, default keys
    (from the keymap, so a `vd.bind` shows up).
  - Menu paths copy VisiData's tree from `addMenuItems` (`File > Save sheet`, `Column > Set
    precision > more`) so its users find things where they expect.
  - Used by: the TUI help sidebar page now, a TUI menu later (VisiData `Alt+f`, …), and the GUI
    menubar, toolbar and palette.
- Tests: unit (registry types, `options.lua` round trip under the `cfg!(test)` config dir);
  scenario (`O`, `e`, the option takes effect; `z<C-s>`, then reload, and the value is kept).

## 4. P1: core / frontend split (5–7 d, gated)

- `App` stops owning `Terminal` and `EventStream`. Its surface:
  - `on_input(Input)`, where `Input` is a key, mouse, paste, resize or drop of a path;
  - `on_job(result)`;
  - `take_effects() -> Vec<Effect>`.
- `Effect` covers what only a frontend can do:
  - `Suspend`: `<C-z>`, the only command that needs the terminal (`suspend()` in `src/app.rs`
    stops the process so the shell gets the terminal back). vdrs has no `$EDITOR` command
    today. In the GUI, `<C-z>` minimizes the window (K4);
  - `Clipboard` (OSC 52 is terminal-only; the GUI uses the native clipboard, and the
    `clipboard_copy_cmd` / `clipboard_paste_cmd` programs work in both), `Bell`, `Quit`,
    `OpenDialog`.
- **Screen model:** `SheetWidget::draw` splits in two.
  - `GridModel` is pure. It holds the visible header cells and rows, each cell's text and
    `Style`, the frozen columns, cursor, selection and notes. It holds no widths: the frontend
    measures (below).
  - `paint` writes a `GridModel` into a ratatui `Buffer`.
  - The same applies to the status line, the sidebar pages and the plot. The plot model holds
    the points and the view box, not Braille.
- **Widths are the frontend's (K3, proportional):**
  - Today `Sheet.widths: Vec<u16>` (cells) and `refresh_widths`, `clamp_view(view, df, area,
    widths)`, `column_at(view, widths, x)` all count terminal cells.
  - They take a `Measure` the frontend passes in (text → width) and work in its unit: cells
    (`unicode-width`) for the TUI, pixels (egui font metrics) for the GUI. Width caps (`MIN` /
    `MAX_COL_WIDTH`, 4 / 40 cells) become per-frontend constants.
  - The cached widths move from `Sheet` to a per-frontend cache keyed by `widths_key`, so the
    two frontends never share them.
  - Rows stay one line high in both.
- The `Harness` keeps using `TestBackend` and the same cells. New unit tests can assert on
  `GridModel` directly.

## 5. P2: GUI shell (5–8 d, gated)

- **Entry (K2):** a separate binary `vdrs-gui <files>`, same arguments as `vdrs`.
  - `[[bin]] name = "vdrs-gui"`, `required-features = ["gui"]` in the same crate, so `vdrs` and
    its build never pull in egui.
  - Argument parsing moves from `main.rs` into a shared function both binaries call.
- **Grid:** egui's painter draws the `GridModel` in a proportional font, column widths from
  egui's font metrics (K3), theme colours at 24 bit (no `downgrade`), scrollbars from the window
  offset. A setting switches to a monospace font. The keymap is unchanged and every key works
  the same. The ex line and prompts show in an egui text field fed by the same line editor.
- **Toolbar:**
  - Default buttons: open, save, undo, redo, filter (`|`), sort asc/desc (`[` `]`), freq (`F`),
    pivot (`W`), plot (`.`), columns sheet (`C`), describe (`I`), options (`O`).
  - Each is a catalog action; the tooltip shows its help and key.
  - User-set (K5): `vd.toolbar { "save", "freq", ":plot x y" }` in `init.lua`. Items are
    catalog longnames, `":ex"` commands or Lua functions (as `vd.bind` takes); `"|"` is a
    separator. Unknown names warn at load, as `vd.bind` does. It's a new Lua API, so it bumps
    `vd.api_version` (S-10). The TUI ignores it.
- **Menubar:** the P0d catalog tree, each item with its current key.
- **Palette:** `Ctrl+Shift+P` (no clash: VisiData binds no `Ctrl+Shift` keys; check before
  binding) fuzzy-searches the catalog.
- **Settings window:** generated from the P0c registry. Widgets by type, help as tooltip,
  global/sheet scope, Apply. Save goes through the same `options.lua` commit.
- **OS:** `rfd` dialogs for open/save, files dropped onto the window open as sheets, copy puts TSV
  and HTML on the clipboard.
- **Tests:**
  - unit: egui key event → `Input` mapping table;
  - `egui_kittest`: open a CSV, click the freq button, see the freq sheet; change an option in
    settings and see it take effect.
  - No real file dialogs in tests (the dialog is an `Effect`; tests answer it).

## 6. P3: plots (4–6 d, gated)

- `:plot` / `.` in the GUI opens an `egui_plot` pane, or its own window, from the P1 plot model.
- **Interaction:**
  - Zoom and pan by scroll and drag.
  - Hover shows the row's key columns and values.
  - Box-select sends the S-23 region-select action, so the rows get selected on the sheet and the
    TUI's region select and the GUI's share one code path.
  - Up to about 1M points, with an alpha that scales with point density.
- `:hist` and `:bar` use egui_plot bars.
- Tests: `egui_kittest` drags a box, and the sheet's selection count matches the TUI region
  select on the same data.

## 7. P4: later, each on evidence

- Drag column borders to resize (writes the column width, which sessions keep), drag headers to
  reorder (S-34 `order`).
- Several windows, one per sheet or split.
- Image cells.

## Decisions

Agreed 2026-11-14 unless marked.

- **K1** Toolkit: egui.
- **K2** Entry: a separate binary `vdrs-gui`, needing the `gui` feature (P2).
- **K3** Grid: proportional fonts from the start; widths per frontend (P1). Adds about 2 d to P1.
- **K4** (proposed; open) `<C-z>` in the GUI minimizes the window. If vdrs gains an `$EDITOR`
  command later: the GUI runs `$VISUAL` (a GUI editor such as `code --wait` opens its own window
  and works), and refuses a terminal editor (`vim`, `nano`) with a toast, since a GUI process
  has no terminal to lend it.
- **K5** Toolbar: user-set with `vd.toolbar` (P2).
- **K6** Order: P0 goes before S-36. P0a/P0b are pure refactors; P0c (`O`) and P0d (catalog) are
  VisiData features the TUI lacks, so they're worth having without a GUI.

## Effort

P0 5–7 d, P1 5–7 d (with K3), P2 5–8 d, P3 4–6 d: about 4–6 weeks for a GUI with toolbar, settings and
plots. The ongoing cost is that every later UI feature needs a GUI form and a GUI test. Specs from
then on name both.
