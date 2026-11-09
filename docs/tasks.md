# Tasks

Phase-ordered punchlist. `[spec?]` = a spec must land before starting (see `specs/`).
What the binary supports today: [`specs/S-15-feature-inventory.md`](specs/S-15-feature-inventory.md) (§10 cut, §10.1 not yet supported).

## Open issues & gaps

Rescan 2026-11-05 list — all closed on branch `feature/open-features` the same day:

- [x] Harness-only features → binary: `:join` (inner/left/full/merge/append), `:uniform`, `:setcol`, `:dup`/`g"`, `W` (pivot with/without keys), `,` select-equal
- [x] Background jobs for search, select(-all), `|`, `,`, `"`, `I`, `W`, `:pivot`, `:save`, `:uniform`
- [x] Selections key on row ids → survive sort/filter
- [x] Broken `init.lua` keeps earlier registrations; hot reload
- [x] `vd.bind` accepts `":ex commands"`; input-layer action names exposed
- [x] Quoted ex-line args; readline-style prompt editing
- [x] CSV date inference; lenient `@`
- [x] Multi-sheet xlsx / multi-table sqlite list
- [x] Incremental `:tail`; Parquet directory tail
- [x] No `Box::leak` in `vd.bind`
- [x] Column widths include on-screen rows
- [x] Freq dive: `<CR>` / `g<CR>` on a frequency sheet (user report)

Done in the same pass (2026-11-05): live `:pivot` preview; Lua function bindings, `vd.toast`, `vd.sheet()` (api 2); `vd.push_sheet` (api 3); renderer memo + widths; Lua aggregators off the UI thread; deterministic join / `:sql` order; `:export` for derived sheets, describe, row diff, `W`, Arrow/Excel/SQLite sources; `path:part` targets. Later: `vd.command` (api 4, 2026-11-07).

## Roadmap (ordered, 2026-11-08)

One list, worked top to bottom. Each item ships alone with its spec's test plan and updates S-15 + the guide. Keymap rule for all of them: [S-26 §0](specs/S-26-vim-grammar-tui-conventions.md) — the default keymap matches VisiData; new or more standard meanings live in the opt-in vim profile. After each item that ships a VisiData command: translate its longnames in `tests/vd_replay.rs`, re-check the quarantine, raise `MIN_PASS`.

Cross-spec dependencies: `vd.option` (S-18 B) blocks S-23 options, S-25 P6, S-26 D; S-18 C's `Arc` rid set blocks S-21 B; S-18 E (stable rids) blocks S-26 B `:'<,'>d` and S-30 B; S-19 `Action::to_ex` blocks S-29 C; S-23 A lands `:plan` (= `:sidebar plan`) before S-18 F needs it.

