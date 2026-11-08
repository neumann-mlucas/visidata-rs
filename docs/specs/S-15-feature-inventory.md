# S-15 — Feature inventory (what `visidata-rs` actually ships)

Status: **landed 2026-10-16**; keys re-aligned with VisiData 2026-11-08 (S-26 phase 0, S-21 A); re-verified against `Keymap::defaults()` / `exline::parse` on 2026-11-05; §3.10 lists the open-features pass of that day, §3.11 the 2026-11-07 additions. Planned work: `../tasks.md` roadmap.

User-facing reference: [`docs/index.html`](../index.html) (GitHub Pages). Unit tests fail if it misses a default key binding or documents an ex command that doesn't exist. This is the authoritative list of what the `vdrs` binary and companion crate expose. Update when a user-visible surface changes.
Phase: n/a (cross-cutting documentation)
Depends on: all prior specs
Blocks: nothing — reference material

## Why this spec exists

Previous specs (S-01…S-14) describe *decisions*. This spec describes *the shipped surface*: every reader, action, keybind, exline command, Lua function, derived-sheet type, and plan-visualization. If you can't find a feature named below, it isn't shipped — regardless of what other specs say.

## 1. CLI surface

```
vdrs <path>                  open a file
vdrs -                       read from stdin (auto-sniffed)
vdrs --format <fmt> <path>   override format detection (csv|tsv|parquet|arrow|jsonl|json|xlsx|sqlite)
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
| Excel/ODS | `.xlsx`, `.xls`, `.xlsm`, `.xlsb`, `.ods` | `calamine` → DataFrame | one worksheet (list when several; `file:Sheet`), all cells as String (downstream `type-*` casts); empty header names preserved |
| SQLite | `.sqlite`, `.sqlite3`, `.db` | `rusqlite` (bundled) | one table (list when several; `file:table`) ordered by `rowid`, all columns as String, UTF-8-lossy decode (handles invalid-unicode TEXT) |
| DuckDB | `.duckdb`, `.ddb` | the `duckdb` program (`vd.option("duckdb_cmd")`), Parquet tempfiles | always VisiData's index (name, schema_name, table_type, rows, cols, keys, source); `<CR>` / `file:schema.table` opens one, typed; `:exec-sql` runs SQL in the file (last query's rows open, index reloads); `:save x.duckdb` = `CREATE OR REPLACE TABLE <sheet>` |
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
| `PageDown`, `PageUp` | `<C-f>`/`<C-d>`/`<PgDn>`, `<C-b>`/`<C-u>`/`<PgUp>` | one screen of rows |
| `GotoTop`, `GotoBottom` | `gg`, `G` | first/last row of the whole view (G waits for the background row count) |

All vertical moves go through `App::goto_row`, which re-centres the 1024-row window when the target is outside it. The title shows `row N/total` (1-based; `?` while counting).
| `GotoLeftmost`, `GotoRightmost` | `gh` or `0`, `gl` | first/last column |

### 3.2 Column meta (data-affecting via `View` state, not plan mutation)

| Action | Key | Exline | Semantics |
|---|---|---|---|
| `FreezeColumn` / `UnfreezeAll` | `zs` / `zc` | — | pin cols up to the cursor to the left of the viewport |
| `HideCol` / `UnhideAll` | `-` / `gv` | `:hide` / `:unhide` | View.hidden set |
| `HideUniform` | `g-` | `:uniform` | hide columns with ≤ 1 distinct value |
| `KeyCol` (toggle) / `UnkeyAll` | `!` / `z!` | `:key` / `:unkey` | View.keys vec (preserved order) |
| `StartRenameColInput` → `RenameCol { new_name }` | `^` | `:rename NEW` | plan.rename() + updates hidden/keys/casts/filter/sort refs; refuses an existing name |
| `TypeCastInt/Float/String/Date/Currency` | `#` / `%` / `~` / `@` / `$` | `:type int\|float\|string\|date\|currency` | `View.type_casts` applied in window_plan |

