# Tasks

Phase-ordered punchlist. `[spec?]` = a spec must land before starting (see `specs/`).

## Phase 0 — viewer parity (csvlens++)
- [ ] Prep: extract VisiData default binding table from `../visidata/visidata/**/*.py` (grep `addCommand\(`) → canonical TOML — _deferred: Phase 0 ships only the nav/freeze/search subset, baked in; full extract waits until a user actually asks for VisiData parity on a non-nav key_
- [x] CSV/TSV reader via Polars `scan_csv` (S-03)
- [x] Parquet reader via Polars `scan_parquet` (S-03)
- [x] Arrow IPC reader (S-03)
- [x] JSON Lines reader (S-03)
- [x] stdin reader with `SeekableFile` tempfile fallback (S-03)
- [x] Ratatui app loop with `tokio::select!` + crossterm `EventStream` (S-02)
- [x] `Action` enum with `affects_data()` + serde snake_case (S-02)
- [x] Materialize task: `plan.slice(offset, WINDOW).collect()` on tokio task; cancel-and-replace on window slide (S-02)
- [x] Panic hook: restore terminal, abort cleanly (S-02)
- [x] Sheet stack: `Vec<Sheet>` + `q` to pop (S-02) — sheet-switcher view still deferred (needs a tiny meta-sheet)
- [ ] Keymap loader: parse TOML, build `Keymap` with per-layer chord vecs (S-04) — _baked-in defaults ship; TOML loader deferred until Phase 3 lands config alongside Lua_
- [x] Chord resolver + which-key overlay with 1000 ms timeout (S-04) — _resolver + pending-indicator in status line; full overlay panel deferred_
- [x] Count prefix (`3j`) with digit-vs-command resolution rules + tests (S-04)
- [x] `.` repeat capturing resolved `Action` (S-04)
- [x] Scrollable table widget over `window` DataFrame
- [x] Column freeze (left-pinned)
- [x] Substring search `/` with next/prev
- [x] Row filter by regex on current column
- [x] Hardcoded color scheme (user-override comes with plugin system)
- [x] CLI entrypoint: `vdrs <path>`, `--headless`, `--format`, `--head`, `-` for stdin
- [x] Window-slide on continuous `j`/`k` past window edge
- [x] Which-key full modal overlay (candidate list panel at bottom-right)
- [x] TestBackend-based render snapshot test for `SheetWidget`
- [x] Toast TTL (clears on next key — simpler than a timer)
- [x] `cargo clippy --no-deps` is clean — fmt-check is clean too
- [x] Sheet-switcher meta-sheet — `S` key or `:ls`; `<CR>` switches to row's sheet
- [x] `Action::SwitchToSheet(SheetId)` + `SubmitRow` on `<CR>` in sheet-switcher
- [x] Clean up `materialize_handles` on sheet pop

## Phase 1 — types, sort, freq, ex-line
- [ ] [spec?] Type-inference adapter over Polars dtypes — _polars dtypes already pass through unchanged; adapter only needed when VisiData-shaped type coercion lands_
- [x] Sort by column (asc/desc, multi-col toggle) — `[` asc, `]` desc, `zo` clear
- [x] Frequency table sheet — `F` on current column; pushes derived sheet
- [x] Ex-line parser (`:filter`/`:unfilter`/`:sort`/`:unsort`/`:q`/`:qa`/`:ls`/`:pivot`/`:unpivot`) — `:derive` deferred (needs real expression parser; S-06)
- [x] Ex-line history, file-persisted at `$XDG_STATE_HOME/vdrs/exline_history`; `<Up>`/`<Down>` cycle
- [x] `.` repeat for last ex-command — works via existing `last_action` capture; no extra code needed

## Phase 2 — pivot + plan panel (the differentiating release)
- [x] Pivot sheet: `:pivot <index> <on> <values> [agg]` — eager impl via polars-ops. Direction §8 OOM risk noted.
- [x] Melt/unpivot sheet: `:unpivot <on1,on2,...> -- <idx1,idx2,...>` — lazy via `LazyFrame::unpivot`
- [ ] Pivot via a dedicated setup sheet (`W` key, mark rowkeys/aggs) — current flow is ex-only; VisiData-style UI pending
- [ ] Live preview panel (shape + first N rows before materialize) — _basic form already works: stale window renders until new materialize lands_
- [x] Plan side-panel: `P` cycles Off → Normal → Optimized (`describe_optimized_plan`)
- [ ] Plan panel: `s` export as `.pl` / SQL
- [ ] DuckDB backend switch for OOC pivots (optional feature) — _dropped from direction §9.3; revisit only if Polars OOC wall hit_

## Phase 3 — mlua plugins
- [ ] [spec?] Plugin loader (`init.lua`, discovery, sandbox, error reporting)
- [ ] [spec?] API versioning (`since`, `deprecate`)
- [ ] Loader plugin shape
- [ ] Keymap plugin shape (feeds the deferred TOML loader; `~/.config/vdrs/keymap.toml` + Lua `vd.bind()`)
- [ ] Aggregator plugin shape
- [ ] Hot-reload on save
- [ ] Extract `trait Source` from the current enum when `LuaSource` lands

## Phase 4 — streaming / tail-sheet
- [ ] [spec?] Streaming source abstraction (parquet dir watcher, jsonl tail)
- [ ] Append-only cursor sheet with tail-mode

## Phase 5 — modal frame diff
- [x] Diff sheet via `:diff <path>` (schema diff) or `:diff <path> <keys>` (full outer join; `__diff` flags only_a/only_b/changed; same rows suppressed)
- [ ] Dedicated UI (`D` key, visual key picker) — current flow is ex-only

## Cross-cutting — do when it hurts, not before
- [x] CI: fmt + clippy + build — `.github/workflows/ci.yml`
- [ ] `hyperfine` bench harness for Phase 0 scroll latency — needs TTY-less TUI driver; defer
- [ ] `rust-toolchain.toml` only when a nightly is needed
- [ ] Release path (cargo-dist or similar)
- [ ] Refactor: extract `sheet_actions.rs` from `app.rs` dispatch match when it next grows
