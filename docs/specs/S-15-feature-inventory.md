# S-15 — Feature inventory (what `visidata-rs` actually ships)

Status: **landed 2026-10-16**; keys re-aligned with VisiData 2026-11-08 (S-26 phase 0, S-21 A) and 2026-11-14 (S-26 §0 sweep: `s t u zs zu zc zo _ g% <C-v>`); re-verified against `Keymap::defaults()` / `exline::parse` on 2026-11-05; §3.10 lists the open-features pass of that day, §3.11 the 2026-11-07 additions. Planned work: `../tasks.md` roadmap.

User-facing reference: [`docs/index.html`](../index.html) (GitHub Pages). Unit tests fail if it misses a default key binding or documents an ex command that doesn't exist. This is the authoritative list of what the `vdrs` binary and companion crate expose. Update when a user-visible surface changes.
Phase: n/a (cross-cutting documentation)
Depends on: all prior specs
Blocks: nothing — reference material

## Why this spec exists

Previous specs (S-01…S-14) describe *decisions*. This spec describes *the shipped surface*: every reader, action, keybind, exline command, Lua function, derived-sheet type, and plan-visualization. If you can't find a feature named below, it isn't shipped — regardless of what other specs say.

## 1. CLI surface

```text
vdrs <path>                  open a file
vdrs -                       read from stdin (auto-sniffed)
vdrs --format <fmt> <path>   override format detection (csv|tsv|parquet|arrow|jsonl|json|xlsx|sqlite|fixed|usv|lsv|jsonla|yaml|toml|html|zip)
vdrs --headless <path>       print source, schema, and first N rows (Polars table), no TUI
vdrs --head N                limit --headless row count
```

Entry point: `src/main.rs`. A missing file fails before the TUI starts, naming the path.

## 2. Readers (eight formats + decompression)

