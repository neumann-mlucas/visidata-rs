# S-30 — Live derived sheets, cell notes, selection stats

Status: **C done 2026-11-10**; A, B not started. Three independent Excel-gap items.
Depends on: S-15 §6 (lineage), S-18 (row ids, D3), S-23 A (sidebar cell page), S-26 C (visual ranges).

## 1. Part A — Refresh and live derived sheets (2–3 d)

Today a derived sheet (freq, pivot, join, `:sql`, describe, …) is built once from the parent's
`view_plan()`; later parent changes don't reach it (S-18 §2.5 "open children don't change"). Every
derived sheet already stores how it was made (`SheetState.lineage = { parent snapshot, Derivation }`),
which `:export` replays.

- `:refresh` (`<C-r>`, VisiData reload-sheet; K1) rebuilds the current derived
  sheet: same `Derivation`, the parent's **current** view, keeps this sheet's own view state (sort,
  hidden, formats) where columns still exist. Undoable (snapshot before).
- `:link` / `:unlink`: a linked sheet refreshes itself when its parent's revision changes (checked when
  the sheet becomes active, not continuously — no work for sheets you're not looking at). Title shows
  `⟳` when linked; `⟳!` when the parent changed and the sheet hasn't caught up yet.
- Parent closed (`q`): refresh uses the stored snapshot (today's behavior), with a toast.
- Chains (freq of a join of a filtered sheet): refresh walks the lineage, parents first.
- Lua-aggregator pivots and `vd.push_sheet` sheets: refresh refused (no replayable derivation).
- Tests: scenario — `F` on region, filter the parent, `:refresh` → counts change; linked sheet updates
  on activation; parent popped → snapshot refresh + toast; golden `refresh-freq`.

## 2. Part B — Cell notes (2 d)

Annotations on cells; source files stay read-only.

- `:note <text>` (and a key, decision K2) on the cursor cell; empty text deletes; `:notes` pushes a sheet
  of all notes (`row keys, column, note`), `Enter` jumps to the cell.
- Display: a marker in the cell's last character cell (theme color); the sidebar cell page (S-23 A)
  shows the note.
- Identity: notes key on the **key columns' values** when keys are set (stable across file changes),
  else on the S-18 E row id plus the source's size+mtime stamp; on a stamp mismatch notes still show,
  flagged `stale?` in `:notes`.
- Storage: `$XDG_STATE_HOME/vdrs/notes/<sha256 of canonical source path>.tsv` (`keys/rid, column,
  note, created`); written on change, atomically (temp + rename). Never next to the data file (could be
  read-only or shared).
- Derived sheets: notes view only; a note made on a freq row keys on the freq value (its key column).
- Export/save: not included (data only); sessions (S-29) reference the notes file by path.
- Tests: unit — key-based and rid-based identity, stale stamp; scenario — note, quit, reopen → note
  shown; `:notes` jump; atomic write leaves no partial file on a simulated failure.

## 3. Part C — Selection stats in the status line (1 d)

Excel's status bar (sum / average / count of the selected cells).

- When rows are selected (or a S-26 visual range is active) and the cursor column is numeric, the
  status line shows `sel 1,204 · sum 88,410.5 · avg 73.4 · min 0 · max 990`; non-numeric: `count`,
  `distinct`.
- Computed in a background job over `view_plan()` filtered to the selected rids, debounced 150 ms
  after the last selection/cursor-column change; cached by (selection revision, column).
- Visual block (`<C-v>`, several columns): sum over all numeric cells in the block.
- `:stats` copies them as TSV (decision K3; no key — `y` is copy-row).
- Not a "sum of a range" *cell* (that is `:derive x = sum(col) OVER ()`, already works — Polars SQL
  window functions verified 2026-11-08 via `sql_expr`; note `:derive` runs on the base plan, so
  `OVER ()` spans all source rows, not the filtered view).
- Tests: scenario — select 3 rows → exact status text; switch column → recomputes; 1M-row selection
  stays a background job (spinner, no UI stall).

## 4. Decisions (confirmed 2026-11-08)

- **K1** `<C-r>` = `:refresh` (VisiData `reload-sheet`, S-26 §0), asking first when the sheet has
  unsaved edits; redo stays `R` (and `<C-r>` in the vim profile).
- **K2** notes on `zn` (unbound in both VisiData and vdrs, checked 2026-11-08) + `:note`. VisiData has
  no notes (`Alt+m` is memo-cell = store a named value).
- **K3** `:stats` copies the stats; no key.
