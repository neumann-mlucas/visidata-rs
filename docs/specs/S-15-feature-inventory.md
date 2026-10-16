# S-15 — Feature inventory (what `visidata-rs` actually ships)

Status: **landed 2026-10-16** — snapshot after Phase 1.5 close (iteration 14). This is the authoritative list of what the `vdrs` binary and companion crate expose. Update when a user-visible surface changes.
Phase: n/a (cross-cutting documentation)
Depends on: all prior specs
Blocks: nothing — reference material

## Why this spec exists

Previous specs (S-01…S-14) describe *decisions*. This spec describes *the shipped surface*: every reader, action, keybind, exline command, Lua function, derived-sheet type, and plan-visualization. If you can't find a feature named below, it isn't shipped — regardless of what other specs say.

## 1. CLI surface

```
vdrs <path>                  open a file
vdrs -                       read from stdin (auto-sniffed)
vdrs --format <fmt> <path>   override format detection (csv|tsv|parquet|arrow|jsonl|xlsx|sqlite)
vdrs --headless <path>       materialize + print first N rows as TSV, no TUI
vdrs --head N                limit --headless row count
```

Entry point: `src/main.rs:1-107`.

## 2. Readers (eight formats + decompression)

| Format | Extensions | Backend | Behavior |
|---|---|---|---|
| CSV | `.csv` | Polars `LazyCsvReader` | first row = header |
| TSV | `.tsv`, `.tab` | Polars (`\t` separator) | first row = header |
| Parquet | `.parquet`, `.pq` | Polars `scan_parquet` | native lazy |
| Arrow IPC | `.arrow`, `.ipc`, `.feather` | Polars `scan_ipc` | native lazy |
| JSON Lines | `.jsonl`, `.ndjson`, `.json` | Polars `LazyJsonLineReader` | one record per line |
| Excel/ODS | `.xlsx`, `.xls`, `.xlsm`, `.xlsb`, `.ods` | `calamine` → DataFrame | **first worksheet only**, all cells as String (downstream `type-*` casts); empty header names preserved |
| SQLite | `.sqlite`, `.sqlite3`, `.db` | `rusqlite` (bundled) | **first user table only** ordered by `rowid`, all columns as String, UTF-8-lossy decode (handles invalid-unicode TEXT) |
| stdin | `-` | drained to tempfile, sniffed | csvlens-style fallback |
| gzip | `*.gz` suffix | `flate2` → tempfile | one layer; wraps any of the above, e.g. `.json.gz` |

**Deliberately out of scope** (per direction §1): xml, html, yaml, toml, zip, stata, hdf5, numpy, msgpack, xpt, fixed-width, jsonla, usv, png, http. These are candidates for `vdrs-ext-<fmt>` Lua plugin crates (per S-09 extension ADR) when a user asks.

Entry: `src/reader.rs:open()`.

## 3. User-triggered Actions

Full enum: `src/action.rs`. Grouped by category:

### 3.1 Cursor / scroll (cosmetic — no re-materialize)

| Action | Default key | Semantics |
|---|---|---|
| `CursorDown`, `CursorUp` | `j`, `k` | row ±1; supports count prefix (`3j`) |
| `CursorLeft`, `CursorRight` | `h`, `l` | col ±1 |
| `PageDown`, `PageUp` | `<Space>`, `-` | window slide |
| `GotoTop`, `GotoBottom` | `gg`, `G` | first/last row |
| `GotoLeftmost`, `GotoRightmost` | `0`, `$` | first/last column |

### 3.2 Column meta (data-affecting via `View` state, not plan mutation)

| Action | Key | Exline | Semantics |
|---|---|---|---|
| `FreezeColumn` / `UnfreezeAll` | `z=` / `gz=` | — | pin col to left of viewport |
| `HideCol` / `UnhideAll` | `-` / `g-` | `:hide` / `:unhide` | View.hidden set |
| `KeyCol` (toggle) / `UnkeyAll` | `!` / `z!` | `:key` / `:unkey` | View.keys vec (preserved order) |
| `StartRenameColInput` → `RenameCol { new_name }` | `^` | `:rename NEW` | plan.rename() + updates hidden/keys refs |
| `TypeCastInt/Float/String/Date/Currency` | `#` / `%` / `~` / `@` / (no key — use `:type currency`) | `:type int\|float\|string\|date\|currency` | `View.type_casts` applied in window_plan |

### 3.3 Sort / filter / search

| Action | Key | Exline | Semantics |
|---|---|---|---|
| `SortAsc` / `SortDesc` | `[` / `]` | `:sort [+-]<col>` | replace-mode sort on current col |
| `ClearSort` | `zo` | `:unsort` | drop sort state |
| `FilterCurrentColumn { regex }` / `SetFilter { column, regex }` | `\|` (prompts) | `:filter <col> <regex>` | regex filter on col |
| `ClearFilter` | `z\|` | `:unfilter` | — |
| `SearchNext` / `SearchPrev` | `n` / `N` | — | — |
| `StartSearchInput` → regex | `/` | — | string match within current col |

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
| `ToggleSelectRow` / `SelectRow` / `UnselectRow` | `t` / `s` / `u` | View.selected (`BTreeSet<u64>` of absolute row indexes) |
| `SelectAll` / `UnselectAll` | `gs` / `gu` | all window rows |
| `SelectByRegex { regex }` | — | regex over current col |
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
| `Quit` | `gq` | hard exit |
| `RepeatLast` | `.` | capture of resolved Action (not keystrokes) |
| `Redraw` | `^L` | force repaint |

