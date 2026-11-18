# S-51 Transpose, freq-keys, open-cell, join-all

Status: shipped 2026-11-17.

The four VisiData default keys S-50 left out, each a new sheet.

## `T` transpose

- `Sheet::transpose_from`: a row per visible non-key column, a column per row.
  Column names are the row's key values joined with `_` (VisiData `rowkey`),
  the row number without keys, made unique (`a_duplicated_3`). The first column
  holds the old names and is named after the keys (`column` without keys; VisiData
  leaves it blank, which vdrs keeps for the JSON error column). It is the key.
- Eager, and every cell is text (VisiData's transposed sheet loses types too, #2575).
  Read-only. `:refresh` rebuilds it; `:export` refuses.

## `gF` freq-keys

- `Sheet::freq_keys_from`: groups by every key column at once; `count`, `percent`,
  `histogram` and the parent's aggregators (`percent`/`histogram` hidden with
  aggregators, as `F`); count desc, ties first-seen. No keys → toast.
- `<CR>` / `g<CR>`: `rows_with_keys`, the parent's rows holding those key values
  (nulls match). Grouped by values, not a fmtstr's text. `:export` refuses.
  (S-52 makes it `F`'s sheet: grouped by shown text, exported.)

## `zEnter` open-cell

- `Sheet::open_cell`: the cursor cell (a list, a struct, or text holding a JSON
  array or object). A list of objects becomes a table, another list one column named
  after the sheet, an object `key`/`value` rows keyed on `key`. Values of mixed types
  become text, JSON for the nested ones, which `zEnter` opens again. Named
  `sheet[rowkey].column` (row number without keys), as VisiData.
- `<CR>` on such a sheet (`Origin::Cell`, editable): the row as `key`/`value` rows,
  `sheet[rowkey]`.
- serde_json and toml keep source key order now, so JSON objects and TOML tables
  read in file order, as VisiData.

## `g&` join-all

- `:join-all <how>`: the active sheet joined with every sheet below it, nearest
  first, `join_from` chained on the active sheet's keys (same names in the others).
  `g&` puts `join-all ` on the ex line, VisiData asks only for the type too.

## Tests

- Scenario `freq_keys_transpose_join_all_and_open_cell`.
- Replay: `freq-keys`, `transpose`, `open-cell` and `open-row` on a cell sheet
  translated. `test_quartiles` and `load-toml` pass; `MIN_PASS` 97.
  `listofdictobj` quarantined: VisiData adds Python float attributes `imag`/`real`
  as columns.
