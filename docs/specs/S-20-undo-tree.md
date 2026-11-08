# S-20 — Undo tree

Status: **drafted 2026-11-08**, optional, not started. Independent of S-18 (works for every undoable
action); worth doing before or with S-18 C so edits never lose a branch.

## 1. Today
Per sheet `undo`/`redo: Vec<Snapshot>` (max 100); a change after `U` clears `redo`, so that branch is
gone. Snapshots are immutable (`plan`, `view`, `steps`), which makes a tree cheap.

## 2. Design
- Replace the two lists with `UndoTree { nodes: Vec<Node>, current }`,
  `Node { snapshot, parent, children, seq, made_at }`. A change after `U` adds a sibling branch.
- `U` → parent; `R` → the most recently visited child (vim). `:earlier [n]` / `:later [n]` walk all
  states in the order they were made (vim `g-`/`g+`; `g-` is unhide-all in vdrs, so no default keys,
  bindable via `vd.bind`).
- `:undotree` opens a sheet: one row per state (`seq`, time, step summary, `*` current); `Enter`
  jumps there.
- Limit: 100 nodes per sheet; drop the oldest leaf branch first, never the current path.
- Grouping (for S-19 macros): `begin_group`/`end_group` make one node of several changes.

## 3. Test plan
- Unit: tree ops — branch on change after undo, `U`/`R`/`:earlier`/`:later` sequences against a
  hand-written expected path; eviction keeps the current path.
- Property (seeded loop): random actions + undo/redo/earlier/later; every state recorded at creation
  is restored exactly when revisited (compare view + `steps` + collected window).
- Monkey: add `U R` heavy runs; undo-all = start still holds.
- Scenario: `:undotree` lists states, `Enter` restores one; works across S-18 edits.
- Golden: `undo-branch` (edit, undo, other edit, `:earlier` back to the first).

Effort: 1–2 days.
