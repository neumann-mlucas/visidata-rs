# S-48 Option `csv_skipinitialspace`

Status: shipped 2026-11-16.

VisiData passes `skipinitialspace=True` to Python's `csv.reader` (option
`csv_skipinitialspace`, default on): `1, ann` reads as `1` and `ann`, column names too.
vdrs read the blanks. Found by S-47's `cast-date` / `cast-float`.

## Behavior

- Option `csv_skipinitialspace`, Bool, default `"true"`; `vd.option`, `:set`, `O`.
  Applies to `.csv` opens after it is set (VisiData's tsv loader doesn't use it).
- Polars has no such setting. `csv_schema`'s inference pass already reads every
  text cell (first 100k rows of a big file): when a name or text cell starts with
  a blank, the scan strips leading spaces from the text columns and the names
  (`LoadedSource.stripped`). A file without blanks reads as before.
- A column with blanks was inferred as text, and stays text (`" 5"` → `"5"`).
- `:export` of a stripped source strips too: Polars `new_columns` +
  `str.strip_chars_start`, DuckDB `ltrim` on text columns.
- The flag is a reader global set from the options (as `duckdb_cmd`); every app
  and config reload sets it. Golden cases set it per case before opening.

## Tests

- Golden `csv-skipinitialspace` (default) and `csv-keep-space` (`init.lua` off),
  both exports checked.
- Scenario `csv_skipinitialspace_set_off_keeps_blanks` (`:set`, then `:open`).
- `tests/vd_local` `cast-date`, `cast-float` pass.