1. - [x] Doc cleanup: specs/tasks/direction consistent with the 2026-11-08 roadmap
2. - [x] S-24 R1 — module layout: `app.rs` tests/render out, `sheet.rs` → `sheet/` (moves only) (0.5–1 d)
3. - [x] S-25 P0 — `VDRS_REPLAY_TRIAGE=1`, quarantine rewritten by real blocker, `aggregate-*` out of UNTESTABLE, NO_GOLDEN investigation (0.5–1 d)
4. - [x] S-26 phase 0 — VisiData key alignment (§0 "phase 0" rows: `Space`, `$`/`gl`/`gh`, `g-`/`gv`, `<C-c>`/`gq`/`<C-q>` + job cancel, `<C-f>`/`<C-b>`, `/`/`g/`/`?`) (2–2.5 d), with S-21 A — `=` → `:derive`, `z/` search-expr, `:select` / `:unselect`, `z|` select-expr, regex filter → `gz|` (1 d)
5. - [x] S-18 A — copy to register: `y gy zy gzy` (0.5–1 d)
6. - [x] S-18 B — system clipboard: `Y gY zY gzY`, VisiData-style copy command / OSC 52, `vd.option` (Lua API 5)
7. - [x] S-25 P1 — translate shipped features' longnames (~26 files); `I` describe switches to VisiData's columns (1.5–2.5 d) — done 2026-11-08, PASS 30→35 (+6 DuckDB files → 41)
8. - [x] S-18 C — cell editing `e`, `ge`, `:edit`; edits after the column's last writer; `SetCol.rids` → `Arc<[u64]>`; S-26 A1 quit guard
9. - [x] S-21 B — `g=` / `:setexpr` (source sheets only, S-18 D3), `:filter-expr` (1–1.5 d) — done 2026-11-08
10. - [x] S-18 D — paste into cells: `zp gzp zx gzx` (1–2 d) — done 2026-11-08
11. - [x] S-24 R2 + S-18 E — window invariants behind `Sheet` methods; stable row ids (no-visible-change commit), then delete rows `d gd x gx` (3–5 d) — done 2026-11-08
12. - [x] S-23 A + S-18 F — sidebar `b` / `Ctrl+G` (pages plan, column, cell, help; `:plan` = `:sidebar plan`, `P` freed), then add/paste rows `a ga p P`, `A` open-new (4–6 d) — done 2026-11-08
13. - [x] S-18 G — system paste in: bracketed paste → register, `zP` `gzP`, `clipboard_paste_cmd` (1 d) — done 2026-11-09
14. - [x] S-20 — undo tree: branches, `:earlier` / `:later`, `:undotree` (1–2 d) — done 2026-11-09; the Lua `vd.undo_tree()` example waits for S-29 `vd.exec`
15. - [x] S-24 R3 + S-25 P2 — typed xlsx/sqlite; correctness fixes from FAIL/runtime buckets, incl. S-24 R4 zero-padded ids (3–4 d) — done 2026-11-09; replay PASS=46; same-named columns and malformed JSON lines stay out (S-25 P2 notes)
16. - [x] S-22 A/B/C — `(` `z(` `g(` expand, `)` contract; `zM` unfurl sheet; `:type json` (2.5–3 d) — done 2026-11-09; replay PASS=47
17. - [ ] S-25 P3 — columns sheet `C` (3–5 d); P4 — column aggregators `+` / `g+` / `z+` (1.5–2 d)
18. - [ ] S-19 — macros: `m` record, `macros.lua`, `vd.bind` lists, `Action::to_ex` (2–3 d)
19. - [ ] S-29 A–D — `vd.on` + `vd.exec`; `:source`; `:mksession`; autosave + `vdrs -S` (4–5 d)
20. - [ ] S-28 A/B — `:s`, `*` / `g*`, `PlanStep::Replace`; `:hits` sheet (2.5–3 d)
21. - [ ] S-30 C, A, B — selection stats; `:refresh` (`<C-r>`) + `:link`; cell notes `zn` (5–6 d)
22. - [ ] S-27 A/B/C — `:color` / `:nocolor`; `:scale`, `:databar`; Lua renderer styles (3–4 d)
23. - [ ] S-23 B — split panes `Z` / `gZ` / `Tab` (2–3 d); C1–C4 — `:plot` (`.` / `g.`, repeat → vim profile), `:hist`, `:bar`, pan/zoom, region select (8–11 d)
24. - [ ] S-26 A (rest), B, C, D, E — TUI conventions, ex ranges + `:g`/`:v`/`:normal`, visual `V`/`<C-v>`, vim profile, line-editor normal mode (11–14 d)

Evidence-gated, any time: S-25 P6 options subset (after S-18 B), S-25 P7 cheap formats (each a yes/no vs direction §7), S-21 C0 → C Python subset, S-16 (rest of the S-10 Lua API) when a Lua user asks, S-24 R5 release binaries when the alpha is finished (not before).

## Test layout

- `src/app/scenario.rs`: hand-written end-to-end tests.
- `src/app/golden.rs` + `tests/golden/<case>/`: key script → expected TSV + screen; exported `.sql`/`.py` run through DuckDB and Polars (`VDRS_PYTHON`, `VDRS_REQUIRE_ROUNDTRIP` in CI). `UPDATE_GOLDEN=1` rewrites expectations.
- `src/app/monkey.rs`: seeded random keys, undo/redo round trip.
- `tests/cli.rs`: the binary (arguments, stdin, exit codes).
- `tests/vd_replay.rs`: VisiData corpus; `MIN_PASS` floor.