### 3.9 Input mode

Modal input state (`InputBuf::Search` / `Filter` / `Exline` / `Select` / `Rename`): `InputChar(c)`, `InputBackspace`, `SubmitInput`, `CancelInput`. History on exline via `$XDG_STATE_HOME/vdrs/exline_history`.

## 4. Keymap

Entry: `src/keymap.rs`. Model: linear vec of `Chord { on: Vec<Key>, run: Vec<Action>, desc: &'static str }` per `Layer` (`Sheet`, `Input`). Resolution: first-match; incomplete prefix → `Dispatch::Pending(vec)` which status bar echoes as the which-key hint.

**Count prefix**: digit-first-keystroke resolves to `App.pending_count` which is then baked into cursor/scroll Actions on next match (`3j` = 3-step `CursorDown`).

**`.` repeat**: `App.last_action: Option<Action>` captured on every `affects_data()` dispatch; `.` re-fires it with the same args.

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
```

History: file-persisted, `<Up>`/`<Down>` cycle, dedup against last entry.

## 6. Derived-sheet types

Each returns a new `Sheet` with an `Origin` enum entry so the lineage is visible:

- `Origin::File` — opened via reader
- `Origin::Freq { parent, column }` — count + percent per distinct value, sorted desc
- `Origin::Pivot { parent, index, on, values, agg }`
- `Origin::Unpivot { parent, on, index }`
- `Origin::Diff { left, right, keys }`
- `Origin::Tail { parent, path, follow }`
- `Origin::Switcher` — sheet-stack listing

Each derived sheet is a first-class entry on the stack; `q` pops back.

## 7. Lua plugin surface

VM: mlua Lua 5.4, vendored. Loaded from `$XDG_CONFIG_HOME/vdrs/init.lua` (missing file is not an error). API:

```lua
vd.bind(layer, keys, action, desc)   -- layer: "sheet" | "input"
                                     -- keys: string | table of strings
                                     -- action: snake_case name per src/plugins.rs
vd.unbind(layer, keys)               -- no-op patch (last-wins, doesn't remove from base)
```

Last-write wins semantics via `Keymap::append()` + reverse resolve.

**Known action names** (from `action_from_name()` in `src/plugins.rs`): all 3.1–3.9 Actions that take no args / default-arg form. Composite Actions (`Pivot`, `SetFilter`, etc.) are not yet exposed to Lua.

Not shipped: loader plugin shape, aggregator plugin shape, renderer plugin shape, hot-reload. See Phase 3 todos in `tasks.md`.

## 8. Theme

Fixed dark/light pair in `src/theme.rs`. No user override; hooks a `Theme::dark()` and `Theme::light()`. Phase-3-pending: user-defined theme via Lua.

## 9. Testing

- `cargo test --lib` — 33 unit tests over sheet/keymap/widget/exline/action/plugins
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
- **No save-sheet / save-all** (`^S` / `g^S`): the `vdrs` binary opens read-only; the harness compares in-memory output. Save-to-file lives in a future "export" ex-command.
- **No plot column**, **no LLM-on-cells**, **no live chart previews**.

## 11. File map (where to look)

| Concern | File | LOC |
|---|---|---|
| Sheet / View / LazyFrame plan | `src/sheet.rs` | 876 |
| Reader (CSV/TSV/Parquet/Arrow/Jsonl/Xlsx/Sqlite/stdin + gzip) | `src/reader.rs` | 380 |
| App loop / Model/Update/View | `src/app.rs` | 1259 |
| Keymap / count prefix / `.` repeat | `src/keymap.rs` | 232 |
| Key parsing | `src/key.rs` | 199 |
| Action enum + `affects_data()` + `repeatable()` | `src/action.rs` | 176 |
| Ex-line parser + history | `src/exline.rs` | 311 |
| Sheet widget (ratatui table + status bar + plan panel) | `src/widget.rs` | 389 |
| Lua plugin host (vd.bind / vd.unbind) | `src/plugins.rs` | 275 |
| Theme | `src/theme.rs` | 118 |
| Binary entry | `src/main.rs` | 107 |
| Library façade (re-exports for integration tests) | `src/lib.rs` | 14 |
| Golden corpus replay harness | `tests/vd_replay.rs` | ~1230 |
| Quarantine list w/ per-entry reasons | `tests/vd_replay/quarantine.txt` | ~165 lines |

Total: ~4336 LOC of library + binary code, ~1395 LOC of test harness + quarantine.

## 12. What belongs in the next spec

When a change touches this spec, it probably belongs in a successor:

- **S-16**: loader plugin shape (per S-09 §Lifecycle, Phase 3 blocker)
- **S-17**: aggregator plugin shape (per S-09 §Lifecycle)
- **S-18**: renderer plugin shape (per S-09 §Lifecycle)
- **S-19**: pivot setup sheet + live preview panel UI (bet #1 Tier 2)
- **S-20**: plan panel export (bet #2 Tier 2)

Only draft one when the implementation starts (direction §README "specs rot, late-bind to reality").
