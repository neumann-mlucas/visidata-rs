# S-61 — Column widths: `g_`, `z_`, `gz_`, `z-`, and a screen-wide cap

Status: **shipped 2026-11-26**.
Depends on: S-38 B (copies keep widths), S-57 (layout properties).

## 1. Why

vdrs binds only `_` of VisiData's width keys. On a wide CSV the user wants `g_` to widen
every column at once, and `z_` to set one exact width. `_` also widens a column past the
screen when one cell is long (a URL, a JSON blob), so no other column shows.

## 2. Keys

VisiData's `features/layout.py`:

| Key | Longname | Does |
|---|---|---|
| `_` | `resize-col-max` | toggle the column between full and default width (exists) |
| `g_` | `resize-cols-max` | every visible column to full width; if all are full, back to default |
| `z_` | `resize-col-input` | ask for N, set the column width to N |
| `gz_` | `resize-cols-input` | ask for N, set every visible column to N |
| `z-` | `resize-col-half` | halve the column's width |

## 3. Design

- **Full width is capped** at the sheet area's width minus 1, as VisiData's `getMaxWidth`
  stops at `windowWidth - 1`. The cap applies to `_` and `g_`.
- **Full width is measured on the loaded window**, as `_` does now: up to 256 rows spread
  through 1024. VisiData measures the rows on screen only. This difference stays, because a
  width measured on screen is wrong as soon as you scroll.
- **Default width** is vdrs's automatic width (up to 40), not VisiData's `default_width` 20.
  vdrs has no `default_width` option.
- **Exact widths.** `width_floor` only widens a column, so `z_` and `z-` need a width that
  can also shrink it. `SheetState::width_set` holds one per column name. It wins over the
  measured width and the floor. `_` and `g_` clear it for the columns they change. Copies
  keep it, as they keep the floor.
- **Width 0** is refused with a toast. VisiData hides the column; vdrs has `-` for that.
- **Ex commands**, for macros, `.` and sessions: `:width N` (the current column) and
  `:widths N` (every visible column). `g_` and `z-` are named actions
  (`resize_cols_max`, `resize_col_half`).

## 4. Plan

1. `width_set` in `SheetState`, applied in `widget::ensure_widths`, copied by
   `inherit_attrs`. — verify: a unit test where a set width shrinks a measured column.
2. Cap `_` at the sheet width; add `g_`. — verify: Harness tests for a long cell and for
   `g_` widening then resetting.
3. `z_`, `gz_` prompts and `:width` / `:widths`; `z-`. — verify: Harness tests through
   the keys; `to_ex` round trip.
4. Keymap, guide key rows and prose, S-15, `vd_replay` longnames. — verify: `just check`.

## 5. Out of scope

- `resize-col-max-all` / `resize-cols-max-all` (no key in VisiData; they measure every row).
- Saving widths in sessions: sessions don't save `_` widths either.
