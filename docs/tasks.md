# Tasks

Phase-ordered punchlist. `[spec?]` = a spec must land before starting (see `specs/`).

## Phase 0 — viewer parity (csvlens++)
- [ ] Prep: extract VisiData default binding table from `../visidata/visidata/**/*.py` (grep `addCommand\(`) → canonical TOML
- [ ] CSV/TSV reader via Polars `scan_csv` (S-03)
- [ ] Parquet reader via Polars `scan_parquet` (S-03)
- [ ] Arrow IPC reader (S-03)
- [ ] JSON Lines reader (S-03)
- [ ] stdin reader with `SeekableFile` tempfile fallback (S-03)
- [ ] Ratatui app loop with `tokio::select!` + crossterm `EventStream` (S-02)
- [ ] `Action` enum with `affects_data()` + serde snake_case (S-02)
- [ ] Materialize task: `plan.slice(offset, WINDOW).collect()` on tokio task; cancel-and-replace on window slide (S-02)
- [ ] Panic hook: restore terminal, abort cleanly (S-02)
- [ ] Sheet stack: `Vec<Sheet>` + `q` to pop; sheet-switcher sheet (S-02)
- [ ] Keymap loader: parse TOML, build `Keymap` with per-layer chord vecs (S-04)
- [ ] Chord resolver + which-key overlay with 1000 ms timeout (S-04)
- [ ] Count prefix (`3j`) with digit-vs-command resolution rules + tests (S-04)
- [ ] `.` repeat capturing resolved `Action` (S-04)
- [ ] Scrollable table widget over `window` DataFrame
- [ ] Column freeze (left-pinned)
- [ ] Substring search `/` with next/prev
- [ ] Row filter by regex on current column
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
