# visidata-rs

Modal terminal data explorer with VisiData's taste: Polars + Ratatui + mlua,
Lua-extensible like Neovim. Pre-alpha.

```
cargo install --path . --locked              # installs `vdrs`; --locked = tested dep versions
cargo run --release -- data.parquet          # TUI
cargo run --release -- --headless data.csv   # schema + first rows, no TUI
cat data.csv | cargo run --release -- -      # stdin, format sniffed
```

Reads CSV, TSV, Parquet, Arrow IPC, JSON Lines, JSON arrays, xlsx/ods (first
sheet), SQLite (first table), and any of these gzipped. Files are scanned
lazily; only a 1024-row window is materialized, and the row count is computed
in the background.

Basics: `hjkl` move, `gg`/`G` top/bottom, `/` search, `[`/`]` sort, `z|` filter,
`F` frequency table (`Enter` opens a value's rows), `I` describe, `W` pivot,
`s`/`gs`/`|`/`,` select, `"` sheet of selected rows, `-` hide, `!` key column,
`#%~@` type casts, `U`/`R` undo/redo, `P` plan panel, `S` sheet list, `:` ex
commands (`:join`, `:sql`, `:derive`, `:save out.parquet`, `:export view.sql`,
`:tail -f`, …), `q` back/quit.

- **User guide**: [`docs/index.html`](docs/index.html) — publish it with GitHub Pages
  (Settings → Pages → Deploy from a branch → `master`, folder `/docs`)
- What ships: [`docs/specs/S-15-feature-inventory.md`](docs/specs/S-15-feature-inventory.md)
- Direction & scope: [`docs/direction.md`](docs/direction.md)
- Punchlist: [`docs/tasks.md`](docs/tasks.md)
- Config: `$XDG_CONFIG_HOME/vdrs/init.lua` (`vd.bind`, `vd.register_loader`)

Not vendored: a VisiData clone lives at `../visidata` as read-only reference
(the golden-corpus replay test skips cleanly without it).
