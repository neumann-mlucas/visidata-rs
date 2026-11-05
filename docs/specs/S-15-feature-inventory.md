# S-15 — Feature inventory (what `visidata-rs` actually ships)

Status: **landed 2026-10-16**, re-verified against `Keymap::defaults()` / `exline::parse` on 2026-11-05. This is the authoritative list of what the `vdrs` binary and companion crate expose. Update when a user-visible surface changes.
Phase: n/a (cross-cutting documentation)
Depends on: all prior specs
Blocks: nothing — reference material

## Why this spec exists

Previous specs (S-01…S-14) describe *decisions*. This spec describes *the shipped surface*: every reader, action, keybind, exline command, Lua function, derived-sheet type, and plan-visualization. If you can't find a feature named below, it isn't shipped — regardless of what other specs say.

## 1. CLI surface

```
vdrs <path>                  open a file
vdrs -                       read from stdin (auto-sniffed)
vdrs --format <fmt> <path>   override format detection (csv|tsv|parquet|arrow|jsonl)
vdrs --headless <path>       print source, schema, and first N rows (Polars table), no TUI
vdrs --head N                limit --headless row count
```

Entry point: `src/main.rs`. A missing file fails before the TUI starts, naming the path.

## 2. Readers (eight formats + decompression)

| Format | Extensions | Backend | Behavior |
|---|---|---|---|
| CSV | `.csv` | Polars `LazyCsvReader` | first row = header |
| TSV | `.tsv`, `.tab` | Polars (`\t` separator) | first row = header |
| Parquet | `.parquet`, `.pq` | Polars `scan_parquet` | native lazy |
| Arrow IPC | `.arrow`, `.ipc`, `.feather` | Polars `scan_ipc` | native lazy |
| JSON Lines | `.jsonl`, `.ndjson` | Polars `LazyJsonLineReader` | one record per line |
| JSON | `.json` | Polars `JsonReader` (eager) | array of records; a `.json` file holding JSON Lines is sniffed and read as JSONL |
| Excel/ODS | `.xlsx`, `.xls`, `.xlsm`, `.xlsb`, `.ods` | `calamine` → DataFrame | **first worksheet only**, all cells as String (downstream `type-*` casts); empty header names preserved |
| SQLite | `.sqlite`, `.sqlite3`, `.db` | `rusqlite` (bundled) | **first user table only** ordered by `rowid`, all columns as String, UTF-8-lossy decode (handles invalid-unicode TEXT) |
| stdin | `-` | drained to tempfile, sniffed | csvlens-style fallback |
| gzip | `*.gz` suffix (any case) | `flate2` → tempfile | one layer; format from the inner extension (`.json.gz`) or `--format` |

**Deliberately out of scope** (per direction §1): xml, html, yaml, toml, zip, stata, hdf5, numpy, msgpack, xpt, fixed-width, jsonla, usv, png, http. These are candidates for `vdrs-ext-<fmt>` Lua plugin crates (per S-09 extension ADR) when a user asks.

Entry: `src/reader.rs:open()`.

## 3. User-triggered Actions

Full enum: `src/action.rs`. Grouped by category:

### 3.1 Cursor / scroll (cosmetic — no re-materialize)

| Action | Default key | Semantics |
|---|---|---|
| `CursorDown`, `CursorUp` | `j`, `k` | row ±1; supports count prefix (`3j`) |
| `CursorLeft`, `CursorRight` | `h`, `l` | col ±1 |
| `PageDown`, `PageUp` | `<C-d>`/`<PgDn>`, `<C-u>`/`<PgUp>` | one screen of rows |
| `GotoTop`, `GotoBottom` | `gg`, `G` | first/last row of the whole view (G waits for the background row count) |

All vertical moves go through `App::goto_row`, which re-centres the 1024-row window when the target is outside it. The title shows `row N/total` (1-based; `?` while counting).
| `GotoLeftmost`, `GotoRightmost` | `0`, `$` | first/last column |

### 3.2 Column meta (data-affecting via `View` state, not plan mutation)

| Action | Key | Exline | Semantics |
|---|---|---|---|
| `FreezeColumn` / `UnfreezeAll` | `zs` / `zc` | — | pin cols up to the cursor to the left of the viewport |
| `HideCol` / `UnhideAll` | `-` / `g-` | `:hide` / `:unhide` | View.hidden set |
| `KeyCol` (toggle) / `UnkeyAll` | `!` / `z!` | `:key` / `:unkey` | View.keys vec (preserved order) |
| `StartRenameColInput` → `RenameCol { new_name }` | `^` | `:rename NEW` | plan.rename() + updates hidden/keys/casts/filter/sort refs; refuses an existing name |
| `TypeCastInt/Float/String/Date/Currency` | `#` / `%` / `~` / `@` / (no key — use `:type currency`) | `:type int\|float\|string\|date\|currency` | `View.type_casts` applied in window_plan |

