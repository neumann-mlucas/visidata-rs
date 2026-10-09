# S-01 — Core data model

Status: **draft** (unapproved)
Phase: 0
Depends on: direction.md §4A (Polars-primary), §9.3 (Polars-only)
Blocks: S-02 (app loop), S-03 (readers), S-04 (keybindings)

## Problem

A `Sheet` in VisiData is Row-objects + Columns-as-accessors over them. That's the perf ceiling. In a Polars-first world the primitive is a `LazyFrame`; the "sheet" is a *view spec* over that frame. We need an abstraction that (a) stays columnar end-to-end, (b) makes the user's keystrokes compile to a LazyFrame plan (direction §5 bets #1, #2), and (c) still exposes a cursor + rows + columns UX indistinguishable from VisiData.

## Decision

A sheet is **three decoupled layers**: a `Source`, a `LazyFrame` plan built from it, and a `View` that specifies what the user sees. A small materialized `DataFrame` is a cache of the view's current window — it is the only thing the renderer touches. The `LazyFrame` is the undo/history log; mutations push transforms onto it. Nothing in the renderer ever iterates rows of the source.

## Shape

```rust
pub struct Sheet {
    id: SheetId,
    name: String,
    source: Source,
    plan: LazyFrame,       // every user transform appended here
    view: View,            // what's displayed right now
    window: DataFrame,     // materialized head/window of plan.filter(view).limit(N)
    columns: Vec<Column>,  // metadata only; names must match plan.schema() OR be Derived
    revision: u64,         // bumps on any plan or view change; drives dirty tracking
}

pub enum Source {
    File { path: PathBuf, format: Format },
    Stdin,
    Derived { parent: SheetId, kind: DerivationKind }, // pivot/melt/freq/filter result
    Empty,
}

pub struct Column {
    name: String,              // key in schema OR expr output name
    display: ColDisplay,       // width, align, format, color-override
    kind: ColKind,
}
pub enum ColKind {
    Native,                     // present in plan.schema() already
    Derived(polars::Expr),      // added via :derive or lua; appended to plan on first draw
}

pub struct View {
    visible: Vec<ColIdx>,       // ordered, left-to-right
    frozen_left: usize,         // first N stay pinned during horizontal scroll
    hidden: HashSet<ColIdx>,
    sort: Vec<SortKey>,
    filters: Vec<FilterExpr>,   // compile to plan.filter() chain
    cursor: Cursor,             // (row_in_window, col_in_visible)
    scroll: Scroll,             // row_offset, col_offset
}
```

**Mutation discipline**: nothing mutates `window` directly. User action → mutate `view` or push onto `plan` → bump `revision` → background task re-materializes `window` → UI re-renders. Single-writer, single-reader for the dataframe; cursor/scroll edits are view-only and don't require re-materialization.

**Row addressing**: there is no stable "row id" — rows are indices into the current `window` for display and into `plan` after `.collect()` for everything else. If stable identity is needed (e.g. multi-sheet diff S-12), the sheet owns a hidden `__rowid` column via `.with_row_index()`.

**Derived columns**: `ColKind::Derived(Expr)` holds a Polars `Expr`. On the next re-materialize the plan becomes `plan.with_columns([expr.alias(name)])`. This is also how Lua-defined columns land in Phase 3.

**Plan panel (bet #2)** reads `plan.describe_optimized_plan()` for free. No extra data structure needed.

## Why not <alternative>

- **Sheet owns a `DataFrame` directly** (simpler): kills bet #2 (no plan to show), re-ingests on every filter change, and re-introduces the VisiData ceiling if a user builds up 20 transforms.
- **Sheet IS a LazyFrame subclass / newtype**: couples view state to data state; view edits trigger plan re-collection; wrong direction.
- **Virtual rows (lazy row iterator over plan)**: brings us back to the row-at-a-time model VisiData already has. Rejected.

## Open sub-decisions

- `window` size: fixed (e.g. 10k rows), terminal-height-based, or dynamic growing on scroll? Lazy answer: `max(terminal_height * 4, 1024)`, revisit if scrolling stutters.
- Derived column eval cost on big frames: `with_columns` is cheap in Polars because of lazy composition, but errors in a user expr need to surface in-cell, not kill the sheet. S-05 (types) will spec the error-wrapping path.
- `SheetId` type: `u32` is fine for now (billions of sheets per session is absurd). Not worth a UUID.
- Stable cursor across re-materialize: when `plan` changes, where does the cursor land? Lazy answer: clamp row to new window length, keep col. Smarter answers (follow-key) wait until a user complaint.

## Verification before leaving this spec

- Can S-02 (app loop) render a `Sheet` without touching `plan` or `source`? Yes — it only reads `window` + `view` + `columns`.
- Can S-04 (keybindings) express "sort by current column" as a plan-mutation? Yes — push `SortKey` into `view.sort`, trigger re-materialize.
- Can bet #2 (plan panel) render without a new abstraction? Yes — one Polars method call.

If any of those three answers flip, this spec is wrong and must change before S-02 starts.
