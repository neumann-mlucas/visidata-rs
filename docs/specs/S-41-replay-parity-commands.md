# S-41 — VisiData commands the replay corpus is blocked on

Status: **drafted 2026-11-15**; A, B shipped 2026-11-15.
Depends on: S-25 P0 (replay harness), S-33 (background selection jobs).

## 1. Today

`tests/vd_replay.rs` replays VisiData's own test scripts: `PASS=71 FAIL=0 SKIP=114 UNTESTABLE=20
QUARANTINED=10 NO_GOLDEN=25`. 36 files stop at a longname vdrs doesn't have
(`tests/vd_replay/triage.txt`). Most of them are one small command each. This spec adds the
commands vdrs has the machinery for, as real commands (key, action, ex/NAMED name, guide row),
and only then translates them in the replay. The harness never gets semantics the app lacks.

## 2. Parts (one commit each)

| Part | VisiData | Key | vdrs | Replay files |
|---|---|---|---|---|
| A | `unselect-col-regex`, `select-cols-regex`, `unselect-cols-regex` | `\` `g\|` `g\` | the `\|` prompt grows `unselect` and `all_columns`: any visible column matches | load-pandas-2 |
| B | `select-to-next-selected`, `select-to-prev-selected` | `z}` `z{` | select from the cursor to the next (previous) selected row, inclusive; none → toast `no selected row` | select-to-selected |
| C | `search-keys` | `r` | regex search over the key columns (first visible column without keys); the cursor column stays | issue3163 |
| D | `freeze-col` | `'` | a copy of the cursor column after it, named `<col>_frozen`, holding values (not the expression): later edits to the source don't reach it; keeps type, fmtstr, width, aggregators | column-copy, frozen-attrs |
| E | `addcol-incr-key` | — (ex `:addcol-incr-key`) | `i`, restarting at each change of the key columns' values (view order); option `incr_base` (default 1) also feeds `i`/`gi` | incr-key |
| F | `select-cell` on the describe sheet | `zs` there | select the source rows the cursor stat describes (min, max, mode, median, nulls, errors, distinct) | issue3223, issue3223-median |

Keys come from `../visidata` (`addCommand`/`bindkey`). `r` and `'` are free in vdrs's default
keymap; the vim profile is unchanged.

## 2.1 As built

- **A**: `Sheet::rids_matching_any` ORs a case-insensitive `contains` over the columns. `|` turned
  case-insensitive too (it was case-sensitive, unlike VisiData). The replay's own
  `select-col-regex` / `unselect-col-regex` code (case-sensitive, row index as row id) is gone:
  all four longnames go through `rids_matching_any`. load-pandas-2 passes (72).
- **B**: one background job over the sorted view's row ids. select-to-selected passes (73).

## 3. Out of scope

`type-decimal`, `memo-aggregate` files continue with Python `addcol-expr`, so they stay
UNTESTABLE whatever we add. Formats (`.vds`, fixed-width, Stata, HTML, postgres) are their own
specs. `allow-error` needs VisiData's exact error texts.

## 4. Tests

Each part: a `Harness` scenario on the real key path, the guide's key list and prose, S-15, and
`MIN_PASS` raised to the new count.
