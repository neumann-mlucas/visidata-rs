# S-35 — New columns at the cursor, `W` with aggregators, numeric binning and `fmtstr`

Status: **A shipped 2026-11-13**. B and C are planned; C2 (`fmtstr`) needs agreement first.
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

## B. `W` uses the `+` aggregators (planned)

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

## C. Freq groups (planned)

- C1. **`numeric_binning`** (VisiData option, default off) with **`histogram_bins`** (default 0):
  - Bins: `F` on a numeric column bins it into `nbins = histogram_bins or ⌊√rows⌋` equal ranges
    from min to max. Labels are `lo - hi` with two decimals; every bin is a row, empty ones count 0.
  - Ints with more bins than the value range give one bin per value.
  - Membership: a value goes in bin `⌊(v − min) / width⌋`, capped at the last bin.
  - Diving into a bin filters `lo <= v < hi` (`<=` on the last bin).
  - `vd.option("numeric_binning", true)` and `set-option` in the replay. The export bins with the
    same arithmetic.
- C2. **`fmtstr` (to agree):** a column format string, VisiData's `fmtstr` attribute.
  - Where to set it: the columns sheet's `fmtstr` column, plus `:fmtstr <fmt>` so sessions can
    write it back.
  - Proposed scope: date and datetime columns (strftime codes: `%Y-%m`, `%b %d`, …) and C-style
    float precision (`%.2f`, `{:.2f}` → S-32 precision).
  - Effects: it changes the display and freq's grouping. Freq groups by the formatted text, so
    `%Y-%m` counts months. Export: `dt.strftime` / DuckDB `strftime` in the group-by.
