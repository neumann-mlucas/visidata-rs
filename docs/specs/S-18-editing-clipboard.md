# S-18 — Cell/row editing + clipboard

Status: **drafted 2026-11-08**, not started. Decision 2026-11-07: a Day-1 must-have, deferred until now.
Depends on: S-01 (sheet/plan model), S-04 (keymap), S-15 (inventory: update on each phase).
Supersedes: the S-18 sketch in `README.md` ("overlay applied as the last plan step"). That sketch
was wrong about placement (see §2.2), and an overlay isn't needed: `PlanStep` already is one.

Direction note: `tests/vd_replay.rs` lists clipboard/add-row/delete-row as untestable because "we're a
non-mutable projection engine". That stance changes here: the file stays read-only, the *sheet* becomes
editable and gets a clipboard. Update that comment and `direction.md` in phase A, the first phase
that crosses it.

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
- Keys free in vdrs today: `e d a A y p x Y m`. `P` is taken (plan panel).

## 2. Model

### 2.1 Edits are plan steps
New step kinds, each applied by `PlanStep::apply`, so undo, `:save`, `:tail`, `:export` and the plan
panel get them with no new machinery:

| Step | Made by | Effect |
|---|---|---|
| `SetCells { column, dtype, patch }` | `e`, `zp`, `zx`, paste | per-row values; `patch` = rid → value (`Arc<DataFrame>` of `rid, value`) |
| `SetCol` (exists) | `:setcol`, `ge`, `gzp` | one value on many rows |
| `DeleteRows { rids }` | `d gd x gx` | filter out `rids` |
| `AddRows { rows }` | `a ga p`, paste-before (§5 D1) | append rows (`Arc<DataFrame>`, own rids) |

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

### 2.4 Types
The value typed or pasted must convert to the column **as displayed** (after `#%~@` casts), and is
stored converted to the **base** type (so the view cast still applies). Empty input = null.
Rejected values: nothing changes, status line `"abc" doesn't fit qty (i64)` (same as `:setcol`).
Dates: ISO only in v1 (`2024-01-31`); the lenient `@` layouts are a follow-up.

### 2.5 Which sheets are editable
Every sheet, and edits stay on the sheet where they're made: a child sheet (freq, pivot, join,
selected rows…) is built from its parent's view when opened; editing it never writes back, editing
the parent later doesn't change an open child. Matches how vdrs derives sheets today; VisiData's
write-through to shared row objects is out of scope. Exceptions: the sheet list (`S`) and the
columns/describe sheets refuse edits with a message (they describe, they don't hold data).

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
| F | Add/paste rows | `a` add-row, `ga` add-rows, `p` paste-after, paste-before (§5 D1), `A` open-new | 2–3 d |
| G | System paste in | terminal paste → register; `gzP` syspaste-cells-selected | 1 d |

Total ≈ 11–16 days.

### A — Copy to register
Collect the cursor row / selected rows / cell / column-of-selected from the view (background job for
selections, like `"`), store typed. Status: `copied 1,204 rows`. Copying a wide selection of a huge
file is bounded by memory; refuse above 10M cells with a message (`ponytail:` limit, raise on demand).

### B — System clipboard out
TSV text of the same data (header line for rows, none for cells), as VisiData. Backend picked at
startup: `arboard` when a display server answers, else OSC 52 (`ESC ] 52 ; c ; base64 BEL`) through the
terminal when `$SSH_TTY`/`$TMUX`, else an error message. Behind a `ClipboardSink` trait so tests use an
in-memory sink (no real side effects). Config: `vd.option("clipboard", "auto"|"osc52"|"system"|"off")`
**[needs the small option API; else a Lua global, decide in phase]**.

### C — Cell editing
`e` opens the existing line editor on the cell's display text; Enter → `SetCells` (one cell) via
§2.2/§2.4; Esc cancels. `ge` = `:setcol` with the prompt prefilled. `:edit <value>` for scripts and
`vd.bind`. Undo/redo as any step. The cursor stays on the edited cell; sort/filter re-run (an edited
row can leave the filter; the cursor then behaves as after `:setcol`, to be pinned by a scenario test).

### D — Paste into cells
`zp`: register cell → cursor cell. `gzp`: register's first value → current column of selected rows
(VisiData `setcol-clipboard`). `zx`: copy cell, then set null. All reuse `SetCells`/`SetCol`.

### E — Stable rids + delete
Do §2.3 first, as its own commit with no visible change (all existing tests must pass unchanged — the
proof the rid switch is safe). Then `DeleteRows`. `d` keeps the cursor on the next row; `gd` on the
selection; `x`/`gx` = copy then delete.

### F — Add/paste rows
`AddRows` appends: v1 adds rows **at the end of the source order** and moves the cursor to them
(§5 D2). `a` one blank row, `ga` asks how many, `p` pastes register rows. `A` (open-new) = an empty
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
- Corpus: driver handles `copy-row`/`copy-selected`/`copy-cells`; remove them from the untestable list
  (they become SKIP-free only with paste in D/F; count moves recorded in S-14).

### B — System clipboard out
- Unit: TSV encoding (tabs/newlines inside values, nulls empty, header rules), matched to VisiData's
  `syscopyCells` output (`visidata/clipboard.py`) before writing it.
- Unit: backend choice table (`auto` with/without display, SSH, tmux, `off`) → expected backend.
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
- Scenario: `a` adds a blank row at the end and moves there; `ga 3`; edit the new row; `U`.
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

## 5. Decisions to confirm before phase C

- **D1 paste-before key.** VisiData's `P` is vdrs's plan panel (shipped, documented). Recommend: `P`
  stays; paste-before has no default key, `:paste-before` + bindable.
- **D2 where added rows go.** Positional insert (VisiData) needs a sort over the whole table on every
  plan rebuild. Recommend v1: append at the end; positional insert later, when the table is in memory.
- **D3 editing child sheets.** Recommend §2.5 (allowed, local, no write-back).
- **D4 export size.** Recommend inline literals (§2.7) until someone hits a big paste.
