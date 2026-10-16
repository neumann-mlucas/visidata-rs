# S-09 — mlua plugin system: shapes, sandbox, errors

Status: **draft** (unapproved)
Phase: 3
Depends on: direction §4 (Scripting: mlua, Neovim-style API discipline), S-01 (`Sheet.Source`), S-02 (tokio runtime), S-04 (`Keymap` layer vecs)
Blocks: S-10 (API surface + versioning)

## Problem

Direction §4 commits to mlua + Lua 5.4 + yazi-shaped plugin lifecycle. A heavy VisiData user wants to:

- Add a loader for a quirky format without recompiling.
- Rebind keys without touching `keymap.rs`.
- Register an aggregator (`:derive total = sum(qty)` land here eventually).
- Not have a buggy plugin kill the TUI.

Four shapes. One lifecycle. Must survive plugin crashes without taking the app down (S-02 three-tier error policy applies).

## Decision

**yazi-shaped plugin shapes with Neovim-style API discipline + mlua's `Lua::sandbox()` flag.** One shared `Lua` VM for all plugins (per yazi). Plugins register themselves via calls to `vd.register_{loader,aggregator,renderer,keymap}` from `init.lua`. Hot-reload reruns `init.lua`; prior registrations are replaced wholesale on each reload. Panics from Rust-boundary conversions caught; Lua `error()` surfaces as a status-line toast and the previous good state stays. Sandbox is **on** by default; `vd.sandbox = false` toggles for power users.

## Shape

### Discovery

```
$XDG_CONFIG_HOME/vdrs/init.lua           # single entry point (Neovim model)
$XDG_CONFIG_HOME/vdrs/lua/<plugin>/*.lua # user-managed; use `require("<plugin>")`
```

No per-plugin manifest in Phase 3. A loose convention — `lua/<name>/init.lua` + `require("<name>")` — is enough. Package managers can land later if a user wants them.

If `$XDG_CONFIG_HOME/vdrs/init.lua` is absent, the app runs without Lua. No warning, no toast. Starting with no config is the common case.

### Lifecycle

```rust
pub struct Plugins {
    lua: Lua,                                 // single mlua instance, sandbox=true by default
    config_path: PathBuf,
    watcher: Option<notify::RecommendedWatcher>,
    reg: PluginRegistry,
}

#[derive(Default)]
pub struct PluginRegistry {
    loaders: Vec<LoaderRule>,     // {extensions, mimes, fn_name}
    aggregators: HashMap<String, LuaFunction>,  // name -> fn
    renderers: Vec<RendererRule>, // {dtype, fn}
    keymap_patches: Vec<KeymapPatch>, // append-after-defaults
}

impl Plugins {
    pub fn load(config_path: PathBuf) -> Result<Self, PluginError> { ... }
    pub fn reload(&mut self) -> Result<(), PluginError> { ... }
    pub fn apply_keymap_patches(&self, km: &mut Keymap) { ... }
    pub fn resolve_loader(&self, path: &Path) -> Option<LoaderRule> { ... }
}
```

**Reload wipes `reg` first, then runs `init.lua` fresh.** No `vd.unregister` API; the whole table rebuilds. Simpler, surprising-less — matches Neovim's `:source $MYVIMRC`.

### Four plugin shapes

#### 1. Loader

```lua
vd.register_loader {
  name = "pcap",
  extensions = { "pcap", "pcapng" },
  mimes = { "application/vnd.tcpdump.pcap" },  -- optional, used when `infer` lands
  open = function(path, opts)
    -- opts.format_hint, opts.delimiter, ... (format-specific)
    -- must return a table convertible to a Polars DataFrame via vd.frame({...}) OR
    -- a path to a converted file we can scan normally.
    return vd.frame { columns = { ... } }
  end,
}
```

Rust side: on `reader::open(path)`, after extension detect fails (or alongside), probe `plugins.resolve_loader(path)`. If matched, call the Lua function. Return a `LoadedSource` wrapping either an in-memory DataFrame or a path we can scan.

The `LuaSource` variant reserved in S-03 lands here as a plumbing concern (no new enum variant: just construct a normal `LoadedSource` after Lua returns).