### 3.3 Sort / filter / search

| Action | Key | Exline | Semantics |
|---|---|---|---|
| `SortAsc` / `SortDesc` | `[` / `]` | `:sort [+-]<col>` | adds/updates current col as a sort key (multi-col) |
| `ClearSort` | `zo` | `:unsort` | drop sort state |
| `FilterCurrentColumn { regex }` / `SetFilter { column, regex }` | `z\|` (prompts) | `:filter <col> <regex>` | regex over the column's text (any dtype); bad regex / unknown column rejected with a toast |
| `ClearFilter` | `zf` | `:unfilter` | — |
| `SearchNext` / `SearchPrev` | `n` / `N` | — | next/prev row of the whole view, wrapping |
| `StartSearchInput` | `/` | — | case-insensitive literal match in any column of the whole view |

Changing sort or filter clears the selection (selection is positional).

### 3.4 Derived sheets

| Action | Key | Exline | Semantics |
|---|---|---|---|
| `OpenFreqTable` | `F` | — | `Sheet::freq_from()` — current col with `count` + `percent` cols (2-decimal, round_series feature) |
| `Pivot { index, on, values, agg }` | — | `:pivot <index> <on> <values> [agg]` | polars-ops `pivot_stable`; eager |
| `Unpivot { on, index }` | — | `:unpivot <on,on> -- <idx,idx>` | `LazyFrame::unpivot` |
| `Diff { path, keys }` | — | `:diff <path> [key1,key2,...]` | schema diff or full outer join flagging only_a/only_b/changed |
| `Tail { path, follow }` | — | `:tail <path> [--follow\|-f]` | 2s tick re-reads; auto-scroll when at bottom |

### 3.5 Selection

| Action | Key | Semantics |
|---|---|---|
| `ToggleSelectRow` / `UnselectRow` | `s` or `t` / `u` | View.selected (`BTreeSet<u64>` of view row indexes) |
| `SelectAll` / `UnselectAll` | `gs` / `gu` | every row of the view (needs the row count) |
| `SelectByRegex { regex }` | — | regex over current col, whole view |
| `StartSelectInput` → prompt | `\|` | — |
| `DupSelected` | `"` | new sheet filtered to selected rows via `Sheet::dup_selected_from` |

### 3.6 Sheet stack

| Action | Key | Semantics |
|---|---|---|
| `OpenSheetSwitcher` | `S` | push meta-sheet listing stack; `<CR>` on row → `SwitchToSheet` |
| `SwitchToSheet(id)` + `SubmitRow` | `<CR>` | pop sheet-switcher, activate selected |
| `PopSheet` | `q` | drop active, show one below |

### 3.7 Plan panel

| Action | Key | Semantics |
|---|---|---|
| `TogglePlanPanel` | `P` | cycles Off → Normal (`describe_plan`) → Optimized (`describe_optimized_plan`) |

### 3.8 Meta

| Action | Key | Semantics |
|---|---|---|
| `StartExInput` → `ExCommand(str)` | `:` | — |
| `Save { path }` | — | `:save path.ext` — writes every row of the active sheet's `view_plan()` (casts + filter + sort + visible cols); writer by extension (csv/tsv/parquet/ipc/jsonl/json); unknown extension errors without touching the file; `:w` and `:write` aliases |
| `Quit` | `<C-c>` | hard exit |
| `RepeatLast` | `.` | capture of resolved Action (not keystrokes) |
| `Redraw` | `^L` | force repaint |

### 3.9 Input mode

Modal input state (`InputBuf::Search` / `Filter` / `Exline` / `Select` / `Rename`): `InputChar(c)`, `InputBackspace`, `SubmitInput`, `CancelInput`. History on exline via `$XDG_STATE_HOME/vdrs/exline_history`.

## 4. Keymap

Entry: `src/keymap.rs`. Model: linear vec of `Chord { on: Vec<Key>, run: Vec<Action>, desc: &'static str }` per `Layer` (`Sheet`, `Input`). Resolution: first-match; incomplete prefix → `Dispatch::Pending(vec)` which status bar echoes as the which-key hint.

**Count prefix**: digit-first-keystroke resolves to `App.pending_count` which is then baked into cursor/scroll Actions on next match (`3j` = 3-step `CursorDown`).

**`.` repeat**: `App.last_action: Option<Vec<Action>>` captured on every dispatch of a `repeatable()` action; `.` re-fires it with the same args.

Full default binding table lives in `Keymap::defaults()`; see source.

## 5. Ex-line

Full grammar: `src/exline.rs:parse()`. Commands:

