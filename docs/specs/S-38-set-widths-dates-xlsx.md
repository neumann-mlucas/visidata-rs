# S-38 — `:set`, copies keep widths, `@` keeps the time, xlsx colour columns

Status: **A, B, C shipped 2026-11-14**; D agreed, waiting on the parity finding in D3.
Depends on: S-35 C1 (`numeric_binning`), S-35 C2 (date `fmtstr`), S-31 (derived sheets inherit
attributes), S-24 R3 (typed xlsx), S-29 (sessions).

Replay cases: A none (sessions); B `dup-rows-attrs`; C `describe` (stays quarantined over `#ERR`,
C4); D `xlsx-color-cells`, `issue2988` (stay quarantined, D3).

## A. `:set <option> <value>`

S-35 C1 left a hole: `F` with `numeric_binning` depends on `init.lua`, and a session writes only
`F`, so a restore under another `init.lua` bins differently. VisiData writes `set-option` into
the cmdlog for options marked `replay=True`, and replaying it changes the option for the rest of
the run. Vim's `:set` in a sourced file does the same.

- A1. **`:set name value`** changes an option for this run only; `init.lua` is never written.
  `:set name` alone shows its value. Options: `numeric_binning` (`true`/`false`) and
  `histogram_bins` (a count), the two that change data. Others refuse with
  `:set: <name> is set in init.lua (vd.option)`. Values are checked as `vd.option` checks them
  (one parser for both).
- A2. **Sessions** write the options a binned `F` used, right before it:
  `:set numeric_binning true`, `:set histogram_bins N` (`N` from the `Binning::Ranges` it holds),
  or `:set numeric_binning false` before an `F` on a number column that wasn't binned.
- A3. **They stay in effect after the restore** (VisiData's behaviour). Each `:set` toasts
  `name=value`.
- A4. A later `init.lua` hot reload resets them to `init.lua`'s values, as it resets everything.

## B. Copies keep the parent's column widths

VisiData's `"` copies the column objects, so a copy keeps the parent's widths (`Rep` stays 10
wide although the two copied rows need 8). vdrs measures each sheet's own window.

- B1. `Sheet::copy_from` and `dup_selected_from` (`g"` `"`) seed `state.width_floor`
  (column → width) from the parent's measured widths. `refresh_widths` never goes below a floor.
  Freq, pivot and other derived sheets don't (their columns are new).
- B2. Replay: the columns-sheet serializer measures VisiData-style widths on the parent for a
  sheet made by `dup-selected`/`dup-sheet`.

## C. `@` keeps the time

VisiData's date type keeps the whole datetime and shows `%Y-%m-%d`. vdrs's `@` throws the time
away (`lenient_date` strips it), which loses data: rows can't be sorted within a day, and
`describe` counts 32 distinct `Date`s where VisiData counts 47.

- C1. **Cast:** a Polars column has one type, so `@` picks it when pressed: `Datetime[μs]` when
  the loaded rows of a text column hold a time (`7/3/2018 1:47p`, `2018-07-03T10:00`), else
  `Date`, as before (a pure-date column must not save as midnight timestamps). Rows without a
  time read midnight. `@` on a datetime column keeps it (no truncation). Deciding from the loaded
  window keeps dispatch scan-free (S-33 D6); a time that only later rows have is dropped. The
  `type_casts` entry is the type picked; sessions write `:type date` and the restore picks again.
- C1b. **Time of day:** `TIME_OF_DAY` reads `H:MM[:SS][.fff][ ]a|p`; fractions and zones are
  ignored.
- C2. **Display:** a datetime column `@` made shows `%Y-%m-%d`, as VisiData. A date `fmtstr` (S-35 C2)
  overrides it (`%Y-%m-%d %H:%M` shows the time).
- C3. **Effects:** sort and `describe` use the full value (47 distinct). `F` groups by the shown
  text (S-35 C2), so it still counts days, as VisiData. Exports parse the same way
  (`str.to_datetime` / DuckDB `strptime` per layout).
- C4. **Not done:** VisiData's `#ERR` for a date column's sum/mean/stdev. vdrs leaves them empty,
  so `describe` stays quarantined with the reason narrowed to `#ERR`.

## D. xlsx colour columns (`xlsx_meta_columns`)

VisiData's `xlsx_meta_columns` adds `X_cellPyObj`, `X_fontColor`, `X_fillColor` (and a hidden
`X_colorizer`) after each column `X`.

- D1. `vd.option("xlsx_meta_columns", "true")` (and `:set`, A1). The xlsx reader asks calamine
  for `worksheet_style` and adds `X_fontColor` and `X_fillColor` as `AARRGGBB` text after each
  column; empty when the cell has no colour.
- D2. No `X_cellPyObj` (a Python object; nothing to show) and no `X_colorizer` (curses colour
  numbers).
- D3. **Parity finding:** VisiData shows openpyxl's raw colour values: a theme index (`1`, `9`),
  `True` for "automatic", or ARGB. calamine resolves theme colours to ARGB, so the values differ
  (`FFFF0000` matches; `1` doesn't). The two replay cases stay quarantined with that reason.
  What vdrs shows is the colour itself, which is easier to filter on.
- D4. Size about 1 d. Build after A–C if still wanted with D3.

## Test plan

- A: exline parse tests; a scenario: `:set numeric_binning true`, `F` bins; a session round trip
  restores the bins under a harness with binning off, and the toast names the option.
- B: unit: a `"` copy of two narrow rows keeps the parent's width; replay `dup-rows-attrs`.
- C: unit `lenient_date` keeps times; scenario: `@` shows `2018-07-03`, sorts within the day,
  `fmtstr %H:%M` shows the time; golden case with export round trips.
- D: reader test on a styled xlsx fixture (font and fill colours); scenario with the option on.
