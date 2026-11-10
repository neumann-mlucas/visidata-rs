# S-27 — Conditional formatting

Status: **A shipped 2026-11-10**; B, C open.
Depends on: S-15 §7.3 (renderers), S-21 (SQL predicates), theme (`src/theme.rs`), S-26 A4 (color
depth). Excel-gap item: VisiData colors rows only by state (cursor, selected, error).

## 1. What exists (verified)

- `vd.register_renderer { column | dtype, render = fn(v) -> string|nil }`, memoized per value, counted
  in column widths. Returns text only.
- Theme is two fixed palettes (`Theme::dark/light`), 24-bit RGB.
- Only the 1024-row window is ever materialized, so any per-cell rule costs O(window), not O(file).

## 2. Design

Rules live in `View.formats: Vec<FormatRule>`: per sheet, undoable, display only. Derived sheets start
without rules (F1).

| Rule | Command | Evaluated as |
|---|---|---|
| Predicate color | `:color <col\|*> <sql pred> <style>` | `sql_expr(pred)` as an extra Boolean column in `window_plan` (`__vdrs_fmt_N`), so Polars computes it with the window |
| Color scale | `:scale <col> [low-color high-color]` | min/max from a background stats job (the `I` describe path, one column); cell color interpolated in Rust at render |
| Data bar | `:databar <col>` | same min/max; cell background filled proportional (`█▉▊▋▌▍▎▏`, eighths) |
| Lua | renderer returns `{ text = …, fg = …, bg = …, bold = … }` or a string (api bump) | memoized as today |
| Clear | `:nocolor [col]` | — |

- `<style>`: `red`, `bold`, `fg=#ff8800`, `bg=yellow,bold`; named colors map through the theme so
  dark/light both read well; RGB downgraded by S-26 A4.
- `*` = whole row (row style), else the cell.
- Precedence: cursor > selection > rules (last rule wins) > base theme. Selected rows keep a visible
  marker even when a rule paints the row.
- Rules reference columns by name; renaming a column updates rules (same code path as filter/sort refs
  in `RenameCol`). A rule on a dropped column is removed with a toast.
- Export: `:export` ignores formats (data only). Persist via S-29 sessions.

## 3. Phases

| Phase | Ships | Effort |
|---|---|---|
| A | `:color` predicate rules + `:nocolor`, row and cell, styles | 1.5–2 d |
| B | `:scale`, `:databar` (background min/max job) | 1–1.5 d |
| C | Lua renderer styles (`vd.api_version` bump) | 0.5 d |

## 4. Test plan

- Unit: style parser (`bg=yellow,bold`, bad names refused); scale interpolation endpoints + nulls; data
  bar eighths for 0, 0.5, 1.
- Scenario: `:color amount amount > 100 red` → TestBackend buffer has the red fg exactly on matching
  cells; after `[` sort the colors follow the rows; `U` removes the rule; rename keeps it.
- Scenario: rule columns never reach `:save` or `:export` output (`__vdrs_fmt_*` dropped like rids).
- Render snapshot per phase, dark + light theme; `NO_COLOR` → bold/reverse only.
- Docs: guide card + `docs-format` recording.

## 5. Decisions (confirmed 2026-11-08)

- **F1** derived sheets start without format rules.
- **F2** data bars are `:databar` (`:bar` is the S-23 bar chart).
