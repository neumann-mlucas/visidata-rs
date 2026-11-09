# S-18 — Cell/row editing + clipboard

Status: **drafted 2026-11-08**; phase A shipped 2026-11-08 (`App.register`: `Register { kind, df }`; the replay treats `copy-*` as no-ops, which moved 4 files' first blocker to paste/open-new); phase B shipped 2026-11-08 (`src/clipboard.rs`, `vd.option`, Lua API 5); phase C shipped 2026-11-08 (see the C notes in §3); phase D shipped 2026-11-08 (see the D notes). Decision 2026-11-07: a Day-1 must-have, deferred until now.
Depends on: S-01 (sheet/plan model), S-04 (keymap), S-15 (inventory: update on each phase).
Supersedes: the S-18 sketch in `README.md` ("overlay applied as the last plan step"). That sketch
was wrong about placement (see §2.2), and an overlay isn't needed: `PlanStep` already is one.

Direction note: `tests/vd_replay.rs` lists clipboard/add-row/delete-row as untestable because "we're a
non-mutable projection engine". That stance changes here: the file stays read-only, the *sheet* becomes
editable and gets a clipboard. `direction.md` records it (§9.8, 2026-11-08); update the
`vd_replay.rs` comment in phase A, the first phase that crosses it.

## 1. What exists today (verified in `src/sheet.rs`)

- A sheet is `plan` (source scan + ordered `Vec<PlanStep>`) + `View` (types, filter, sort, hidden,
  selection). `PlanStep::apply` is the one place steps hit the plan; `:tail` re-applies them to a
  grown table.
- `PlanStep::SetCol { column, value, dtype, rids }` (`:setcol`) is already a cell edit: one value on
  a set of rows, type-checked up front with `strict_cast`, undoable, exported (`src/export.rs`).
- Row ids: `ROW_ID` (`__vdrs_rid`) = `with_row_index` on the plan, i.e. **position in the source**.
  `SetCol` numbers rows the same way (`__vdrs_set`). Stable only while no step adds/removes rows.
- Undo: per sheet, linear `undo`/`redo: Vec<Snapshot { plan, view, steps }>`, max 100; a new change
  clears redo.
- Keys free in vdrs today: `e d a A y p x Y m`. `P` is the plan panel; it moves to `:plan` in phase F
  (D1) so `P` can be paste-before, as in VisiData.

## 2. Model

### 2.1 Edits are plan steps
New step kinds, each applied by `PlanStep::apply`, so undo, `:save`, `:tail`, `:export` and the plan
panel get them with no new machinery:

| Step | Made by | Effect |
|---|---|---|
| `SetCells { column, dtype, patch }` | `e`, `zp`, `zx`, paste | per-row values; `patch` = rid → value (`Arc<DataFrame>` of `rid, value`) |
| `SetCol` (exists) | `:setcol`, `ge`, `gzp` | one value on many rows |
| `DeleteRows { rids }` | `d gd x gx` | filter out `rids` |
| `AddRows { anchor, rows }` | `a ga p P`, paste rows | insert rows after `anchor` (`Arc<DataFrame>`, own rids; §2.3) |

`patch`/`rows` live behind `Arc` so 100 undo snapshots share them instead of copying (`steps` is cloned
per snapshot). `SetCells` applies as a left join on rid + `coalesce`-style `when(patched)`; one code
path for 1 cell or 50k (no giant `when` chains).

### 2.2 Placement: an edit goes after the last step that writes its column
Not "last": `:derive total = qty * price`, then edit `qty` → `total` must follow (VisiData computes
expression columns live). Rule: insert the edit right after the last step that *writes* `column`
(`Derive name=column`, `Currency`, `Rename new=column`, an earlier edit), or at the front if none.
Later derives that read `column` then see the edit. Editing a derived column itself pins those
cells (the edit sits after its `Derive`). One function, `insert_point(steps, column) -> usize`,
unit-tested on its own.

Consequence: inserting mid-list rebuilds the plan from the source + all steps (already how `:tail`
works); fine, it's lazy.

### 2.3 Stable row ids (phase E, before any row add/delete)
Positional ids break once rows are deleted or added. Phase E makes the rid a real column, added once
right after the source scan and carried through every step; `filtered_plan` and `SetCol` read it
instead of numbering rows. Added rows get ids `1<<62 | n` (`n` = per-sheet counter), so a growing
`:tail` source (positional ids 0..) never collides. `__vdrs_rid` stays hidden and never reaches
`:save` output (dropped in `view_plan` like today).

Row order (D2: insert at the cursor, as VisiData). Without added rows nothing changes: the source
order is the order. Once a sheet has `AddRows`, its plan gets a hidden position key and is sorted by
it before the view's own sort: source rows have key `(rid, 0.0)`; rows added after row `r` get
`(anchor, s)` with `anchor` = `r`'s own anchor and `s` strictly between `r`'s `s` and the next row's in
that group (midpoints, so inserting between inserted rows never renumbers). `p` = after the cursor
row, `P` = before it (after the previous row; at the top: anchor `-1`). With a user sort active the
new rows land where their values sort, as in VisiData. Cost: a full sort of the table on each plan
rebuild, only for sheets that have added rows; `ponytail:` fine in memory, slow on multi-GB scans,
upgrade path = keep added rows in a side table and merge them into the window.

### 2.4 Types
The value typed or pasted must convert to the column **as displayed** (after `#%~@` casts), and is
stored converted to the **base** type (so the view cast still applies). Empty input = null.
Rejected values: nothing changes, status line `"abc" doesn't fit qty (i64)` (same as `:setcol`).
Dates: ISO only in v1 (`2024-01-31`); the lenient `@` layouts are a follow-up.

### 2.5 Which sheets are editable (D3: source sheets only)
Only sheets read from a source take edits: a file opened from the command line or `:open`-like paths,
a `:tail`, a part of an xlsx/sqlite file, and `A` (open-new). Derived sheets (freq, describe, pivot,
join, `:sql`, diff, selected rows, `:dup`, dedupe, the sheet list) refuse every edit key with
`read-only: freq(region) is derived from sales.csv` and change nothing. Copying (`y`, `Y`…) works on
every sheet. One predicate, `Sheet::editable()`, decided from `Origin`; every edit action checks it
first. Children opened after an edit see it (they're built from the parent's view); open children
don't change, as today.

### 2.6 Register (internal clipboard)
`App.register: Option<Register>`, one per app (copy on one sheet, paste on another):
`Register { kind: Rows | Cells, df: DataFrame }`, typed, column names kept. Rows paste by column
name (missing → null, extra → ignored, type mismatch → refused as a whole with the first bad value).
Cells paste by position from the cursor column rightwards.

### 2.7 Export
`SetCells`/`AddRows` export their data literally: Polars `pl.DataFrame({...})` joined/concatenated,
DuckDB `VALUES (...)` lists; `DeleteRows` = `filter(~pl.col(rid).is_in([...]))` / `NOT IN`. Since
exports need the rid, exported scripts add `with_row_index` once at the source, as `:setcol` export
does today. `ponytail:` inlines every edit; a 1M-cell paste makes a big script, add a side file
(`.parquet` next to the script) when someone hits that.

## 3. Phases (implementation order)

Ordered so each phase only builds on shipped ones: copy-only first (no edits), then edits on existing
rows, then row ids, then rows, then the system paste last. Each phase ends with `just check` green
and S-15 + the user guide updated.

| Phase | Ships | Keys / commands | Effort |
|---|---|---|---|
| A | Copy to register | `y` copy-row, `gy` copy-selected, `zy` copy-cell, `gzy` copy-cells | 0.5–1 d |
| B | Copy to system clipboard | `Y` syscopy-row, `gY` syscopy-selected, `zY` syscopy-cell, `gzY` syscopy-cells | 1–1.5 d |
| C | Cell editing | `e` edit-cell, `ge` setcol-input, `:edit` | 3–4 d |
| D | Paste into cells | `zp` paste-cell, `gzp` setcol-clipboard, `zx` cut-cell | 1–2 d |
| E | Stable rids + delete rows | `d` delete-row, `gd` delete-selected, `x` cut-row, `gx` cut-selected | 2–3 d |
| F | Add/paste rows; plan panel → `:plan` | `a` add-row, `ga` add-rows, `p` paste-after, `P` paste-before, `A` open-new | 3–5 d |
| G | System paste in | terminal paste → register; `gzP` syspaste-cells-selected | 1 d |

Total ≈ 12–18 days.

### A — Copy to register
Collect the cursor row / selected rows / cell / column-of-selected from the view (background job for
selections, like `"`), store typed. Status: `copied 1,204 rows`. Copying a wide selection of a huge
file is bounded by memory; refuse above 10M cells with a message (`ponytail:` limit, raise on demand).

### B — System clipboard out
As VisiData's `clipboard.py` (decision 2026-11-08, replaces the arboard plan): pipe text to a copy
command. `Y`/`gY` send visible columns as TSV with a header; `zY` the raw cell text (no newline);
`gzY` the current column of the selected rows, one value per line. Tab/newline inside a value become
U+001F/U+001E, nulls empty (VisiData's `tsv_safe_*`). No filetype prompt (`Y` asks for one in
VisiData; `:export` covers other formats). Command: `vd.option("clipboard_copy_cmd", "…")` (split on
spaces) or `"osc52"`; default `osc52` when `$SSH_TTY`, else `clip.exe` (Windows/WSL), `pbcopy`
(macOS), `wl-copy` (`$WAYLAND_DISPLAY`), `xclip -selection clipboard -filter`. Runs as a background
job; a missing or failing command is a status message naming the option. OSC 52 bytes are written by
the UI thread. Unit tests always get an in-memory sink (`cfg(test)`), never the real clipboard.
`vd.option(name, value)` is the general setter (Lua API v5, S-10); unknown names are init.lua errors.

### C — Cell editing
`e` opens the existing line editor on the cell's display text; Enter → `SetCells` (one cell) via
§2.2/§2.4; Esc cancels. `ge` = `:setcol` with the prompt prefilled. `:edit <value>` for scripts and
`vd.bind`. Undo/redo as any step. The cursor stays on the edited cell; sort/filter re-run (an edited
row can leave the filter; the cursor then behaves as after `:setcol`, to be pinned by a scenario test).

As shipped: `e`/`:edit` reuse `SetCol` with one rid (`value: Option<String>`, `rids: Arc<[u64]>`), so
`SetCells` waits for the first per-row-values paste (D `zp` multi-cell / G `gzP`). `insert_point`
(`src/sheet/step.rs`) + `Sheet::insert_edit` rebuild the plan from the base (tail rows, the in-memory
copy `state.memory_base`, or the source); `:setcol`/`ge` take the same rule. Bool columns take
`true`/`false` (Polars casts no text to bool); Python export parses date/datetime literals (Polars 2
dropped text → date casts). `Sheet::editable()` refuses on derived sheets (`:setcol` included). The
quit guard (S-26 A1) counts edit steps against `state.saved_edits` (set when `:save` finishes):
`q`/`:q`/`gq`/`<C-q>` ask `… y/N` in the status line, one key; `:q!`/`:qa!`/`:quit!` don't. The
replay driver still uses its own `edit-cell`/`setcol-input` path; S-25 P1 moves it onto `edit_cell`.

### D — Paste into cells
`zp`: register cell → cursor cell. `gzp`: register's cells → current column of selected rows
(VisiData `setcol-clipboard`). `zx`: copy cell, then set null. All reuse `SetCells`/`SetCol`.

As shipped: `gzp` follows VisiData's `setValuesTyped(onlySelectedRows, *clipcells)`: every copied
cell, cycled over the selected rows in view order (selected rows the filter hides follow, in row
order); one value is a `SetCol`, more a `SetCells` (patch of rid → typed value, one left join;
exported as a joined literal frame / `VALUES`). Any value that doesn't fit the column refuses the
whole paste. `zp` pastes the register's first cell (VisiData: the last `zy` value; same after `zy`).
`gzx` (cut-cells) ships too. A rows register (`y`) gives a message. Replay: `copy-cell(s)`,
`paste-cell`, `cut-cell(s)`, `setcol-clipboard` translated; `pr2372` still waits on `addcol-new`.

### E — Stable rids + delete
Do §2.3 first, as its own commit with no visible change (all existing tests must pass unchanged — the
proof the rid switch is safe). Then `DeleteRows`. `d` keeps the cursor on the next row; `gd` on the
selection; `x`/`gx` = copy then delete.

### F — Add/paste rows
First, as its own commit: `P` stops toggling the plan panel; `:plan` (from S-23 A, which ships with or
before this phase: `:sidebar plan`, same plan → optimized → off cycle; `:toggle-plan-panel` keeps working) replaces it in the keymap, the guide
(`docs-plan` recording, key tables), S-15 and the `init.lua` template. Then `AddRows` with the
position key of §2.3: `a` one blank row after the cursor, `ga` asks how many, `p`/`P` paste register
rows after/before the cursor row, cursor moves to the first new row. `A` (open-new) = an empty
in-memory sheet with one column `A`, editable; `:save` writes it.

### G — System paste in
Enable bracketed paste. In the table, a paste event parses TSV (CSV if no tab) into the register
(`clipboard: 3 rows × 2 cols`); then `p`/`zp`/`gzP` apply it. In a prompt, a paste inserts the text
(first line only for single-line prompts). Reading the clipboard directly (OSC 52 query) is not
used: terminals block it.

## 4. Test plan

Shared tools: unit tests next to the code; `scenario` (`Harness`, real keymap + jobs); golden cases
(`tests/golden/<case>`, TSV + screen + DuckDB/Polars export round trip); docs recordings
(`docs-*`, replayed on a pty); `monkey` (seeded random keys, undo-all = start); corpus replay
(`tests/vd_replay.rs`, `MIN_PASS`). No test touches the real system clipboard.

### A — Copy to register
- Unit: `Register` from rows/cells keeps dtypes (date, float, null) and column names.
- Scenario: `y` on row 3 → register = that row; `gy` with sel 2 after `[` sort → the selected rows,
  view order; `zy`/`gzy` cells; copy with nothing selected → message, register unchanged.
- Scenario: copy survives `q` / switching sheets.
- Corpus: driver handles `copy-row`/`copy-selected`/`copy-cells` (already SKIP, not UNTESTABLE, since
  S-25 P0); files pass only with paste in D/F; count moves recorded in S-14.

### B — System clipboard out
- Unit: TSV encoding (tabs/newlines inside values, nulls empty, header rules), matched to VisiData's
  `syscopyCells` output (`visidata/clipboard.py`) before writing it.
- Unit: backend choice table (option set, `osc52`, SSH, WSL, Wayland, X11) → expected command.
- Unit: OSC 52 bytes for a known string (exact escape sequence).
- Scenario: `Y`/`gY`/`zY` with the in-memory sink → sink holds the expected text; error path message.
- Manual checklist (not automatable): X11, Wayland, macOS, SSH+OSC52 in kitty/wezterm/iTerm, tmux.

### C — Cell editing
- Unit: `insert_point` — no writer, after `Derive`, after `Rename`, after an earlier edit, derived
  column itself.
- Unit: type rule table — int/float/bool/date/text, displayed cast vs base type, empty → null, refusal.
- Scenario: `e` → type → Enter changes the cell; Esc doesn't; `U` restores, `R` reapplies.
- Scenario: edit `qty` after `:derive total = qty * price` → `total` changes (the §2.2 rule).
- Scenario: edit, then sort/filter → edit follows its row (by rid).
- Scenario: every edit key (`e ge zp gzp zx d gd x gx a ga p P`) on a freq, pivot, join, `:sql` and
  selected-rows sheet → the read-only message, sheet unchanged; `y`/`Y` still copy there.
- Property (proptest or seeded loop): for random edits + sort/filter, the view by rid is the same
  whatever order sort/filter happen.
- Golden: `edit-cell`, `edit-after-derive`, `edit-typed` (export round trip proves `:export` replays
  edits in Polars and DuckDB); `:save` then reopen = same TSV.
- `:tail`: edits survive a growing file (existing tail test + an edit).
- Monkey: add `e` + prompt text to `KEYS`/`PROMPTS`; undo-all invariant holds.
- Docs: guide section "Changing data" gets a `docs-edit` recording.
- Corpus: `edit-cell`/`setcol-input` translated; quarantine entries "Editing / commit workflows" and
  edit-join* re-checked, `MIN_PASS` raised by what passes.

### D — Paste into cells
- Scenario: `zy` then `zp` elsewhere; `gzp` over selection; `zx` = copy + null; type mismatch refused
  whole (no partial paste).
- Golden: `paste-cell`, `setcol-clipboard` with export round trip.
- Corpus: `setcol-clipboard` translated.

### E — Stable rids + delete rows
- Rid switch commit: the whole suite passes unchanged (golden, replay, pty), plus a unit test that
  rids are equal before/after the switch on a sorted+filtered view.
- Unit: added-row id range never meets source ids, including after `:tail` growth.
- Scenario: `d` keeps the cursor on the next row; `gd`; `x`/`gx` fill the register; `U` brings rows
  back in place; selection of deleted rows is dropped.
- Scenario: delete, then `:setcol`/edit on remaining rows hits the right rows (no positional drift).
- Golden: `delete-row`, `delete-selected`, `cut-paste` (with F) + export round trip.
- Corpus: `delete-row`, `delete-selected`; S-14 counts updated.

### F — Add/paste rows
- Plan-key commit: scenario `:plan` cycles plan → optimized → off; `P` no longer opens it; the
  `docs-plan` recording and the guide's key tables regenerate; `default_init_lua` test still passes.
- Unit: position keys — insert after/before source rows, between two inserted rows (repeatedly, no
  renumbering), at the top, at the end; order of the result = expected rid list.
- Scenario: `a` adds a blank row right after the cursor and moves there; `ga 3`; edit the new row; `U`.
- Scenario: insert, then sort by a column, then `zo` (clear sort) → rows back at their inserted place.
- Scenario: `:tail` growth with inserted rows: new source rows append after all existing rows.
- Scenario: `p` pastes copied rows by column name; missing columns null; mismatch refused.
- Scenario: `A` new sheet → `a` → `e` → `:save x.csv` → reopen = typed rows.
- Golden + export round trip: `add-row`, `paste-rows`, `open-new` (the `AddRows` literal data).
- Monkey: add `d a p x` keys.
- Corpus: `add-row`, `add-rows`, `paste-after`, `open-new` (`issue3195` re-checked).

### G — System paste in
- Unit: TSV/CSV text → register (ragged rows, quotes, trailing newline, one value).
- Scenario: a synthetic `Event::Paste` in the table fills the register; in a prompt inserts text.
- pty: bracketed-paste bytes (`ESC[200~ … ESC[201~`) into the real binary → register message on screen.
- Manual: paste from a spreadsheet into vdrs in 2–3 terminals.

## 5. Decisions (confirmed 2026-11-08)

- **D1 paste-before key:** `P` = paste-before, as VisiData. The plan panel loses its key: `:plan`
  (plus the existing `:toggle-plan-panel`), bindable. Done at the start of phase F.
- **D2 where added rows go:** at the cursor (§2.3 position key), not appended.
- **D3 which sheets take edits:** source sheets only (§2.5); derived sheets are read-only.
- **D4 export size:** edits are written into exported scripts as literal data.
- **D5 derived cells:** editing a cell of a `:derive`d column pins that value (§2.2).
- **D6 settings:** `vd.option(name, value)` in init.lua (Lua API v5), first used by `clipboard_copy_cmd`.
- **D7 clipboard backend:** VisiData's approach, a copy command (2026-11-08), plus OSC 52 over SSH; no `arboard`.
