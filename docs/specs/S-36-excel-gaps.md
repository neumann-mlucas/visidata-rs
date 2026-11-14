# S-36 — Excel gaps: number formats, macros sheet, totals footer, lookups, validation, named views

Status: **drafted 2026-11-14** (rough; refine before each part starts). Decisions K1, K2, K4, K5
agreed 2026-11-14; K3 agreed 2026-11-14 (option b).
Depends on: S-35 C2 (`fmtstr`), S-32 (precision), S-19 (macros), S-25 P4 (aggregators), S-30 A
(`:link`) and C (selection stats), S-18 (edits, `SetCells` patches), S-27 (formats), S-29 (sessions).

Scope: Excel features that fit a columnar, lazily loaded sheet. Each one works per column or per
sheet, never per cell address. Out of scope: A1 cell formulas and references, merged cells,
freeform layouts, goal seek, manual formatting of single cells (§8).

Order: B, E, C, A, D, F (cheapest and most used first).

## 0. Live recalculation (already shipped, nothing to build)

Checked 2026-11-14:

- **In one sheet:** `Sheet::insert_edit` (`src/sheet/step.rs`) puts an edit right after the
  column's last writer, and after the last writer of each column an expression edit reads. A
  `:derive` that reads the column comes later, so it sees the edit. `rebuild_plan` replays the
  steps. An edit to a derived cell goes after its `Derive` and pins that cell (S-18 §2.2).
  Covered by scenario `edit_goes_before_derives_that_read_it_and_follows_its_row`.
- **Across sheets:** derived sheets are snapshots. `:refresh` / `<C-r>` rebuilds one, and
  `:link` refreshes it when it becomes active (S-30 A).
- The only new cross-sheet edge in this spec is A (lookups), which is live the way `:link` is (A4).
- Doc task: the guide should say "derived columns follow edits" in one place, next to `:derive`.

## A. Lookup columns: XLOOKUP (2.5–3 d)

Today `:join <sheet> <keys> extend` gives a new sheet. Excel users want the looked-up column on
the sheet they're on.

- A1. **Command:**
  - `:lookup <sheet|file> <key>[=<theirkey>] <col>[,<col>…]`.
  - Each `<col>` adds a column with that name, right after the cursor (S-35 A1). A name already
    in use is refused; `col as name` renames it.
  - `<sheet>` resolves as `:join` resolves it (sheet name, `s<id>`, or a file it opens).
  - No key in v1. VisiData has no lookup command; check before binding anything.
- A2. **Semantics:**
  - Left lookup: every row stays and the row count never changes.
  - A key with no match gives null.
  - A key that appears more than once on their side: the first match in their view order
    (XLOOKUP). A toast counts the duplicate keys.
  - Their side is read from their `view_plan()`, so their filters and casts apply.
- A3. **Plan step:** `PlanStep::Lookup { key, their_key, cols: Vec<(String, String)>, source:
  SheetId, rev: u64, table: Arc<DataFrame> }`.
  - `table` is a cache of their side: the deduplicated key column plus the value columns,
    collected in a job from their `view_plan()` at revision `rev`. The `Sheet` never reads
    another sheet itself; the app refreshes the cache (A4), so `rebuild_plan` stays local and
    synchronous.
  - Applied as `replace_strict(key → value)` per column, the same way `SetCells` avoids joins.
    The `SetCells` comment records that a join per column nests the plan until collecting it
    overflows the stack.
  - It reads `key`, so `insert_edit` ordering puts later edits of the key before it.
  - Undoable. Running `:lookup` again with the same columns replaces the step (as
    `Derive.replaces` does).
