# S-43 — `header` option, rename columns from rows

Status: **shipped 2026-11-16**.
Depends on: S-03 (readers), S-42 B (fixed width, usv).

## 1. What

VisiData's `header` option (`loaders/__init__.py`): how many leading rows of a sequence file
are column names. 0 reads every row as data; N > 1 joins each column's N cells. With it come
`z^` `g^` `gz^` (`rename_col.py`), which name columns after cell contents: the usual way to fix
a file whose names aren't on its first row.

| Part | Key / option | vdrs |
|---|---|---|
| A | option `header` (default 1) | CSV/TSV, xlsx (and its parts), fixed width, usv. `header 0` names columns `column_1`, … (Polars' names: a frame can't hold VisiData's blank names twice). Applies to later opens; the source keeps the value it was read with (part dives use it, a session sets it around `:open`). |
| A | `z^` `g^` `gz^` | `RenameColsFromRows { all, overwrite }`: the selected rows' (else the cursor row's) cells, joined by a space (VisiData: newline). `g^` only renames unnamed columns: empty or `column_<n>`. One undo step. |

## 2. As built

Replay 87 → 88 (xlsx-header). The replay honours `set-option header` only for every sheet type
(`""`, `global`, `override`), not a class's own (`SqliteSheet`). load-fixed-header0 and
load-fixed-leadingspaces now run to the end and differ only in the header line (blank names in
VisiData): quarantined. `:export` refuses a sheet read with `header` ≠ 1.

## 3. Not done

- `:export` of a sheet read with `header` ≠ 1 (refused, not translated).
- `:tail`, `:diff` and `:join` files open with `header 1`.
