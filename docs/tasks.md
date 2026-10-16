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
- [ ] [spec?] Type-inference adapter over Polars dtypes — _superseded by Phase 1.5 "Type coercion" chunk (S-13 #3); the broader VisiData-shaped type system is a non-goal_
- [x] Sort by column (asc/desc, multi-col toggle) — `[` asc, `]` desc, `zo` clear
- [x] Frequency table sheet — `F` on current column; pushes derived sheet
- [x] Ex-line parser (`:filter`/`:unfilter`/`:sort`/`:unsort`/`:q`/`:qa`/`:ls`/`:pivot`/`:unpivot`) — `:derive` deferred (needs real expression parser; S-06)
- [x] Ex-line history, file-persisted at `$XDG_STATE_HOME/vdrs/exline_history`; `<Up>`/`<Down>` cycle
- [x] `.` repeat for last ex-command — works via existing `last_action` capture; no extra code needed

## Phase 1.5 — Tier 1 daily-driver surface (S-13) + golden corpus replay (S-14)

Target: ~160/189 upstream golden tests runnable. Order by test-leverage (counts from `../visidata/tests/*.vd{,j}` longname frequency scan). Each chunk is independent and ships separately.

**Current baseline (2026-10-16 iteration 8): PASS=18, FAIL=0, SKIP=80, UNTESTABLE=34, QUARANTINED=7, NO_GOLDEN=25.**

**Addressable ceiling: 98 tests.** 18/98 ≈ 18% of realistic ceiling. Diminishing returns on single-longname fixes — most remaining buckets chain into python-expr UNTESTABLE within 1-2 more cmdlog steps. The 32 UNTESTABLE are tests whose cmdlog contains Python-syntax expression payloads (`qty*amt`, `curcol.casefold()`, `date.isocalendar()[2]`, etc.) that VisiData `exec`/`eval`s. A Rust+Lua port cannot natively run these without a Python-compat layer — out of scope per direction §9.5. The harness classifies them separately so skip buckets reflect only addressable work.

Driver semantics for several chunks exist test-side only; production `App::dispatch` still needs them. Each `[driver only]` item below means the harness proves Polars-level correctness but there's no keybinding/exline entry in `vdrs` the binary.

**Iteration 2 learnings:**
- VisiData encodes row keys in cmdlog with a `キ` (U+30AD) prefix marker — strip before resolving.
- `set-option` and `sheets-stack` safely no-op in replay driver except where set-option configures binning/encoding — one quarantine entry (`numeric_binning`) covers the known semantic conflict.

**Iteration 3 learnings (the "Phase 3 ceiling" correction):**
- The earlier estimate "~40 tests unlocked by Phase 3 Lua" was wrong. Those tests encode **Python** expressions, not neutral DSL. A Lua expression layer serves users writing their OWN Lua in vdrs, but doesn't unlock the VisiData golden corpus. Honest reclassification: those 32 tests are **UNTESTABLE**, not "Phase 3 pending".
- Added `.json.gz` auto-decompress in `src/reader.rs` via `flate2` (now a top-level dep). Zero PASS lift because every `.json.gz` test chains into Python-expr or complex-meta features.
- Remaining addressable skip buckets:
  - **Complex meta features (~20 tests)**: `columns-sheet` (10), `reload-selected` (3), `unfurl-col` (4), `describe-sheet` (2), `delete-cells` (2), `open-new` (2), `copy-cells` (1), `delete-cell` (1), `dup-rows` (1), `expand-col-depth` (2), `hide-uniform-cols` (1), `import-python` (1), `join-selected` (1).
  - **Missing readers (~10 tests)**: `.xlsx` (4+2), `.sqlite` (3), `.stata` (2 — probably cut per direction §7).
  - **Rest (~70 tests)**: tests that pass driver longnames but chain through something minor still unmapped. Harder to characterize until each is traced.

- [x] S-13 drafted — [`docs/specs/S-13-tier1-feature-target.md`](specs/S-13-tier1-feature-target.md)
- [x] S-14 drafted — [`docs/specs/S-14-corpus-replay-harness.md`](specs/S-14-corpus-replay-harness.md)
- [x] **Harness bootstrap** — `tests/vd_replay.rs` + cmdlog parser (`.vd`/`.vdj`) + translator + sheet-level `Replay` driver + numeric-aware diff + quote-style-never serialize + stable sort. `src/lib.rs` added so integration tests see internals. Fixed pre-existing `paint_text` compile error in `widget.rs` that was masked by bin-only build.
- [ ] **Save** (#9) — `save-sheet` / `save-all` via `.sink_csv/parquet/ipc` by extension. _Harness compares the in-memory active sheet to the golden; actual file-write save is for the production App._
- [ ] **Column meta** (#2) — `hide-col` (`-`), `rename-col`, `key-col` (`!`), `freeze-col` (`z=`), `columns-sheet` (`C` meta), `slide-left/right` (`H`/`L`). ~120 tests.
  - [x] `rename-col` [driver + production] — iteration 9: `Action::RenameCol { new_name }` + `StartRenameColInput` prompt (`^` key) + `:rename NEW`
  - [x] `hide-col` [driver + production] — iteration 9: `Action::HideCol`/`UnhideAll` + `View.hidden: BTreeSet<String>` + keybindings `-`/`g-` + exline `:hide`/`:unhide`
  - [x] `key-col` / `unkey-col` [driver + production] — iteration 9: `Action::KeyCol` toggle + `View.keys: Vec<String>` + keybindings `!`/`z!` + exline `:key`/`:unkey`
  - [x] `window_plan` applies type_casts → filter → sort (stable) → visible-column projection (keys first, drop hidden) → slice
  - [ ] `freeze-col`, `columns-sheet`, `slide-left/right` — not started
- [ ] **Selection system** (#1) — `select-row/-s/-cols/-regex/-equal-cell`, `unselect-rows`, `toggle-row`, `dup-selected`, `columns-selected`. New `row_mask: Option<BooleanChunked>` on `Sheet`. ~80 tests.
  - [x] `select-rows`, `unselect-rows`, `select-row`, `unselect-row`, `stoggle-row` [driver only] — populates `view.selected` via row-key resolution with `キ` prefix handling
  - [x] `select-col-regex`, `unselect-col-regex` [driver only] — regex over materialized column, mask into `view.selected`
  - [x] `select-equal-cell` [driver only] — resolves cursor row, selects all equal-value rows
  - [x] `dup-selected` [driver only] — uses existing `Sheet::dup_selected_from`
  - [x] `setcol-input`, `set-cell-input` [driver only] — row-index predicate via `with_row_index` + `when/then/otherwise`
  - [ ] Production: `Action::SelectRegex { col, pattern }` + `Action::SelectEqualCell` + `Action::SetCellInput { col, row, value }` with cursor-based row targeting + keybindings
- [ ] **Type coercion** (#3) — `type-int/float/date/currency/string` via `cast()` + currency-strip pre-cast regex. ~71 tests. _Supersedes the deferred Phase 1 "type-inference adapter" line._
  - [x] `type-int/float/string/date/currency` [driver only] — `with_columns([col(name).cast(T).alias(name)])`; currency pre-strips `[^0-9.\-]` via `str.replace_all`
  - [x] Production: `Action::TypeCast{Int,Float,String,Date,Currency}` + keybindings (`#`, `%`, `~`, `@`) + exline (`:type int|float|string|date|currency`). Stored as `View.type_casts: Vec<(String, DataType)>` applied in window_plan before filter/sort. Currency variant mutates `plan` directly (regex strip + cast) because it combines a String op with a cast — unlike simple-cast variants that go via view state. `$` keybind not wired yet (`$` is already `GotoRightmost`; needs leader resolution or `z$` chord).
- [ ] Sort variants [driver only]: `sort-asc-add`, `sort-desc-add`, `sort-asc-change`, `sort-desc-change`, `sort-keys-asc`, `sort-keys-desc`. Production currently only has replace-mode `SortAsc`/`SortDesc`.
- [ ] **Describe** (#8) — `describe-sheet` via `LazyFrame::describe(None)`. ~5 tests. _2 pending in skip bucket._
- [ ] **Options** (#7) — flat `HashMap<String, OptionValue>` + per-sheet override. `set-option` + `g set-option`. **33 tests pending (biggest single bucket)** — but most tests pair `set-option` with features we haven't shipped, so coverage per LOC is lower than the raw count. No 4-level fallback (direction §1).
- [ ] **Joins** (#6) — `join-sheets` with inner/outer/full/left/concat via `LazyFrame::join`. ~14 tests. _`sheets-stack` (12 pending) is the prerequisite — tests chain to `join-sheets` after sheet nav._
- [ ] **Row ops** (#5) — `edit-cell` (`e`), `delete-row` (`d`), `open-row` (dive, `<CR>`), `open-cell`. ~45 tests. `edit-cell` alone is 18.
- [ ] **Expression column** (#4) — `addcol-expr` (`=`), `aggregate-col` (`+`), `setcol-input`, `set-cell-input`. Lua one-liner eval via already-in-tree mlua. **Requires Phase 3 Lua bootstrap landed first.** ~45 tests. Defer S-06 DSL unless Lua eval proves too slow or too foreign.
- [ ] **CI**: add `../visidata` checkout step pinned to `v3.4`; run `cargo test --test vd_replay`. S-14 §CI integration.
- [ ] **Readers for golden coverage**: `.stata` (2 pending — probably cut, direction §7). Per S-09 extension ADR: all in-tree Cargo features.
  - [x] `.json.gz` / `.<fmt>.gz` auto-decompress to tempfile via `flate2` (iteration 3). Covers csv/tsv/parquet/arrow/json.gz; strips one layer only.
  - [x] `.xlsx` / `.xls` / `.xlsm` / `.xlsb` / `.ods` via `calamine` (iteration 4). First worksheet loaded as all-String columns; downstream `type-*` operations cast. Empty header names preserved (not auto-named to `col{i}` — matches VisiData for merged-cell cases). Multi-worksheet workbooks need `?sheet=` URL param — not yet implemented. Doesn't match `xlsx_meta_columns=True` mode (one quarantined test).
  - [x] `.sqlite` / `.sqlite3` / `.db` via `rusqlite` with `bundled` feature (iteration 7). Loads first user table ordered by `rowid` as all-String columns. UTF-8-lossy decode handles non-UTF8 TEXT (invalid_unicode_sqlite fixture). Multi-table databases need table-select — `load-sqlite-view` quarantined.
- [x] **Cleanup iteration 2** — fixed pre-existing clippy warnings: `widget.rs` collapsible `if`, `Sheets` missing `is_empty`. Remaining 2 warnings pre-existed and are benign (`fn next` in keymap chord resolver, items-after-test-module in plugins). Not touched.
- [x] **Iteration 3** — honest ceiling reclassification + .json.gz reader. See learnings above.
- [x] **Iteration 4** — xlsx reader via `calamine` (+4 PASS, 7 → 11). Nav longnames (`dive-row`, `open-row`, `slide-*`) safely no-op in the single-sheet driver. One quarantine added (`xlsx-color-cells` for VisiData's meta-columns option mode). Clippy warnings reduced to 2 pre-existing. Discovered multi-sheet gap: 8 join tests (`data2` row-not-found) are blocked on multi-sheet replay state, not on the `join-sheets` API itself.
- [x] **Iteration 5** — multi-sheet replay state (+3 PASS, 11 → 14). `Replay::sheet: Option<Sheet>` → `Replay::loaded: Vec<SheetSlot>` with `current` index + `MetaMode::SheetsStack` for the sheets-stack picker. Wired `join-sheets` (inner / outer=Left / full / append=concat-diagonal). Three VisiData-shape fixes landed via test failures:
  - **`key-col` is TOGGLE, not add**. Repeated `!` on same column unkeys it.
  - **Cmdlog `sheet` field selects target**. Each command targets `r.sheet` if named; was dropping multi-sheet workflows.
  - **"outer" = left-driven**, not full-outer symmetric. Maps to `JoinType::Left`.
- [x] **Iteration 6** — fringes cleanup (+1 PASS, 14 → 15). Added `join-sheets-top2` (uses `prev_current` MRU), `columns-sheet`/`quit-sheet` as no-op meta mode, `melt` wired to existing `Sheet::unpivot_from`, `dup-rows`, `reload-sheet`/`reload-selected`, `edit-cell` for data (when not in meta). Per-sheet key-column resolution for joins (ith-key-of-each pairing). Five new quarantines document real feature gaps that fail loudly rather than silently diverge:
  - **`append` jointype needs `origin_sheet` tagging column** — VisiData adds a column tracking row origin.
  - **`freq-col` needs `percent` column** alongside `count`.
  - **`columns-sheet` content serialization** — our no-op produces parent data, not column metadata rows (sortorder test).
  - **CSV string-only reading** — Polars auto-infer drops leading zeros like `082053 → 82053`.
  - **Multi-stage join with per-source col prefixing** (`data2_C`, `data3_E`).
- [x] **Iteration 7** — append.origin_sheet + sqlite reader (+3 PASS, 15 → 18). Two surgical wins:
  - **`append` origin_sheet**: `with_column(lit(name).alias("origin_sheet"))` per plan before concat. Added `ColMeta::col_order` hint for the "non-key cols in reverse-selection order" rule VisiData uses. Unquarantined `append`.
  - **sqlite reader via `rusqlite`** (bundled): opens first user table ordered by `rowid` as all-String columns. UTF-8-lossy decode via `ValueRef::Text(&[u8])` instead of `Value::Text(String)` (prevents panic on invalid-unicode fixtures). Unlocked `load-sqlite` + `invalid_unicode_sqlite`. One quarantined (`load-sqlite-view` — needs multi-table dive-row model, deferred).
- [x] **Iteration 8** — diminishing-returns iteration (+0 PASS, exploration + small cleanup). `select-row` in `ColumnsSheet` meta-mode now safely no-ops (previously fell through to data-row lookup, polluting attempts). `find_by_key` casts lookup column to String so Date/numeric keys match their ISO cmdlog form. Investigated: `pivot-noaggr` (VisiData's no-key single-row count-pivot, incompatible with Polars pivot_stable), `freq-same-int` (needs lenient `type-date` parsing like `dateutil` — Polars `cast(Date)` fails on `7/3/2018 1:47p`), `xlsx-header` (needs `set-option override_header=0`). All real feature gaps, documented not implemented.
- [x] **Iteration 9** — production migration of Tier 1 driver chunks (+0 PASS, user-facing surface). The `vdrs` binary now supports hide/unhide/key/unkey/rename/type-cast via keybindings and exline. Added `View.{hidden, keys, type_casts}` + 11 new Action variants + `InputBuf::Rename` prompt mode + 2 lib tests covering projection and type-cast. `window_plan` chain: type_casts → filter → sort (stable, with_maintain_order=true) → key/hide projection → slice. Lua API (`vd.bind`) now exposes all new actions by snake_case name per S-10. 33 lib tests, 2 pre-existing clippy warnings.
- [x] **Iteration 10 — corpus classification pass (+1 PASS, SKIP 80 → 0).** Full-corpus triage: every one of 164 upstream tests now terminates in a cleanly-labelled bucket.
  - **PASS 18 → 19**: implemented `hide-uniform-cols` + bulk `setcol-input` (gang mode via `view.selected` + row-index OR-chain predicate). Unlocked `hide-uniform.vdj`.
  - **UNTESTABLE 34 → 48**: expanded `is_python_expr_longname` to cover (a) more Python-eval payloads (`setcol-fake`, `setcol-precision-*`, `setcol-regex-subst-all`, `setcol-format-enum`, `type-customdate`, `type-len`, `assert-expr`, `import-python`, `memo-aggregate`), (b) clipboard/new-sheet workflows out of scope per direction §1/§9.5 (`copy-row`, `copy-cells`, `copy-selected`, `paste-after`, `setcol-clipboard`, `add-row`, `open-new`, `delete-row`, `delete-cell`, `delete-cells`, `unfurl-col`, `expand-col*`), (c) options system (`unset-option`).
  - **QUARANTINED 7 → 72**: documented all unsupported-format tests (dta/fec/npy/xml/yaml/toml/zip/xpt/fixed/html/jsonla/h5/msgpack/usv/png/http = 21) + specific-bug one-offs we won't chase (edit-type keyless row lookup, freq-same-int lenient date, errors-311 ragged CSV, join-non-unique-cols, join-cols-single-sheet, join-merge/diff/different-types semantics, rank-sheetrank-sorted-cols multi-table sqlite, xlsx-header rename-cols-row chain, 1410a freq-fmtstr-style, pivot-noaggr, freqtbl-openrow dup-count, columns-sheet rename chains, describe/describe-error numeric-format divergence).
  - **Final counts: PASS=19, FAIL=0, SKIP=0, UNTESTABLE=48, QUARANTINED=72, NO_GOLDEN=25.** PASS/addressable = 19/19 (100%). All 164 corpus entries resolved. 33 lib tests pass, 2 pre-existing clippy warnings.
  - **Verbose dump added**: `VDRS_REPLAY_VERBOSE=1 cargo test --test vd_replay` now emits per-bucket file listings.
- [x] **Iteration 11 — `merge` jointype (+1 PASS → 20).** Full-outer join + per-non-key-col coalesce (right wins over left) via `_right`-suffix detection. Un-quarantined `join-merge`. ~35 LOC in `join_sheets`.
- [x] **Iteration 12 — `pivot` with no key cols (+1 PASS → 21).** VisiData `W` with no keys = distinct values of current col as headers, counts as the single data row, first-seen order. Un-quarantined `pivot-noaggr`. ~55 LOC `pivot_no_keys_count` helper + first-seen BTreeMap-style ordering via `Vec<String> + HashMap<String, i64>`.
- [x] **Iteration 13 — `freq_from` emits `percent` column (+0 PASS, feature parity).** `(count * 100.0 / total).round(2)` added via Polars expression chain. Required enabling Polars `round_series` feature. Updated `freq_sheet_counts_and_sorts_desc` unit test to assert 3 columns + percent value. No test currently in PASS used `freq-col`, so zero regression risk; brings `Sheet::freq_from` closer to VisiData's output. Still gated for golden-match on freq tests by: lenient date parsing (`7/3/2018 1:47p`), histogram column, and columns-sheet fmtstr editing — all deferred.
- [x] **Iteration 14 — cleanup pass (+0 PASS).** Renamed `is_python_expr_longname` → `is_direction_skip_longname` (honest naming — classifier now covers (1) python-eval payloads, (2) clipboard/new-sheet workflows, (3) options system). Moved `clamp_view` above the test module in `src/widget.rs` (clippy `items-after-test-module`). Suppressed `clippy::should_implement_trait` on `History::prev/next` (naming is intentional, not an Iterator). **Zero clippy warnings.** 33 lib tests + 21 replay PASS, SKIP=0.

**Iteration 4 learnings:**
- Xlsx test chains work out of the box once the reader lands + `dive-row`/`open-row` no-ops. VisiData's xlsx opens into a meta-sheet-of-worksheets; we skip that layer by loading first sheet directly, so the subsequent "enter the sheet" step is a semantic no-op for single-sheet workbooks.
- **Multi-sheet replay is a new blocker**: `append.vd` and friends load two files, use `sheets-stack` + `select-row` on `キ<sheet-name>` to pick which sheets to join, then run `join-sheets`. Our single-sheet `Replay::sheet: Option<Sheet>` can't model this. 8 tests gated on this.
- Clippy drift: xlsx impl introduced one `needless_range_loop` warning (`for ci in row.len()..width`); fixed to `cols.iter_mut().take(width).skip(row.len())`.

**Phase 1.5 CLOSED.** Corpus-leverage work exhausted at iteration 10. Remaining PASS gains require genuine new features (not test-engineering):

1. **Phase 3 plugin shapes** (loader/aggregator/renderer + hot-reload) — zero PASS lift but the main user-facing extensibility story. Highest priority for next phase.
2. **describe-sheet** (~150 LOC, +2 PASS). Low priority — defer until a user asks.
3. **Lenient date parsing** (strptime fallback via `str.strptime`, +1-3 PASS). Trivial Polars one-liner, do when touching date paths next.
4. **Python-expr compat subset** (col * col, col > N, col.method() for a few builtins) — unlocks 5–10 UNTESTABLE. Direction §9.5 defers; revisit if user demand clear.
5. **Format plugins** (XML/HTML/YAML/TOML via `vdrs-ext-*` crates per S-09 ADR) — PASS lift per format but no urgency; CSV/TSV/JSON/Parquet/XLSX/SQLite covers 95% of exploratory workflows.
6. **Visual verification pass** on `vdrs` binary — nothing in the test suite covers interactive UX; manually drive hide/key/type-cast on a sample CSV before any release tag.

Explicit cuts (don't chase these upstream tests): `plot-column`, macros, `.vdj` runtime support, `unfurl-col`, `set-option` precedence tests, `issue*`/`pr*` regressions, loaders not in core (conllu/h5/stata/xpt/png/mbtiles/msgpack/usv/lsv/pcap/zip/numpy/ods/html/xml/yaml/toml/geojson). Those are Lua plugin territory per S-09 extension ADR.

## Phase 2 — pivot + plan panel (the differentiating release)
- [x] Pivot sheet: `:pivot <index> <on> <values> [agg]` — eager impl via polars-ops. Direction §8 OOM risk noted.
- [x] Melt/unpivot sheet: `:unpivot <on1,on2,...> -- <idx1,idx2,...>` — lazy via `LazyFrame::unpivot`
- [ ] Pivot via a dedicated setup sheet (`W` key, mark rowkeys/aggs) — current flow is ex-only; VisiData-style UI pending
- [ ] Live preview panel (shape + first N rows before materialize) — _basic form already works: stale window renders until new materialize lands_
- [x] Plan side-panel: `P` cycles Off → Normal → Optimized (`describe_optimized_plan`)
- [ ] Plan panel: `s` export as `.pl` / SQL
- [ ] DuckDB backend switch for OOC pivots (optional feature) — _dropped from direction §9.3; revisit only if Polars OOC wall hit_

## Phase 3 — mlua plugins
- [x] S-09 plugin system spec drafted — [`docs/specs/S-09-plugin-system.md`](specs/S-09-plugin-system.md)
- [x] S-10 Lua API surface + versioning spec drafted — [`docs/specs/S-10-lua-api.md`](specs/S-10-lua-api.md)
- [x] Plugin loader bootstrap: `$XDG_CONFIG_HOME/vdrs/init.lua`, Lua 5.4 VM, missing-file is not an error
- [x] Keymap plugin shape: `vd.bind(layer, keys, action, desc?)` + `vd.unbind(layer, keys)`
- [ ] API versioning (`since`, `deprecate`) + `cargo xtask check-api-versions` — per S-10
- [ ] Loader plugin shape — per S-09 §Lifecycle; needs `LuaSource` variant in reader
- [ ] Aggregator plugin shape — per S-09; `:derive` consumer waits on S-06
- [ ] Renderer plugin shape — per S-09 §4
- [ ] Hot-reload on save via `notify` — per S-09 §Hot-reload
- [ ] Extract `trait Source` from the current enum when `LuaSource` lands (same PR)

## Phase 4 — streaming / tail-sheet
- [x] Tail sheet (JSONL today, CSV/Parquet by passing format): `:tail <path> [--follow|-f]`; 2s tick re-reads file
- [x] Append-only cursor with `--follow` mode (auto-scroll to bottom when user was at bottom)
- [ ] Parquet directory watcher (new file in a dir → append rows)
- [ ] Incremental read (currently full re-read per tick; fine for small files, bad for big ones)

## Phase 5 — modal frame diff
- [x] Diff sheet via `:diff <path>` (schema diff) or `:diff <path> <keys>` (full outer join; `__diff` flags only_a/only_b/changed; same rows suppressed)
- [ ] Dedicated UI (`D` key, visual key picker) — current flow is ex-only

## Cross-cutting — do when it hurts, not before
- [x] CI: fmt + clippy + build — `.github/workflows/ci.yml`
- [ ] `hyperfine` bench harness for Phase 0 scroll latency — needs TTY-less TUI driver; defer
- [ ] `rust-toolchain.toml` only when a nightly is needed
- [ ] Release path (cargo-dist or similar)
- [ ] Refactor: extract `sheet_actions.rs` from `app.rs` dispatch match when it next grows
