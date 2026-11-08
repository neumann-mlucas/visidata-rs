# S-25 — Corpus coverage: from 29 to ~90 passing VisiData goldens

Status: **drafted 2026-11-08**; P0 done 2026-11-08 (PASS=30 FAIL=0 SKIP=140 UNTESTABLE=19 QUARANTINED=12 NO_GOLDEN=39, ceiling 182; see §1.1); P1 batch 1 (longname aliases, `rename_col`, driver origins) 2026-11-08: PASS=34 FAIL=0 SKIP=134 UNTESTABLE=19 QUARANTINED=14 NO_GOLDEN=39; numeric/date freq in first-seen order 2026-11-08: PASS=35, QUARANTINED=13. DuckDB files (`src/duckdb.rs`) 2026-11-08: PASS=41 FAIL=0 SKIP=128 UNTESTABLE=19 QUARANTINED=13 NO_GOLDEN=39. Extends S-14 (harness) — no harness redesign.
Baseline (CI pin `5f67ff8a`, 240 cmdlogs): **PASS=29 FAIL=0 SKIP=0 UNTESTABLE=50 QUARANTINED=121
NO_GOLDEN=40.**

## 1. Triage run (2026-11-08)
Run with `tests/vd_replay/quarantine.txt` emptied (`VDRS_REPLAY_VERBOSE=1`):
**PASS=29 FAIL=12 SKIP=96 UNTESTABLE=63 NO_GOLDEN=40** — addressable ceiling 137. The quarantine
hides 108 tests behind four headings; the largest ("Join + columns-sheet workflow", 73 stems) mixes
duckdb/postgres loaders, html, regressions and real columns-sheet tests. Real first blockers:

| Blocker | Files | Where it goes |
|---|---|---|
| Unknown formats (html 5, postgres 5, stata/dta/xpt 4, npy/npz 3, fixed-width 3, zip 2, msgpack 2, xml 2, toml, yaml, h5, png, usv, jsonla, lsv, fec, dir, http) | ~37 | P7 |
| `save-sheet` to duckdb / sqlite commit / `.vds` | 13 | duckdb: 6 PASS 2026-11-08 (via the `duckdb` program, S-09); `.vds` 4: P7; commit 3: out |
| Longnames of shipped features not in the translator: `describe-sheet` 6, `join-selected` 4, `key-selected` 2, `dive-selected`, `dive-except`, `unhide-cols`, `key-col-off`, `freeze-sheet`, `dup-rows-deep` 3, `search-keys`, `select-to-next-selected`, `type-decimal`, `save-all`, `allow-error` 2 | ~26 | P1 |
| Columns sheet (`columns-selected` 4, `rename-cols-row`, FAIL `sortorder`, `issue3190`) | ~7 (+4 edit-join with S-18 C) | P3 |
| Runtime: duplicate column names (2), lenient JSON (3), CSV quote edge (`errors.csv`), key lookup (2), join key dtype (1), empty parquet glob (1), geoarrow ext type (1), `join-sheets` with 0 sheets (1) | ~12 | P2 |
| FAIL: zero-padded ids `issue964`, sqlite views `load-sqlite-view`, extend-join prefixes `join-extend-mult`, freq `open-row` `freqtbl-openrow` | 4 | P2 |
| FAIL: options (`numeric_binning`, `freq-fmtstr`) | 2 | P6 |
| FAIL: per-cell mixed types (`issue3219-*` ×3), xlsx cell metadata (`xlsx-color-cells`) | 4 | permanent |

**Misclassification found:** `aggregate-col` (11 files), `aggregate-cols` (1), `addcol-aggregate` (1)
sit in UNTESTABLE as "Python-expression payloads", but their input is an aggregator *name*
(`sum`, `mean`: `{"longname": "aggregate-col", "input": "sum"}`). They need a feature (P4), not Python.

### 1.1 P0 results (2026-11-08)
- UNTESTABLE 63 → 19: besides `aggregate-*`, the editing/clipboard (S-18), nested (S-22) and most
  `addcol-*` / `setcol-*` / `type-*` longnames take a regex, name, format or nothing — not Python. They
  are SKIP now, under their real name. Left: Python payloads, `setcol-fake`, `unset-option`.
- Quarantine 121 → 12: only end-to-end FAILs, each with a reason; everything else shows its blocker.
- NO_GOLDEN 40 → 39: 18 are assertion-only upstream (permanent); 21 have non-TSV goldens (`.csv` 5,
  `.json` 5, `.jsonl` 3, `.grep` 2, others 1 each) — comparing those (load the golden with vdrs's own
  reader) is a cheap follow-up; `invalid_unicode_sqlite` passes with a lossy golden read (+1 PASS).
- Nothing to generate with `test-vdx.sh`: every file that can have a golden has one.

## 2. Phases (gains are upper bounds: a file can hit a second blocker)

