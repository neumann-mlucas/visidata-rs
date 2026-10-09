# Specs

One spec per subsystem. Written just-in-time *before* the matching phase starts, not up front — specs rot, late-bind to reality.

File naming: `S-NN-slug.md`. None exist yet.

| ID | Title | Phase | Status |
|---|---|---|---|
| S-00 | Direction & scope | — | approved → see `../direction.md` |
| S-01 | Core data model: Sheet, Column, DataFrame binding | 0 | not-started |
| S-02 | Ratatui app loop & Model/Update/View shape | 0 | not-started |
| S-03 | Reader interface (CSV / Parquet / Arrow in Phase 0) | 0 | not-started |
| S-04 | Keybinding model & prefix maps | 0 | not-started |
| S-05 | Type-system adapter over Polars dtypes | 1 | not-started |
| S-06 | Ex-line grammar | 1 | not-started |
| S-07 | Pivot / melt UX + LazyFrame construction | 2 | not-started |
| S-08 | Plan panel: display, optimize, export | 2 | not-started |
| S-09 | mlua plugin system: shapes, sandbox, errors | 3 | not-started |
| S-10 | Lua API surface + versioning (`since` / `deprecate`) | 3 | not-started |
| S-11 | Streaming source abstraction | 4 | not-started |
| S-12 | Frame diff | 5 | not-started |

Writing order constraint: don't draft S-05+ until Phase 0 ships. Reality will edit them for free.