### 3.3 Sort / filter / search

| Action | Key | Exline | Semantics |
|---|---|---|---|
| `SortAsc` / `SortDesc` | `[` / `]` | `:sort [+-]<col>` | adds/updates current col as a sort key (multi-col) |
| `ClearSort` | `zo` | `:unsort` | drop sort state |
| `FilterCurrentColumn { regex }` / `SetFilter { column, regex }` | `gz\|` (prompts) | `:filter <col> <regex>` | regex over the column's text (any dtype); bad regex / unknown column rejected with a toast |
| `ClearFilter` | `zf` | `:unfilter` | — |
| `SearchNext` / `SearchPrev` | `n` / `N` | — | next hit in the search's direction / against it, whole view, wrapping |
| `StartSearchInput { all_columns, backward }` | `/` `?` / `g/` `g?` | — | case-insensitive regex in the cursor column (at each `n`) / every visible column (VisiData search-col / search-cols) |
| `StartSearchExprInput { backward }` | `z/` / `z?` | — | next row where a SQL predicate holds (hidden columns allowed) |

Selections key on row ids (position in the base plan), so they survive sort and filter.

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
| `ToggleSelectRow` / `UnselectRow` | `s` or `t` / `u` | View.selected (`BTreeSet<u64>` of row ids — position in the base plan) |
| `SelectAll` / `UnselectAll` | `gs` / `gu` | every row of the view (needs the row count) |
| `SelectByRegex { regex }` | — | regex over current col, whole view |
| `StartSelectInput` → prompt | `\|` | — |
| `SelectExpr { expr, unselect }` | `z\|` / `z\\` (prompt) | `:select <pred>` / `:unselect <pred>`: rows where a SQL predicate holds; background job |
| `DupSelected` | `"` | new sheet filtered to selected rows via `Sheet::dup_selected_from` |
| `CopyRow` / `CopySelected` / `CopyCell` / `CopyCells` | `y` / `gy` / `zy` / `gzy` | register (`App.register`, one per app): cursor row / selected rows in view order (every column, hidden too, typed) / cursor cell / current column of selected rows; background job; over 10M cells refused (S-18 A) |
| `SysCopyRow` / `SysCopySelected` / `SysCopyCell` / `SysCopyCells` | `Y` / `gY` / `zY` / `gzY` | same rows as text to the system clipboard: visible columns as TSV with header / raw cell / one value per line; `clipboard_copy_cmd` or OSC 52 (S-18 B) |

### 3.6 Sheet stack

| Action | Key | Semantics |
|---|---|---|
| `OpenSheetSwitcher` | `S` | push meta-sheet listing stack; `<CR>` on row → `SwitchToSheet` |
| `SwitchToSheet(id)` + `SubmitRow` | `<CR>` | pop sheet-switcher, activate selected |
| `PopSheet` | `q` | drop active, show one below; unsaved edits → `discard N edits to x? y/N` (`:q!` skips) |
| `StartEditInput` / `StartSetColInput` | `e` / `ge` | edit cursor cell (prefilled) / `:setcol` prompt; source sheets only (S-18 C) |

### 3.7 Plan panel

| Action | Key | Semantics |
|---|---|---|
| `TogglePlanPanel` | `P` | cycles Off → Normal (`describe_plan`) → Optimized (`describe_optimized_plan`) |

### 3.8 Meta

| Action | Key | Semantics |
|---|---|---|
| `StartExInput` → `ExCommand(str)` | `:`, `<Space>` | — |
| `StartDeriveInput` | `=` | new column from a SQL expression, named after it; `name = expr` names it when `name` is new |
| `Save { path }` | — | `:save path.ext` — writes every row of the active sheet's `view_plan()` (casts + filter + sort + visible cols); writer by extension (csv/tsv/parquet/ipc/jsonl/json); unknown extension errors without touching the file; `:w` and `:write` aliases |
| `Quit` | `gq`, `<C-q>` | exit |
| `CancelJobs` / `CancelAllJobs` | `<C-c>` / `g<C-c>` | drop the active sheet's / every background job (`working: …`); the window load and row count are not cancelled |
| `RepeatLast` | `.` | capture of resolved Action (not keystrokes) |
| `Redraw` | `^L` | force repaint |

