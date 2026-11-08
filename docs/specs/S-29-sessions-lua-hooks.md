# S-29 — Sessions and Lua event hooks

Status: **drafted 2026-11-08**, not started.
Depends on: S-19 (`Action::to_ex`), S-10 (Lua API versioning), S-15 §6 (lineage), S-18 (edits as steps).
Direction §9.5 holds: no `.vdj` compat; this is vdrs's own format.

## 1. Sessions
**What is saved**: every sheet on the stack, in order, with its source path, plan steps, view (sort,
filter, hidden, keys, casts, formats S-27, cursor, frozen cols), and — for derived sheets — how it was
derived from which parent (`SheetState.lineage`). Not saved: windows, undo history, selections over
100k rows (saved as a count + warning instead).

**Format: an ex script.** A session is a `.vdrs` text file of ex commands, one sheet per block:
```
:open data/sales.csv
:type amount float
:filter region ^n
:sort -amount
:hide notes
:cursor 12 amount
:freq region          # derived: runs on the sheet above
```
- Reuses S-19's `Action::to_ex` (every action with a payload has an ex form) and `:export`'s lineage
  walk (one block per sheet, parents first). Readable, diffable, hand-editable, shareable.
- Edits (S-18) serialize as their ex form (`:edit`, `:setcol`); `SetCells` patches over 1k cells are
  written to a sidecar `<session>.d/<sheet>.parquet` and loaded with `:patch <file>` (`ponytail:`
  threshold).
- Missing source on load: that block and its derived blocks are skipped with one toast each; the rest
  loads.
- Commands: `:mksession [path]` (default `$XDG_STATE_HOME/vdrs/session.vdrs`), `:source <path>` runs
  any ex script (session or not). Auto-save on quit: `vd.option("session_autosave", false)`.
- CLI: `vdrs -S <file>` — **new flag; discuss before adding** (decision H1).

## 2. Lua hooks
`vd.on(event, fn)` registers a handler; several per event, run in registration order; an error in a
handler is a toast, never aborts the event.

| Event | Fires | Arg |
|---|---|---|
| `open` | a sheet read from a source is pushed (CLI, `:open`, parts) | `vd.sheet()`-shaped table + `path`, `format` |
| `push` | any sheet is pushed (incl. derived) | same + `origin` (`"freq"`, `"join"`, …) |
| `save` | after `:save` writes | `path`, `rows` |
| `quit` | before exit | — |
| `tick` | the 2 s tick (rate-limited) | — |

Inside a handler: `vd.exec(":sort -date")` (new: run any ex command on the sheet the event is about),
`vd.toast`, `vd.sheet`. Typical use:
```lua
vd.on("open", function(s)
  if s.path:match("%.log%.csv$") then vd.exec(":type ts date"); vd.exec(":sort -ts") end
end)
```
- `vd.exec` inside handlers is dispatched after the handler returns (same rule as `vd.push_sheet`), so
  hooks can't re-enter themselves mid-dispatch; an `open` handler running `:open` is refused (no
  loops).
- Hot reload (`init.lua` mtime) clears and re-registers handlers.
- API: `vd.api_version` bump; `since` once S-16's registry exists.

## 3. Phases
| Phase | Ships | Effort |
|---|---|---|
| A | `vd.on` + `vd.exec`, events `open` `push` `save` `quit` | 1–1.5 d |
| B | `:source` (run an ex script) | 0.5 d |
| C | `:mksession` writer (needs S-19 `to_ex`), sidecar patches, load = `:source` | 2–3 d |
| D | autosave option; `-S` flag if H1 approves | 0.5 d |

## 4. Test plan
- Unit: session writer → `:source` → same sheets: compare names, steps, view, collected TSV per sheet
  (round trip over the golden cases' final states — every `tests/golden/*` case becomes a session test).
- Unit: missing source skips its block and children only.
- Scenario: `vd.on("open")` handler runs `:sort`; an erroring handler toasts and the sheet still opens;
  `:open` from an `open` handler refused.
- pty: `:mksession` then restart with `:source` (or `-S`) → same screen.
- Session files only under the test's temp `XDG_STATE_HOME`.

## 5. Decisions (open)
- **H1** `vdrs -S <file>` CLI flag vs. `vdrs` + `:source`. Recommend the flag (vim parity), but it is a
  public interface — confirm first.
- **H2** session format: ex script (recommend) vs. Lua table (more structure, less readable).
