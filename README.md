# visidata-rs

Modal terminal data explorer with VisiData's taste: Polars + Ratatui + mlua,
Lua-extensible like Neovim. Pre-alpha.

```
cargo install --path . --locked              # installs `vdrs`; --locked = tested dep versions
cargo run --release -- data.parquet          # TUI
cargo run --release -- --headless data.csv   # schema + first rows, no TUI
cat data.csv | cargo run --release -- -      # stdin, format sniffed
```

Reads CSV, TSV, Parquet (file or directory), Arrow IPC, JSON Lines, JSON
arrays, Excel/ODS and SQLite (several sheets/tables → a list; `book.xlsx:Sheet2`
opens one), and any of these gzipped; Lua loaders add more. Files are scanned
lazily: only a 1024-row window is materialized, and row counts, searches and
pivots run in the background.

Basics: `hjkl` move, `gg`/`G` top/bottom, `/` search, `[`/`]` sort, `z|` filter,
`F` frequency table (`Enter` opens a value's rows), `I` describe, `W` pivot,
`s`/`gs`/`|`/`,` select, `"` sheet of selected rows, `-` hide, `!` key column,
`#%~@` type casts, `U`/`R` undo/redo, `P` plan panel, `S` sheet list, `:` ex
commands (`:join`, `:sql`, `:derive`, `:save out.parquet`, `:export view.sql`,
`:tail -f`, …), `q` back/quit. `:pivot` previews its result while you type;
`:export` writes a Polars or DuckDB script that rebuilds the view, including
derived sheets.

- **User guide**: [`docs/index.html`](docs/index.html) — publish it with GitHub Pages
  (Settings → Pages → Deploy from a branch → `master`, folder `/docs`)
- What ships: [`docs/specs/S-15-feature-inventory.md`](docs/specs/S-15-feature-inventory.md)
- Direction & scope: [`docs/direction.md`](docs/direction.md)
- Punchlist: [`docs/tasks.md`](docs/tasks.md)
- Config: `$XDG_CONFIG_HOME/vdrs/init.lua`, hot-reloaded: `vd.bind` (to an action, ex
  command or Lua function), `vd.register_loader`, `vd.register_aggregator`,
  `vd.register_renderer`, `vd.set_theme` — see the guide's Lua section

Development: `cargo test` runs unit tests, end-to-end UI scenarios
(`app::scenario`) and, when a VisiData checkout sits at `../visidata`, its
golden-corpus replay (`tests/vd_replay.rs`; CI pins the commit). Without the
checkout that test skips.