```
:q / :quit                           pop sheet
:qa / :quit!                         hard exit
:filter <col> <regex>                regex filter
:unfilter / :clear-filter
:sort [+-]<col>                      asc (+) or desc (-), default asc
:unsort / :clear-sort
:ls / :sheets                        open sheet switcher
:pivot <index> <on> <values> [agg]   agg defaults to "sum"
:unpivot <on1,on2,...> [-- <idx1,idx2,...>]
:tail <path> [--follow|-f]
:diff <path> [key1,key2,...]
:hide / :unhide                      current col
:key / :unkey                        toggle key on current col / clear all
:rename <new-name>                   rename current col
:type int|float|string|str|date|currency
:save / :w / :write <path.ext>       persist active sheet; writer by extension
                                     (csv|tsv|parquet|pq|ipc|arrow|feather|
                                     jsonl|ndjson|json)
```

History: file-persisted, `<Up>`/`<Down>` cycle, dedup against last entry, trimmed to the last 1000 lines on load.

## 6. Derived-sheet types

Each returns a new `Sheet` with an `Origin` enum entry so the lineage is visible:

- `Origin::File` — opened via reader
- `Origin::Freq { parent, column }` — count + percent per distinct value, sorted desc
- `Origin::Pivot { parent }`
- `Origin::Unpivot { parent }`
- `Origin::Diff { left, right_name }`
- `Origin::Tail { path, follow }`
- `Origin::Selected { parent }` — `"` dup of selected rows
- `Origin::SheetSwitcher` — sheet-stack listing

Derived sheets are built from the parent's `view_plan()`: what you see (casts, filter, sort, visible columns) is what freq/pivot/unpivot/diff/dup operate on.

Each derived sheet is a first-class entry on the stack; `q` pops back.

## 7. Lua plugin surface

VM: mlua Lua 5.4, vendored. Loaded from `$XDG_CONFIG_HOME/vdrs/init.lua` (missing file is not an error).

### 7.1 Keymap patches

```lua
vd.bind(layer, keys, action, desc)   -- layer: "sheet" | "input"
                                     -- keys: string | table of strings
                                     -- action: snake_case name per src/plugins.rs
vd.unbind(layer, keys)               -- no-op patch (last-wins, doesn't remove from base)
```

Last-write wins semantics via `Keymap::append()` + reverse resolve.

**Known action names** (from `action_from_name()` in `src/plugins.rs`): all 3.1–3.9 Actions that take no args / default-arg form. Composite Actions (`Pivot`, `SetFilter`, `Save`, etc.) are not yet exposed to Lua.

### 7.2 Loader registration

```lua
vd.register_loader {
  name = "mylog",                        -- optional
  extensions = { "mylog", "ml" },        -- string or array of strings; case-insensitive
  open = function(path)
    -- Parse `path`, write a tempfile in a format vdrs scans natively
    -- (CSV/TSV/Parquet/Arrow/Jsonl). Return the tempfile path.
    local out = os.tmpname() .. ".csv"
    -- ... convert ...
    return out
  end,
}
```

Resolution order: `plugins::open()` consults `Plugins::resolve_loader(path)` BEFORE `reader::open()`. First rule whose extensions list contains the path's extension (case-insensitive) wins; its `open(path)` is called and its string return is handed to the native reader.

**By design NOT supported**: returning a Polars DataFrame directly from Lua. Marshalling Lua tables → `Series` with per-value dtype inference is a plumbing swamp. Formats that can't convert to CSV/TSV/Parquet/Arrow/Jsonl in Lua belong in a `vdrs-ext-<fmt>` Rust crate per S-09 extension ADR.

**Scope**: CLI file open (`vdrs mylog.foo`) and `:diff` targets. `:tail` refuses loader formats: a refresh would re-scan the one-time converted file and never see new rows.

### 7.3 Not yet shipped

Aggregator plugin shape (`vd.register_aggregator`), renderer plugin shape (`vd.register_renderer`), hot-reload via `notify`. See Phase 3 todos in `tasks.md`.

## 8. Theme

`Theme::dark()` in `src/theme.rs` drives the table, title bar, status bar and toasts. No user override; Phase-3-pending: user-defined theme via Lua.

## 9. Testing