## Phase 0 — viewer parity (csvlens++)
- [x] ~~Prep: extract VisiData default binding table~~ — superseded: S-26 §0 compares vdrs keys with VisiData's `addCommand`/`bindkey` table at `5f67ff8a` (2026-11-08)
- [x] CSV/TSV reader via Polars `scan_csv` (S-03)
- [x] Parquet reader via Polars `scan_parquet` (S-03)
- [x] Arrow IPC reader (S-03)
- [x] JSON Lines reader (S-03)
- [x] stdin reader with `SeekableFile` tempfile fallback (S-03)
- [x] Ratatui app loop with `tokio::select!` + crossterm `EventStream` (S-02)
- [x] `Action` enum with `affects_data()` + serde snake_case (S-02)
- [x] Materialize task: `plan.slice(offset, WINDOW).collect()` on tokio task; cancel-and-replace on window slide (S-02)
- [x] Panic hook: restore terminal, abort cleanly (S-02)
- [x] Sheet stack: `Vec<Sheet>` + `q` to pop (S-02); sheet switcher `S` / `:ls`
- [x] ~~Keymap loader (TOML)~~ — superseded: `init.lua` `vd.bind`/`vd.unbind` is the keymap config (Phase 3)
- [x] Chord resolver + which-key overlay with 1000 ms timeout (S-04)
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
- [x] ~~Type-inference adapter~~ — superseded by type coercion (S-13 #3); a VisiData-shaped type system is a non-goal
- [x] Sort by column (asc/desc, multi-col toggle) — `[` asc, `]` desc, `zo` clear
- [x] Frequency table sheet — `F` on current column; pushes derived sheet
- [x] Ex-line parser (`:filter`/`:unfilter`/`:sort`/`:unsort`/`:q`/`:qa`/`:ls`/`:pivot`/`:unpivot`); `:derive` landed later as Polars SQL expressions
- [x] Ex-line history, file-persisted at `$XDG_STATE_HOME/vdrs/exline_history`; `<Up>`/`<Down>` cycle
- [x] `.` repeat for last ex-command — works via existing `last_action` capture; no extra code needed

## Phase 1.5 — Tier 1 daily-driver surface (S-13) + golden corpus replay (S-14)

Target: ~160/189 upstream golden tests runnable. Order by test-leverage (counts from `../visidata/tests/*.vd{,j}` longname frequency scan). Each chunk is independent and ships separately.

**Current state (2026-11-08, S-18 E): PASS=42, FAIL=0, SKIP=123, UNTESTABLE=20, QUARANTINED=16, NO_GOLDEN=39.** At Phase 1.5 close (2026-10-16): PASS=21 of 164 files.

**Addressable ceiling (2026-11-08, S-25 P0 triage): 182** — the iteration-14 figure "21/21 addressable" counted editing, options, nested data and expressions as out of scope; decisions 7–11 in direction §9 bring them back. Plan to ~80–95 PASS: [`specs/S-25-corpus-coverage.md`](specs/S-25-corpus-coverage.md). Iteration notes below are history.

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

- [x] S-13 drafted — [`docs/specs/S-13-tier1-feature-target.md`](specs/S-13-tier1-feature-target.md) (closed iter 14)
- [x] S-14 drafted — [`docs/specs/S-14-corpus-replay-harness.md`](specs/S-14-corpus-replay-harness.md) (landed + stable)
- [x] S-15 drafted — [`docs/specs/S-15-feature-inventory.md`](specs/S-15-feature-inventory.md) (authoritative shipped-feature list)
- [x] **Harness bootstrap** — `tests/vd_replay.rs` + cmdlog parser (`.vd`/`.vdj`) + sheet-level `Replay` driver + numeric-aware diff + quote-style-never serialize + stable sort. `src/lib.rs` added so integration tests see internals.
- [x] **In the `vdrs` binary (per S-15 §3)**: Column meta (hide/rename/key/unkey/freeze), Type coercion (int/float/string/date/currency), Selection (row/all/regex/dup-selected), Sort (multi-col asc/desc), Readers (CSV/TSV/Parquet/Arrow/Jsonl/JSON + xlsx via calamine + sqlite via rusqlite + gzip via flate2), Pivot with keys, unpivot/melt.
- [x] **Harness-first, now also in the binary (2026-11-05)**: Joins (inner/outer/full/left/append/extend/merge → `:join`), pivot without keys (`W`), hide-uniform-cols (`:uniform`), setcol-input gang mode (`:setcol`), dup-rows (`g"`), select-equal-cell (`,`).
- [x] **CI**: `ci.yml` fetches saulpw/visidata pinned at `5f67ff8a` (was `dd1a9f0` until 2026-11-07) into `../visidata`; `cargo test` runs the replay and fails on any regression

**Deferred at iteration 14** (history; struck = shipped, _now planned_ = back on the roadmap):
- ~~**Describe-sheet**~~ — shipped as `I` / `:describe` (own stats, not VisiData's fmtstr output; golden tests stay quarantined).
- **Options system** — 4-level fallback stays cut; _now planned_: flat `vd.option` (S-18 B) + data-changing subset (S-25 P6).
- **Expression column** — _now planned_: `=` / `g=` over Polars SQL (S-21); `aggregate-col` takes an aggregator name, not Python (S-25 P4).
- **Row ops** (`edit-cell`, `delete-row`, copy/paste) — _now planned_: S-18.
- ~~**Multi-worksheet xlsx**, **multi-table sqlite**~~ — shipped: parts list + `file:part`.
- ~~**Lenient date parsing**~~ — shipped: `@` tries 12 regex-guarded layouts.
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

**Phase 1.5 CLOSED** (2026-10-16). Its follow-up list (plugin shapes, describe, lenient dates, visual pass) shipped; the Python subset and format plugins moved to S-21 C and S-25 P7. Still out of scope: `.vdj` runtime, `set-option` precedence tests, loaders not in core except the cheap ones S-25 P7 weighs (Lua / `vdrs-ext-*` territory per S-09). Plots, macros and `unfurl-col` are planned (S-23, S-19, S-22).

## Phase 2 — pivot + plan panel (the differentiating release)
- [x] Pivot sheet: `:pivot <index> <on> <values> [agg]` — eager impl via polars-ops. Direction §8 OOM risk noted.
- [x] Melt/unpivot sheet: `:unpivot <on1,on2,...> -- <idx1,idx2,...>` — lazy via `LazyFrame::unpivot`
- [x] `W` pivot: key columns × current column (counts); custom aggregates via `:pivot … <agg>`
- [x] Live preview panel — `:pivot` prompt previews hints, then the pivot of the first 10k rows (2026-11-05); other ops keep the stale window until the new materialize lands
- [x] Plan side-panel: `P` cycles Off → Normal → Optimized (`describe_optimized_plan`)
- [x] Plan export — `:export view.py` (Polars) / `view.sql` (DuckDB), replaying the view from the source file
- [ ] DuckDB backend switch for OOC pivots (optional feature) — _dropped from direction §9.3; revisit only if Polars OOC wall hit_

## Phase 3 — mlua plugins
- [x] S-09 plugin system spec drafted — [`docs/specs/S-09-plugin-system.md`](specs/S-09-plugin-system.md)
- [x] S-10 Lua API surface + versioning spec drafted — [`docs/specs/S-10-lua-api.md`](specs/S-10-lua-api.md)
- [x] Plugin loader bootstrap: `$XDG_CONFIG_HOME/vdrs/init.lua`, Lua 5.4 VM, missing-file is not an error
- [x] Keymap plugin shape: `vd.bind(layer, keys, action, desc?)` + `vd.unbind(layer, keys)`
- [x] **Loader plugin shape**: `vd.register_loader { name, extensions, open }`. `open(path) -> string` returns a path to a format we scan natively (CSV/TSV/Parquet/Arrow/Jsonl). Resolved by extension (case-insensitive) before `reader::open` in main.rs. Zero Lua↔DataFrame marshalling — Lua writes a tempfile. Heavy/encumbered formats stay in `vdrs-ext-*` Rust crates per S-09 ADR. 5 unit tests including end-to-end roundtrip.
- [x] Loader plugin covers `:diff` too (shared `plugins::open`); `:tail` refuses loader formats since refresh would re-scan the stale converted file
- [ ] API versioning (`since`, `deprecate`) + `cargo xtask check-api-versions` — part of the S-10 rest, deferred until a Lua user asks (S-16, roadmap evidence-gated list)
- [x] Aggregator plugin shape — `vd.register_aggregator(name, fn)`, used by `:pivot`; `:derive` takes SQL expressions (Polars `sql_expr`)
- [x] Renderer plugin shape — `vd.register_renderer { column | dtype, render }`
- [x] Hot-reload on save — mtime poll on the 2 s tick (no `notify` dependency)
- [ ] Extract `trait Source` from the current enum — deferred; current enum + loader-returns-path pattern covers the user-facing extension story. Add only when a `LuaSource` wants in-memory streaming.

## Phase 4 — streaming / tail-sheet
- [x] Tail sheet (JSONL today, CSV/Parquet by passing format): `:tail <path> [--follow|-f]`; 2s tick re-reads file
- [x] Append-only cursor with `--follow` mode (auto-scroll to bottom when user was at bottom)
- [x] Parquet directory watcher — `:tail <dir>` scans `*.parquet`, re-globbed each tick
- [x] Incremental read — CSV/TSV/JSONL parse only appended bytes

## Phase 5 — modal frame diff
- [x] Diff sheet via `:diff <path>` (schema diff) or `:diff <path> <keys>` (full outer join; `__diff` flags only_a/only_b/changed; same rows suppressed)
- [x] `D` key: path prompt, key columns as diff keys

## Correctness pass (2026-11-05, branch `fix/save-full-plan`)
- [x] `:save` writes every view row (was: 1024-row window); unknown extension no longer truncates the target
- [x] Derived sheets (freq/pivot/unpivot/diff/`"`) built from the parent's view (casts/filter/sort/visible cols)
- [x] Background row count → `G` reaches the true last row, title shows `row N/total`
- [x] One `goto_row` path for j/k/paging/gg/G; PageDown = one screen; never walks past EOF
- [x] Search, `gs`, `|` cover the whole view, not just the loaded window
- [x] Cursor row/column always on screen (width-aware column scroll); wide chars and embedded newlines/tabs render on one line
- [x] Input validation: bad regex, unknown sort/filter column, rename onto existing name → toast, sheet stays healthy
- [x] `.json` arrays read; `.gz` honours `--format`; missing file fails fast with path; no-terminal error suggests `--headless`
- [x] End-to-end `app::scenario` tests (keymap → dispatch → Polars tasks → render)
- [x] Rescan fixes: shifted symbol keys (`!`, `$`, `|`, …) match bindings on terminals that flag SHIFT; `:tail -` / `:diff -` refused (reading stdin inside the TUI hung the app)

## Cross-cutting — do when it hurts, not before
- [x] CI: fmt + clippy + build — `.github/workflows/ci.yml`
- [ ] `hyperfine` bench harness for Phase 0 scroll latency — the `app::scenario` Harness drives the UI without a TTY but is test-only; add a bench target when scroll speed regresses
- [ ] `rust-toolchain.toml` only when a nightly is needed
- [x] Release path: `cargo install --path . --locked` from a checkout (decision 2026-11-05); no prebuilt binaries or crates.io until the alpha is finished (decision 2026-11-08; S-24 R5 ships then)
- [x] Refactor: dispatch + action handlers moved to `src/app/actions.rs` (`app.rs` keeps loop, jobs, prompts, render, tests)
