# S-46 — More VisiData commands

Status: **A shipped 2026-11-16; B draft, to refine (`gF`, `z<CR>` shipped in S-51).**
Depends on: S-25 (replay), S-34 (column order), S-35 (freq).

Commands VisiData's replay corpus uses that vdrs lacks (triage `SKIP <longname>`).

## A — Translations and `freeze-col` (shipped)

| Command | Key | vdrs |
|---|---|---|
| `dive-selected` | `g<CR>` | existed (`SubmitSelected`); the replay now dives a freq sheet's selected rows |
| `freeze-col` | `'` | new `FreezeCol`: `<col>_frozen` after the column, a `Derive` copy with the column's type cast, aggregators, fmtstr and width. vdrs's `FreezeColumn` (`:freeze`) pins columns, a different thing. |

The columns sheet now lists non-key columns in display order (S-34 `order`, a new column's
place), not schema order.

Replay 94 → 96: frozen-attrs, freqtbl-nested-dive-selected. column-copy now stops on a freq row
lookup and then edits a freq cell (freq sheets stay read-only, S-45). record-aggr
(`memo-aggregate`, `z+` exists) also needs `addcol-expr`: untestable.

Ponytail: the copy is a `Derive`, so an edit of the original before it shows in the copy;
VisiData's copy keeps the old values.

## B — New commands (draft)

Each is a part of its own, with a spec section, `Harness` test, guide rows and replay
translation when built. Order by value. `freq-keys` `gF` and `open-cell` `z<CR>` shipped in S-51.

| Command | Key | Sketch | Open questions |
|---|---|---|---|
| `dive-except` | `z<CR>` on freq | shipped: B1 | — |
| `addcol-aggregate` | — | shipped: B4 | — |
| `addcol-rank-sheet` | — | shipped: B4 | — |
| `type-len` | `z#` | shipped: B2 | — |
| `sheets-all` | `gS` | every open sheet, not only the stack | vdrs has one stack: same as `S` plus closed-but-kept sheets? |
| `save-all` | `g<C-s>` | every sheet to a path | one file per sheet in a directory, or one multi-part file (xlsx, duckdb); name clashes |
| `open-syspaste` | `gP` | clipboard text as a new sheet, filetype asked (default tsv) | tests stub the paste command (`paste_cmd_for`) |
| `join-cols` | — (columns sheet) | shipped: B3 | — |
| `commit-sheet` | `z<C-s>` | VisiData writes a duckdb/sqlite sheet's edits back to its table; vdrs's commits the macros/options sheets | edits → `UPDATE`/`INSERT`/`DELETE` by primary key; no key |

Decisions (2026-11-23): all nine ship on one branch, one commit each. New
columns from `addcol-aggregate` and `addcol-rank-sheet` are live `Derive`
steps, so `:export` and sessions rebuild them. `type-len` is a view cast.

### B1 — `dive-except` (shipped)

- `zEnter` on a freq sheet is VisiData's `dive-except`. On other sheets it stays `open-cell`.
- `gzEnter` is `dive-except-selected`: the rows outside every selected bin.
- `Derivation::Values` gets `except`. The predicate is negated after nulls become false.
  So a null key stays unless the null bin is picked.
- On a `gF` sheet, `rows_with_keys` keeps the complement of the matched row ids.
- Exports negate the same predicate: `~(…).fill_null(False)` and `NOT COALESCE(…, FALSE)`.
- Sessions replay the dive as `dive_except_selected` over the picked bins.

Row order follows VisiData's `openRows` for every freq dive. Each bin's rows come
together, the biggest bin first, ties by first appearance. Before, a multi-bin
dive kept source order. Ponytail: a freq sheet that the user sorted again still
orders the dive by count.

Replay 102 → 103: freq-dive-except.

### B2 — `type-len` (shipped)

- `z#` and `:type len` add a view cast to `LEN_CAST` (UInt32). `#` casts to Int64,
  so no other cast picks this type.
- `len_expr` is VisiData's `vlen`: a list's items, a struct's fields (a dict's keys),
  else the characters of the value's text. Null stays null.
- `cast_expr` now takes the loaded type, since a list and a text need different exprs.
- The columns sheet shows the type as `len`. `gz#` and a `len` type cell cast there.
- Exports: Polars `list.len` / `str.len_chars`; DuckDB `len` / `length`.
- The vim profile keeps `z#` as the int cast; `:type len` works there.

Replay 103 → 104: 1410c.

### B3 — `join-cols` (shipped)

- `:join-cols` on the columns sheet joins the selected columns. VisiData binds no key.
- `Sheet::join_cols` adds a `Derive`. Its SQL runs in Polars and DuckDB alike:
  `COALESCE(CAST(c AS VARCHAR), '') || ' ' || …`. So undo, export and sessions need nothing new.
- A null joins as empty text, as VisiData's display value `''`.
- Options `name_joiner` (`_`) and `value_joiner` (one space) are VisiData's.
  The new option kind `Text` keeps blanks: `check` trims every other kind.
- The column goes before the cursor row's column, as VisiData's `index=cursorRowIndex`.
- The columns must come from one sheet (`gC` can list several).
- Ponytail: values join as their plain text, not their display format
  (`fmtstr`, precision). VisiData joins display values.

Replay 104 → 105: join-cols-single-sheet.

### B4 — `addcol-aggregate`, `addcol-rank-sheet` (shipped)

- `:addcol-aggregate <aggs>` adds `<col>_<agg>` per aggregator, after the cursor column.
  On a key column, the new columns come first among the non-keys, as VisiData's `addColumnAtCursor`.
- Each column is a new `PlanStep::GroupAgg`: `agg_of(col)` within the key groups (`over`).
  Without keys, the group is the whole sheet. Null keys group together.
- `rank` is VisiData's `RankAggregator`: a dense rank within the key group.
  It ranks descending when the sheet sorts that column descending. It needs a key column.
- `:addcol-rank-sheet` adds `<sheet>_sheetrank`: the dense rank of each row's key tuple.
  Sorted key columns come first, in their direction, then the other keys ascending.
- Null is the smallest value, as VisiData's `TypedWrapper`: first ascending, last descending.
- `dense_rank` sorts the rows, counts where the tuple changes, then gathers the counts back.
  Exports do the same in Polars. DuckDB uses `dense_rank() OVER (... NULLS FIRST)`.
- The step keeps the view casts of the columns it reads, so a later cast doesn't change it.
  Only `int`, `float` and `str` casts can be read. A date or len cast refuses.
- `:addcol-agg <name> <agg> by=<col>|desc=<col> … [over=<col> …] [cast=<col>:<type> …]`
  spells the step out. Sessions replay both commands with it, before the view's keys and casts.
- Replay: an `open-row` with a row number on a sqlite or xlsx sheet opens that part.
- Ponytail: an error row (S-44 A) groups with the null keys. VisiData gives each error row its own group.

Replay 105 → 105. Both corpus files run but are quarantined:

- `aggregators-cols`: an error row shows `#ERR` in every cell. VisiData shows the added columns' values.
- `rank-sheetrank-sorted-cols`: every rank matches. A sqlite `date` column loads as text,
  so it doesn't show in `disp_date_fmt`.

Not planned: `setcol-iter` (Python expression), `setcol-format-enum`, `type-decimal`,
`addcol-window`, `join-sheets-cols`.
