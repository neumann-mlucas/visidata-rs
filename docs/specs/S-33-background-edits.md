# S-33 — Edits that scan run in the background

Status: **shipped 2026-11-13**. As built (§8) differs from the draft in three places. Supersedes S-24 §6 (R6), which
listed three paths; the S-28 `:s` and S-31 builders added four more.

## 1. Problem

Sort, freq, pivot, search and select-all run as jobs: the status bar shows a spinner, `<C-c>`
drops them, and macro replay waits for them. Seven edit paths still collect the whole view inside
`dispatch`, so on a big scanned file the screen freezes, `<C-c>` does nothing, and the terminal
looks hung.

| Keys | Path | Blocking call |
|---|---|---|
| `a` `ga` `p` `P` | `App::add_rows` | `Sheet::view_index_of`: every sorted row id, to put the cursor on the new row |
| `gzp` `gzP` | `App::paste_cells` | `selected_in_view_order` |
| `:type json` (and `json` in the columns sheet) | `Sheet::json_decode` | parses the whole column to infer the struct type |
| `i` | `Sheet::add_incr_col` | `view_order_rids` |
| `gi` | `Sheet::set_incr` | `selected_in_view_order` |
| `f` | `Sheet::fill_down` | the column's values over the whole view |
| `:s` with a filter on | `App::substitute` | `view_rids` |

## 2. Design

Two ways to do it:

- **A. A `JobDone` variant per path**, as the existing jobs do (`Select`, `Scale`, `Ranged`). Seven new
  variants, each with its own apply arm.
- **B. One scan-then-apply shape.** The job does the read-only part and returns data; the UI thread
  applies the edit. Three result kinds cover all seven paths:

  ```rust
  enum Scanned {
      Rows(Vec<u64>),              // rids in view order: gzp, i, gi, :s
      Patch(Vec<u64>, Series),     // rids and their new values: f
      Dtype(DataType),             // :type json
  }
  JobDone::Edit { revision: u64, scanned: Scanned, then: Then }
  enum Then { PasteCells { column, values }, AddIncr, SetIncr { column },
              Fill { column }, Substitute { column, pattern, replacement, all },
              JsonType { column } }
  ```

**Chosen: B.** One apply path holds the shared rules (revision check, undo snapshot, toast). It
mirrors `JobDone::Ranged`, which already scans for rids and then runs a command. Each `Sheet`
method splits into a pure `scan_*` (`&self`, runs in the job) and an `apply_*` (`&mut self`, fast).

`a` `ga` `p` `P` aren't edits that wait: the rows are added at once (they need no scan). Only the
cursor move waits. As S-24 §6 planned, `add_rows` sets a pending-row marker on `View` (like
`jump_bottom`), and the materialize job looks up the row's index and puts the cursor there when
the window lands.

## 3. Decisions

- **D1 — stale results are refused.** `JobDone::Edit` carries the sheet's `revision()` from when the
  job started. If the sheet changed meanwhile (another edit, undo, sort, filter), the result is
  dropped with `i: the sheet changed while it ran; run it again`. Rids from a scan of another
  view could put values on the wrong rows, so it's never applied "best effort".
- **D2 — keys aren't queued** (kept from S-24 §6, decision 2026-11-10). A key pressed while a job
  runs acts on what's shown, as in VisiData. Pressing `i` twice quickly makes one column and refuses
  the second (D1). Macro replay already waits for `idle()`, so recorded macros are unaffected.
- **D3 — `<C-c>` drops the job; nothing changes.** The edit happens on the UI thread only after the
  scan is done, so there is nothing to roll back.
- **D4 — the undo snapshot is taken at apply time.** One undo step per edit, as today.
- **D5 — small views skip the job.** When the whole view is in memory (the window holds every row,
  or the sheet's row count is under the window size), the scan runs inline. Small sheets, the
  scenario tests and the golden cases keep their current behaviour: no spinner flash.
- **D6 — the rule.** Code in `dispatch` may read the schema and the loaded window, nothing else.
  Anything that reads more goes through `spawn_job` or the materialize path.

## 4. Status line

The job label names the key: `i: numbering rows…`, `f: filling…`, `gzp: pasting…`,
`:type json: reading…`, `:s: replacing…`. The finished toast is unchanged
(`set incr to 1…n`, `filled 12 values`, …).

## 5. Test plan

- **One guard for every action.** A scenario harness on a sheet whose plan sleeps (a `map_batches`
  that sleeps 2 s per collect). For every name in `Action::NAMED` and every ex command with a fixed
  sample argument, `dispatch` must return in under 200 ms. A short, commented list of exemptions
  covers actions that are instant by nature. New actions are covered automatically, so a future
  blocking path fails this test instead of a user's terminal.
- **Per path,** on the sleeping sheet: the key returns at once and `busy` shows the job. After
  `settled()` the edit is there and it's one undo step. `<C-c>` during the job leaves the sheet as
  it was. An edit made during the job makes the result be refused (D1).
- **Unchanged results:** the existing scenario tests and golden cases pass as they are (D5 keeps them
  inline).

## 6. Size

About 2 days:

- Split the seven paths into scan and apply: 1 d.
- `Scanned`/`Then` apply path and the pending-row cursor: 0.5 d.
- The every-action guard test, plus the per-path tests: 0.5 d.

## 7. Out of scope

- Search `n`/`N` rescans per press (already a job; caching hits is a separate speed-up).
- Long plans overflowing the stack (deferred 2026-11-12: merging consecutive column steps).

## 8. As built

- **One closure, no `Scanned`/`Then` enums.** `App::scan_edit(label, scan, apply)`: `scan(&Sheet)`
  runs in the job on a copy of the sheet, and `apply(&mut App, T)` on the UI thread.
  `JobDone::Edit { stamp, apply }` carries the boxed apply. Same shape as B, with one apply arm
  and no enum per path. Each `Sheet` method splits into a read (`view_order_rids`,
  `selected_in_view_order`, `fill_scan`, `json_dtype`, `view_rids`) and a `*_with` / `*_apply`
  write; the old method stays as read + write for inline callers (replay, unit tests).
- **D1 uses a stamp, not `revision()`.** `revision` moves on every materialize, so scrolling would
  refuse results. `Sheet::scan_stamp()` is the count key (edits, undo, filter, casts), the sort, and
  the selection's `Arc` by pointer. The refusal reads
  `<label>: the sheet changed while it ran; not applied` (`a`'s cursor move uses the same path,
  where "run it again" would be wrong).
- **`a` `ga` `p` `P`**: rows are added at once; the cursor move to the new row is the scanned part
  (`cursor to new row`), with no pending-row marker on `View`.
- **D5**: inline when the window holds the whole counted view (`row_count <= window height`).
- **Not moved:** `json` typed in the columns sheet (`columns_write` over several sheets) still scans
  inline.
- **Tests:** `src/app/scenario/jobs.rs`. A 3000-row sheet whose every collect naps 1.5 s. Every
  `Action::NAMED` and every ex command that parses bare or with `id` must return from `dispatch`
  in under 500 ms (no exemptions needed). Plus per-path checks, the stale refusal, and `<C-c>`.
