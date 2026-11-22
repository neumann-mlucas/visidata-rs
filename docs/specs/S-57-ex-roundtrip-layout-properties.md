# S-57 Ex round trip and layout properties

Status: shipped 2026-11-22.
Depends on: S-19 (`Action::to_ex`), S-29 C (sessions), `src/widget.rs`.

## 1. Problem

Macros and sessions store actions as ex lines (`Action::to_ex`), and
`exline::parse` reads them back. `to_ex_round_trips_through_the_ex_parser`
checks about 80 fixed actions. A column name with a quote, a `#` or a
trailing space can still write a line that reads back as another action, or
fails to parse. Then a saved session restores the wrong view.

The layout functions in `src/widget.rs` place columns on screen. Commit
`2e3a4b5` fixed a `u16` overflow in them. Three unit tests cover the file.

This spec adds the first property tests (proptest) for both. It is the
smallest step before S-55 and S-56.

## 2. Ex round trip

Tests in `src/action/props.rs`.

### 2.1 Generators

- Text: plain words, then a pool of hard strings (empty, spaces at the
  ends, `"`, `'`, `\`, `#`, `,`, ` -- `, `=`, `|`, newline, tab, wide and
  combining Unicode), then any printable string up to 8 characters.
- Actions: every variant that `to_ex` writes with arguments, with fields
  from the text generator and from small integers.

### 2.2 Properties

1. **Parse back.** If `a.to_ex()` is `Some(line)`, then
   `exline::parse(line)` is `Ok(vec![a])`.
2. **No silent refusal.** If every text field is a plain word, then
   `a.to_ex()` is `Some`. Property 1 alone passes when `to_ex` returns
   `None` for every action. Property 2 stops that fix.
3. **Quote round trip.** For any string `s`,
   `split_args(quote_arg(s))` is `[s]`.

A failure in property 1 is a bug in `to_ex` or in the parser. The fix is
one of these:

- Change the parser so that it reads the line back.
- Change `to_ex` so that it returns `None` for a value that ex syntax can't
  spell, such as a newline. The contract of `to_ex` allows `None`.

## 3. Layout

Tests in `src/widget/props.rs`. Widths go up to `u16::MAX`. Areas go from
1×1 to 400×100, at any `x` offset that keeps `x + width` in `u16`.

1. **`take_width` fits.** The output is a prefix of the input. Its display
   width is at most `max`. The next character would exceed `max`.
2. **`visible_columns` starts in order.** Frozen columns come first, then
   scrolled columns from `col_offset`. Each column starts where the one
   before it ends, plus `CELL_PAD`. Only the last scrolled column may start
   or end past the right edge.
3. **`column_at` agrees with `visible_columns`.** For each visible column
   that starts inside the area, `column_at` at that `x` returns that column.
4. **`clamp_view` keeps the cursor on screen.**
   - The row cursor is in range, and between `row_offset` and the last
     visible data row.
   - The column cursor is in range and in `visible_columns`.
   - If the frozen columns and the cursor column fit in the area, the cursor
     column ends inside the area.
   - A second `clamp_view` changes nothing (idempotent).
5. **Render does not panic.** `SheetWidget` draws a sheet with generated
   cell text (wide Unicode, empty, 70,000 characters) in any area, with any
   cursor and frozen count.

## 4. Running

- All properties run in `just check`, with proptest's default 256 cases.
  `PROPTEST_CASES` changes the count.
- Proptest writes failing seeds to `proptest-regressions/`. Commit them.
  The prek hooks skip that folder, as they skip other test data.

## 5. What the properties found

- `first_arg` closed a quoted argument at the first `"`, also an escaped
  one. A column name with a quote broke `:filter`, `:color`, `:validate`,
  `:join` and `:lookup` lines. Now a backslash escapes the next character,
  as in `split_args`.
- `to_ex` wrote free text (`:hits`, `:capture`, `:note`, `:set`, `:derive`
  expressions and more) without quotes, and the parser trims it. `hits
  "trail "` came back as `hits trail`, and empty text wrote lines that don't
  parse. Now `to_ex` returns `None` for text that is empty, has spaces at its
  ends or holds a newline. A column named `*` in `:color` also returns `None`,
  as `*` means every column.
- A substitute pattern that ends in a lone `\` escaped the delimiter.
  `to_ex` now returns `None` for it.
- `SheetWidget` overflowed `u16` for a column after a frozen one widened
  past 65,535 cells (`_` on a huge cell, then `:freeze`). Each drawn column
  now keeps `x + width` in `u16`, and a zero-width cell draws nothing
  (it drew `…` one cell past its column).

## 6. Done when

- The properties pass in `just check`.
- Each bug they found has a fix and a fixed case in the existing unit test.
