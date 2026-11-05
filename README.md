# visidata-rs

Modal terminal data explorer with VisiData's taste: Polars + Ratatui + mlua,
Lua-extensible like Neovim. Pre-alpha.

```
cargo run --release -- data.parquet          # TUI
cargo run --release -- --headless data.csv   # schema + first rows, no TUI
cat data.csv | cargo run --release -- -      # stdin, format sniffed
```

Reads CSV, TSV, Parquet, Arrow IPC, JSON Lines, JSON arrays, xlsx/ods (first
sheet), SQLite (first table), and any of these gzipped. Files are scanned
lazily; only a 1024-row window is materialized, and the row count is computed
in the background.

Basics: `hjkl` move, `gg`/`G` top/bottom, `/` search, `[`/`]` sort, `z|` filter,
`F` frequency table, `s`/`gs`/`|` select, `"` sheet of selected rows, `-` hide,
`!` key column, `#%~@` type casts, `P` plan panel, `S` sheet list, `:` ex
commands (`:save out.parquet`, `:pivot`, `:unpivot`, `:diff`, `:tail -f`),
`q` back/quit.

- What ships: [`docs/specs/S-15-feature-inventory.md`](docs/specs/S-15-feature-inventory.md)
- Direction & scope: [`docs/direction.md`](docs/direction.md)
- Punchlist: [`docs/tasks.md`](docs/tasks.md)
- Config: `$XDG_CONFIG_HOME/vdrs/init.lua` (`vd.bind`, `vd.register_loader`)

Not vendored: a VisiData clone lives at `../visidata` as read-only reference
(the golden-corpus replay test skips cleanly without it).