### 3.9 Input mode

Modal input state (`InputBuf::Search` / `Filter` / `Exline` / `Select` / `Rename`): `InputChar(c)`, `InputBackspace`, `SubmitInput`, `CancelInput`. History on exline via `$XDG_STATE_HOME/vdrs/exline_history`.

### 3.10 Added 2026-11-05 (open-features pass)

| Surface | Key / command | Semantics |
|---|---|---|
| Freq dive | `<CR>` / `g<CR>` on a freq sheet | parent's rows with the cursor's / selected values (text compare, null matches null) |
| Describe | `I`, `:describe` | VisiData's describe sheet: per visible column `sheet` `column` `type` (sheet, type hidden), errors (failed type casts), nulls, distinct (null = one value), mode (first seen on ties); numbers: min max sum median mean stdev (sample); dates: min max, median when the count is odd (VisiData `#ERR` → empty) (background job) |
| Pivot | `W` | key cols × current col, counts; no keys → one row of counts |
| Pivot preview | typing `:pivot …` | right half: next argument + columns + aggs, then the pivot of the first 10k view rows (background, Lua aggs too); Enter runs the full pivot |
| Diff prompt | `D` | prompts for a path; key cols are the diff keys |
| Copy | `g"`, `:dup` | lazy copy of the view |
| Select equal | `,` | rows whose current col equals the cursor cell |
| Undo / redo | `U` / `R`, `:undo` / `:redo` | per-sheet snapshots of (plan, view, steps), depth 100 |
| Join | `:join <file\|sheet\|#id> [keys] [inner\|left\|full\|merge\|append]` | same-named keys; merge = full + right non-null wins; append = diagonal concat + `origin_sheet`; result collected once (background) |
| Derive | `:derive name = <SQL expr>` | Polars `sql_expr` on the base plan; validated against the schema |
| SQL | `:sql <query>` | Polars SQL; `self` = view, `s<id>` = other open sheets; result collected once (background) so row order is fixed |
| Set column | `:setcol <value>` | current col on selected rows; value must fit the displayed and base type; empty = null; placed after the column's last writer |
| Edit cell | `:edit [value]` | cursor cell, same rules (S-18 C) |
| Force quit | `:q!`, `:qa!` / `:quit!` | pop / quit without the unsaved-edits question |
| Uniform | `:uniform` | hide columns with ≤ 1 distinct value |
| Export | `:export x.py\|x.sql` | Polars script / DuckDB SQL replaying plan steps, casts, filter, sort, projection; derived sheets (freq, copy, selected, freq dive, join, sql, pivot, `W`, unpivot, describe, row diff) replay from a parent snapshot (`SheetState.lineage`), one variable/CTE per sheet. Not: schema diff, Lua-agg pivots, stdin |
| Prompts | all prompts | Left/Right/Home/End, `<C-a>` `<C-e>` `<C-u>` `<C-w>`, `<Del>`; ex args accept `"…"`/`'…'`/`\` quoting |
| Background jobs | search, `gs`, `\|`, `,`, `"`, `I`, `W`, `:pivot`, `:save`, `:uniform`, `z\|`, `z/` | run on the blocking pool; status line shows `working: …`; `<C-c>` drops them |
| Readers | — | ISO-only CSV/TSV date inference (others stay text); lenient `@` (12 regex-guarded layouts, time suffix ignored); directory = `*.parquet` glob; multi-sheet xlsx / multi-table sqlite open a parts list; `file:part` opens one directly (CLI, `:join`, `:diff`) |
| Tail | `:tail` | CSV/TSV/JSONL parse only appended bytes (partial last line waits; truncation reloads); unchanged file isn't re-materialized |

### 3.11 Added 2026-11-07

