# Specs

One spec per subsystem. Written just-in-time *before* the matching phase starts, not up front — specs rot, late-bind to reality.

File naming: `S-NN-slug.md`.

| ID | Title | Phase | Status |
|---|---|---|---|
| S-00 | Direction & scope | — | approved → see `../direction.md` |
| S-01 | Core data model: Sheet, Column, DataFrame binding | 0 | **shipped** → [`S-01-core-data-model.md`](S-01-core-data-model.md) |
| S-02 | Ratatui app loop & Model/Update/View shape | 0 | **shipped** → [`S-02-app-loop.md`](S-02-app-loop.md) |
| S-03 | Reader interface | 0 | **shipped + extended** (JSON arrays, Parquet directories, multi-part xlsx/sqlite + `file:part`, gzip; Lua loaders via `plugins::open`) → [`S-03-reader-interface.md`](S-03-reader-interface.md) |
| S-04 | Keybinding model & prefix maps | 0 | **shipped** (Lua `vd.bind()` instead of planned TOML; default-key policy S-26 §0) → [`S-04-keybindings.md`](S-04-keybindings.md) |
| S-05 | Type-system adapter over Polars dtypes | 1 | cut (superseded by S-13 #3 Type coercion chunk; shipped as `TypeCast*` Actions) |
| S-06 | Ex-line grammar | 1 | **shipped as ad-hoc parser** in `src/exline.rs` — no formal grammar spec; 28 commands incl. `:derive`/`:sql`/`:join`; quote-aware args |
| S-07 | Pivot / melt UX + LazyFrame construction | 2 | shipped — `:pivot` / `:unpivot` / `W` (Lua aggregators run in the background); live preview while typing `:pivot` |
| S-08 | Plan panel: display, optimize, export | 2 | **shipped** — `P` display + optimize; `:export` writes Polars `.py` / DuckDB `.sql` |
| S-09 | mlua plugin system: shapes, sandbox, errors | 3 | **keymap, loader, aggregator, renderer shapes + hot-reload shipped** → [`S-09-plugin-system.md`](S-09-plugin-system.md) |
| S-10 | Lua API surface + versioning (`since` / `deprecate`) | 3 | `vd.api_version` 5 shipped (bind functions, `vd.sheet`, `vd.toast`, `vd.push_sheet`, `vd.command`, `vd.option`); `since`/`deprecate` registry + `vd.frame` not started → [`S-10-lua-api.md`](S-10-lua-api.md) |
| S-11 | Streaming source abstraction | 4 | not-started as a trait; `:tail` reads CSV/JSONL incrementally and watches Parquet dirs |
| S-12 | Frame diff | 5 | **shipped** — `:diff` and `D` (prompt; key columns as keys) |
| S-13 | Tier 1 feature target (daily-driver parity for upstream golden corpus) | 1.5 | **closed** (iter 14 — honest classification replaced the "~160 PASS" target) → [`S-13-tier1-feature-target.md`](S-13-tier1-feature-target.md) |
| S-14 | Golden corpus replay harness | 1.5 | **landed and stable** (PASS=34, ceiling 182; triage mode + honest quarantine since S-25 P0; `.vdx` scripts since 2026-11-07). its driver-only ops are now also in the binary (`:join`, `:uniform`, `:setcol`, `W`, `g"`, `,`) → [`S-14-corpus-replay-harness.md`](S-14-corpus-replay-harness.md) |
| S-15 | **Feature inventory** — what the binary actually ships, plus what it doesn't (§10 cut, §10.1 not yet) | cross-cutting | **re-verified 2026-11-05** → [`S-15-feature-inventory.md`](S-15-feature-inventory.md) |
| S-18 | Cell/row editing + clipboard (Day-1 must-have) | — | **drafted 2026-11-08**, A (register copy), B (system clipboard), C (cell editing) shipped — 7 phases A–G, copy first, paste after editing → [`S-18-editing-clipboard.md`](S-18-editing-clipboard.md) |
| S-19 | Macros | — | **drafted 2026-11-08**, after S-18 C → [`S-19-macros.md`](S-19-macros.md) |
| S-20 | Undo tree | — | **drafted 2026-11-08**, deferred until after S-18 G (2026-11-08) → [`S-20-undo-tree.md`](S-20-undo-tree.md) |
| S-21 | Expression columns + selection (`=`, `g=`, `z/`, `:select`), Python subset gated | — | **drafted 2026-11-08** → [`S-21-expressions.md`](S-21-expressions.md) |
| S-22 | Nested data: expand structs `(`, unfurl lists `zM` | — | **drafted 2026-11-08** → [`S-22-nested-data.md`](S-22-nested-data.md) |
| S-23 | Layout: sidebar, split panes, plots | — | **drafted 2026-11-08** → [`S-23-layout-sidebar-splits-plots.md`](S-23-layout-sidebar-splits-plots.md) |
| S-24 | Hardening: module layout, window invariants, typed readers, release binaries | — | **drafted 2026-11-08** → [`S-24-hardening-refactor.md`](S-24-hardening-refactor.md) |
| S-25 | Corpus coverage plan (29 → ~90 PASS), columns sheet, column aggregators | 1.5 | **drafted 2026-11-08** → [`S-25-corpus-coverage.md`](S-25-corpus-coverage.md) |
| S-26 | Vim grammar (ranges, visual, opt-in vim profile) + TUI conventions | — | **drafted 2026-11-08** → [`S-26-vim-grammar-tui-conventions.md`](S-26-vim-grammar-tui-conventions.md) |
| S-27 | Conditional formatting (`:color`, `:scale`, data bars, Lua styles) | — | **drafted 2026-11-08** → [`S-27-conditional-formatting.md`](S-27-conditional-formatting.md) |
| S-28 | `:s` substitute on a column, search-hit sheet | — | **drafted 2026-11-08** → [`S-28-substitute-search-hits.md`](S-28-substitute-search-hits.md) |
| S-29 | Sessions (ex-script `.vdrs`), Lua event hooks (`vd.on`, `vd.exec`) | — | **drafted 2026-11-08** → [`S-29-sessions-lua-hooks.md`](S-29-sessions-lua-hooks.md) |
| S-30 | Live derived sheets (`:refresh`, `:link`), cell notes, selection stats | — | **drafted 2026-11-08** → [`S-30-linked-sheets-notes-selection-stats.md`](S-30-linked-sheets-notes-selection-stats.md) |

**Not drafted yet** (draft only when the work starts):

| ID | Title | Phase | Trigger |
|---|---|---|---|
| S-16 | Rest of the Lua data/app API (`vd.frame`, pop sheet, sandbox) + `since`/`deprecate` registry | 3 | first plugin that needs data access from Lua (`vd.toast`, `vd.sheet`, function bindings, `vd.push_sheet` shipped 2026-11-05) |
| S-17 | Pivot setup sheet (beyond the `:pivot` live preview shipped 2026-11-05) | 2 | the preview proves too little |

Writing order constraint: don't draft S-05+ until the matching phase actually starts — reality edits specs for free when written late. S-13/S-14 drafted post-Phase 0 to pin Tier 1 scope + test strategy. S-15 drafted post-Phase 1.5 as the authoritative "what's shipped" reference — update it when a user-visible surface changes.
