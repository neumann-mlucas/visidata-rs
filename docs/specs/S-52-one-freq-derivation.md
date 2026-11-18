# S-52 One freq derivation for `F` and `gF`

Status: shipped 2026-11-17.

S-51 shipped `gF` as its own sheet kind (`Origin::FreqKeys`,
`Derivation::FreqKeys`, `freq_keys_from`, `dive_freq_keys`). It is `F` over
several columns, so it now is `F`'s sheet:

- `Origin::Freq { parent, columns }` and `Derivation::Freq { columns, bins }`.
  `F` groups by one column; `gF` by the key columns. `bins` (`numeric_binning`)
  only ever holds one column's bins.
- `Sheet::freq_binned(parent, id, columns, bins)` builds both. Every key
  groups by what it shows (a date `fmtstr`, a number format, `#ERR`), as `F`
  did; `gF` grouped by raw values before.
- Stat names: `freq_count`… when any key is named `count`, `percent` or
  `histogram`. Order: one numeric or date column keeps first-seen order (as `F`);
  otherwise count desc, ties first-seen.
- The grouped columns are the freq sheet's keys, `F`'s too (as VisiData), so
  `g&` joins freq sheets. `:export` skips a `select` that keeps the column order.
- Dive (`<CR>`, `g<CR>`): one column picks rows by value (`Derivation::Values`,
  as before); more columns pick rows by every key, each compared as the text it
  grouped by (`rows_with_keys`), giving `Derivation::Rids`.
- `:export` writes `gF`'s sheet too (a `GROUP BY` / `group_by` on every key).
- `:bar` on a `gF` sheet: bars labelled by its values joined with `,`.
- Sessions replay `gF` as before (the parent's keys as they are then).

Not done: `:join` over several sheets in one derivation (`g&` folds pairwise
joins, so only the last joined sheet takes edits back), and folding `Origin`
into `Derivation`.
