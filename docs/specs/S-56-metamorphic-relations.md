# S-56 Metamorphic relations

Status: drafted 2026-11-22.
Depends on: S-55 (generators), `Harness` (`src/app/scenario.rs`).

## 1. Problem

A metamorphic relation runs two scripts and states how their results relate.
It needs no expected output, so generated inputs can feed it (S-55 §3).

A relation can test Polars instead of vdrs. "A filter and its negation
partition the rows" is one Polars `filter` call on each side, so it finds
Polars bugs, not vdrs bugs. This spec keeps only relations whose two sides
take different vdrs code paths to the same answer.

## 2. Rule for a relation

A relation enters this spec only if both statements are true:

1. The two sides go through different vdrs code: a different module, a
   different state (view, plan, rid set, derived sheet), or a different
   entry point (key, ex command, Lua).
2. A plausible vdrs bug breaks it: wrong state bookkeeping, a lost step in
   the plan, a wrong derivation, or a cursor or selection that points at
   the wrong rows.

Each relation below names its two paths and the bug class it targets.
Section 4 lists rejected relations and the reason for each.

## 3. Relations

Notation: `rows(S)` is the multiset of rows of sheet `S` (`csv_text`).
`view` is the active sheet after the generated script (S-55 §3.2).

### 3.1 Selection and filter

1. **Select then copy equals filter.** `|` on column `c` with regex `r`,
   then `"`, gives `rows` equal to `:filter c r`.
   - Paths: rid set in `rows_matching` versus `View` filter in `view_plan`.
   - Bug class: a rid set that drifts from the view after an edit or sort.
2. **Expression select equals expression filter.** `:select p` then `"`
   equals `:filter-expr p`.
   - Paths: `:select` reads the typed view; `:filter-expr` sets
     `View.filter_expr` (S-21 phase B).
   - Bug class: the two read different schemas, such as before or after
     a cast.
3. **Select all, then copy, equals copy.** `gs` then `"` equals `g"`.
   - Paths: rid set over all rows versus the full copy derivation.

### 3.2 Sort

4. **Sort then unsort restores order.** `rows` and row order after
   `:sort k` then `:unsort` equal the order before the sort.
   - Paths: `View` sort state versus the plan without it.
   - Bug class: a sort that leaks into the plan or into a derived sheet.
5. **Sort keeps every row.** `rows` after `:sort k` equal `rows` before.
   - Paths: the sorted `view_plan` versus the unsorted one, through the
     background window loader.
   - Bug class: a window or row-count bug that drops or repeats rows
     past the first window (1024 rows).

### 3.3 Frequency, pivot, describe, aggregators

6. **Freq counts sum to the view.** The sum of the `count` column of `F`
   on `c` equals the row count of the view below.
7. **Freq dive returns the counted rows.** Enter on each freq row opens a
   sheet with exactly `count` rows, and each row has that value of `c`.
   - Paths: freq derivation (S-52) versus `DiveKey` lookup.
   - Bug class: nulls, empty strings and shown text that the dive
     key matches differently from the group key.
8. **Freq equals SQL group-by.** `F` on `c` equals
   `:sql SELECT c, count(*) FROM self GROUP BY c` as a multiset.
   - Paths: `freq_from` versus `sql_from`.
9. **Describe equals the footer.** For a numeric column, `describe` min,
   max, mean and null count equal the footer aggregators of the same names.
   - Paths: `describe_from` versus `footer_totals` / `agg_expr`.
10. **Pivot totals equal the column sum.** The sum of all cells of a
    pivot with `sum` of `v` equals the `sum` footer aggregator of `v`.
    - Paths: `pivot_from` versus `footer_totals`.

### 3.4 Reshape and nested data

11. **Unpivot then pivot restores.** For a table with a unique key `k`,
    `:unpivot` on `k` then a pivot back gives the original `rows`.
    - Paths: melt versus pivot derivation.
    - Bug class: column order, null cells (S-47: nulls dropped on melt).
12. **Expand then contract restores.** On a struct column, expand then
    contract gives the original `rows` and column order.
    - Paths: S-22 expand versus contract.
13. **Unfurl length.** `zM` on a list column gives one row per list
    element, plus one row for each empty or null list.
    - Paths: `unfurl_from` versus the list lengths read from the view.
    - Check VisiData's rule for empty and null lists before fixing the "plus one".

### 3.5 Columns

14. **View changes keep values.** `hide`, `unhide`, column order and freeze
    change no row values. Compare the rows of all columns as a set of
    (`id`, column, value) triples.
15. **Rename round trip.** `:rename a b` then `:rename b a` restores
    `rows`, column order and the footer aggregators of `a`.
    - Bug class: aggregators, casts or key flags keyed by the old name.
16. **Derive copy.** `:derive d = "c"` gives a column `d` equal to `c`, for
    every type.
    - Paths: `PlanStep::Derive` versus the base column, with quoting of
      odd column names.

### 3.6 Edits

17. **Edit then undo restores.** Any edit command (`setcol`, cell edit,
    `d`, paste, `addcol`) followed by `U` restores `rows`, columns and
    cursor. This is the cheap form of an undo state-machine test (not specced yet).
18. **Delete count.** `gd` on a selection of `n` rows lowers the row count
    by exactly `n`, and no unselected row goes.
    - Paths: rid set versus the plan step that drops rows.
19. **Yank then paste duplicates.** `y` then `p` on a row adds one row equal
    to the yanked row, apart from the row id.

### 3.7 Sources and sinks

20. **Save then open.** `:save x.csv` (and `.tsv`, `.jsonl`, `.parquet`),
    then open it, gives the view `rows`.
    - Paths: `write_df_by_extension` and the save formatting (S-47 §2)
      versus `reader::open` and its type inference.
21. **SQL identity.** `:sql SELECT * FROM self` equals the view.
    - Paths: the view registered as an SQL table versus `view_plan`.
22. **Self diff is empty.** `:diff` of a sheet against its own copy (`g"`)
    reports no changed rows or cells.
23. **Self join.** An inner join of a sheet with its copy on the unique
    `id` column has the view's row count. Each shared column appears
    twice, as `<sheet>_<col>` on each side (S-47).

## 4. Rejected relations

- **Filter partition** (`filter p` plus `filter NOT p` plus nulls equals
  all rows): both sides are one Polars `filter`. Relation 1 covers the vdrs
  part.
- **Sort is ordered by its key**: one Polars `sort`. The golden and export
  oracles (S-55 §4.1) already check the order.
- **Cast to text and back is identity**: Polars cast semantics, and
  VisiData keeps failed casts as errors (S-47 quarantine), so the relation
  is false by design.

## 5. Running

- All relations run in process through the `Harness`, on tables from the
  S-55 generator. They need no external tool, so they run in `just check`.
- One proptest per section, with a low case count (about 32 each).
- A relation that fails on a VisiData-compatible behavior is wrong, not
  the code. Remove or fix the relation, and give the reason in §4.

## 6. Open questions

1. Relation 13: VisiData's row rule for empty and null lists in `zM`.
2. Relation 9: whether `describe` and the footer use the same mean for
   integer columns (integer or float result).
3. Can relations 1 to 3 share one generated selection, to save run time?
