# S-13 — Tier 1 feature target: daily-driver parity for upstream golden corpus

Status: **in flight** (2026-10-15 baseline: driver covers types + rename/hide/key + sort variants; production App dispatch pending per chunk — see `docs/tasks.md` Phase 1.5)
Phase: 1.5 (interleaves with Phases 1–3)
Depends on: direction §4 (engine), S-01 (data model), S-04 (keybindings), S-09 (plugins)
Blocks: S-14 (test corpus replay harness — Tier 1 is what the harness grows into)

## Problem

Current implementation ships ~15 commands (nav, freeze, search, filter, sort, freq, pivot/unpivot, exline, plan panel, diff, tail). That is below the threshold where a daily VisiData user can switch over without flinching. Direction §9.2 names this user as primary.

"Full VisiData" is explicitly a non-goal (direction §7). The question is: what is the smallest subset of VisiData's command vocabulary that a heavy user can live inside, measured not by hand-wavy coverage but by **how much of the upstream test corpus becomes runnable**.

## Decision

**Target the top-25 longnames by test-file count in `../visidata/tests/`.** That subset covers ~160 of 189 upstream cmdlog files. Ship it before chasing any Tier 2 differentiator (pivot preview UI, plan export, hot-reload) that doesn't have a VisiData golden.

Coverage grows mechanically with each longname added to the test harness translator (see S-14). No new tests to write per feature — the goldens already exist.

## The target set

Grouped by implementation chunk, ordered by test-leverage (counts from `../visidata/tests/*.vd{,j}` longname frequency scan, 2026-10-15).

| # | Group | Longnames | Upstream tests unlocked | Status | Key bindings | Polars primitive |
|---|---|---|---|---|---|---|
| 1 | **Selection system** | `select-row`, `select-rows`, `unselect-rows`, `select-col-regex`, `select-equal-cell`, `toggle-row`, `dup-selected`, `columns-selected` | ~80 | missing | `s t u \| \ "` | row-mask `BooleanChunked` kept beside `LazyFrame`; dup = `.filter(mask).collect()` into new sheet |
| 2 | **Column meta** | `hide-col`, `rename-col`, `key-col`, `freeze-col`, `columns-sheet`, `slide-left`, `slide-right` | ~120 (overlaps #1) | missing | `- ^ ! z= C H L` | hide = visible-col list; rename = `.rename()`; freeze = `.collect()` of expr → materialized col; cols-sheet = meta-sheet over `self.schema()` |
| 3 | **Type coercion** | `type-int`, `type-float`, `type-date`, `type-currency`, `type-string` | ~71 | deferred (tasks.md Phase 1) | `# % @ $ ~` | `col(name).cast(dtype)` + currency-strip pre-cast regex |
| 4 | **Expression column** | `addcol-expr`, `aggregate-col`, `setcol-input`, `set-cell-input` | ~45 | missing — needs eval surface | `= + g=` | **Lazy pick: Lua one-liner** evaluated via already-in-tree mlua. `=` prompts for Lua expression; closure captures row proxy. Defer dedicated S-06 DSL unless Lua eval proves too slow or too foreign. |
| 5 | **Row ops** | `edit-cell`, `delete-row`, `open-row`, `open-cell` | ~45 | missing | `e d <CR> z<CR>` | edit = cell-level `.with_column(when/then/otherwise)`; delete = row-mask invert; dive = push row-as-sheet (ColumnsSheet shape) |
| 6 | **Joins** | `join-sheets`, `join-sheets-top2` | ~14 | missing | `& g&` | `LazyFrame::join` with inner/outer/full/left strategies; concat = `concat([a,b])` |
| 7 | **Options** | `set-option`, `g set-option` | ~30 | missing | `<Space>o` | flat `HashMap<String, OptionValue>` + per-sheet override map. No 4-level fallback (direction §1 cut). |
| 8 | **Describe** | `describe-sheet` | ~5 | missing | `I` | `LazyFrame::describe(None)` |
| 9 | **Save** | `save-sheet`, `save-all` | implicit (every golden is a save) | missing | `Ctrl-S g Ctrl-S` | `.sink_csv/parquet/ipc` by extension |

**Nine chunks → ~160/189 goldens runnable.** Current ~15 longnames unlock an estimated ~15–25.

## Explicit cuts (don't chase these upstream tests)

- `plot-column` — plotting (direction cut)
- Macros (`macro-param-*`, record/playback) — not Tier 1
- `.vdj` as a runtime format (direction §9.5) — supported in tests/ harness only
- `unfurl-col` — niche, defer
- `set-option` precedence tests — flat config only
- `issue*`, `pr*` regression files — opt-in later
- Loaders you don't ship: conllu, h5, stata, xpt, png, mbtiles, msgpack, usv, lsv, pcap, zip, numpy, ods, html, xml, yaml, toml, geojson

Those loaders may land as Lua plugins (per S-09 extension ADR) once a user asks.

## Tier 2 (differentiators — no upstream goldens; hand-roll Rust tests)

Not blocked by Tier 1 but shipped after it for the user it was built for.

- Pivot setup sheet (`W` interactive, mark rowkeys/aggs) — current flow is ex-only
- Pivot live preview panel
- Plan panel export (`s` → `.pl` / SQL)
- Lua plugin shapes (loader, aggregator, renderer) full lifecycle — S-09 §4
- Hot-reload via `notify`
- Incremental tail (currently full re-read per tick)
- Diff UI (`D` key, visual key picker)
- Parquet directory watcher

## Implementation order (lazy, by test-leverage)

Ship in this order. Each step independently useful; each step unlocks a measurable chunk of the corpus.

1. **Save** (#9) — the harness needs save to compare against goldens at all. **Blocks S-14.**
2. **Column meta** (#2) — biggest coverage per LOC: hide/rename/key cover ~120 tests. Mostly Polars one-liners.
3. **Selection system** (#1) — ~80 tests. New state (`row_mask: Option<BooleanChunked>`).
4. **Type coercion** (#3) — ~71 tests. Thin wrapper on `cast()`.
5. **Describe** (#8) — one `.describe()` call.
6. **Options** (#7) — flat kv; needed by any test that sets a format option.
7. **Joins** (#6) — ~14 tests but opens the "two-sheet workflows" door.
8. **Row ops** (#5) — edit/delete/dive. `edit-cell` is 18 tests on its own.
9. **Expression column** (#4) — ships *after* mlua loader bootstrap (Phase 3) so we get Lua eval for free. Blocks `aggregate-col` and `addcol-expr`.

**Rough budget**: each chunk is 1–3 days of real work. Nine chunks ≈ 2–4 weeks sustained. Harness (S-14) in parallel, started day one.

## Verification before leaving this spec

- Does each Tier 1 chunk map to at least one upstream `.vd/.vdj` → golden pair we can target? **Yes** — the frequency scan is the source of the table above.
- Does the ordering avoid cross-chunk blockers? **Yes** — #1 (selection) is needed by `dup-selected`-style tests but #2 (column meta) and #3 (type coercion) are independent.
- Does any Tier 1 chunk require inventing new spec surface? **One**: #4 depends on Lua eval, which falls out of S-09 for free rather than needing S-06 (ex-line grammar). S-06 deferred until a user asks for a non-Lua expression syntax.

## Non-decisions (deferred until a user hits them)

- Whether `aggregate-col` eventually gets a non-Lua DSL (S-06).
- Whether `set-option` grows a precedence chain.
- Whether `undo`/`redo` ship in Tier 1 (currently out — plan-based undo per direction §1 lands when it hurts).
