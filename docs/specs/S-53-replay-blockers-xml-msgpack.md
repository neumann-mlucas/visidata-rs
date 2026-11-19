# S-53 Replay blockers, XML and msgpack readers

Status: in progress.

Replay files whose first blocker looked cheap (`tests/vd_replay/triage.txt`).

## A. Keyed columns sheet

- `C`'s sheet keys on `name`, `gC`'s on `sheet` and `name` (VisiData's
  `ColumnsSheet.nKeys = 2`), so a cmdlog row `キCustomer` finds its row. The
  replay matches a composite rowname (`sheet,name`) key by key.
- `join-concat` then runs and is quarantined: VisiData's concat of two columns
  sheets holds the Column objects, so editing `name` there renames the source
  column. `getitem` stops later (pasting rows into a columns sheet).

## B. Rename onto a hidden column

- `^` onto the name of a hidden column moves that one aside to `<name>_1`
  (`_2`… when taken) instead of refusing; VisiData keeps both under one name.
  A visible column still refuses. Checks first, so a refused rename changes
  nothing.
- The replay maps VisiData's `Date_re[0]` (a captured list's item) to vdrs's
  struct field `Date_re.0`, as S-31 planned. `monthly-revenue` passes:
  replay 98.
- Not done: same-named columns from append/concat (`issue3178`,
  `issue3208-duplicates`): VisiData's output has two columns of one name, which
  a Polars frame can't hold.

## C. Looked at, not done

- `pr2308`: `keymax`/`keymin` need the sheet's keys in every aggregator path
  (footer, `z+`, freq, pivot, both exports), and `zF` (freq-summary) is a new
  command.
- `pr2372`: blank `col` means VisiData's cursor column; tracking the cursor in
  `.vd`/`.vdj` replays too breaks `numeric-cols`.
- `type-customdate`: text into a date column, refused by typed columns.
- `sqlite-commit` (`commit-sheet` to SQLite), `errors-311` (multi-line quoted
  CSV with encoding errors and `skip`).
