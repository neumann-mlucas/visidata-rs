# S-59 Bar layout: no title bar, title in the status line, header rule

Status: shipped 2026-11-24.

The title bar's filled pills touch the filled header row: terminal rows have
no spacing, so the two read as one block. A user can now move the title's
content to the status line, drop the top row and put a rule under the header.
The defaults do not change.

## Lua API (`vd.api_version` 23)

- `vd.titlebar(false)` removes the title bar. The table starts on row 0.
  `vd.titlebar(fn)` and `vd.titlebar(nil)` keep their meaning.
- A `Z` split keeps a title row for each pane, so the panes stay named.
- `vd.statusline(false)` is an error. Prompts, questions and messages need
  that line.
- Every bar `ctx` carries both built-in bars. `vd.bar.titlebar(ctx)` works in
  a `vd.statusline` function, `vd.bar.statusline(ctx)` in a `vd.titlebar`
  function, and both work in a `vd.register_page` render.
- In a `vd.statusline` ctx, the built-in title fits the room that the status
  line's right side leaves. The active sheet's crumb stays in sight.

## Crumbs

- A file shows by its file name, not its full path.
- A part of a file that the stack already names shows by the part name alone:
  `book.xlsx: sheets › Sheet2 › freq(region)`.

## Option

- `disp_header_rule` (`"true"` or `"false"`, default `"false"`) draws a
  `─` rule under the header row. The rule crosses the pinned-column divider
  as `┼`. `:set disp_header_rule true` changes it for one run.
- The rule takes one table row. Page moves, the mouse and the cursor clamp
  count it as part of the header.

## Example: the title in the status line

```lua
vd.titlebar(false)
vd.statusline(function(ctx)
  local t, s = vd.bar.titlebar(ctx), vd.bar.statusline(ctx)
  for _, seg in ipairs(s.left) do table.insert(t.left, seg) end
  for _, seg in ipairs(t.right) do table.insert(s.right, seg) end
  return { left = t.left, right = s.right }
end)
vd.option("disp_header_rule", "true")
```

## Not done

- A `vd.bars.top { … }` table API. `false` on the existing functions covers
  the need.
- A way to hide the title bar only when a function returns nothing. An empty
  result still draws a blank bar.
- Column separators. S-54 explains their cost.
