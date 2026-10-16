# Specs

One spec per subsystem. Written just-in-time *before* the matching phase starts, not up front — specs rot, late-bind to reality.

File naming: `S-NN-slug.md`.

| ID | Title | Phase | Status |
|---|---|---|---|
| S-00 | Direction & scope | — | approved → see `../direction.md` |
| S-01 | Core data model: Sheet, Column, DataFrame binding | 0 | **shipped** → [`S-01-core-data-model.md`](S-01-core-data-model.md) |
| S-02 | Ratatui app loop & Model/Update/View shape | 0 | **shipped** → [`S-02-app-loop.md`](S-02-app-loop.md) |
| S-03 | Reader interface | 0 | **shipped + extended** (xlsx/sqlite/gzip added post-Phase-0) → [`S-03-reader-interface.md`](S-03-reader-interface.md) |
| S-04 | Keybinding model & prefix maps | 0 | **shipped** (Lua `vd.bind()` instead of planned TOML) → [`S-04-keybindings.md`](S-04-keybindings.md) |
| S-05 | Type-system adapter over Polars dtypes | 1 | cut (superseded by S-13 #3 Type coercion chunk; shipped as `TypeCast*` Actions) |
| S-06 | Ex-line grammar | 1 | **shipped as ad-hoc parser** in `src/exline.rs` — no formal grammar spec written; parser covers 15 commands |
| S-07 | Pivot / melt UX + LazyFrame construction | 2 | partial — `:pivot` / `:unpivot` ex-commands shipped; interactive setup sheet + live preview panel deferred |
| S-08 | Plan panel: display, optimize, export | 2 | partial — display + optimize shipped (`P` key); export (`.pl` / SQL) deferred |
| S-09 | mlua plugin system: shapes, sandbox, errors | 3 | **spec drafted + keymap shape shipped** → [`S-09-plugin-system.md`](S-09-plugin-system.md) |
| S-10 | Lua API surface + versioning (`since` / `deprecate`) | 3 | spec drafted, `since`/`deprecate` enforcement not started → [`S-10-lua-api.md`](S-10-lua-api.md) |
| S-11 | Streaming source abstraction | 4 | not-started (tail-sheet ships via `:tail` without the trait; abstraction waits on second streaming source) |
| S-12 | Frame diff | 5 | **shipped as ex-command** `:diff` — dedicated UI (`D` key, visual picker) deferred |
| S-13 | Tier 1 feature target (daily-driver parity for upstream golden corpus) | 1.5 | **closed** (iter 14 — honest classification replaced the "~160 PASS" target) → [`S-13-tier1-feature-target.md`](S-13-tier1-feature-target.md) |
| S-14 | Golden corpus replay harness | 1.5 | **landed and stable** (PASS=21/21 addressable, SKIP=0) → [`S-14-corpus-replay-harness.md`](S-14-corpus-replay-harness.md) |
| S-15 | **Feature inventory** — what the binary actually ships (post-Phase-1.5) | cross-cutting | **landed 2026-10-16** → [`S-15-feature-inventory.md`](S-15-feature-inventory.md) |

Writing order constraint: don't draft S-05+ until the matching phase actually starts — reality edits specs for free when written late. S-13/S-14 drafted post-Phase 0 to pin Tier 1 scope + test strategy. S-15 drafted post-Phase 1.5 as the authoritative "what's shipped" reference — update it when a user-visible surface changes.