| Surface | Semantics |
|---|---|
| freq `histogram` column | `■` × (count·38 // max count), after `percent`; exported to `.py` / `.sql` |
| `:join <src> concat` | rows stacked, columns paired by position, current sheet's names |
| hidden `origin_sheet` | `append` / `concat` add it at width 0 like VisiData (#2929); `g-` shows it |
| `dedupe_rows` (`:dedupe-rows`) | new sheet: first row per key (per distinct row with no keys), view order; exports as row ids |
| `select_col_duplicates` (`:select-col-duplicates`) | select rows whose current-column value occurred above |
| `:<action-name>` | every `Action::NAMED` entry, kebab or snake case, no arguments |
| `:N` | go to row N |
| `<Tab>` in `:` | completes the command word (built-ins, actions, `vd.command`s) |
| `vd.command(name, fn)` | Lua ex command; `fn(args)`; api_version 4 |
| `vd.option(name, value)` | set an option: `clipboard_copy_cmd`, `duckdb_cmd`; unknown name = error; api_version 6 |
| first-run `init.lua` | written at TUI start when missing: every default commented out (`plugins::default_init_lua`) |

## 4. Keymap

Entry: `src/keymap.rs`. Model: linear vec of `Chord { on: Vec<Key>, run: Vec<Action>, desc: &'static str }` per `Layer` (`Sheet`, `Input`). Resolution: first-match; incomplete prefix → `Dispatch::Pending(vec)` which status bar echoes as the which-key hint.

**Count prefix**: digit-first-keystroke resolves to `App.pending_count` which is then baked into cursor/scroll Actions on next match (`3j` = 3-step `CursorDown`).

**`.` repeat**: `App.last_action: Option<Vec<Action>>` captured on every dispatch of a `repeatable()` action; `.` re-fires it with the same args.

Full default binding table lives in `Keymap::defaults()`; see source.

## 5. Ex-line

Full grammar: `src/exline.rs:parse()`; the complete list is what `:<Tab>` completes and what the guide documents (doc-sync test). The original commands below; later ones (`:join`, `:derive`, `:sql`, `:setcol`, `:export`, `:describe`, `:dup`, `:uniform`, `:undo`/`:redo`, `:N`, `:<action-name>`, `vd.command`s) in §3.10–3.11:

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
- `Origin::Selected { parent }` — `"` dup of selected rows, and freq dives
- `Origin::Copy { parent }` — `g"` / `:dup`
- `Origin::Describe { parent }` — `I`
- `Origin::Sql { parent }` — `:sql`
- `Origin::Join { left, right_name }` — `:join`
- `Origin::Parts { path, kind }` — worksheet / table list
- `Origin::SheetSwitcher` — sheet-stack listing

Derived sheets are built from the parent's `view_plan()`: what you see (casts, filter, sort, visible columns) is what freq/pivot/unpivot/diff/dup operate on.

Each derived sheet is a first-class entry on the stack; `q` pops back.

## 7. Lua plugin surface

VM: mlua Lua 5.4, vendored. Loaded from `$XDG_CONFIG_HOME/vdrs/init.lua` (missing file is not an error).

### 7.1 Keymap patches

```lua
vd.bind(layer, keys, action, desc)   -- layer: "sheet" | "input"
                                     -- keys: string | table of strings
                                     -- action: snake_case name per src/plugins.rs,
                                     --   ":ex command", or a Lua function (Action::Lua;
                                     --   vd.sheet() / vd.toast() inside it)
vd.unbind(layer, keys)               -- no-op patch (last-wins, doesn't remove from base)
```

Last-write wins semantics via `Keymap::append()` + reverse resolve.

**Known action names** (from `action_from_name()` in `src/plugins.rs`): all 3.1–3.9 Actions that take no args / default-arg form. Composite Actions (`Pivot`, `SetFilter`, `Save`, etc.) are bound as ex-command strings (`":pivot …"`, §7.3).

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

**By design NOT supported**: loaders returning data from Lua. They return a path; Lua tables → `Series` (per-column inference, `vd.push_sheet`) is only for small results built in a binding. Formats that can't convert to CSV/TSV/Parquet/Arrow/Jsonl in Lua belong in a `vdrs-ext-<fmt>` Rust crate per S-09 extension ADR.

**Scope**: CLI file open (`vdrs mylog.foo`) and `:diff` targets. `:tail` refuses loader formats: a refresh would re-scan the one-time converted file and never see new rows.

### 7.3 Added 2026-11-05

```lua
vd.bind(layer, keys, ":sort -id")              -- ex-command bindings (validated at load)
vd.register_aggregator(name, fn(values) -> number|string|nil)   -- used by :pivot … <name>
vd.register_renderer { column = "x" | dtype = "f64", render = fn(v) -> string|nil }  -- memoized per value; sets widths
vd.set_theme("dark" | "light")
vd.api_version                                  -- 6 (vd.option duckdb_cmd)
vd.bind(layer, keys, function() … end, desc?)  -- runs on the key (api 2)
vd.toast(msg)                                   -- status line; only inside a bound function
vd.sheet() -> { name, cursor = {row, col}, column, value, column_names }  -- 1-based; nil outside a bound function
vd.push_sheet { name?, columns = {…}, rows = { {…}, … } }  -- api 3; inside a bound function; opens after it returns.
                                                -- per column: all ints → i64, numbers → f64, all bools → bool, else str; nil = null.
                                                -- Origin::Lua; not exportable
```

`init.lua` reloads when its mtime changes (polled on the 2 s tick; no `notify` dependency). A failing `init.lua` keeps registrations made before the error and reports it. Input-layer action names: `submit_input`, `cancel_input`, `input_backspace`.

## 8. Theme

`Theme::dark()` / `Theme::light()` in `src/theme.rs` drive the table, title bar, status bar and toasts; `vd.set_theme` picks one. No per-colour override.

Colours are 24-bit RGB. Numbers are right-aligned; a sorted column's header shows ▲/▼ in place of its type (with its rank under a multi-column sort); a `│` follows the pinned columns.

The title bar reads name, then file kind and what shapes the view (`sheets`, `filter`, `sort`, `sel`, `hidden`; each only when in use), then the cursor column and row with percent. A spinner joins it after 150 ms of background work, and rows from a window that's still loading dim.

CSV and TSV files up to 1 GiB move into memory in the background after opening, unless edited first; compressed files stay on disk.

## 9. Testing

- `cargo test --lib` — unit tests over sheet/keymap/widget/exline/action/plugins, plus `app::scenario` end-to-end tests: real keymap → dispatch → background Polars tasks → TestBackend render (never touches the user's ex history)
- `cargo test --test vd_replay` — golden corpus replay (S-14). Needs `../visidata` checked out alongside; skips cleanly otherwise. CI fetches it pinned at `5f67ff8a` and fails on any regression.
- Doc sync: tests fail if docs/index.html misses a default key (`keymap.rs`) or documents an unknown ex command (`exline.rs`), or if its `init.lua` example stops working (`plugins.rs`)
- `cargo clippy --all-targets` — zero warnings
- No benchmarks shipped; `hyperfine` harness is a tasks.md todo

## 10. Non-features (explicitly cut)

Enumerated here so no one re-asks:

- **No Python-expression eval** (direction §9.5): `qty*amt`, `curcol.casefold()`, `date.isocalendar()[2]`, `curcol.strip()`, `str.lower(curcol)`. Users write Lua instead; VisiData golden corpus tests encoding Python expressions are UNTESTABLE in S-14.
- **No options fallback chain** (direction §1): no 4-level default→global→class→instance resolution, no per-sheet options. A flat `vd.option` is planned (§10.1).
- **No `.vd` / `.vdj` runtime** (direction §9.5): the cmdlog format exists only in the S-14 test harness, not the `vdrs` binary.
- **No VisiData state file compat** (direction §9.5): no `~/.visidata`, no `.visidatarc` import.
- **No save-all** (`g^S`) and no `^S` binding: saving is the `:save <path.ext>` ex-command only.
- **No LLM-on-cells**.

### 10.1 Not yet supported (planned or open)

In scope but missing from the `vdrs` binary today. Tracked in `tasks.md`.

- **Nested data** (`expand-col`, `unfurl-col`): JSON/JSONL/Parquet/Arrow already load nested fields as native `struct`/`list` columns; the expand/unfurl operations are missing. Plan: [`S-22-nested-data.md`](S-22-nested-data.md).
- **Plots** (`plot-column`, histograms): plan [`S-23-layout-sidebar-splits-plots.md`](S-23-layout-sidebar-splits-plots.md) part C.
- **Options**: flat `vd.option(name, value)` (S-18 D6), data-changing subset in S-25 P6.
- **Expression keys** `g=` / `:setexpr`, `:filter-expr` (S-21 B); `=`, `z/`, `z|`, `:select` shipped (S-21 A).
- **Everything else on the roadmap** (S-18 … S-30): `../tasks.md`.
- **Cell/row editing + clipboard** (`delete-row`, `add-row`, `open-new`, `paste-*`, `setcol-clipboard`; copy to the register shipped in S-18 A, `syscopy-*` in S-18 B, `edit-cell`/`setcol-input` in S-18 C) — a must-have, deferred (decision 2026-11-07). Plan: [`S-18-editing-clipboard.md`](S-18-editing-clipboard.md) (phases A–G).
- **Macros** (record/playback, `macro-param-*`) — deferred (decision 2026-11-07); `.` repeat and Lua function bindings cover the common case today. Plan: [`S-19-macros.md`](S-19-macros.md); undo tree: [`S-20-undo-tree.md`](S-20-undo-tree.md).

- **Rest of S-10** (to be drafted as S-16): `vd.frame`, `Series` methods, `vd.pop_sheet`, `vd.sandbox`, `vd.co`; `since`/`deprecate` registry + `cargo xtask check-api-versions`. Deferred until a Lua user asks (decision 2026-11-05); `vd.toast`, `vd.sheet`, function bindings and `vd.push_sheet` shipped.
- **Export** of `vd.push_sheet` sheets (the data lives only in Lua).
- **Export**: schema diff and Lua-aggregator pivots.
- **DuckDB backend** — replaced by Polars SQL for `:sql` / `:derive` (decision 2026-11-05). `.duckdb` *files* are read and written through the `duckdb` program (S-09, 2026-11-08); the engine stays Polars.

## 11. File map (where to look)

| Concern | File |
|---|---|
| Sheet / View / LazyFrame plan | `src/sheet/mod.rs`; derived sheets `derive.rs`, diff `diff.rs`, tail `tail.rs`, `PlanStep` + undo `step.rs`, tests `tests.rs` |
| Reader (CSV/TSV/Parquet/Arrow/Jsonl/Json/Xlsx/Sqlite/stdin + gzip) | `src/reader.rs` |
| App loop, jobs, prompts | `src/app.rs`; rendering `src/app/render.rs`; unit tests `src/app/tests.rs`; `scenario` end-to-end tests `src/app/scenario.rs` |
| Action dispatch + handlers | `src/app/actions.rs` |
| Keymap / chord resolution | `src/keymap.rs` |
| Key parsing | `src/key.rs` |
| Action enum + `affects_data()` + `repeatable()` | `src/action.rs` |
| Ex-line parser + history | `src/exline.rs` |
| Sheet widget + `clamp_view` | `src/widget.rs` |
| Lua plugin host (`vd.*` API, aggregators, renderers, hot reload, `plugins::open`) | `src/plugins.rs` |
| Theme | `src/theme.rs` |
| Binary entry | `src/main.rs` |
| Library façade (re-exports for integration tests) | `src/lib.rs` |
| Golden corpus replay harness | `tests/vd_replay.rs` |
| Quarantine list w/ per-entry reasons | `tests/vd_replay/quarantine.txt` |
