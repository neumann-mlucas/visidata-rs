# S-23 — Layout: sidebar, split panes, plots

Status: **drafted 2026-11-08**, not started. Three independent parts, ship in order A → B → C.
Depends on: S-02 (app loop), S-04 (keymap). C depends on nothing in A/B.

## 1. What exists (verified in `src/app.rs`)
- `App::render` splits the screen: title (1 row) / main / status (1 row); main splits 65/35 for the
  plan panel or 50/50 for the `:pivot` preview. One table: `SheetWidget { sheet: sheets.active() }`.
- Single-sheet assumptions: `App.viewport_rows` (one value), `refresh_widths`/`clamp_view` on the
  active sheet only, ~90 uses of `sheets.active()` / per-sheet job handles in `app.rs` + `actions.rs`.
  Jobs are already keyed by `SheetId` (`materialize_handles: HashMap<SheetId, _>`), so a second visible
  sheet does not need new job plumbing.
- Ratatui 0.30 ships `Canvas` (braille / half-block markers), `Chart`, `BarChart`, `Sparkline`.

VisiData reference keys: `b` sidebar-toggle, `Ctrl+G` sidebar-cycle, `zb` sidebar-choose; `Z`
splitwin-half, `gZ` close, `Tab` swap focus, `gTab` swap panes, `zZ` split at N rows; `.` plot-column,
`g.` plot-numerics (in vdrs `.` is repeat — see L3).

## 2. Part A — Sidebar (0.5–1 d)
A right-hand panel that shows text about the current context; the plan panel becomes one of its pages.
- Pages: `plan` (today's panel), `column` (dtype, nulls, distinct, min/max of the cursor column — the
  `I` describe row, computed for one column in the background), `cell` (full value, wrapped; nested
  values pretty-printed — pairs with S-22), `help` (bindings of the current layer, which-key list).
- Keys: `b` toggle (VisiData), `Ctrl+G` cycle pages (VisiData). `:sidebar <page>`.
  Interaction with S-18 F (`P` → `:plan`): `:plan` = `:sidebar plan`.
- Rendering: reuse the plan-panel split; pivot preview keeps priority while typing `:pivot`.
- Width: 35 % (today's value); `vd.option("sidebar_width", 35)` via S-18 D6 if asked.

## 3. Part B — Split panes (2–3 d)
- Model: `App.panes: Vec<Pane>` (1 or 2), `Pane { sheet: SheetId, area: Rect, viewport_rows }`,
  `App.focus: usize`. The sheet stack stays global; a pane points at a sheet id. `sheets.active()`
  becomes "the focused pane's sheet" — one accessor change, then grep every `sheets.active()` caller
  for code that should touch *both* panes (render, widths, job landing → redraw both).
- Keys (VisiData): `Z` split horizontally (top/bottom) showing the previous sheet in the new pane, `gZ`
  close other pane, `Tab` focus other pane, `gTab` swap. Vim aliases via `vd.bind` (S-26):
  `<C-w>s`/`<C-w>v`/`<C-w>w`/`<C-w>o`. Vertical split (`:vsplit`) is the same code with
  `Direction::Horizontal`.
- `q` in a pane pops the pane's sheet; last sheet in a pane closes the pane.
- Same sheet in both panes: allowed; cursor/scroll are per `View`, so v1 shares them (VisiData does too).
  Per-pane cursors only if asked.
- Prompts/status line/which-key belong to the focused pane; title bar shows both sheet names.

## 4. Part C — Plots (L: 1–2 weeks in stages)
A plot is a derived **sheet kind** (`Origin::Plot { parent, x, ys, kind }`), so it lives on the stack,
`q` pops it, `S` lists it. Its data is an aggregated/sampled frame computed in the background from the
parent's `view_plan()`; the widget draws it.

| Stage | Ships | Effort |
|---|---|---|
| C1 | scatter/line: `:plot <x> <y…>` (numeric/date x); braille `Canvas`; axes + legend; colors per y (theme); data downsampled to ≤ 2 points per screen cell (`group_by_dynamic` / binning by x pixel, min+max per bin so spikes survive) | 3–4 d |
| C2 | histogram of the current numeric column (`:hist`, binned in Polars) and bar chart of a freq sheet (`:bar` on `F` output; VisiData can't) | 1–2 d |
| C3 | navigation: `hjkl` pan, `+`/`-` zoom (VisiData `zoom-*`), `0` reset; re-sample on zoom | 2 d |
| C4 | select by region: a box cursor; `s` selects the parent rows inside it (rid-keyed → shows in the parent), `Enter` dives to them | 2–3 d |
| C5 | `:export` of a plot as a Polars/plotnine or DuckDB+vega-lite snippet — only if asked | — |

Drawing library (checked 2026-11-08):
- **ratatui built-ins** (`Canvas` braille/half-block, `Chart` line/scatter with axes + legend,
  `BarChart`, `Sparkline`): already a dependency, cover C1–C3. **Use these.**
- `tui-chart` 0.6 (ratatui 0.30; line, bar, histogram, heatmap, candlestick, pie via `tui-piechart`):
  compatible, but first published days before this spec (~2k downloads). Re-evaluate for heatmaps
  after it has a few months of releases; not needed for C1–C4.
- `plotters-ratatui-backend` 0.3 (full `plotters` API): pins ratatui 0.29, incompatible with our 0.30
  until it updates.
- `textplots` / `drawille` / `termplot-rs`: print to stdout, not ratatui widgets; no gain over `Canvas`.
- The heavy part (sampling/binning millions of rows) is Polars either way; a chart crate only draws
  ≤ 2 × width points.

Rules:
- Keys: `:plot`/`:hist`/`:bar` first; key binding decided in L3.
- x types: int/float/date/datetime; y numeric; anything else refused with the column + dtype.
- A plot of a 100M-row view must stay O(screen) in memory: the sample/bin step runs lazily in Polars,
  never collects raw rows.
- Terminals without braille glyphs: `vd.option("plot_marker", "braille"|"halfblock"|"dot")`.

## 5. Test plan
- A: scenario — `b` shows the column page with the cursor column's stats; `Ctrl+G` cycles; `:plan`
  still cycles plan → optimized → off; render snapshot (TestBackend) per page.
- B: scenario — `Z` → two panes, `Tab` moves focus, `j` moves only the focused pane, a background job
  landing for the unfocused sheet redraws it; `gZ` back to one; `q` in a pane. Monkey: add `Z gZ Tab`
  to `KEYS`; undo-all invariant holds. pty recording `docs-split`.
- C1: unit — downsampling keeps the min and max of each bin, output size ≤ 2 × width; dtype refusals.
  Render snapshot of a known 10-point scatter (braille cells exact). Scenario: `:plot x y` on a 1M-row
  generated frame stays under a memory/time budget (row count asserted on the sampled frame, not wall
  clock).
- C2: unit — histogram bin edges match Polars `hist`; bar chart of a freq sheet lists the same counts.
- C3/C4: scenario — zoom changes the sampled x range; box select on the plot → parent selection count
  equals a `:select x between … and y between …` (S-21) on the same bounds.
- Docs: guide cards + recordings per part; doc-sync test covers new keys.

## 6. Decisions (open)
- **L1** sidebar default: off (VisiData) — recommend.
- **L2** split direction for `Z`: horizontal (VisiData) — recommend; `:vsplit` for side by side.
- **L3** plot key: VisiData `.` is `.`-repeat in vdrs (S-04, vim). Recommend keep `.` = repeat and bind
  plots to `g.` (VisiData plot-numerics) + `:plot`; record the deviation in S-04.
