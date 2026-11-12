# S-31 — Column builders (`za` `i` `gi` `f` `;`), inherited column attributes

Status: **drafted 2026-11-12; A, B, C shipped 2026-11-12**. As built: the scans of `i` `gi` `f` run on
the UI thread, as `gzp` does (moving them to jobs is S-24 R6); unnamed groups are fields `0`, `1`…
(VisiData's `Date_re[0]`); freq dives inherit too; `SetCells` became a `replace_strict` lookup instead of a
join (a join per numbered column nested the plan until Polars overflowed the stack), and the runtime's
threads get 16 MiB stacks. Replay PASS 55 → 58 (`fill`, `fill-zero`, `freeze-sheet-attrs`); quarantined
`pr2614`/`pr2855` (VisiData names the column `A`, vdrs `incr`), `capture-col-named` (S2). Depends on: S-18 C/D (`SetCol`, `SetCells` steps), S-21 (`Derive`),
S-22 (`(` expands structs), S-25 P3/P4 (columns sheet, aggregators).

Corpus (first blocker today, `tests/vd_replay/triage.txt`): `addcol-new` 4, `addcol-incr` 2,
`setcol-incr` 1, `setcol-fill` 2, `addcol-capture` 2, `dup-rows-attrs`, `freeze-sheet-attrs`. Ceiling
~12 files; most chain into a later blocker, so expect fewer.

## 1. VisiData reference

| Key | Longname | Does |
|---|---|---|
| `za` | `addcol-new` | prompt a name, add an empty (settable) column after the cursor column |
| `i` | `addcol-incr` | add an int column numbering the rows 1…n (`incr_base` = 1) in sheet order |
| `gi` | `setcol-incr` | set the current column of the selected rows to 1…k, in sheet order |
| `f` | `setcol-fill` | in sheet order, fill null cells of the current column (selected rows, else all) with the last non-null value above |
| `;` | `addcol-capture` | prompt a regex with groups; add one column `<col>_re` holding the groups (tuple, or dict for named groups); `(` expands it |
| `"` `g"` `g'` | `dup-selected` `dup-rows` `freeze-sheet` | the copy keeps the parent's column objects: type, width, fmtstr, aggregators |

`addcol-incr-key`, `addcol-window` (`w`), `addcol-rank-sheet`, `addcol-aggregate`, `addcol-shell`,
`freeze-col` (`'`): out of scope (S1). All keys above are free in the vdrs default keymap.

## 2. Design

Every builder is made of existing steps, so undo, `:save`, `:export`, sessions and macros work unchanged.

- **`za`** = `Derive { name, expr: "CAST(NULL AS VARCHAR)" }`: a text column of nulls; `e` edits it
  as today. Name prompt refuses an existing name (`column already exists: x`).
- **`i`** = `Derive { name: <col>_incr…, expr: "CAST(NULL AS BIGINT)" }` + `SetCells` with
  `rid → 1..n` in **view order** (sort applies, rows hidden by a filter stay null). The rids in view
  order come from a background job (the view can be 10M rows), as `|` and `,` do. Name: `incr`,
  then `incr2`, … when taken (VisiData leaves it unnamed; vdrs columns need a name).
- **`gi`** = `SetCells` on the current column: selected rows in view order → 1..k, cast to the
  column's type (text columns get `"1"`, `"2"`…). No selection: refused (`no rows selected`).
- **`f`** = `SetCells` with only the null cells it fills: scan the current column in view order,
  carry the last non-null value; rows = selection, else every view row. Status: `filled 12 values`.
  Nothing to fill: no step, status `filled 0 values`.
- **`;`** = new step `PlanStep::Capture { column, pattern, name }` → one struct column `name` =
  `<col>_re`: `str.extract_groups(pattern)` (Polars; fields = group names, unnamed groups `1`, `2`…).
  No match → null struct. Pattern without a group refused (`specify a capture group using
  parentheses`, VisiData's text). Non-text columns read as their text (cast first). Export: Polars
  `str.extract_groups`; DuckDB `regexp_extract(col, pat, [names])` (struct). `(` then gives
  `Date_re.month` or `Date_re.1`.
- **Placement**: new columns go last, as `=` does today (S2).
- **Inherited attributes**: sheets that copy the parent's rows with its columns — `"` (`Selected`),
  `g"`/`:dup` (`Copy`), dedupe, and new `g'` (= `g"`, VisiData's `freeze-sheet`) — start with the
  parent's `type_casts` labels, `aggregators`, `formats`, `scales` and S-32 `precision`, for the
  columns they still have. Freq, pivot, describe, join, `:sql`, columns sheets start fresh: their
  columns mean something else. The data is already cast when copied, so the labels only restore
  what the columns sheet and the header badge say.

## 3. Phases

| Phase | Ships | Effort |
|---|---|---|
| A | `za`, `i`, `gi`, `f` (Derive + SetCells, rids in view order as a job) | 1–1.5 d |
| B | `;` + `PlanStep::Capture` + exports | 0.5–1 d |
| C | inherited attributes, `g'` | 0.5 d |

## 4. Test plan

- Unit: fill-down patch on nulls only, view order with a sort, selection-only, all-null column.
- Unit: `Capture` named / unnamed groups, no match → null, no group refused.
- Scenario (`Harness`, real keys): `za` + `e`; `i` after `[` sort numbers in sorted order; `gi` on
  three selected rows; `f` fills, `U` undoes; `;` then `(`; `"` keeps `+` aggregator and `#` type.
- Golden + export round trip: `fill-down`, `incr-sorted`, `capture-groups`.
- Corpus: translate `addcol-new`, `addcol-incr`, `setcol-incr`, `setcol-fill`, `addcol-capture`,
  `freeze-sheet`; driver maps VisiData's `Date_re[0]` to `Date_re.1`; raise `MIN_PASS`.

## 5. Decisions (proposed, 2026-11-12)

- **S1** Only the five builders above. `w` window, rank, per-key incr and shell columns wait for a
  user asking (each is one Polars expression when it comes).
- **S2** New columns go last, not after the cursor (vdrs has no column-move step yet). Revisit with a
  column-move feature.
- **S3** `i`/`gi`/`f` freeze values in view order at the time of the command (VisiData's semantics);
  a later sort doesn't renumber.