#### 2. Aggregator

```lua
vd.register_aggregator("stdev", function(col)
  -- col is a Series proxy. We expose a tiny set of operations in Phase 3:
  -- :sum() :mean() :count() :min() :max() :std() :collect()
  return col:std()
end)
```

Rust side: `polars::prelude::Expr` builder calls into Lua when it encounters a plugin-provided agg name. For Phase 3 the only consumer is `:derive` (depends on S-06); until then aggregators are callable but unused.

#### 3. Renderer

```lua
vd.register_renderer {
  dtype = "List(Int64)",         -- Polars dtype string
  render = function(cell)
    return "[" .. #cell .. " items]"
  end,
}
```

Rust side: `widget.rs::display_av` consults the registry when the dtype matches. Fallback remains `AnyValue::to_string()`.

#### 4. Keymap

```lua
vd.bind("sheet", { "<leader>", "f" }, "find_in_column", "find in column")
vd.unbind("sheet", "x")
```

Rust side: patches applied **after** the defaults load. Last declaration wins (S-04 rule). Unbinding is modeled as pushing a chord that fires `Action::Noop` — simpler than remove-from-vec and still honors last-wins.

### Error surfacing (echoes S-02's three tiers)

- Config parse error (syntax, bad `register_*` args) → `PluginError`, surfaced as a modal `Confirm` once at startup; app runs with the previous good registry (or empty on first boot).
- Loader call error → open fails, modal `Confirm` with the Lua traceback; sheet never pushes.
- Aggregator / renderer runtime error → `MaterializeError` toast; last-good value stays. Catch panics around every Lua-to-Rust boundary via `std::panic::catch_unwind`; the Lua VM itself is single-threaded so a panic in one call doesn't poison the next.

### Hot-reload

```rust
// in app loop, select! branch:
Some(ev) = self.config_watcher.recv() => {
    if let Err(e) = self.plugins.reload() {
        self.toast = Some(format!("init.lua: {e}"));
    } else {
        self.plugins.apply_keymap_patches(&mut self.keymap);
        self.toast = Some("init.lua reloaded".into());
    }
}
```

`notify` crate watches `init.lua` and `lua/**/*.lua`. Debounce 200 ms to avoid double-fires from editor saves. Reload is best-effort; failures leave the last-good registry in place.

### Threading

Lua VM is NOT `Send + Sync`. Confine it to the UI thread. Any async operation a plugin wants (preview a URL, pipe to a shell) uses `vd.co(...)` → mlua coroutines backed by tokio tasks. The yazi pattern. Phase 3 initial scope: synchronous callbacks only. Coroutines land when a plugin actually needs them.

## Extension beyond Lua: when to spin out a Rust crate vs ship as Lua plugin vs Cargo feature

Three ways to extend vdrs. Different costs. Pick the lowest rung that works for the format.

| Mechanism | Example | When it's right |
|---|---|---|
| **Polars-native scan** | csv, tsv, parquet, arrow, jsonl, json | First choice. Zero extra code. |
| **In-tree Cargo dep + feature flag** | xlsx (`calamine`), sqlite (`rusqlite`) | Rust crate exists, format is high-value, fits in default binary. One binary, compile-time, no dylib ABI. |
| **Lua loader plugin** (shape §4.1) | yaml, toml, lsv, usv, lrc, any config/text format, prototyping | Row count small, per-row cost tolerable, user wants hot-reload without recompile. |
| **Workspace `vdrs-ext-<fmt>` crate behind a feature flag** | qvd, pcap, hdf5, SPSS .sav, SAS .xpt, Stata .dta, any proprietary SDK binding, GPL-conflicting dep | Dep is heavy, security-sensitive, legally encumbered, or GPL-incompatible with the default permissive binary. Opt-in at compile time (`cargo install vdrs --features qvd`); still one binary at runtime. Lives in the same cargo workspace as `visidata-rs`, not a published-separately crate. |
| **Separate standalone crate + runtime dylib** | — | **Rejected.** Rust has no stable ABI. Dylib loaders need C-FFI shims, bespoke versioned ABIs, and runtime discovery paths. Lua already covers "runtime plugin, no recompile." The marginal gain over Cargo features is "format author ships binary separately," which Lua also covers for everything that fits Lua, and the heavy/encumbered-dep cases need compile-time trust anyway. |

