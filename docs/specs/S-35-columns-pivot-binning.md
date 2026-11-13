# S-35 — New columns at the cursor, `W` with aggregators, numeric binning and `fmtstr`

Status: **A, B, C1 shipped 2026-11-13**. C2 (`fmtstr`) needs agreement first.
Depends on: S-34 (`View.order`), S-25 P4 (aggregators), S-31 (column builders).

Replay cases: A `capture-col-named`, `pr2855` (`pr2614` stays quarantined, see A3); B `pivot`;
C `numeric_binning`, `freq-fmtstr`.

## A. New columns go right after the cursor (shipped)

VisiData `addColumnAtCursor` (`sheets.py`): a new column goes right after the cursor column. On a
key column (or no column) it goes first of the non-key columns, and the cursor moves onto it.

- A1. **Placement:** `Sheet::place_cols(new, after)` writes `View.order` (S-34). It names only the
  shortest prefix after which the display is plan order again, so a column added last leaves
  `order` empty. Callers:
  - `za` `:addcol`, `i`, `;`, `*`, `=` (only when the name is new);
  - `(` puts the fields right after the struct;
  - the replay harness, which uses the cmdlog's `col`.
- A2. **Names:** an unnamed new column (`i`) takes the sheet's next letter, `A` `B` … `Z` `AA`
  (VisiData `incremented_colname`). The counter is `View.colname_ctr`, so undo puts it back. The
  `A` sheet (`open-new`) starts at 1. This supersedes S-31's `incr`, `incr2`.
- A3. **No duplicate names:** a letter already taken is skipped. VisiData allows two columns with
  the same name; Polars doesn't. That is why `pr2614` stays quarantined: VisiData names the new
  column `A` next to data1's own `A`.
- A4. **Sessions:** replaying `derive` places the column at the replay's cursor. So a sheet with
  steps always writes its `order` line, and a bare `:order` (empty `order`) means plan order.

## B. `W` uses the `+` aggregators (shipped)

VisiData `W` (`pivot.py`):

- Rows: the key columns. Columns: one per value of the cursor column.
- Each value gets one column per aggregator set with `+` on a visible column.
- No aggregators: `count`.

Column names:

| Aggregated columns | Aggregators | Name |
|---|---|---|
| one | one | `<value>` |
| one | several | `<agg>_<value>` |
| several | any | `<col>_<agg>_<value>` |

- Several pivot columns would prefix `<pivotcol>_`. vdrs `W` pivots one column.
- Values come in first-seen order. For each value, the aggregators follow source column order,
  then the order they were added.
- A key/value pair with no rows: `sum` and `count` give 0, the others empty, as VisiData's
  aggregators do on no rows.

`vdrs` today: `W` always counts (`Sheet::pivot_current_from`). It becomes
`pivot_current_from(parent, id, keys, on, aggs)`, where `aggs` is the parent's
`View.aggregators` for visible columns. `Derivation::PivotCount` gains `aggs`, and the
Python/SQL export emits one aggregate per pair. `:pivot index on values agg` is unchanged.

As built:

- The aggregators on key columns and on the pivot column itself are left out, since those columns
  are what the rows and columns are grouped by.
- `Sheet::pivot_aggs` groups once, then pivots once per aggregate.
- The Python export does the same. The SQL export cross-joins keys × values so empty pairs get 0,
  then PIVOT. It renames `<value>_<alias>` to `<alias>_<value>` with
  `COLUMNS('^(.*)_(alias|…)$') AS '\2_\1'`. A key column whose name ends in `_<alias>` would match
  that rename too.
- Tests: golden `pivot-aggregators`, `pivot-aggregators-cols` (both export round trips), unit
  `pivot_with_aggregators_names_and_fills`, replay `pivot`.

## C. Freq groups (C1 shipped, C2 planned)

- C1. **`numeric_binning`** (VisiData option, default off) with **`histogram_bins`** (default 0):
  - Bins: `F` on a numeric column bins it into `nbins = histogram_bins or ⌊√rows⌋` equal ranges
    from min to max. Labels are `lo - hi` with two decimals; every bin is a row, empty ones count 0.
  - Ints with more bins than the value range give one bin per value.
  - Membership: a value goes in bin `⌊(v − min) / width⌋`, capped at the last bin.
  - Diving into a bin filters `lo <= v < hi` (`<=` on the last bin).
  - `vd.option("numeric_binning", true)` and `set-option` in the replay. The export bins with the
    same arithmetic.
- C1 as built:
  - `Derivation::Freq { column, bins: Option<Binning> }`; `Binning::Ranges { min, width, n }` or
    `Values` (one row per value, in value order). `Sheet::numeric_bins` picks it, in the `F` job.
    Bins are listed in range order, the null row last.
  - The dive computes each row's bin index with the same expression (`Sheet::rows_in_bins`), so a
    float on an edge lands where the count put it.
  - Options: `vd.option("numeric_binning", "true")`, `vd.option("histogram_bins", "N")`.
  - Sessions write `F` only: the restore bins under the `init.lua` it runs in (no `:set` command).
  - Export: Python `floor` + `clip` + a left join on every bin; SQL `least(floor(…))`, a
    `range(n)` bin table, `UNION ALL BY NAME` for the null row.
  - Tests: golden `freq-binned`, `freq-binned-dive` (a case `init.lua`, export round trips), unit
    `numeric_bins_pick_ranges_or_values`, scenario `binned_freq_counts_ranges_and_dives`, replay
    `numeric_binning`.
- C2. **`fmtstr` (to agree):** a column format string, VisiData's `fmtstr` attribute.
  - Where to set it: the columns sheet's `fmtstr` column, plus `:fmtstr <fmt>` so sessions can
    write it back.
  - Proposed scope: date and datetime columns (strftime codes: `%Y-%m`, `%b %d`, …) and C-style
    float precision (`%.2f`, `{:.2f}` → S-32 precision).
  - Effects: it changes the display and freq's grouping. Freq groups by the formatted text, so
    `%Y-%m` counts months. Export: `dt.strftime` / DuckDB `strftime` in the group-by.
