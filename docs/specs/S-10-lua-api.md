# S-10 — Lua API surface + versioning

Status: **shipped in part**, `vd.api_version` 11 (5: `vd.option`, S-18 B; 6: `duckdb_cmd`; 7: `clipboard_paste_cmd`; 8: `vd.bind` lists, S-19; 9: `vd.on`/`vd.exec`, S-29; 10: styled renderers, S-27 C; 11: `disp_float_fmt`, S-32): registration fns, `vd.bind` with a function, `vd.toast`, `vd.sheet()`, `vd.push_sheet { name?, columns, rows }`, `vd.command` (api 4, 2026-11-07) (row arrays, chosen over column arrays / records: explicit order, nil-safe). The rest waits for a Lua user to ask and will be drafted as S-16.
Phase: 3
Depends on: S-09 (plugin lifecycle), direction §4 (Neovim-style API discipline)

## Problem

The Lua API is a contract. A plugin written against `vd.register_loader` at v0.4 must still work (or fail loudly with upgrade instructions) at v0.7. Neovim's `vim.deprecate()` is the one piece of plugin UX everyone imports from. Direction §4 commits to this discipline — this spec locks the shape.

## Decision

**Every `vd.*` function carries a `since` version. Removals go through a 2-minor deprecation window with `vd.deprecate()` warnings. Semver on the binary doubles as semver on the Lua API.** Removal windows are tracked in a small registry; CI fails if a release crosses a removal boundary without the deprecation having been shipped first.

## Shape

### API surface (initial, v0.3.0)

```lua
-- registration
vd.register_loader { name, extensions, mimes?, open }     -- since 0.3.0
vd.register_aggregator(name, fn)                           -- since 0.3.0
vd.register_renderer { dtype, render }                     -- since 0.3.0
vd.bind(layer, keys, action, desc?)                        -- since 0.3.0
vd.unbind(layer, keys)                                     -- since 0.3.0

-- data
vd.frame { columns = { name = { ... } } } -> Frame         -- since 0.3.0
Frame:column(name) -> Series                               -- since 0.3.0
Series:sum() / :mean() / :count() / :min() / :max()        -- since 0.3.0
Series:std()                                               -- since 0.3.0
Series:collect() -> table                                  -- since 0.3.0 (be careful on big data)

-- app
vd.toast(msg)                                              -- since 0.3.0
vd.sheet() -> Sheet                                        -- since 0.3.0
Sheet.name, Sheet.cursor, Sheet.column_names               -- since 0.3.0
vd.push_sheet { name?, columns, rows }      -- api_version 3
vd.pop_sheet()                                             -- since 0.3.0

-- config
vd.sandbox = true  -- default; set false to re-enable os.*, io.*  -- since 0.3.0

-- coroutines (lands when first plugin needs it)
vd.co(fn) -> coroutine                                     -- since ?.?
```

This is small on purpose. S-09's rule: ship what's needed for the four shapes to work, nothing more. New verbs enter the API when a Lua user asks for them.

### `since` and `deprecate`

```rust
// Rust side — exposed as a decorator-ish helper on the Lua table builder:
fn api(lua: &Lua) -> LuaTable {
    let t = lua.create_table()?;
    api_fn!(t, "register_loader", "0.3.0", register_loader);
    api_fn!(t, "register_aggregator", "0.3.0", register_aggregator);
    // ...
    t
}

// Each `api_fn!` wraps the raw Rust fn with:
//   - a `_version` field attached as metatable info (introspectable from Lua)
//   - a one-shot warning if a deprecated version is called
//   - CI tracks the version table (serialized to docs/api-versions.json)
```

Deprecation:

```lua
-- in init.lua
vd.old_fn(...)  -- fires once per session:
                -- [vdrs] vd.old_fn is deprecated since 0.5.0 and will be removed in 0.7.0; use vd.new_fn
```

The deprecation registry (`docs/api-versions.json`, generated) lists `{name, since, deprecated_since?, removed_in?}`. A release-time check:

- If a fn has `deprecated_since = X.Y.Z` and current version ≥ `X.Y.Z + 2 minors`, it MUST be removed.
- If a fn is removed, it MUST have had a deprecated_since at least 2 minors prior.

CI gate: `cargo xtask check-api-versions` fails the build on violation.

### Error signatures

Lua errors are Rust `anyhow::Error` on the way out. Lua strings become `.chain()` entries. Status-line toast shows the first line. `tracing::warn` captures the full chain.

## Why not <alternative>

- **Semver only on the binary; Lua API floats.** Users can't tell a plugin bump from a breaking API change. Doesn't scale past 10 plugins.
- **Per-fn versioned namespaces (`vd.v1.*`, `vd.v2.*`).** Explicit but exhausts goodwill fast; Neovim doesn't do this.
- **No deprecation window — remove at any minor.** Breaks the one Lua UX people actually rely on.
- **Yazi-shaped `since` field as a table attribute on each registered thing.** More flexible but harder to statically introspect from CI.

## Open sub-decisions

- Where does the generated `api-versions.json` live? `docs/api-versions.json` — easy to diff in PRs. Alternative: `plugins/` subdirectory. Lazy answer: docs/.
- Introspection from Lua: `vd._version(name) -> "0.3.0"` or `vd.register_loader._version` as a metatable field? Lean metatable — fewer footguns.
- Minor-count for the removal window: 2 minors (per Neovim). 3 minors is also defensible on a quarterly release cadence. Lock 2.
- Who writes the migration note? Author of the deprecation PR. Doc under `docs/migrations/`.

## Verification before leaving this spec

- Can a plugin written against v0.3.0 be forward-compatible with v0.5.0 if no deprecations fire? Yes — `since 0.3.0` fns don't change signatures within a major.
- Can CI catch "fn X was removed without deprecation"? Yes — the registry diff.
- Can we evolve a signature? Only by adding fields — `register_loader` takes a table, extra fields ignored by old Rust side but accepted by new. For removals, deprecate first.

Flip any of those → redraft.

## Prep for Phase 3 implementation

- `api_fn!` macro: lives in `src/lua/api.rs`, prints to a static registry at startup.
- `vd.deprecate`: a thin wrapper over `tracing::warn!(target: "vdrs::lua", ...)` + a once-per-session HashSet so a repeated call doesn't spam.
- The 2-minor rule is checked by `cargo xtask check-api-versions` — simple script, not a build.rs.