- A4. **Live (K1):**
  - Same model as `:link` (S-30 A): when the sheet becomes active and their revision isn't `rev`,
    a job recollects `table` and the plan rebuilds. Sheets you're not looking at do no work.
  - Title shows `⟳!` while a lookup is behind, as linked sheets do.
  - Their sheet closed: the last `table` stays, with a toast. `:unlink` on the lookup (or their
    sheet gone at session restore) freezes it as a snapshot.
  - Cycles: A looks up B and B looks up A (directly or through a chain) is refused when the
    `:lookup` is made, by walking the lookup edges.
  - Their sheet is derived and itself linked: refresh parents first, as S-30 A chains do.
  - Sessions: restore their sheet before this one and refer to it by its session index; also
    write `table` as a sidecar (as `SetCells` patches, `session.rs`) so a missing source still
    restores the values.
  - Export: their side exported as its own frame (as `Derivation::Join` exports the right sheet),
    then deduplicated and left-joined. A snapshot (unlinked) lookup exports the inline table, as
    `SetCells` does; large tables make large scripts (known ceiling).
- A5. **Keys:** one key column in v1. Several keys (`a,b=x,y`) go through a struct key; added when
  someone asks.
- Tests:
  - unit: dedup keeps the first match; no match gives null; type comes from their side.
  - scenario: `:lookup` from a second sheet, then edit the key cell, and the looked-up value
    follows the new key. Edit a value on their sheet, switch back: the column shows the new
    value. A cycle is refused. Their sheet closed: values stay, toast.
  - golden: `lookup` with export round trips; session restore.

## B. Number formats: thousands separators, %, currency text, freq groups (1.5–2 d)

Today `fmtstr` on a float takes only `%.Nf` / `{:.Nf}` (routed to S-32 precision). Ints are
refused (`Sheet::set_fmtstr`, `src/sheet/builders.rs`).

- B1. **Syntax:** VisiData's (`numericFormatter`, `visidata/_types.py`), so replayed `fmtstr`s
  match:
  - C style `%…`: `%.2f`, `%d`, `%e`, `%x`. Python's `locale.format_string`, grouping off.
  - Python style `{…}` with literal text around it: `{:,.2f}`, `{:_d}`, `{:.1%}`, `${:,.2f}`,
    `{:.2f} kg`.
  - The supported subset: optional `,` or `_` grouping, `.N`, type `f` `d` `e` `%`, one `{}` field.
    Anything else (fill/align/width, `!r`, two fields) is refused when set, as dates already are.