### Decision rule

1. Polars has a `scan_*` for it → use it.
2. A well-maintained Rust crate exists AND the format is high-leverage → in-tree Cargo feature, OR workspace `vdrs-ext-<fmt>` crate if the dep is heavy/encumbered.
3. Otherwise → Lua loader plugin per §4.1.

### Why not "visidata-rs-<fmt>" as a separate top-level crate per format?

- Rust has no stable ABI → every format crate would need a hand-rolled C-FFI wrapper, versioned independently from the vdrs API. That ABI boundary is the real cost, not the parsing code.
- Users expect "install vdrs, open file." Shipping N dylib discovery paths is UX debt.
- Yazi's precedent: Lua + Rust compile-time features. No dylib plugins. We copy that.
- The one legitimate case (GPL/permissive license split, security-sensitive binding) is already handled by workspace-local `vdrs-ext-*` crates with Cargo features — same effect, no ABI invented.

### Core binary target (direction §1 "ships ~10 formats")

CSV, TSV, Parquet, Arrow/IPC, JSONL, JSON, XLSX, SQLite, stdin-sniff → nine formats, all via Polars + one or two deps. Everything else: Lua (long tail) or opt-in Cargo feature (encumbered).

## Why not <alternative>

- **One `Lua` per plugin.** mlua VMs are heavy. 50 plugins = 50 VMs. No gain in isolation that `catch_unwind` + the single-threaded VM doesn't already give.
- **Luau for sandboxing.** Luau is sandboxed by default and smaller. Rejected per direction §9.3: pick one Lua, freeze, document. Lua 5.4 picked; sticking with it. Users who want Luau can wait.
- **Per-plugin manifest TOML.** Phase 3 ships four shapes. A manifest adds surface users don't need yet. Add if a package manager appears.
- **Permission system.** Yazi has one; Neovim doesn't. Users running arbitrary Lua from their own `init.lua` have already made the trust call. Revisit if we gain a plugin index.

## Open sub-decisions

- `sandbox = true` or `false` default? Yazi defaults sandboxed; Neovim doesn't sandbox at all. Lean sandboxed — safer, breaks `os.execute` and `io.open` which Phase 3 plugins shouldn't need anyway. Opt out via `vd.sandbox = false`.
- Plugin → DataFrame conversion: `vd.frame { columns = {name = {...}} }` the simplest path. Polars `DataFrame::new(Vec<Column>)` on the Rust side. Larger datasets should go via a path, not table-marshaling.
- Error traceback: `pcall` wraps every Lua call; traceback via `debug.traceback()`. Full stack logged via `tracing`; toast shows last 100 chars.
- Lua `require` path: `package.path` includes `$XDG_CONFIG_HOME/vdrs/lua/?/init.lua;$XDG_CONFIG_HOME/vdrs/lua/?.lua`. No support for `~/.luarocks`.
- Testing: no Lua tests in Phase 3. Each plugin shape has a Rust unit test loading a 10-line Lua fixture and asserting registration. Lua test framework (busted etc.) can land when a plugin ecosystem exists.

## Verification before leaving this spec

- Can a buggy loader crash the app? No — `catch_unwind` around every Lua call; traceback → toast; previous good state stays.
- Can hot-reload leak keymap patches across reloads? No — `reg` wiped before each run; `Keymap` re-applies defaults + fresh patches.
- Can an aggregator be registered without recompiling the binary? Yes — single `vd.register_aggregator(name, fn)` call; `:derive` (S-06) will look it up at expression-eval time.
- Can we migrate to Luau later? Yes — `trait LuaRuntime` would wrap `mlua::Lua`; current spec doesn't require that abstraction yet.

If any of those flip, redraft before S-10 lands.

## Prep for Phase 3 implementation

- Add `mlua = { version = "0.10", features = ["lua54", "async", "vendored", "send"] }` — matches direction §4.
- Add `notify = "6"` for the file watcher.
- `trait Source` extraction (tasks.md Phase 3 item) happens together with the first `LuaSource` call site — one PR, not two.
