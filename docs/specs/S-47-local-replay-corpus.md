# S-47 Local replay corpus

Status: shipped 2026-11-16.

VisiData's own tests (`../visidata/tests`, S-14) cover what VisiData's authors
cared about. `tests/vd_local/` holds our cases: `.vd` scripts run through
VisiData to make a golden, then replayed by vdrs (`vd_replay_local` in
`tests/vd_replay.rs`, same driver as the VisiData corpus).

## Layout

- `tests/vd_local/*.vd`: cmdlog scripts (all 7 columns; empty sheet = current).
- `tests/vd_local/data/`: fixtures, paths relative to the repo root.
- `tests/vd_local/golden/<name>.tsv`: VisiData's output. `just vd-golden [names]`
  regenerates them (needs `just setup`; `$VD_TESTTMP` is a scratch dir for saves).
- `tests/vd_local/quarantine.txt`: known divergences, one reason each.
- Strict: every case not quarantined must PASS (no `MIN_PASS` floor).
  `VDRS_REPLAY_TRIAGE=1` writes `tests/vd_local/triage.txt`;
  `VDRS_REPLAY_DUMP=<name>` prints want/got.

## Coverage (36 cases)

| Section | Cases |
|---|---|
| 1. S-43..S-45 | `header` 0/2, `z^` `gz^` renames, JSONL error rows (freq, save), edits on pivot keys, melt and joins (key, shared, source write-back) |
| 2. Save formatting | csv/tsv/json/jsonl and typed csv round trips: quoting, nulls, booleans |
| 3. Nulls | sort (asc, desc, text), freq, aggregators, pivot, join on null keys |
| 4. Typed casts | int/float/date casts on messy text, describe |
| 5. Joins | inner/full/outer/merge/extend/append/concat, column collisions |
| 6. Aggregators | every aggregator and quantile on int and float |

## What it changed in vdrs

- Join: a non-key column on both sides is `<sheet>_<col>` on each; null keys match.
- Melt: `Variable`/`Value`, row-major, nulls dropped, index columns stay keys.
- Pivot `W`: absent pair counts 0; the null/empty value column is `''`.
- Pivot edits: key columns only.
- `csv_skipinitialspace` (S-48) unquarantined `cast-date` and `cast-float`.
- Replay: bools print `False`; `%.2f` float display; append column order is the sheet's own.

## Quarantined

- `cast-int`, `cast-describe`: failed casts are VisiData errors that keep their
  text; vdrs casts them to null.
- `join-extend`: VisiData repeats the first match for duplicate keys.

VisiData corpus: `pr2400` now quarantined (merge skips falsy cells, so `False`
shows empty); `MIN_PASS` 95.
