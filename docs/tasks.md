# Tasks

Phase-ordered punchlist. `[spec?]` = a spec must land before starting (see `specs/`).

## Phase 0 — viewer parity (csvlens++)
- [ ] [spec?] CSV/TSV reader via Polars `scan_csv`
- [ ] [spec?] Parquet reader via Polars `scan_parquet`
- [ ] [spec?] Arrow IPC reader
- [ ] [spec?] Ratatui app loop (Model/Update/View, render-on-event)
- [ ] Scrollable table widget over DataFrame
- [ ] Column freeze
- [ ] Substring search `/` with next/prev
- [ ] Row filter by regex on current column
- [ ] crossterm input + keybind dispatcher w/ `g` / `z` prefix maps
- [ ] Hardcoded color scheme (user-override comes with plugin system)

## Phase 1 — types, sort, freq, ex-line
- [ ] [spec?] Type-inference adapter over Polars dtypes
- [ ] Sort by column (asc/desc, multi-col toggle)
- [ ] Frequency table sheet
- [ ] [spec?] Ex-line parser (`:filter`, `:sort`, `:derive`, `:q`)
- [ ] Ex-line history, file-persisted
- [ ] `.` repeat for last ex-command

## Phase 2 — pivot + plan panel (the differentiating release)
- [ ] [spec?] Pivot sheet: rowkeys / colkeys / aggs → LazyFrame
- [ ] [spec?] Melt sheet
- [ ] Live preview panel (shape + first N rows before materialize)
- [ ] Plan side-panel; `o` show optimized plan, `s` export as `.pl` / SQL
- [ ] DuckDB backend switch for OOC pivots (optional feature)

## Phase 3 — mlua plugins
- [ ] [spec?] Plugin loader (`init.lua`, discovery, sandbox, error reporting)
- [ ] [spec?] API versioning (`since`, `deprecate`)
- [ ] Loader plugin shape
- [ ] Keymap plugin shape
- [ ] Aggregator plugin shape
- [ ] Hot-reload on save

## Phase 4 — streaming / tail-sheet
- [ ] [spec?] Streaming source abstraction (parquet dir watcher, jsonl tail)
- [ ] Append-only cursor sheet with tail-mode

## Phase 5 — modal frame diff
- [ ] [spec?] Diff sheet: two frames + key cols → schema diff + row diff view

## Cross-cutting — do when it hurts, not before
- [ ] CI: fmt + clippy + build
- [ ] `hyperfine` bench harness for Phase 0 scroll latency
- [ ] `rust-toolchain.toml` only when a nightly is needed
- [ ] Release path (cargo-dist or similar)