- B2. **Where it lives:**
  - `View.num_fmts: Vec<(String, NumFmt)>` beside `date_fmts`. A plain `%.Nf` / `{:.Nf}` still
    goes to `View.precision`, so `Alt++` / `Alt+-` keep working on it.
  - `Alt++` / `Alt+-` on a rich format change its `.N` (K2): `{:,.2f}` → `{:,.3f}`. Same keys
    and longnames (`setcol-precision-more` / `-less`). VisiData fails there ("could not parse
    column fmtstr": `FLOAT_FORMAT_RE` must match the whole fmtstr, `change_precision.py`), so no
    replay case depends on the failure. vdrs never infers a rich format; the user sets it.
  - Out of scope here: VisiData's `Alt++` on a date column cycles `%Y` → `%Y-%m` → … (same file).
    vdrs doesn't do that yet; a follow-up.
  - Ints accept `d`-type formats and `%`.
  - Set through the same paths as today: the columns sheet's `fmtstr` column, `:fmtstr`, sessions.
- B3. **Rendering:**
  - `widget::cell_fmt` gains `CellFmt::Number(&NumFmt)`.
  - A small formatter in Rust (no new dependency), with grouping and rounding as Python does:
    half-to-even, matching `format()`. Unit test table against Python's output.
  - Widths include the formatted text (`refresh_widths`).
- B4. **Freq (K3: option b, agreed 2026-11-14):**
  - VisiData groups `F` by the formatted text of each value: `groupRows` keys on
    `c.format(v)` (`visidata/pivot.py`), for every type, with the column's fmtstr or the type's
    default (`disp_float_fmt = {:.2f}`).
  - vdrs groups floats by the raw value today (`freq_from` formats only dates).
  - So VisiData counts 1.001 and 1.004 as one group "1.00" even with no fmtstr, and a
    `{:,.0f}` format turns `F` into rounding buckets. vdrs counts them apart.
  - Options: (a) follow VisiData for every number, including the default precision; (b) group
    by text only when the user set a fmtstr; (c) always raw.
  - (a) matches replay but changes current counts; (b) is the least surprising change; (c)
    keeps today's behavior and diverges from VisiData.
  - **Chosen: (b).** A per-column format the user set groups `F` by its text, for any type: a
    rich `fmtstr` (B1), and a `View.precision` entry set by `Alt++` / `Alt+-`, `:precision` or a
    `%.Nf` fmtstr, since VisiData's `Alt++` sets the column's fmtstr too. The global
    `disp_float_fmt` doesn't count, so a column nobody formatted groups by the raw value.
  - The dive matches rows by the same expression, as the date dive does (`Sheet::rows_formatted`).
    The export groups by the same text.
  - Effect on tests, checked 2026-11-14: no golden case runs `F` on a number with a format
    (`precision` and `docs-wf-fill` set precision but don't `F`), so none changes. No replay case
    fails today on float grouping: `issue3219-freq` fails on mixed `1` / `1.0` JSON values, and
    `freq-fmtstr` is skipped on a row lookup (`cannot locate row 'Date'`). Neither flips from
    this. New golden: `freq-fmtstr-number`.
- B5. **Export, `:save`, copies:** raw values, as with dates. Display only.
- B6. **Not covered:** negatives in parentheses, colours on negatives, Excel's `#,##0.00` codes. A
  Lua renderer (S-27 C) or `:color col "col < 0" red` covers these.
- Tests:
  - unit: formatter table (`{:,.2f}` on 1234567.891, -0.005, NaN, null; `{:.1%}`; `%d` on an int;
    refusals).
  - scenario: `:fmtstr {:,.0f}`, then the cell text, then `Alt++` gives `{:,.1f}`.
  - golden: `fmtstr-number`.

## C. Totals footer (1.5 d)

Excel puts a totals row under the table. vdrs has `+` aggregators per column (`View.aggregators`)
and selection stats in the status line (S-30 C). Neither gives a total per column on screen.

- C1. **Toggle:** `:footer`, plus option `disp_footer` (default off). No key: VisiData has none.
- C2. **Contents:** one row pinned above the status line.
  - Each visible column shows its `+` aggregators over the filtered view (`view_plan()`): `sum
    88,410.5`, or `sum 88k · avg 73` for several, cut to the width.
  - A column with no aggregator is blank.
  - The key-column area says `Σ` and the row count.
- C3. **Computation:**
  - One background job per change: plan revision, filter, aggregators.
  - It aggregates the whole view with no grouping, as selection stats do over the selection.
    Reuse `freq_aggregators` (`src/sheet/columns.rs`) so Lua aggregators run off the UI thread
    as they do in freq.
  - A spinner while it runs. `:tail` growth recomputes, debounced.
- C4. **Formats:** footer values use the column's `fmtstr` / precision (B).
- C5. **Not covered:** subtotals per group. That's `F` with aggregators, or `W`.
- Tests:
  - scenario: `+` sum on a column, then `:footer`, then the footer text. Filter, and the total
    changes. An edit to a cell updates it too.
  - Job test: a 1M-row view doesn't stall the UI.

## D. Validation on edit (1.5–2 d)

- D1. **Command:**
  - `:validate <col> <sql predicate>`, e.g. `:validate qty qty >= 0` or
    `:validate status status IN ('open','done')`.
  - `:validate <col>` with no predicate clears it; `:validations` lists them.
  - Stored in `View.validations`; undoable; written by sessions. Rename and drop follow S-27's
    rules for formats.
- D2. **Checked on:** every edit that writes the column: `e`, `ge`, `:setcol`, `g=`/`:setexpr`,
  `zp`/`gzp`, `:s`, `a`/`p` (pasted rows).
  - The check runs on the edited rows only. It is the plan with the new step, filtered to those
    rids, then `NOT pred`.
  - It runs inline for a few rows and in the S-33 edit job for scans.
- D3. **On a failed check (K4):**
  - Default: the whole edit is refused with a toast naming the first failing row.
  - `vd.option("validation", "warn")`: the edit lands, a toast counts the failing cells, and
    they get the S-27 error style.
- D4. **Existing data:** not checked. `:select NOT (<pred>)` already finds bad rows; `:validate`
  prints that command in its toast.
- D5. **Allowed values:** later, if wanted. `e` completes values from an `IN (…)` list with `Tab`.
- Tests:
  - unit: predicate check on a step.
  - scenario: `:validate qty qty >= 0`, then `e` with `-1` is refused (toast, cell unchanged). `ge`
    on 3 selected rows with one bad is refused whole. Warn mode lets the edit through and styles
    the cell.

## E. Macros sheet `gm` (1.5–2 d)

VisiData `gm` (`macro-sheet`, `visidata/macros.py`): an index of macros.

- E1. **Key:** `gm`, unbound in vdrs (checked 2026-11-14 in `src/keymap.rs`). Also `:macros`.
  Replay longname `macro-sheet`.
- E2. **Rows:** one per macro binding in the keymap.
  - Columns: `keys`, `steps` (count), `first` (the first step's text), `source` (`macros.lua` or
    `init.lua`).
  - Macros defined in `init.lua` are read-only rows.
- E3. **Actions:**
  - `Enter` opens the macro's steps as a sheet, one row per step text. `e` on a step changes it;
    `d` marks a step for deletion.
  - `d` on a macro row marks it for deletion (shown struck through, as VisiData marks deletes).
  - `e` on `keys` rebinds. It refuses keys already bound, as `m` does.
  - Deferred (K5), as VisiData: nothing is written until `z<C-s>` (`commit-sheet`,
    `visidata/modify.py`). The commit unbinds, rebinds and rewrites `macros.lua`.
  - Leaving the sheet (`q`) with changes not committed asks first, as the quit guard does for
    edits (S-26 A1).
- E4. **Storage:** reuse `plugins::save_macro`, and add `remove_macro(path, keys)`. Both keep hand
  edits to other lines.
- E5. **Parameters:** VisiData macros take parameters (`{name=default}`, `MACRO_PARAM_RE`). Not in
  v1; a separate part if wanted.
- Tests:
  - scenario: record a macro, `gm` lists it, `d` marks it (still bound, file unchanged),
    `z<C-s>` removes it (key unbound, file line gone, other lines kept); rebind via `e` then
    commit; `q` with pending changes asks.
  - All of it runs under the `cfg!(test)` temp `macros_path`.

## F. Named views (1 d, gated)

Excel's named ranges and custom views. Sessions (S-29) and undo already cover part of this.
Build it only when someone asks to switch between views of one sheet.

- `:view save <name>` stores filter, `filter_expr`, sort, hidden, order, keys, selection and
  formats.
- `:view <name>` restores them (undoable). `:views` lists them as a sheet; `Enter` applies one.
- Sessions write each saved view.
- Tests: scenario save → change → restore gives the same `view_csv`.

## Decisions

- **K1** Lookup: **live**, as `:link` (agreed 2026-11-14). A4.
- **K2** `Alt++` / `Alt+-` change `.N` in a rich number format (agreed 2026-11-14). B2.
- **K3** `F` on numbers: group by the text only when the user set a format on the column (b)
  (agreed 2026-11-14). B4.
- **K4** Validation: refuse by default, `warn` mode as an option (agreed 2026-11-14). D3.
- **K5** Macros sheet: deferred commit with `z<C-s>`, as VisiData (agreed 2026-11-14). E3.

## 8. Not doing

A1 references and per-cell formulas; merged cells; several tables on a sheet; goal seek and
solver; manual formatting of single cells. Each needs cell addresses that survive sort and filter
over a lazy, windowed plan. A small eager "scratch" sheet on an existing spreadsheet engine would
be the way in, if a real case shows up.
