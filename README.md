# visidata-rs

Modal terminal data explorer. Polars + Ratatui + mlua. Lua-extensible like Neovim.
Pre-alpha, no code shipped yet.

**Direction & scope**: [`docs/direction.md`](docs/direction.md). Read §9 first — six open
questions gate Phase 0.

**Working defaults** (change in `docs/direction.md` §9 if any are wrong):

- New product with VisiData's taste, not a feature-parity port
- Polars-only engine through Phase 2; DuckDB behind an optional backend from Phase 2 onward
- Zero back-compat with `.vdj` / `.visidatarc`
- Pure `init.lua` config (Neovim model); no TOML/KDL layer
- Lua 5.4 via mlua
- Phase 0 scope: viewer parity with csvlens, plus Parquet + Arrow

Not vendored: a VisiData clone lives at `../visidata` as read-only reference.