- `cargo test --lib` — unit tests over sheet/keymap/widget/exline/action/plugins, plus `app::scenario` end-to-end tests: real keymap → dispatch → background Polars tasks → TestBackend render (never touches the user's ex history)
- `cargo test --test vd_replay` — golden corpus replay (S-14). Needs `../visidata` checked out alongside; skips cleanly otherwise.
- `cargo clippy --tests` — zero warnings as of iteration 14
- No benchmarks shipped; `hyperfine` harness is a tasks.md todo

## 10. Non-features (explicitly cut)

Enumerated here so no one re-asks:

- **No Python-expression eval** (direction §9.5): `qty*amt`, `curcol.casefold()`, `date.isocalendar()[2]`, `curcol.strip()`, `str.lower(curcol)`. Users write Lua instead; VisiData golden corpus tests encoding Python expressions are UNTESTABLE in S-14.
- **No options system** (direction §1): `set-option`, `unset-option`, 4-level default→global→class→instance fallback. User overrides go in `init.lua`.
- **No clipboard** (`copy-row`, `copy-cells`, `copy-selected`, `paste-after`, `setcol-clipboard`): we're a non-mutable projection engine; "copy to new sheet" is `dup-selected` + sheet-switcher instead.
- **No interactive new sheet** (`open-new`, `add-row`): data sheets are read from sources, not authored in-place. Users compose transforms, not rows.
- **No nested-field drilldown** (`unfurl-col`, `expand-col`, `expand-col-depth`): JSON records load with nested fields as JSON strings; users project via `:derive` (future).
- **No `.vd` / `.vdj` runtime** (direction §9.5): the cmdlog format exists only in the S-14 test harness, not the `vdrs` binary.
- **No VisiData state file compat** (direction §9.5): no `~/.visidata`, no `.visidatarc` import.
- **No multi-table SQLite** (direction §1 default): single-table loader; multi-table workflows need `?table=` URL param (not yet shipped).
- **No multi-sheet xlsx workbook**: first-worksheet-only, same reasoning as sqlite. Users split workbooks externally.
- **No macros** (`macro-param-*`, record/playback): `.` repeat covers the common case; record/replay is clipboard-shaped, see above.
- **No undo/redo** (direction §1): the LazyFrame plan IS the undo log; stack-pop replays an earlier plan. Not surfaced as a user command yet.
- **No save-all** (`g^S`) and no `^S` binding: saving is the `:save <path.ext>` ex-command only.
- **No plot column**, **no LLM-on-cells**, **no live chart previews**.

### 10.1 Not yet supported (planned or open)

In scope but missing from the `vdrs` binary today. Tracked in `tasks.md` ("Open issues & gaps" + phase lists).

- **Joins** across sheets (inner/left/full/append/merge), `hide-uniform-cols`, `setcol` gang edit, `dup-rows`, pivot without keys: implemented only in the S-14 replay driver (`tests/vd_replay.rs`).
- **Expression columns** (`:derive`, `=` key) and **aggregators / describe-sheet** (per-column stats).
- **Undo/redo** as a user command.
- **Pivot setup sheet** (`W`), live pivot preview, **plan export** (`.pl` / SQL), **dedicated diff UI** (`D`).
- **`:sql`** via DuckDB (direction §7).
- **Lua**: aggregator and renderer shapes, hot-reload, API versioning (`since`/`deprecate`), binding parametrized actions or ex-commands, partial apply of a failing `init.lua`.
- **Streaming**: incremental tail, Parquet directory watcher.
- **Readers**: multi-sheet xlsx and multi-table sqlite (`?sheet=` / `?table=`), CSV date inference, lenient date casts.
- **Ex-line**: quoted arguments (names/paths with spaces), line editing beyond append/backspace.
- **Responsiveness**: search, `|`, `:save`, `:pivot`, `"` run on the UI thread; selections don't survive re-sorts.
- **Theme**: only `Theme::dark()`; no light theme or user override.

## 11. File map (where to look)

| Concern | File |
|---|---|
| Sheet / View / LazyFrame plan | `src/sheet.rs` |
| Reader (CSV/TSV/Parquet/Arrow/Jsonl/Json/Xlsx/Sqlite/stdin + gzip) | `src/reader.rs` |
| App loop, dispatch, rendering + `scenario` end-to-end tests | `src/app.rs` |
| Keymap / chord resolution | `src/keymap.rs` |
| Key parsing | `src/key.rs` |
| Action enum + `affects_data()` + `repeatable()` | `src/action.rs` |
| Ex-line parser + history | `src/exline.rs` |
| Sheet widget + `clamp_view` | `src/widget.rs` |
| Lua plugin host (vd.bind / vd.unbind / vd.register_loader, `plugins::open`) | `src/plugins.rs` |
| Theme | `src/theme.rs` |
| Binary entry | `src/main.rs` |
| Library façade (re-exports for integration tests) | `src/lib.rs` |
| Golden corpus replay harness | `tests/vd_replay.rs` |
| Quarantine list w/ per-entry reasons | `tests/vd_replay/quarantine.txt` |

## 12. What belongs in the next spec

When a change touches this spec, it probably belongs in a successor:

- **S-17**: aggregator plugin shape (per S-09 §Lifecycle)
- **S-18**: renderer plugin shape (per S-09 §Lifecycle)
- **S-19**: pivot setup sheet + live preview panel UI (bet #1 Tier 2)
- **S-20**: plan panel export (bet #2 Tier 2)

Only draft one when the implementation starts (direction §README "specs rot, late-bind to reality").
