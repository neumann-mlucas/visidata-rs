# visidata-rs

Modal terminal data explorer with VisiData's taste: Polars + Ratatui + mlua,
Lua-extensible like Neovim. Pre-alpha.

A pivot sheet, from the test suite (`:pivot grp name amt sum`):

```
 …(grp×name→amt): tests/golden/pivot-sum/input.csv  row 1/3 · col 1/6 (grp) · sel 0  Csv · sheets 2
grp str  ann f  bob f  cid f  dee f  eve f
a        1.5    ·      0.0    ·      ·
b        ·      2.25   ·      ·      1234567.89
c        ·      ·      ·      4.0    ·

 hjkl · gg/G · / search · [ ] sort · z| filter · s sel · F freq · I describe · U undo · : ex · q
```

```
cargo install --git https://github.com/neumann-mlucas/visidata-rs --locked   # installs `vdrs`; needs Rust 1.88+
vdrs data.parquet          # TUI
vdrs --headless data.csv   # schema + first rows, no TUI
cat data.csv | vdrs -      # stdin, format sniffed
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

- **User guide**: <https://neumann-mlucas.github.io/visidata-rs/>
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

## License

[GPL-3.0-or-later](LICENSE). Independent project, not affiliated with
[VisiData](https://github.com/saulpw/visidata).
