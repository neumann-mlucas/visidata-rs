# S-54 Title bar crumbs, VisiData status line

Status: shipped 2026-11-19.

The doc screens showed the same 98-character key hint on every status line,
the sort twice (title chip and header arrow) and chips that rarely change.

- Title bar left: the sheet stack as crumbs, `sales.csv › freq(region) ›
  region=north`, each name without the `: parent` its command appended. The
  head gives way first. Replaces the `sheets N` chip.
- Chips: no source kind; `sort` only for several keys (one key is the header's
  arrow). Position: `row N/total`, no percent.
- Status line: VisiData's layout. Left: messages, selection stats, prompts as
  before, else empty (no key hint). Right, dim: the last command's keys and
  name (`]  sort_desc`; `derive` after `:derive`). Prompts and questions keep
  the whole line.
- Lua: `ctx.last` holds that text; `vd.bar.statusline(ctx).right` the segment.
  `vd.api_version` 22.
- Header type mark: VisiData's icons, `#` integer, `%` float, `@` date or
  datetime, `~` text. Reverted 2026-11-22: the header shows the short
  type name (`dtype_badge`: `i` `f` `str` `date`) again, as the columns
  sheet and Lua `dtype` do.
- Config stays in Lua (`vd.statusline`, `vd.titlebar`); no format-string
  options.

Not done: column separators. VisiData pads each column by one cell per side
and draws `│` after (`getMaxWidth` + 2), two cells more per column.
