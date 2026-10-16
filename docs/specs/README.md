# Specs

One spec per subsystem. Written just-in-time *before* the matching phase starts, not up front — specs rot, late-bind to reality.

File naming: `S-NN-slug.md`. None exist yet.

| ID | Title | Phase | Status |
|---|---|---|---|
| S-00 | Direction & scope | — | approved → see `../direction.md` |
| S-01 | Core data model: Sheet, Column, DataFrame binding | 0 | draft → [`S-01-core-data-model.md`](S-01-core-data-model.md) |
| S-02 | Ratatui app loop & Model/Update/View shape | 0 | draft → [`S-02-app-loop.md`](S-02-app-loop.md) |
| S-03 | Reader interface (CSV / Parquet / Arrow in Phase 0) | 0 | draft → [`S-03-reader-interface.md`](S-03-reader-interface.md) |
| S-04 | Keybinding model & prefix maps | 0 | draft → [`S-04-keybindings.md`](S-04-keybindings.md) |
| S-05 | Type-system adapter over Polars dtypes | 1 | not-started |
| S-06 | Ex-line grammar | 1 | not-started |
| S-07 | Pivot / melt UX + LazyFrame construction | 2 | not-started |
| S-08 | Plan panel: display, optimize, export | 2 | not-started |
| S-09 | mlua plugin system: shapes, sandbox, errors | 3 | draft → [`S-09-plugin-system.md`](S-09-plugin-system.md) |
| S-10 | Lua API surface + versioning (`since` / `deprecate`) | 3 | draft → [`S-10-lua-api.md`](S-10-lua-api.md) |
| S-11 | Streaming source abstraction | 4 | not-started |
| S-12 | Frame diff | 5 | not-started |
| S-13 | Tier 1 feature target (daily-driver parity for upstream golden corpus) | 1.5 | in flight → [`S-13-tier1-feature-target.md`](S-13-tier1-feature-target.md) |
| S-14 | Golden corpus replay harness | 1.5 | landed (PASS=6, SKIP=134 baseline) → [`S-14-corpus-replay-harness.md`](S-14-corpus-replay-harness.md) |

Writing order constraint: don't draft S-05+ until Phase 0 ships. Reality will edit them for free. S-13/S-14 drafted post-Phase 0 to pin the Tier 1 scope + the test strategy that drives it.
