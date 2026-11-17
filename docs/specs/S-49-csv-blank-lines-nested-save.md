# S-49 CSV blank lines; nested cells save as JSON

Status: shipped 2026-11-16.

Two VisiData fixes vdrs lacked, found by the parity probes (`docs/visidata-parity.md`).

## Blank lines (VisiData #3085, 3.4)

VisiData's csv and tsv loaders skip blank lines; Polars reads each as a row of nulls.

- At open, one byte pass looks for empty lines (`\n\n`, `\n\r\n`) after the
  first (`blank_lines`). If one has data after it, the scan drops rows whose cells
  are all null (`LoadedSource.csv.blank_lines`). Blank lines that only end the
  file drop that many last rows (`csv.trailing_blanks`), so a `,,` row stays.
  A file without blank lines reads as before.
- Ceiling: with a blank line inside the data, a `,,` line goes too (VisiData
  keeps it as an empty row).
- `:export` repeats it: Polars `.filter(pl.any_horizontal(pl.all().is_not_null()))`
  (trailing only: `.filter(pl.int_range(pl.len()) < pl.len() - k)`),
  DuckDB `WHERE NOT (a IS NULL AND …)` (DuckDB skips trailing blank lines itself).

## Nested cells in text saves (VisiData #2944, 3.5)

VisiData writes dict/list cells as `json.dumps(v, default=str, ensure_ascii=False)`;
vdrs failed with "CSV format does not support nested data".

- `:save` to csv/tsv turns List/Array/Struct columns into text first (`write_df`,
  `py_json`): Python's spacing `["x", "y"]`, `{"p": 1}`, non-ASCII kept, other
  scalars inside as quoted text. Null cells stay empty. JSON, Parquet and Arrow
  keep the nested types.

## Tests

- Golden `csv-blank-lines` (both exports checked). `freq-fmtstr-number` gets a
  second column so its blank-cell row isn't a blank line.
- Unit `write_df_text_formats_save_nested_cells_as_json`.
- One-column fixtures that used a blank line as a null got a second column
  (`null_bin_dive_opens_nan_rows`, `describe_select_cell`, `nulls_lead_…`); the
  failed-save tests use a binary column, which CSV still can't write.
- `tests/vd_local` `csv-blank-lines`, `save-nested-csv` pass (35/3).
