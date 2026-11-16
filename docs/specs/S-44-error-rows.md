# S-44 — Error rows

Status: **shipped 2026-11-16**.
Depends on: S-42 A (JSON Lines comment lines).

## 1. What

VisiData loads a JSON line it can't parse as an error row: a row whose only field is `''`, every
cell shown `#ERR`. Its `test.jsonl` sample has one, so 7 replay files used to fail at load.

| Part | vdrs |
|---|---|
| A | A JSON Lines line that isn't an object loads as `{"": true}`. A sheet whose view has a boolean column named `""` (`reader::ERR_COL`) has error rows: rows where it is set. Render: `#ERR` in every cell. `F` and pivot keys: text, `#ERR` on error rows (`Sheet::err_key`). `:save` and the replay write `Sheet::save_plan`: every cell text, `#ERR` on error rows. |

## 2. As built

Replay 88 → 89 (save-json). Quarantined, each for a non-error difference:

- 1410b and pivot-error: VisiData sorts an error before a null; vdrs sorts `#ERR` as text, after
  the null. pivot-error also has a `null` column (VisiData drops null pivot values).
- describe-error: `distinct` of a date column counts VisiData's datetimes (vdrs truncates to
  dates); the `''` column counts the error row as null; sum of dates is `#ERR`.
- aggregators-errors: pivot of a binned date key.
- melt-error: VisiData names melt columns `Variable` / `Value` and drops null values.
- freq-error stays untestable (`addcol-expr`).

## 3. Not done

- Per-cell errors: a failed type cast shows an empty cell (describe's `errors` counts them).
- Hiding `""` hides `#ERR` too.
- Error rows in JSON Lines files over 256 MiB: a bad line still fails the read.
- `allow-error` replay steps (VisiData error messages), not error cells.
