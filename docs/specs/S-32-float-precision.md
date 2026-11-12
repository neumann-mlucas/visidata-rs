# S-32 — Float precision: `Alt++` / `Alt+-`, `disp_float_fmt`

Status: **drafted 2026-11-12**. Depends on: S-18 B (`vd.option`), S-25 P3 (columns sheet `fmtstr`).

Corpus: `setcol_precision_more`, `setcol_precision_less`, `setcol-precision` (first blocker), and the
2-decimal FAILs `avg-nulls`, `edit-type` (+ part of `describe`, `numeric_binning`).

## 1. VisiData reference

- Float, currency and decimal columns show with `options.disp_float_fmt` = `{:.2f}`; a column's
  `fmtstr` overrides it. Untyped text (a CSV column never cast) shows as read: `189.05` stays.
- `Alt++` `setcol-precision-more` / `Alt+-` `setcol-precision-less`: the column's `fmtstr`
  becomes `%.{2±1}f` the first time, then ±1 from there, never below 0.
- VisiData saves display values, so its goldens hold `999.50`, `2.0`, `3.1416`.

## 2. Design

- `View.precision: Vec<(String, u8)>` (column, decimals), like `aggregators`: renamed with the column,
  in sessions (`:mksession` writes `:precision col N`), inherited by S-31 copies.
- Display (`widget::float_text`): a float column with a precision shows `format!("{f:.N$}")`;
  otherwise `vd.option("disp_float_fmt", "%.2f")` if set (accepts `%.Nf` and `{:.Nf}`, else a
  load error); otherwise today's shortest 15-significant-digit text. Aggregator footer and
  selection stats follow the column's precision.
- `Alt++` / `Alt+-` on a float column: unset → 2±1 (VisiData's start), then ±1, clamped 0..=15.
  Status `Total: 3 decimals`. On a non-float column: `precision: Total is str (float columns
  only)`; dates are out of scope (P2). Not undoable (a view setting, like column width).
- `:precision [N]` sets the current column (no N: clear). Columns sheet `fmtstr` shows `%.Nf` and
  writes back `%.Nf` / `{:.Nf}` / empty.
- **Display only** (P1): `:save`, `:export`, `y` and `Y` keep full values.

## 3. Phases

| Phase | Ships | Effort |
|---|---|---|
| A | `View.precision`, display, `Alt++`/`Alt+-`, `:precision`, session line | 0.5–1 d |
| B | `disp_float_fmt` option, columns sheet `fmtstr` read/write | 0.5 d |
| C | replay: driver keeps VisiData's typed-float columns (`%` `$`, aggregates, describe stats) and writes them with the column's fmtstr, else `%.2f` | 0.5–1 d |

## 4. Test plan

- Unit: `float_text` with precision 0/3, option forms `%.2f` `{:.3f}`, bad option text refused.
- Scenario: `Alt++` twice on a float column shows 4 decimals; `Alt+-` on 0 stays 0; refused on text;
  `:mksession` round trip keeps precision; `"` keeps it.
- Golden: `precision-more` screen (the docs screen of the formatting section).
- Corpus: translate `setcol-precision-more`/`-less`; driver formats typed floats; un-quarantine
  `avg-nulls`, `edit-type` if they pass; raise `MIN_PASS`.

## 5. Decisions (proposed, 2026-11-12)

- **P1** Precision changes the display only; saved files keep every digit (VisiData rounds on save —
  silent data loss on a round trip).
- **P2** No date precision cycling (VisiData's `%Y` … `%f` list) until asked.
- **P3** The vdrs default stays the 15-digit text; VisiData's 2 decimals is one option line away.
