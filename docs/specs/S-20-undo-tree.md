# S-20 — Undo tree

Status: **shipped 2026-11-09** (see §4). Independent of S-18 (works for every undoable action).

Idea (2026-11-08): ship the tree view as a Lua plugin, an example of extending vdrs with Lua: needs a
read-only `vd.undo_tree()` (nodes: id, parent, label, time) and a way to jump (`vd.exec(":undo-to N")`,
S-29) — plus `vd.push_sheet` to show it.

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

## 4. As shipped (2026-11-09)

`src/sheet/undo.rs`: `UndoTree { nodes: BTreeMap<id, Node>, current }`; ids count up, so id order
is creation order (`:earlier`/`:later` walk ids). The current node holds no snapshot (the sheet is
the state); leaving it stores one. `goto_state(id)` sets each ancestor's redo child along the
target's path, so `R` retraces any jump. Eviction at 100 states: the oldest leaf off the current
path, else (one line of states) the root. Labels are derived, not recorded: the first differing
step's `PlanStep::summary`, else the view change (sort, filter, hide, keys, type, selection,
freeze). `:undotree` is a built-in meta-sheet (`Origin::UndoTree`), not the Lua-plugin idea: that
needs `vd.exec` (S-29) to jump, so `vd.undo_tree()` waits for it. Not done: `begin_group`/
`end_group` (S-19 macros will add them), vim's time arguments (`:earlier 10m`). S-18 G's region
paste now makes its one undo step without truncating history (`insert_rows` = `add_rows` minus the
undo step).
