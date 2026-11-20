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
| `dive-except` | `z<CR>` on freq | parent rows not in the cursor row's bin: `rows_with_values` negated | `DiveKey::Bin` too |
| `addcol-aggregate` | — | `<col>_<agg>` per key group: `agg_expr(col).over(keys)` as a `Derive`-like step | aggregators the window can't take (mode, percentiles?); no keys = whole sheet |
| `addcol-rank-sheet` | — | rank of each row by the key columns: `rank(Ordinal)` over the key tuple | ties (VisiData: sorted position); nulls |
| `type-len` | `z#` | column shows its length: `str.len_chars` / `list.len` as a view cast | a "type" that changes values: cast or derive? sort/filter on it |
| `sheets-all` | `gS` | every open sheet, not only the stack | vdrs has one stack: same as `S` plus closed-but-kept sheets? |
| `save-all` | `g<C-s>` | every sheet to a path | one file per sheet in a directory, or one multi-part file (xlsx, duckdb); name clashes |
| `open-syspaste` | `gP` | clipboard text as a new sheet, filetype asked (default tsv) | tests stub the paste command (`paste_cmd_for`) |
| `join-cols` | — (columns sheet) | a new column concatenating the selected columns | separator (VisiData: none?); types → text |
| `commit-sheet` | `z<C-s>` | VisiData writes a duckdb/sqlite sheet's edits back to its table; vdrs's commits the macros/options sheets | edits → `UPDATE`/`INSERT`/`DELETE` by primary key; no key |

Not planned: `setcol-iter` (Python expression), `setcol-format-enum`, `type-decimal`,
`addcol-window`, `join-sheets-cols`.