| Phase | Work | Files in reach | Effort |
|---|---|---|---|
| P0 | Harness: `VDRS_REPLAY_TRIAGE=1` runs quarantined files too and prints first blockers; quarantine rewritten with one real reason per stem (headings by blocker, as the table above); `aggregate-*` moved out of UNTESTABLE; look at the 40 NO_GOLDEN (generate with VisiData's own `tests/test-vdx.sh --batch` where the test simply never had a golden committed — run in CI only, never needed locally) | 0 (+ up to 40 testable) | 0.5–1 d |
| P1 | Translate shipped features' longnames (table row 3); `describe-sheet`: `I` matches VisiData's describe sheet (decided 2026-11-08) — columns `column errors nulls distinct mode min max sum median mean stdev` (`sheet`, `type` hidden at width 0; mean/stdev = VisiData `describe_aggrs` default; `errors` = cells a type cast failed on, 0 when none), replacing vdrs's `type count …` layout; update `:export`, guide, `docs-describe` recording | ~26 | 1–2 d |
| P2 | Correctness fixes (S-24 R4 is the zero-padded-ids item, done here — not twice): sqlite views listed with tables; `extend` join with `<sheet>_` prefixes on clashing columns; freq `open-row` semantics; duplicate header names → VisiData naming; JSON with comments/trailing commas → decide accept (lenient parser) or permanent | ~16 | 2–3 d |
| P3 | Columns sheet `C` (design §3) | ~7, +4 with S-18 C | 3–5 d |
| P4 | Column aggregators `+` / `g+` (design §4) | ~13 | 1.5–2 d |
| P5 | Ride on S-18 (copy/delete/open-new/edit-cell ~10), S-21 C (Python subset, measured in S-21 C0), S-22 (`unfurl`/`expand` ~6) | ~20–30 | in those specs |
| P6 | Options subset (design §5) | ~2 + files where `set-option` is a no-op today | 1–2 d |
| P7 | Formats, each a separate yes/no against direction §7: cheap in core — `usv` (U+241F/U+241E separators are multi-byte, Polars needs one byte: transcode to a
tempfile TSV, as stdin does), fixed-width, zip members, toml/yaml (→ JSON → Polars); everything else (html, xml, stata, npy, msgpack, h5, postgres, duckdb) as `vdrs-ext-*` / Lua or stays out | 5–37 | 0.5 d each (cheap ones) |

Realistic target after P0–P4 + S-18: **~80–95 PASS**. Raise `MIN_PASS` after every phase; never lower it.

## 3. Columns sheet (P3) — design sketch
- `C` pushes `Origin::Columns { parent }`: one row per parent column — `name, type, width, hidden,
  key, sortorder, aggregators` (VisiData order: `name width type fmtstr value aggregators sortorder`;
  match it for the goldens, extra vdrs columns after).
- Rows map to parent columns by name; row selection is a column selection: `s` + `-` hides the
  selected columns in the parent, `!` keys them, `#%~@` cast them, `e` on `name` renames (S-18 C
  prompt), `d` hides (VisiData). Edits write to the parent's `View`/`PlanStep::Rename`, not to the
  columns sheet's own plan — the columns sheet is rebuilt from the parent on every change.
- `gC` = columns of all open sheets (VisiData `columns-all`), read-only in v1.
- Export: none of its own; the parent's steps export as today.
- Tests: scenario per write-back op; goldens `columns-hide`, `columns-rename`, `columns-type`; corpus
  `sortorder`, `issue3190`, `columns-selected*`.

## 4. Column aggregators (P4) — design sketch
- `+` prompts for an aggregator (`sum mean median min max count distinct mode stdev` + Lua
  `register_aggregator` names) and adds it to the current column; `g+` on selected columns (columns
  sheet) or all numeric ones; `z+` shows the aggregate of the current column over the view in the
  status line (VisiData `aggregate-col` behavior).
- Stored in `View.aggregators: Vec<(column, agg)>`. Used by: `F` (freq gets `sum_Units` etc. columns,
  as VisiData), `W`/`:pivot` defaults, the describe sheet, an optional footer row.
- Export: freq/pivot lineage carries them; `.py`/`.sql` emit the extra `agg` exprs.
- Tests: unit per aggregator vs Polars; scenario `+ sum` then `F`; goldens `freq-aggregators`,
  corpus `sum-freq-table`, `aggregators-set`, `petsdiet`, `monthly-revenue`.

## 5. Options subset (P6)
Only options that change *data* in the corpus, mapped to vdrs settings through `vd.option` (S-18 D6):
`numeric_binning` + `histogram_bins` (freq on numeric columns bins into ranges), `disp_float_fmt`
/ `disp_date_fmt` (display + freq keys), `csv_delimiter`, `header`, `encoding`, `null_value`. The
replay maps `set-option` for these and keeps the rest a no-op. Global scope only (no per-sheet
override) — see the VisiData options note in `direction.md` §1.

## 6. Test plan
- P0: the triage output is checked in (`tests/vd_replay/triage.txt`, regenerated by
  `VDRS_REPLAY_TRIAGE=1 cargo test --test vd_replay`, documented in S-14) so blocker shifts show in review diffs.
- Every phase: listed goldens pass byte-for-byte post-normalize; `MIN_PASS` raised in the same commit;
  quarantine entries removed with their reason.
