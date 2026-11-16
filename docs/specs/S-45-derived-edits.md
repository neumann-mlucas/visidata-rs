# S-45 — Edits on derived sheets

Status: **shipped 2026-11-16**.
Depends on: S-18 (editing), S-30 A (`:refresh`).

## 1. What

S-18 D3 made every derived sheet read-only. VisiData edits them: rows it made itself (pivot,
melt, unfurl) take the edit; join rows share their source rows, so a join edit is a source edit.

| Part | vdrs |
|---|---|
| A | `Sheet::editable` admits pivot, unpivot, unfurl, `:sql`, join and Lua sheets. A pivot takes edits in its key columns only; its counts are "cannot edit readonly column" (VisiData's; `set_rows` reads the keys from the lineage). The first edit (`make_editable`) collects the plan into `memory_base`, clears the steps (they're in the copy) and rebuilds with [`ROW_ID`]: from then on edits work as on a source sheet. Undo past it restores the derived plan (no `ROW_ID`), and the next edit copies again. `:refresh` re-derives and drops the copy's edits. `:export` refuses an edited derived sheet. The quit guard counts its edits. |
| B | A join's first edit also re-runs the join from its lineage snapshots with each side's row ids (`join_plan(…, rids: true)`), kept as `JoinSources`. `e` (`edit_cell`) sets the cell on every join row showing the same source row (both sides' rows for a key), and the app (and replay) edits that source row on the open left/right sheet (`source_cells`); each is an undo step on its own sheet. |

Freq, selected-row (`"`) and copy (`g"`) sheets stay read-only: VisiData edits their source rows,
and vdrs doesn't map those back.

## 2. As built

Replay 89 → 94: pr2647, edit-joinkey-1/2, edit-joinregular-1/2. The replay builds a two-sheet
join on same-named keys with `Sheet::join_from` (its N-way join has no lineage), and
`reload-sheet` on a derived sheet is `:refresh`. unfurl-empty is quarantined: it sets text into
an int column (VisiData's columns aren't typed).

## 3. Not done

- Write-back for `ge`, `:setcol`, paste and delete on a join: they stay on the join.
- Append and concat joins, and a join's right side opened from a path (not an open sheet): no write-back.
- Freq/selected/copy edits writing to the parent rows.
- issue3146-paste: copy and paste of columns-sheet rows (a new column).