| Format | Extensions | Backend | Behavior |
|---|---|---|---|
| CSV | `.csv` | Polars `LazyCsvReader` | first row = header; option `csv_skipinitialspace` (default on, VisiData's) strips leading blanks from names and text cells when inference saw any (`LoadedSource.csv.stripped`; export strips too; S-48); a file with blank lines inside its data drops its all-empty rows; blank lines only at the end drop just those rows (S-49) |
| TSV | `.tsv`, `.tab` | Polars (`\t` separator) | first row = header; blank lines skipped as CSV (S-49) |
| Parquet | `.parquet`, `.pq` | Polars `scan_parquet` | native lazy |
| Arrow IPC | `.arrow`, `.ipc`, `.feather` | Polars `scan_ipc` | native lazy |
| JSON Lines | `.jsonl`, `.ndjson` | Polars `LazyJsonLineReader` | one record per line; blank and `#` / `//` lines skipped (copied to a tempfile when present, S-42 A); a line that isn't an object becomes `{"": true}`, an error row (files up to 256 MiB; S-44 A) |
| JSON | `.json` | Polars `JsonReader` (eager) | array of records; a `.json` file holding JSON Lines is sniffed and read as JSONL |
| Excel/ODS | `.xlsx`, `.xls`, `.xlsm`, `.xlsb`, `.ods` | `calamine` → DataFrame | one worksheet (list when several; `file:Sheet`), a column whose cells are all numbers, all true/false or all dates loads with that type; a mixed column loads as text (S-24 R3); empty header names preserved |
| SQLite | `.sqlite`, `.sqlite3`, `.db` | `rusqlite` (bundled) | one table (list when several; `file:table`) ordered by `rowid`; a column holding only integers loads as integers, only numbers as floats, anything else as text, nothing lost (S-24 R3); UTF-8-lossy decode (handles invalid-unicode TEXT) |
| DuckDB | `.duckdb`, `.ddb` | the `duckdb` program (`vd.option("duckdb_cmd")`), Parquet tempfiles | always VisiData's index (name, schema_name, table_type, rows, cols, keys, source); `<CR>` / `file:schema.table` opens one, typed; `:exec-sql` runs SQL in the file (last query's rows open, index reloads); `:save x.duckdb` = `CREATE OR REPLACE TABLE <sheet>` |
| Fixed width | `.fixed`, or `--format fixed` | `reader/text.rs` (eager) | columns where the header row has text, widened to the data (VisiData's `columnize`); values keep their padding; text columns (S-42 B) |
| USV | `.usv` | `reader/text.rs` | `␟` between fields, `␞` after rows; first row = header; text columns |
| LSV | `.lsv` | `reader/text.rs` | `key: value` lines, blank line between records, a line without `:` continues the key; columns in order of first appearance; missing = empty text |
| JSON arrays | `.jsonla` | `serde_json` → Polars `JsonReader` | first line = header array, then one array per row; JSON types kept |
| YAML | `.yaml`, `.yml` | `yaml-rust2` → JSON records → Polars `JsonReader` | one list document → a row per item; one other document → one row; several documents → a row each (S-42 D) |
| TOML | `.toml` | `toml` → JSON record | one row, a column per top-level key; tables are structs, arrays lists; datetimes text |
| HTML | `.html`, `.htm` | `scraper` | always the index (VisiData's `name rows cols id classes title aria_label caption summary heading`): each `<table>` (`id`, else `table_<i>`) and `links`; `<CR>` / `file:name` opens one. Header rows (`th`, scope col) join with `_`, colspan/rowspan fill, no header → first row; a cell's `<a href>`s → `<col>_link<i>` columns; text columns; names stripped |
| Zip | `.zip` | `zip` | always the members list `directory filename ext size compressed_size date_time` (directory `.` at the top); `<CR>` / `file:member` extracts the member to a tempfile and opens it by its extension |
| XML | `.xml`, `.svg` | `quick-xml` (`reader/xml.rs`, eager) | VisiData's XmlSheet: a row per element in document order, `tag` (local name), `children` (child elements), `text` (before the first child, lxml's `text`; entities resolved), a column per attribute first seen first; no dive, no `path`/`tail`/`sourceline` columns (S-53) |
| MessagePack | `.msgpack`, `.msgpackz` | `rmp-serde` → JSON records → Polars `JsonReader`; `.msgpackz` through `brotli` | as YAML: a list → a row per item, another value one row (S-53) |
| stdin | `-` | drained to tempfile, sniffed | csvlens-style fallback |
| gzip | `*.gz` suffix (any case) | `flate2` → tempfile | one layer; format from the inner extension (`.json.gz`) or `--format` |

**Deliberately out of scope** (per direction §1): stata, hdf5, numpy, xpt, png, http. These are candidates for `vdrs-ext-<fmt>` Lua plugin crates (per S-09 extension ADR) when a user asks.

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
| `GotoLeftmost`, `GotoRightmost` | `gh` or `0`, `gl` | first/last column |
| `StartGoColInput` | `zc` | prompt for a column number: 0-based, negative from the end, clamped with a toast (VisiData `go-col-number`) |

All vertical moves go through `App::goto_row`, which re-centres the 1024-row window when the target is outside it. The title shows `row N/total` (1-based; `?` while counting).

### 3.2 Column meta (data-affecting via `View` state, not plan mutation)

| Action | Key | Exline | Semantics |
|---|---|---|---|
| `FreezeColumn` / `UnfreezeAll` | vim profile `zs` / `zc` | `:freeze` / `:unfreeze` | pin cols up to the cursor to the left of the viewport |
| `FreezeCol` | `'` | `:freeze-col` | VisiData `freeze-col`: `<col>_frozen` after the column, a `Derive` copy with its type casts, aggregators, fmtstr and width (`Sheet::freeze_col`, S-46 A) |
| `ResizeColMax` | `_` | — | toggle the column between full width (widest loaded value) and default (VisiData `resize-col-max`); on a plot `_` zooms to fit |
| `HideCol` / `UnhideAll` | `-` / `gv` | `:hide` / `:unhide` | View.hidden set |
| `HideUniform` | `g-` | `:uniform` | hide columns with ≤ 1 distinct value |
| `KeyCol` (toggle) / `UnkeyAll` | `!` / `z!` | `:key` / `:unkey` | View.keys vec (preserved order) |
| `SlideCol { by }` / `StartSlideInput { left }` / `Order { columns }` | `H` `L` / `gH` `gL` / `zH` `zL` | `:order a b c` | `View.order`: non-key columns placed first in that order (keys stay first; slides stay inside their section); `[` `]` on the columns sheet writes its row order back; sessions save `order` (S-34) |
| `StartRenameColInput` → `RenameCol { new_name }` | `^` | `:rename NEW` | plan.rename() + updates hidden/keys/casts/filter/sort refs; refuses a visible column's name, moves a hidden one aside to `<name>_1` (S-53) |
| `RenameColsFromRows { all, overwrite }` | `z^` / `g^` / `gz^` | `:rename-col-selected` `:rename-cols-row` `:rename-cols-selected` | columns named after their cells in the selected rows (else cursor row), joined by a space; `g^` only unnamed ones (empty or `column_<n>`); one undo step; the names come from a background scan (`Sheet::names_from_rows`, S-43 A) |
| Option `header` | `vd.option`, `:set` | — | rows read as column names by later opens of CSV/TSV, xlsx (also its parts), fixed width, usv: 0 → `column_1…`, N → each column's N cells joined (VisiData's); kept on `LoadedSource.header` (part dives, sessions set it around `:open`); `reader::open_with` (S-43 A) |
| `TypeCastInt/Float/String/Date/Currency` | `#` / `%` / `~` / `@` / `$` | `:type int\|float\|string\|date\|datetime\|date-only\|currency` | `View.type_casts` applied in window_plan; `TypeCastDateAs { time }` = `:type datetime` / `:type date-only` (`@` with the type named, for sessions) |
| `TypeCastJson` | — | `:type json` | `PlanStep::Json`: `str.json_decode` with the type inferred from every value (S-22 C); export: `str.json_decode(dtype)` / DuckDB `from_json(c, structure)` |
| `ExpandCol { depth }` / `StartExpandDepthInput` / `ExpandCols { depth }` | `(` / `z(` `gz(` / `g(` | `:expand-col` … | `PlanStep::Expand`: struct fields as `col.field` columns right after the struct, list items as `col[0]`… (as many as the longest list; null past a shorter one; S-53) (depth 0 = all the way; taken names get `_2`), struct hidden; one undo step; `(` on an expanded struct only shows its columns again (S-22 A) |
| `OpenColumns` / `OpenColumnsAll` | `C` / `gC` | `:columns-sheet` `:columns-all` | `Origin::Columns`, keyed on `name` (`gC`: `sheet`, `name`, as VisiData's `nKeys = 2`, S-53): a row per column (hidden too) `name width type value aggregators sortorder key` (`gC`: `sheet` first); `e` on name/width/type/value/aggregators, `d` / `g-` `gd` hide, `Enter` freq, `U`/`R` write back to the listed sheet(s), one undo step per sheet (S-25 P3) |
| `KeySelected { on }` / `TypeSelected { name }` | `g!` `gz!` / `g#` `g%` `g~` `g@` `g$` | `:key-selected` … | columns sheet only: key / unkey / cast the selected rows' columns; elsewhere `g%` opens `:precision ` (VisiData `setcol-precision-input`) |
| `StartAggregateInput { memo, selected }` → `AggregateCol` / `MemoAggregate` / `AggregateCols` | `+` / `z+` / `g+` | — | `View.aggregators` `(column, agg)`: `F` adds `{col}_{agg}` columns in view column order and hides percent/histogram; `z+` toasts `col_agg=v` over selected (or all) rows; `g+` on the columns sheet: selected rows, or every visible non-key column; export: freq lineage emits the same aggs (S-25 P4); `p10`…`p99` (VisiData's 15 percentiles, linear, cast to the column's type) and `q3 q4 q5 q10` (expand to their percentiles; S-42 C) |
| `ContractCol` | `)` | `:contract-col` | view only: hide the struct's columns, show the struct, cursor onto it |
| `UnfurlCol` | `zM` | `:unfurl-col` | derived sheet `unfurl(col): parent`, `Origin::Unfurl` / `Derivation::Unfurl`: `col_key` (position / field) + `col_value` in the column's place; empty list drops its row (kept, key and value null, under option `unfurl_empty`, `:set` live; S-42 C), null keeps it as item 0; struct values text when field types differ (S-22 B) |

### 3.3 Sort / filter / search

| Action | Key | Exline | Semantics |
|---|---|---|---|
| `SortAsc` / `SortDesc` | `[` / `]` | `:sort [+-]<col>` | adds/updates current col as a sort key (multi-col) |
| `ClearSort` | vim profile `zo` | `:unsort` | drop sort state |
| `FilterCurrentColumn { regex }` / `SetFilter { column, regex }` | `gz\|` (prompts) | `:filter <col> <regex>` | regex over the column's text (any dtype); bad regex / unknown column rejected with a toast |
| `ClearFilter` | `zf` | `:unfilter` | — |
| `SearchNext` / `SearchPrev` | `n` / `N` | — | next hit in the search's direction / against it, whole view, wrapping |
| `StartSearchInput { all_columns, backward }` | `/` `?` / `g/` `g?` | — | case-insensitive regex in the cursor column (at each `n`) / every visible column (VisiData search-col / search-cols) |
| `StartSearchKeysInput` | `r` | — | the same regex over the visible key columns (the first visible column without keys), forward; `n`/`N` repeat; the cursor column stays (VisiData search-keys, S-41 C) |
| `StartSearchExprInput { backward }` | `z/` / `z?` | — | next row where a SQL predicate holds (hidden columns allowed) |

Selections key on row ids (position in the base plan), so they survive sort and filter.

### 3.4 Derived sheets

| Action | Key | Exline | Semantics |
|---|---|---|---|
| `OpenFreqTable` | `F` | — | `Sheet::freq_from()` — current col (the freq sheet's key, S-52) with `count` + `percent` cols (2-decimal, round_series feature); `numeric_binning`: `freq_binned` ranges in a job, dive by bin index (S-35 C1) |
| `Pivot { index, on, values, agg }` | — | `:pivot <index> <on> <values> [agg]` | `sheet::pivot_df` (Polars dropped `pivot_stable`); eager |
| `Unpivot { on, index }` | — | `:unpivot <on,on> -- <idx,idx>` | `LazyFrame::unpivot`: VisiData's `Variable`/`Value`, row-major, nulls dropped, index columns as keys, S-44 error flag not melted |
| `Diff { path, keys }` | — | `:diff <path> [key1,key2,...]` | schema diff or full outer join flagging only_a/only_b/changed |
| `Tail { path, follow }` | — | `:tail <path> [--follow\|-f]` | 2s tick re-reads; auto-scroll when at bottom |
| `OpenCellFile` | `zo` | — | open the cursor cell's text as a path (`Open`; VisiData `open-cell-file`) |

### 3.5 Selection

| Action | Key | Semantics |
|---|---|---|
| `SelectRow` / `ToggleSelectRow` / `UnselectRow` | `s` / `t` / `u` | View.selected (`BTreeSet<u64>` of row ids — position in the base plan); cursor moves down after, as VisiData |
| `SelectBefore` / `UnselectBefore` | `zs` / `zu` | rows from the top to the cursor; background job |
| `GotoSelected { forward }` | `}` / `{` | VisiData go-next/prev-selected: cursor to the next (previous) selected row, view order; `Sheet::rids_to_selected`, background job (S-50) |
| `ValueChange` / `NullChange { forward }` | `>` `<` / `z>` `z<` | VisiData go-next/prev-value, go-next/prev-null: down (up) the column to a different value / a null, loaded rows (S-50) |
| `SortKeys { descending }` | `g[` / `g]` | VisiData sort-keys-asc/desc: sort by the key columns, replacing the sort; none → toast (S-50) |
| `Melt` | `M` | VisiData melt: `Unpivot` of the visible non-key columns, keys as index (S-50) |
| `Transpose` | `T` | VisiData transpose: `Sheet::transpose_from`, eager, refused past `MAX_CELLS` (10M, as copy); a row per visible non-key column, a column per row named by its key values (`_`-joined; row number without keys), first column = key names (`column` without keys), all text; repeated names get `_duplicated_<i>`, never another column's name (S-51) |
| `FreqKeys` | `gF` | VisiData freq-keys: `F`'s sheet (`Sheet::freq_binned`, S-52) over every key column, each grouped by its shown text (fmtstr, number format), then count, percent, histogram (`freq_*` when a key has one of those names) and aggregators, count desc; `<CR>`/`g<CR>` → `rows_with_keys` on that text (nulls match); exports (S-51, S-52) |
| `JoinAll { how }` | `g&` | `:join-all <how>`, VisiData join-sheets-all: `join_from` chained over the sheets below, nearest first, on the active sheet's keys; an edit goes back to every sheet behind the cell, through the inner joins (`JoinSources::inner`) (S-51, S-52) |
| `OpenCell` | `zEnter` | VisiData open-cell: `Sheet::open_cell`, list/struct/JSON text → table, one column or `key`/`value` rows (`Origin::Cell`); `<CR>` there → `open_cell_row`; mixed types → text, NaN/inf too; JSON and TOML keep source key order (S-51) |
| `StartGoColRegexInput` | `c` | VisiData go-col-regex: next column whose name matches, wrapping (S-50) |
| `ExPrompt { cmd }` | `o` / `Ctrl+S` / `&` | VisiData open-file, save-sheet, join-sheets-top2: the ex line holding `open ` / `save ` / `join <sheet below> ` (S-50) |
| `SelectToSelected { forward }` | `z}` / `z{` | the cursor row through the next (previous) selected row, view order; none → `no selected row`; `Sheet::rids_to_selected`, background job (S-41 B) |
| `SelectAll` / `UnselectAll` | `gs` / `gu` | every row of the view (needs the row count) |
| `SelectByRegex { regex, unselect, all }` | — | regex (case-insensitive, VisiData's `regex_flags` `I`) over the current col, or any visible col (`all`); whole view; `Sheet::rids_matching_any` (S-41 A) |
| `StartSelectInput { unselect, all }` → prompt | `\|` `\\` `g\|` `g\\` | — |
| `SelectExpr { expr, unselect }` | `z\|` / `z\\` (prompt) | `:select <pred>` / `:unselect <pred>`: rows where a SQL predicate holds; background job |
| `DupSelected` | `"` | new sheet filtered to selected rows via `Sheet::dup_selected_from` |
| `CopyRow` / `CopySelected` / `CopyCell` / `CopyCells` | `y` / `gy` / `zy` / `gzy` | register (`App.register`, one per app): cursor row / selected rows in view order (every column, hidden too, typed) / cursor cell / current column of selected rows; background job; over 10M cells refused (S-18 A) |
| `SysCopyRow` / `SysCopySelected` / `SysCopyCell` / `SysCopyCells` | `Y` / `gY` / `zY` / `gzY` | same rows as text to the system clipboard: visible columns as TSV with header / raw cell / one value per line; `clipboard_copy_cmd` or OSC 52 (S-18 B) |
| `SysPasteCells` / `SysPasteCellsSelected` | `zP` / `gzP` | system clipboard (`clipboard_paste_cmd`, else the last terminal paste) as TSV lines into cells from the cursor / into selected rows, cursor column rightwards; extra lines add rows; all fit or refused; one undo (S-18 G) |
| terminal paste (bracketed) | — | table: TSV/CSV → register (header line = column names, else by position; 1 column = cells); prompt: types the first line (S-18 G) |
| `PasteCell` / `PasteCells` / `CutCell` / `CutCells` | `zp` / `gzp` / `zx` / `gzx` | copied cells → cursor cell (first value) / current column of selected rows in view order, cycled (one `SetCells` step; all must fit, else refused whole); cut = copy, then null (S-18 D) |
| `DeleteCell` / `DeleteCells` | `zd` / `gzd` | null the cursor cell / current column of selected rows, register untouched (VisiData `delete-cell(s)`) |
| `AddRow` / `StartAddRowsInput` / `PasteAfter` / `PasteBefore` / `OpenNew` | `a` / `ga` / `p` / `P` / `A` | an `AddRows` step (front of the steps; rows concatenated onto the base by `rebuild_plan`, ids from `1<<62`, f64 position key = midpoint between neighbours in base order, a source row's key is its id); cursor to the first new row; paste matches columns by name (missing → null, extra → new columns, strict type cast or refused); `A` = empty in-memory sheet with column `A` (S-18 F) |
| `DeleteRow` / `DeleteSelected` / `CutRow` / `CutSelected` | `d` / `gd` / `x` / `gx` | a `DeleteRows` step: the rows are masked after all steps (row ids stay base positions); cursor stays, so the next row moves up; deleted rows leave the selection; `x`/`gx` copy (as `y`/`gy`) first (S-18 E) |

### 3.6 Sheet stack

| Action | Key | Semantics |
|---|---|---|
| `OpenSheetSwitcher` | `S` | push meta-sheet listing stack; `<CR>` on row → `SwitchToSheet` |
| `SwitchToSheet(id)` + `SubmitRow` | `<CR>` | pop sheet-switcher, activate selected |
| `PopSheet` | `q` | drop active, show one below; unsaved edits → `discard N edits to x? y/N` (`:q!` skips) |
| `StartEditInput` / `StartSetColInput` | `e` / `ge` | edit cursor cell (prefilled) / `:setcol` prompt; source sheets, and pivot (key and added columns only, for every write: `e`, `ge`, `g=`, paste, `:s`; `Sheet::check_writable` against `Lineage.made`)/unpivot/unfurl/`:sql`/join/Lua sheets: the first edit copies the rows into memory (`Sheet::make_editable`; `:refresh` drops the edits, `:export` refuses; S-45 A); a join's `e` also edits the open source sheets' rows (`Sheet::source_cells`) and every join row sharing them (S-45 B) |
| `StartSetExprInput` | `g=` | `:setexpr` prompt; source sheets only (S-21 B) |
| `JumpPrev` | `<C-^>` / `<C-6>` | the sheet under the active one comes to the top (VisiData `jump-prev`); twice = back (S-26 A7) |
| `HelpCommands` | `z<C-h>` / `z<F1>` / `z<BS>`, `:help` | push a `help` sheet: key, action (`to_ex`), description, menu per sheet-layer chord (S-26 A5); menu = VisiData's menu path for a key still bound as by default (`keymap::menu_path`, `src/menu.tsv` from `scripts/gen_menu.py`, S-37 P0d) |
| Operators | vim `d` `y` `c` + `dd`/motion/`i{r,s,c,v,b}` | `App.operator: Pending`; motions `j k G gg H M L <C-d> <C-u> { }` (counts multiply) → rows → `run_ranged` (`cut-row`, `y`, `:'<,'> setcol`); `ic iv ib` act on cells; `.` = `:.,.+N cmd` (S-26 D3) |
| Registers | `:registers`; vim `"x` prefix | `App.registers`: `"0` last yank, `"1`–`"9` cut ring, `"a`–`"z`; `""` = `App.register`; `"+` routes to the system clipboard; a prefixed paste swaps the named one in for that action (S-26 D2) |
| Vim profile | `vd.option("keymap", "vim")` | `plugins::VIM_PROFILE`: `vd.bind` patches over the defaults (guide table synced by test); new actions `HalfPage`, `ScreenRow`, `ScrollTo`, `CellMotion`, `ValueChange` (loaded rows), `SearchCell` (`^value$`) (S-26 D1) |
| Marks, `q` macros | vim `m{a-z}` `'{a-z}` `q{a-z}` `@{a-z}` `@@` | `App.awaiting: Await` names the next key; `App.marks` (view positions, like the jumplist); `App.macros` (ex steps, session only) replay via `Action::Macro` (S-26 D4) |
| Prompt normal mode | vim `Esc` in a prompt | `LineEdit` normal mode: `h l w b e 0 $ x dw cw D C i a A I u`; input-layer `input_normal`; `Esc` again cancels (S-26 E) |
| Visual | `V` rows, `:visual-block` (vim profile `<C-v>`) block, `Esc`; `:reselect`; `'<,'>` | `App.visual` (anchor + cursor, view rows); verbs `s u y d x e " :` → `run_ranged` (`e`/`:` prefill `:'<,'>`); block `-` hides its columns (undoable); highlighted reverse; status line `-- VISUAL --` (S-26 C) |
| Ex ranges | `:A,B cmd` (`5`, `.`, `$`), `:% cmd`, `:g/re/cmd`, `:v/re/cmd`, `:g/col=re/`, `:g{pred}cmd`, `:A,B normal keys` | `exline::ranged` → a job resolves row ids (`rids_in_rows`, `rids_matching`, `rids_where`) → `cmd` runs with them as the selection (`DeleteRow`/`CopyRow`/`CutRow`/`SysCopyRow` → selected forms; `:d` `:y`); the user's selection comes back less what `cmd` unselected, in undo states too; no cmd = select; `:normal` types keys per row, loaded window only, 10k max (S-26 B) |
| `JumpBack` / `JumpForward` / `Jumps` | `:jump-back` `:jump-forward` `:jumps` (vim profile `<C-o>` `<C-i>`) | jumplist of (sheet, view row, column) recorded before `gg G :cursor n N <C-^> Enter F S`; back from the newest first adds the current place; a new jump drops what was ahead; 100 kept (S-26 A7) |
| mouse | `vd.option("mouse", "true")` | `EnableMouseCapture`; wheel = 3 rows; click = cell, header click = column (`widget::column_at`) (S-26 A9) |
| keyboard protocol | — | kitty `DISAMBIGUATE_ESCAPE_CODES` pushed at start / after `<C-z>`, popped on exit, suspend, panic (S-26 A6) |
| `ShowVersion` | `<C-v>` | toast `vdrs <version>` (VisiData `show-version`) |
| `Suspend` | `<C-z>` | leave raw mode + alt screen, `kill -TSTP` self, take the terminal back on `SIGCONT` (unix; full redraw without a cursor query) (S-26 A3) |

### 3.7 Plan panel

| Action | Key | Semantics |
|---|---|---|
| `TogglePlanPanel` | `:plan` (was `P`; `P` is paste-before, S-18 F) | sidebar's plan page: Off → Normal (`describe_plan`) → Optimized (`describe_optimized_plan`) |
| `ToggleSidebar` / `CycleSidebar` / `ShowSidebar` | `b` / `Ctrl+G` / `:sidebar [page]` | right-hand sidebar (35 %, starts closed): column (one `I` row for the cursor column, background job keyed by sheet/column/filters/steps), cell (full value), help (sheet-layer bindings), plan (S-23 A) |

### 3.8 Meta

| Action | Key | Semantics |
|---|---|---|
| `StartExInput` → `ExCommand(str)` | `:`, `<Space>` | — |
| `StartDeriveInput` | `=` | new column from a SQL expression, named after it; `name = expr` names it when `name` is new |
| `Save { path }` | — | `:save path.ext` — writes every row of the active sheet's `save_plan()` (`view_plan()`: casts + filter + sort + visible cols; on a sheet with error rows every cell as text, `#ERR` on those rows, S-44 A); writer by extension (csv/tsv/parquet/ipc/jsonl/json); csv/tsv write list/struct cells as Python `json.dumps` text (S-49); unknown extension errors without touching the file; `:w` and `:write` aliases |
| `Quit` | `gq`, `<C-q>` | exit |
| `CancelJobs` / `CancelAllJobs` | `<C-c>` / `g<C-c>` | drop the active sheet's / every background job (`working: …`); the window load and row count are not cancelled |
| `RepeatLast` | `:repeat-last` (`.` plots since S-23 C1) | capture of resolved Action (not keystrokes) |
| `ToggleMacroRecord` / `Macro` | `m` | record what `.` captures as ex text (`Action::to_ex`), bind it, save to `macros.lua`; playback waits for jobs between steps (S-19) |
| `Redraw` | `^L` | force repaint |

### 3.9 Input mode

Modal input state (`InputBuf::Search` / `Filter` / `Exline` / `Select` / `Rename`): `InputChar(c)`, `InputBackspace`, `SubmitInput`, `CancelInput`. History on exline via `$XDG_STATE_HOME/vdrs/exline_history`.

### 3.10 Added 2026-11-05 (open-features pass)

| Surface | Key / command | Semantics |
|---|---|---|
| Freq dive | `<CR>` / `g<CR>` on a freq sheet | parent's rows with the cursor's / selected values (text compare, null matches null); rows dived from a freq sheet are a freq sheet again (`Origin::Freq` kept), so `<CR>` goes on to the source |
| Describe | `I`, `:describe` | VisiData's describe sheet: per visible column `sheet` `column` `type` (sheet, type hidden), errors (failed type casts), nulls, distinct (null = one value), mode (first seen on ties); numbers: min max sum median mean stdev (sample); dates: min max, median when the count is odd (VisiData `#ERR` → empty) (background job) |
| Describe select-cell | `zs` / `zu` on the describe sheet | the parent's view rows the cursor stat describes (`Sheet::described_rids`: errors, nulls, = mode/min/max, median low/high, distinct all, else non-null); a null stat → `no <stat> for <col>`; background job, `JobDone::SelectIn` on the parent (S-41 F) |
| Pivot | `W` | key cols × current col, counts (absent pair 0, null/empty value column `''`); with `+` aggregators one column per aggregator and value (`sum_Pencil`, VisiData names; S-35 B); no keys → one row |
| Pivot preview | typing `:pivot …` | right half: next argument + columns + aggs, then the pivot of the first 10k view rows (background, Lua aggs too); Enter runs the full pivot |
| Diff prompt | `D` | prompts for a path; key cols are the diff keys |
| Copy | `g"`, `:dup` | lazy copy of the view |
| Select equal | `,` | rows whose current col equals the cursor cell |
| Undo / redo | `U` / `R`, `:undo` / `:redo` | per-sheet undo tree of (plan, view, steps) snapshots, 100 states; a change after `U` branches; `R` = child last made or come back from (S-20) |
| Undo history | `:earlier [n]` / `:later [n]`, `:undotree` (`open_undo_tree`) | walk states in creation order across branches; a sheet of states (id, parent, ago, change, `*` current), `Enter` restores one (S-20) |
| Join | `:join <file\|sheet\|#id> [keys] [inner\|left\|full\|merge\|extend\|append]` | same-named keys; rows in VisiData's order (keys as first seen, left then right-only, each key's rows together); inner/left/full: a non-key column on both sides is `<name>_<col>` on each (VisiData's); null keys match (`nulls_equal`, VisiData's `""`); merge = full + right non-null wins; extend = left + right non-key columns renamed `<name>_<col>` (name = file name before its first `.`); append = diagonal concat + `origin_sheet`; result collected once (background) |
| Substitute | `:s/re/rep/[g][i]`, `*` `g*` `gz*` | `PlanStep::Replace` on selected rows (none: the view's); `*` = new `<col>_re`; exported (S-28) |
| Search hits | `:hits <regex>` | sheet of (row, column, value, keys); `Enter` jumps to the cell (S-28) |
| Selection stats | status line, `:stats` | sum/avg/min/max (numbers) or count/distinct of the cursor column over the selection, a background job keyed by (sheet, selection, column, revision); `:stats` copies TSV (S-30 C) |
| Cell notes | `zn`, `:note [text]`, `:notes` | per source file TSV in `$XDG_STATE_HOME/vdrs/notes/<fnv64 of path>.tsv` (header names the path), atomic write; row = key values when keys are set, else rid + size/mtime stamp (`stale?` on mismatch); `•` marker, sidebar cell page; `:notes` = hits-shaped sheet, `Enter` jumps; file sheets only (S-30 B) |
| Split panes | `Z` `:split`, `:vsplit`, `gZ`, `Tab`, `gTab` | `App.split = { other, vertical, focus_first }`: the focused pane is the active sheet (every command unchanged), the other pane shows `other` dimmed with its own title row; `Tab` raises `other`; the pane's last `q` closes the split; the other pane materializes when stale (S-23 B) |
| Plots | `.` `g.` `:plot x y…` | `Origin::Plot`; the parent's view binned by x into 2 × screen-width bins in Polars (mean x, min/max per y), a background job; ratatui `Chart` braille scatter, legend top left; x numbers/dates (`.`: first numeric key column, VisiData's messages), y numbers (S-23 C1) |
| Histogram / bars | `:hist [bins]`, `:bar` | `Origin::Bars { label, value, horizontal }`; hist = lazy right-closed equal-width bins (20 default) of the cursor column → `bin, count, lo, hi`; bar = a freq sheet's first 1000 (value, count) rows; ratatui `BarChart` (S-23 C2) |
| Plot navigation, box select | on a plot: `hjkl`, `HJKL`, `+` `-`, `zz`, `_`, `s` `u`, `Enter` | `SheetState.plot = PlotView { x, y, cursor, parent snapshot }`; VisiData's cursor-box model; zoom re-bins only the x range in view (`plot_from(.., Some(range))`); `s`/`u` = parent rids with x and any y in the box (`SelectIn` job, undoable on the parent); `Enter` = `rows_from` of them; drawn on a ratatui `Canvas` (S-23 C3/C4) |
| Refresh / link | `<C-r>` `:refresh`, `:link` / `:unlink` | replay the `Derivation` on the parent's current view, keep this sheet's view, undoable; changed ancestors first; parent closed → its snapshot; linked sheets refresh on activation when the parent's view plan text differs; title `⟳` (S-30 A) |
| Sessions | `:mksession [path]`, `:source <path>`, `vdrs -S <file>`, autosave on quit | the stack as an ex script (S-29): `open`, steps, view, cursor; pasted cells / added rows as parquet next to it; what can't replay is a `#` comment |
| Lua hooks | `vd.on(open\|push\|save\|quit\|enter\|loaded\|tick\|reload, [opts,] fn)`, `vd.exec(cmd)`, `vd.off(id)`, `vd.augroup(name)`, `vd.clear_group`, `vd.handlers()`, `:handlers` | run on the new sheet once its window lands; commands queue like macro steps; hook-made sheets fire nothing (S-29); opts `pattern` (glob on name, or path with `/`), `once`, `group`; `enter` from `follow_link`, `loaded` after `push`, `tick` on the 2 s tail tick, `reload` after hot reload (S-40 A) |
| Column builders | `za` `:addcol`, `i` `gi`, `f`, `;` `:capture` | `za` = `Derive` of a null text column; `i` = `Derive` + `SetCells` numbering the view's rows in view order (named `A`, `B`, … per sheet, as VisiData; S-35), `gi` the selected rows; `f` fills nulls from above in view order (selection, else all); `;` = `PlanStep::Capture` struct `<col>_re` (fields: group names, else `0`, `1`…). Scans run in a job on big views (S-33). New columns (`za` `i` `;` `*` `=`, `(` fields) go right after the cursor column (S-35) (S-31) |
| Numbers by key | `:addcol-incr-key`, option `incr_base` | `i` restarting at each change of the key columns' text in view order (`Sheet::key_run_numbers`); `incr_base` (default 1) is where `i`, `gi` and it start; `:set` live (S-41 E) |
| Float precision | `Alt++` `Alt+-`, `:precision [N]`, columns sheet `fmtstr`, `vd.option("disp_float_fmt", "%.2f")` | `View.precision` (column, decimals), display only (`widget::float_digits`: the column's, else `Theme.float_digits`, else 15 significant digits); first `Alt++` = 3, as VisiData's `%.2f` + 1; float columns only; in sessions and copies (S-32) |
| Runtime options | `:set numeric_binning true`, `:set histogram_bins N` | `Theme` fields for this run; sessions write them before each `F`; `init.lua` reload resets them (S-38 A) |
| Date format | columns sheet `fmtstr`, `:fmtstr [fmt]` | `View.fmtstrs` (column, strftime); display via Polars `strftime`; `F` groups and dives by the formatted text; export `dt.strftime` / `strftime` (S-35 C2) |
| Number format | columns sheet `fmtstr`, `:fmtstr {:,.2f}` | `View.fmtstrs` (column, fmtstr), `src/numfmt.rs`: Python `{:,.2f}` `{:_d}` `{:.1%}` `${:,.2f}` `{:.2f} kg` `{:e}`, C `%d` `%e`; ints too. `Alt++`/`Alt+-` change its `.N`. `F` groups and dives by the text when the user set a format or precision (K3); export `map_elements(fmt.format)` / DuckDB `format` / `printf` (S-36 B) |
| Lookup columns | `:lookup <src\|#id> key[=theirkey] [name=]col[,…]` | `PlanStep::Lookup` per column: `replace_strict` on the key as text from a first-match table of their `view_plan()`; open sheet = live (`LookupLink` stamp = plan key + undo state, refreshed in a job when the sheet becomes active, frozen when theirs closes), file = snapshot; cycles refused; file sheets only; session = parquet sidecar snapshot; export inline (S-36 A) |
| Validation | `:validate <col> [pred]`, `:validations`, `validation` option (`refuse`/`warn`) | `View.validations` (one per column, undo, sessions, rename refused while a rule reads the column); `Sheet::check_edit` after `insert_edit` / `add_rows` on the written rows: `NOT pred` true (null passes) refuses the edit, warn mode keeps it with a toast; failing cells painted red+underline as an extra `:color`-style window column (S-36 D) |
| Totals footer | `:footer`, `disp_footer` (`vd.option`, `:set`) | a row under the table: each column's `+` aggregators over the filtered view in its number format, `Σ` row count; `totals` job keyed by revision/filter/aggregators/formats (S-36 C) |
| Macros sheet | `gm`, `:macros`; `d` `e` `Enter` `z<C-s>` | a row per macro binding (keys, steps, first, source, change); marks deferred until `z<C-s>` (`commit_sheet`) unbinds/rebinds and rewrites `macros.lua`; `init.lua` macros read-only; `q` with marks asks (S-36 E) |
| Loaders (S-40 C) | `vd.register_loader { name, extensions, patterns, open }`, `-f`/`--format <kind\|loader>`, `:open path [format]`, `vd.loaders()` | last registered claiming the file wins (extension or glob); a name picks one by hand; `open` may return `{ parts = {…} }` → a `part` list (`Origin::Parts`, sources kept in `LoadedSource.parts`), `file:part` opens one; stdin refused for loaders |
| Project config | `.vdrs.lua` (cwd up to `$HOME`), `:trust [view\|off]`, `vd.reset()`, `vd.config_dirs()` | runs after `init.lua`/`options.lua`/`macros.lua` in the same VM when its bytes equal the copy in `state_path("trust/…")`, else a toast; hot reload watches it; last loader/aggregator of a name wins; `require` from `<global>/lua` and `<project>/.vdrs/lua`; `O` shows each option's `from` (S-40 B) |
| Options sheet | `O`, `:options`; `e`, `z<C-s>`; `:set` any option | `src/options.rs` registry (type, default, live, help) checks `vd.option` and `:set`; `z<C-s>` writes `options.lua` (loaded after `init.lua`, before `macros.lua`) (S-37 P0c) |
| Inherited attributes | `"` `g"` `g'` dedupe, freq dive | copies keep the parent's `type_casts`, `aggregators`, `:color` / `:scale` rules for the columns they have (S-31 C) |
| Derive | `:derive name = <SQL expr>` | Polars `sql_expr` on the base plan; validated against the schema |
| SQL | `:sql <query>` | Polars SQL; `self` = view, `s<id>` = other open sheets; result collected once (background) so row order is fixed |
| Set column | `:setcol <value>` | current col on selected rows; value must fit the displayed and base type; empty = null; placed after the column's last writer |
| Set by expression | `:setexpr <sql expr>` | `PlanStep::SetExpr`: current col on selected rows := Polars SQL expr over base columns, non-strict cast to the column's type (result type must fit: same, numeric→numeric, any→text); placed after the last writer of the column and of every column it reads; exported as `pl.when(...)` / `CASE WHEN __rid IN` |
| Predicate filter | `:filter-expr [<pred>]` | `View.filter_expr`: rows where the SQL predicate holds over the typed view (null = false), ANDed with the regex filter; no arg / `zf` / `:unfilter` clears; undoable; rename of a column it reads is refused; exported as `.filter(pl.sql_expr(..))` / `WHERE` |
| Conditional color | `:color <col\|*> <pred> <style>`, `:nocolor [col\|*]` | `View.formats`: each rule's predicate is a `__vdrs_fmt_N` Boolean column of the window plan only (never in save/export/the plan text); cell or row (`*`) style patched over the theme, under cursor and selection; undoable; rename follows a cell rule's column and is refused when a predicate reads it; derived sheets start without rules; `:mksession` writes them; `NO_COLOR` → reverse video (S-27 A) |
| Color scale / data bar | `:scale <col> [low high]`, `:databar <col>` | `View.scales`: a job takes the column's min/max over the view, then the rule lands (undoable); cell bg interpolated in RGB (default `#2c7bb6`→`#d7191c`, text black/white by luminance) or a bar in eighth blocks; values outside the range clamp; nulls unpainted; `:nocolor` clears; `:mksession` writes them (range recomputed on load) (S-27 B) |
| Lua bars | `vd.statusline(fn)`, `vd.titlebar(fn)`, `vd.bar.statusline/titlebar(ctx)` | `plugins::bars`: `ctx` = `vd.sheet()` + mode, rows, selected, jobs, pane, message, keys, last (22, S-54); segments → spans, cached on the Debug text of `ctx` and the built-in bar; error or > 5 ms → built-in bar for the session, reason toasted once; prompts, questions and toasts keep the status line; `vd.api_version` 20 (S-40 D) |
| Lua sidebar pages | `vd.register_page { name, render }`, `<C-g>`, `:sidebar name` | `SidebarPage::Lua(name)` (not `Copy` any more); `next` cycles built-ins then `Plugins::page_names`; `render(ctx)` = the D `ctx` + `cell` → lines of segments, last page cached on its input; an unknown name parses and is refused at dispatch with the page list; `vd.api_version` 21 (S-40 E) |
| Styled renderers | `vd.register_renderer` `render` returns `{ text, fg, bg, bold, italic, underline, reverse }` | `widget::Rendered`; memoized like text; style patched over the theme below `:color`/`:scale`, never on cursor or selected rows; unknown colors dropped; `NO_COLOR` drops colors; `vd.api_version` 10 (S-27 C) |
| Edit cell | `:edit [value]` | cursor cell, same rules (S-18 C) |
| Force quit | `:q!`, `:qa!` / `:quit!` | pop / quit without the unsaved-edits question |
| Uniform | `:uniform` | hide columns with ≤ 1 distinct value |
| Export | `:export x.py\|x.sql` | Polars script / DuckDB SQL replaying plan steps, casts, filter, sort, projection; derived sheets (freq, copy, selected, freq dive, join, sql, pivot, `W`, unpivot, describe, row diff) replay from a parent snapshot (`SheetState.lineage`), one variable/CTE per sheet. Not: schema diff, Lua-agg pivots, stdin |
| Prompts | all prompts | Left/Right/Home/End, `<C-a>` `<C-e>` `<C-u>` `<C-w>`, `<Del>`; ex args accept `"…"`/`'…'`/`\` quoting |
| Background jobs | search, `gs`, `\|`, `,`, `"`, `I`, `W`, `:pivot`, `:save`, `:uniform`, `z\|`, `z/` | run on the blocking pool; status line shows `working: …`; `<C-c>` drops them |
| Readers | — | ISO-only CSV/TSV date inference (others stay text); integer columns with a leading-zero value (`082053`) stay text (S-24 R4); xlsx / sqlite columns typed when every value agrees (numbers, booleans, xlsx dates), else text (S-24 R3); lenient `@` (12 regex-guarded layouts, time suffix ignored); directory = `*.parquet` glob; multi-sheet xlsx / multi-table sqlite open a parts list; `file:part` opens one directly (CLI, `:join`, `:diff`) |
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
| `vd.option(name, value)` | set an option: `clipboard_copy_cmd`, `clipboard_paste_cmd`, `duckdb_cmd`; unknown name = error; api_version 7 |
| first-run `init.lua` | written at TUI start when missing: every default commented out (`plugins::default_init_lua`) |

## 4. Keymap

Entry: `src/keymap.rs`. Model: linear vec of `Chord { on: Vec<Key>, run: Vec<Action>, desc: &'static str }` per `Layer` (`Sheet`, `Input`). Resolution: last declaration wins (so `vd.bind` shadows a default); incomplete prefix → `Dispatch::Pending(vec)` which status bar echoes as the which-key hint.

**Count prefix**: digit-first-keystroke resolves to `App.pending_count` which is then baked into cursor/scroll Actions on next match (`3j` = 3-step `CursorDown`).

**`.` repeat**: `App.last_action: Option<Vec<Action>>` captured on every dispatch of a `repeatable()` action; `.` re-fires it with the same args.

Full default binding table lives in `Keymap::defaults()`; see source.

## 5. Ex-line

Full grammar: `src/exline.rs:parse()`; the complete list is what `:<Tab>` completes and what the guide documents (doc-sync test). The original commands below; later ones (`:join`, `:derive`, `:sql`, `:setcol`, `:export`, `:describe`, `:dup`, `:uniform`, `:undo`/`:redo`, `:N`, `:<action-name>`, `vd.command`s) in §3.10–3.11:

```text
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
:type int|float|string|str|date|datetime|date-only|currency|json
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
- `Origin::Selected { parent }` — `"` dup of selected rows, and freq dives (a dive from a freq-of-freq keeps `Origin::Freq`)
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

**Rows from Lua (api 13, S-39 A)**: `open` may return a `{ columns, rows }` table (the `vd.push_sheet` shape, typed per column) instead of a path. The sheet is in memory (`LoadedSource::via_loader`, no path); sessions reopen the original file through the loader (path loaders too, not the tempfile they wrote); `:export` refuses with a reason. Big or heavy-dep formats belong in a `vdrs-ext-<fmt>` Rust crate per S-09.

**Scope**: CLI file open (`vdrs mylog.foo`) and `:diff` targets. `:tail` refuses loader formats: a refresh would re-scan the one-time converted file and never see new rows.

### 7.3 Added 2026-11-05

```lua
vd.bind(layer, keys, ":sort -id")              -- ex-command bindings (validated at load)
vd.register_aggregator(name, fn(values) -> number|string|nil)   -- used by :pivot … <name>
vd.register_renderer { column = "x" | dtype = "f64", render = fn(v) -> string|nil }  -- memoized per value; sets widths
vd.set_theme("dark" | "light" | { base?, <Theme field> = colour, … })  -- table: api 19
vd.api_version                                  -- 7 (vd.option clipboard_paste_cmd)
vd.bind(layer, keys, function() … end, desc?)  -- runs on the key (api 2)
vd.toast(msg)                                   -- status line; only inside a bound function
vd.sheet() -> { name, cursor = {row, col}, column, value, column_names }  -- 1-based; nil outside a bound function
vd.push_sheet { name?, columns = {…}, rows = { {…}, … } }  -- api 3; inside a bound function; opens after it returns.
                                                -- per column: all ints → i64, numbers → f64, all bools → bool, else str; nil = null.
                                                -- Origin::Lua; not exportable
```

`init.lua` reloads when its mtime changes (polled on the 2 s tick; no `notify` dependency). A failing `init.lua` keeps registrations made before the error and reports it. Input-layer action names: `submit_input`, `cancel_input`, `input_backspace`.

## 8. Theme

`Theme::dark()` / `Theme::light()` in `src/theme.rs` drive the table, title bar, status bar and toasts; `vd.set_theme` picks one, or patches a table of colours over one (`Theme::COLOR_KEYS`, `color_mut`; S-40 G).

Colours are 24-bit RGB when `COLORTERM` is `truecolor`/`24bit`, else each frame maps them to the nearest xterm-256 index; `NO_COLOR` (non-empty) strips colors, keeping the cursor as reverse and the selection as bold (`theme::downgrade`, S-26 A4). Numbers are right-aligned; a sorted column's header shows ▲/▼ in place of its type (with its rank under a multi-column sort); a `│` follows the pinned columns.

The title bar reads the sheet stack as crumbs (`sales.csv › freq(region)`), then what shapes the view (`filter`, `sort` of several keys, `sel`, `hidden`; each only when in use), then the cursor column and row (S-54). The status line's right side shows the last command's keys and name, as VisiData's. A spinner joins it after 150 ms of background work, and rows from a window that's still loading dim.

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

- **Plots** (`plot-column`, histograms): plan [`S-23-layout-sidebar-splits-plots.md`](S-23-layout-sidebar-splits-plots.md) part C.
- **Options**: flat `vd.option(name, value)` (S-18 D6), data-changing subset in S-25 P6.
- **Expression keys** `g=` / `:setexpr`, `:filter-expr` (S-21 B); `=`, `z/`, `z|`, `:select` shipped (S-21 A).
- **Everything else on the roadmap** (S-18 … S-30): `../tasks.md`.
- **Cell/row editing + clipboard** (`delete-row`, `add-row`, `open-new`, `paste-*`, `setcol-clipboard`; copy to the register shipped in S-18 A, `syscopy-*` in S-18 B, `edit-cell`/`setcol-input` in S-18 C) — a must-have, deferred (decision 2026-11-07). Plan: [`S-18-editing-clipboard.md`](S-18-editing-clipboard.md) (phases A–G).
- **Macro extras**: `macro-param-*`, `gm` macros sheet, VisiData `.vdj` macro files (S-19 ships `m` record + `macros.lua`).
- **Rest of S-10** (to be drafted as S-16): `vd.frame`, `Series` methods, `vd.pop_sheet`, `vd.sandbox`, `vd.co`; `since`/`deprecate` registry + `cargo xtask check-api-versions`. Deferred until a Lua user asks (decision 2026-11-05); `vd.toast`, `vd.sheet`, function bindings and `vd.push_sheet` shipped.
- **Export** of `vd.push_sheet` sheets (the data lives only in Lua).
- **Export**: schema diff and Lua-aggregator pivots.
- **DuckDB backend** — replaced by Polars SQL for `:sql` / `:derive` (decision 2026-11-05). `.duckdb` *files* are read and written through the `duckdb` program (S-09, 2026-11-08); the engine stays Polars.

## 11. File map (where to look)

| Concern | File |
|---|---|
| Sheet / View / LazyFrame plan | `src/sheet/mod.rs`; derived sheets `derive.rs`, diff `diff.rs`, tail `tail.rs`, `PlanStep` `step.rs`, undo tree `undo.rs`, tests `tests.rs` |
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
