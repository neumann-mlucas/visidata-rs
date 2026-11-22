# visidata-rs

Modal terminal data explorer with VisiData's taste: Polars + Ratatui + mlua,
Lua-extensible like Neovim. Pre-alpha.

![vdrs: typing :pivot region qty price previews the pivot before Enter](docs/screens/pivot-preview.svg)

## Why

VisiData is open in one of my terminals most days. Treating everything as a
sheet, with a frequency table one keystroke away, changed how I look at data,
and nothing else in the terminal comes close. visidata-rs is not a fork. It
rebuilds that tool in Rust: same keys, same behavior, different insides.
VisiData's own test corpus is replayed against it: 102 of the 193 scripts it
can address pass today (see `MIN_PASS` in `tests/vd_replay.rs`).

The insides matter because that's where VisiData's open issues pile up:

- **Big files.** Rows are Python objects, read cell by cell; the maintainer
  says a real fix means changing the architecture
  ([#1068](https://github.com/saulpw/visidata/issues/1068)). Here Polars does
  the work: files are scanned lazily, only the visible window is loaded, and
  sorts, counts and pivots run off the UI thread.
- **curses.** The largest share of bug reports: resize crashes
  ([#1454](https://github.com/saulpw/visidata/issues/1454),
  [#2138](https://github.com/saulpw/visidata/issues/2138)), color pairs
  ([#3206](https://github.com/saulpw/visidata/issues/3206)), wide characters
  ([#2416](https://github.com/saulpw/visidata/issues/2416)). Ratatui and
  crossterm own the terminal here.
- **Plugins.** Plugins and `.visidatarc` patch live Python internals, so they
  break on upgrades ([#2084](https://github.com/saulpw/visidata/issues/2084),
  [#2142](https://github.com/saulpw/visidata/issues/2142)). Extensions here go
  through a fixed Lua API (loaders, aggregators, renderers, bindings), and
  `init.lua` reloads on save.
- **Python versions.** 3.12 broke expression eval
  ([#2179](https://github.com/saulpw/visidata/issues/2179)). A compiled binary
  doesn't care which Python you have. No prebuilt binaries yet, though:
  `cargo install` still compiles from source.

Two things are new rather than fixes:

- Your keystrokes build a Polars query plan. The plan panel shows it, and
  `:export` writes it out as a Polars or DuckDB script, so an exploratory
  session ends as code you can rerun.
- More vim, which is what VisiData users keep asking for
  ([#2500](https://github.com/saulpw/visidata/issues/2500),
  [#3008](https://github.com/saulpw/visidata/issues/3008)): ex commands with
  live preview and ranges (`:5,20 setcol 0`, `:g/^n/d`), `:s/a/b/g`, macros,
  an undo tree, sessions (`:mksession`, `vdrs -S`) and an opt-in vim keymap.

What it won't fix: VisiData's 60-odd loaders shrink to a core set plus Lua,
hundreds of commands are still hard to discover, and a Rust codebase doesn't
grow more maintainers. If VisiData does what you need, keep using it. I still do.

## Install

```text
cargo install --git https://github.com/neumann-mlucas/visidata-rs --locked   # installs `vdrs`; needs Rust 1.88+
```

The install compiles Polars with release optimizations: expect a long build (about
45 minutes in one test) and over 1 GB of disk while it runs.

## Use

```text
vdrs data.parquet          # TUI
vdrs --headless data.csv   # schema + first rows, no TUI
cat data.csv | vdrs -      # stdin, format sniffed
vdrs data/                 # every *.parquet in a directory, as one table
vdrs book.xlsx:Sheet2      # one worksheet
vdrs -S monday.vdrs        # restore a session (:mksession)
```

Reads CSV, TSV, Parquet (file or directory), Arrow IPC, JSON Lines, JSON,
Excel/ODS, SQLite and DuckDB (through the `duckdb` program), plus fixed width,
USV/LSV, YAML, TOML, HTML tables, XML, MessagePack and zip members, and any of
these gzipped. A file with several sheets or tables opens on a list
(`book.xlsx:Sheet2` opens one); Lua loaders add more. Files are scanned lazily: only a 1024-row window is
materialized, and row counts, searches and pivots run in the background. Edits
never touch the file you opened until `:save`.

## Keys

VisiData's, grouped:

- **Move**: `hjkl`, `gg`/`G` top/bottom, `q` back, `gq` quit, `Ctrl-c` cancels
  background work
- **Find**: `/` search the column (`g/` all), `[`/`]` sort, `gz|` filter
- **Summarize**: `F` frequency table (`Enter` opens a value's rows), `I`
  describe, `W` pivot (`:pivot` previews while you type), `+` aggregators (`F`
  adds `Units_sum` …; `z+` in the status line)
- **Select**: `s`/`gs`/`|`/`,` select (`z|` by SQL condition), `"` sheet of
  selected rows
- **Columns**: `=` new column, `-` hide, `!` key column, `#%~@` type casts, `za`
  empty column, `i` row numbers, `;` regex captures, `(`/`zM` expand structs /
  unfurl lists, `C` columns sheet (edits write back)
- **Edit**: `e` edit, `f` fill down, `a` add row, `d` delete row, `y`/`p`
  copy/paste (`Y`/`zP` system clipboard), `U`/`R` undo/redo (a tree:
  `:undotree`)
- **Sheets**: `b` sidebar, `S` sheet list, `:` ex commands (`:join`, `:sql`,
  `:derive`, `:save out.parquet`, `:export view.sql`, `:tail -f`, …)

Keymaps work like vi and vim: the default is VisiData's, and
`vd.option("keymap", "vim")` in `init.lua` switches on vim meanings (`u` undo,
`d`/`y`/`c` operators, `q` macros, …). Keys changed to match VisiData on
2026-11-14 (`s` selects instead of toggling, `zs` `zc` `zo` `<C-v>`): see the
guide's [changed keys](https://neumann-mlucas.github.io/visidata-rs/#breaking).

## Docs

- **User guide**: <https://neumann-mlucas.github.io/visidata-rs/>
- What ships: [`docs/specs/S-15-feature-inventory.md`](docs/specs/S-15-feature-inventory.md)
- Direction & scope: [`docs/direction.md`](docs/direction.md)
- Punchlist: [`docs/tasks.md`](docs/tasks.md)
- Config: `$XDG_CONFIG_HOME/vdrs/init.lua`, hot-reloaded: `vd.bind` (to an action, ex
  command or Lua function), `vd.register_loader`, `vd.register_aggregator`,
  `vd.register_renderer`, `vd.set_theme` — see the guide's Lua section

## Development

`just check` is the done-gate. Commands, rules and test switches are in
[`CLAUDE.md`](CLAUDE.md).

Run `just setup` once. It fetches the VisiData checkout CI pins (for the corpus
replay, `tests/vd_replay.rs`) and a Python venv with polars (for the golden cases'
exported scripts) into `~/.cache/vdrs`. It also checks that the DuckDB CLI is on
`PATH`. `just check` then runs the same tests as CI, and a missing tool fails it.

- `prek install` once for the pre-commit hooks.
- `just` builds into `~/.cache/vdrs/target`, shared with worktrees.

## License

[GPL-3.0-or-later](LICENSE). Independent project, not affiliated with
[VisiData](https://github.com/saulpw/visidata).
