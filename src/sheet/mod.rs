//! Sheet = Source + LazyFrame plan + View + materialized `window`. See S-01.

use std::sync::Arc;

use polars::prelude::*;

use crate::reader::{LoadedSource, SourceKind};

mod builders;
pub(crate) mod columns;
mod derive;
mod diff;
mod edit;
mod meta;
mod nested;
mod plot;
mod refresh;
mod rows;
mod step;
mod tail;
#[cfg(test)]
mod tests;
mod undo;
mod write;

pub(crate) use derive::{JL, JLK, JOIN_RIGHT_KEYS, JR, JRK};
pub use nested::{UNFURL_ROW, Unfurl, unfurl_kind};
pub use plot::{PlotView, plot_columns, plot_points};
pub use refresh::{Refreshed, refresh};
pub use step::{ADDED_RID, POS, PlanStep, capture_fields};
pub use undo::{Snapshot, UndoTree};
pub use write::write_df_by_extension;

pub type SheetId = u32;

#[derive(Clone)]
pub struct Sheet {
    pub id: SheetId,
    pub name: String,
    pub kind: SourceKind,
    pub source: Arc<LoadedSource>, // keeps tempfile anchor alive across derived sheets
    pub plan: LazyFrame,
    pub view: View,
    // The window and its bookkeeping change only through the methods under
    // "Window" below (S-24 R2): a stale result can't land, a plan change recounts.
    window: Option<DataFrame>, // None until first materialize lands
    revision: u64,
    pub origin: Origin,
    /// Bookkeeping that isn't part of what the sheet shows.
    pub state: SheetState,
}

/// Per-sheet bookkeeping. `Default` so derived-sheet constructors stay short.
#[derive(Default, Clone)]
pub struct SheetState {
    /// Changed while another sheet was on top (`C` writes): materialize
    /// when it is back on top.
    pub stale: bool,
    /// `revision` of the last materialize result applied (Ok or Err).
    loaded_rev: u64,
    /// A row count for the current `count_key` is in flight.
    counting: bool,
    /// Bumped by every [`Sheet::invalidate`]: part of the count key, so a
    /// count started before an edit (`d`, `a`, `U` …) can't land after it.
    count_gen: u64,
    /// [`ROW_ID`] of each row of `window`, split off when the window lands.
    window_rids: Vec<u64>,
    /// Per `:color` rule, whether it holds on each window row (S-27).
    window_fmt: Vec<Vec<bool>>,
    /// Edits baked into `plan`, in order — replayed by `:export`.
    pub steps: Vec<PlanStep>,
    /// Undo history of (plan, view, steps), a tree (S-20).
    pub undo: UndoTree,
    /// `:tail` of a CSV/TSV/JSONL file: rows read so far + bytes consumed.
    pub tail: Option<TailCache>,
    /// `:tail`: size + mtime at the last refresh; unchanged means nothing to do.
    pub tail_stamp: Option<(u64, Option<std::time::SystemTime>)>,
    /// Derived sheets: the parent as it was + how this was made (`:export`).
    pub lineage: Option<Box<Lineage>>,
    /// Column widths of `window`, and what they were measured for.
    pub widths: Vec<u16>,
    pub widths_key: Option<(u64, usize, usize, bool)>,
    pub widths_view: String,
    /// `memory_plan` has had its one chance.
    pub load_tried: bool,
    /// The source read into memory by `memory_plan`: the base edits rebuild on.
    pub memory_base: Option<LazyFrame>,
    /// Edit steps as of the last `:save` (quit guard, S-26 A1).
    pub saved_edits: Vec<PlanStep>,
    /// `:link`: refresh when this becomes active and its parent changed.
    pub linked: bool,
    /// Plot sheets: what's in view and the cursor box (S-23 C3).
    pub plot: Option<PlotView>,
}

/// A snapshot of the parent at derivation time, so later edits to the parent
/// don't change what `:export` rebuilds.
#[derive(Clone)]
pub struct Lineage {
    pub parent: Sheet,
    pub how: Derivation,
}

#[derive(Clone)]
pub enum Derivation {
    Copy,
    Freq {
        column: String,
    },
    /// Rows by [`ROW_ID`] of the parent's base plan (`"`).
    Rids(std::collections::BTreeSet<u64>),
    /// Rows whose `column`, as text, is one of `values` (freq dive; None = null).
    Values {
        column: String,
        values: Vec<Option<String>>,
    },
    /// `others`: the `s<id>` sheets the query names.
    Sql {
        query: String,
        others: Vec<Sheet>,
    },
    Join {
        right: Box<Sheet>,
        keys: Vec<String>,
        how: String,
    },
    Unpivot {
        on: Vec<String>,
        index: Vec<String>,
    },
    Pivot {
        index: String,
        on: String,
        values: String,
        agg: String,
    },
    /// `W`: row counts of `keys` × `on`.
    PivotCount {
        keys: Vec<String>,
        on: String,
    },
    Describe,
    /// `zM`: a row per item of `column` (S-22).
    Unfurl {
        column: String,
    },
    /// Row diff against `other` on `keys`.
    Diff {
        other: Box<Sheet>,
        keys: Vec<String>,
    },
}

/// Row-diff flags: set on each side before the join, so a null here means the
/// row is missing on that side (null data cells can't be mistaken for it).
pub const DIFF_LEFT: &str = "__vdrs_left";
pub const DIFF_RIGHT: &str = "__vdrs_right";

/// Incremental tail: only bytes past `consumed` are parsed on refresh.
#[derive(Clone)]
pub struct TailCache {
    pub consumed: u64,
    pub df: DataFrame,
    /// Bytes after the last newline, parsed as one more row: the file may end
    /// there for good. Re-read each refresh until its newline arrives.
    pub partial: Option<DataFrame>,
}

#[derive(Debug, Clone)]
pub enum Origin {
    File,
    Freq {
        parent: SheetId,
        column: String,
    },
    Pivot {
        parent: SheetId,
    },
    Unpivot {
        parent: SheetId,
    },
    /// `zM` (S-22): a row per item of `parent`'s list / struct column.
    Unfurl {
        parent: SheetId,
    },
    SheetSwitcher,
    /// `:undotree` of sheet `parent`; `Enter` makes a state current there.
    UndoTree {
        parent: SheetId,
    },
    /// `C` / `gC` (S-25 P3): row `i` is column `cols[i].1` of sheet `cols[i].0`.
    Columns {
        cols: Arc<[(SheetId, String)]>,
    },
    Diff {
        left: SheetId,
        right_name: String,
    },
    /// Tail of a growing file. On each tail tick the plan is rebuilt via `scan_*`.
    Tail {
        path: std::path::PathBuf,
        follow: bool,
    },
    /// `:hits` (S-28 B): a row per search hit in `parent`; `<CR>` goes there.
    Hits {
        parent: SheetId,
    },
    /// `:hist`, `:bar` (S-23 C2): a bar per row, `label` and `value` columns.
    Bars {
        parent: SheetId,
        label: String,
        value: String,
        horizontal: bool,
    },
    /// `:plot` (S-23 C1): `parent`'s view binned by `x`; drawn as a chart.
    Plot {
        parent: SheetId,
        x: String,
        ys: Vec<String>,
    },
    /// Rows selected in parent (VisiData `"`).
    Selected {
        parent: SheetId,
    },
    /// `:sql` result over `parent` (registered as `self`).
    Sql {
        parent: SheetId,
    },
    /// `I`: per-column stats of `parent`.
    Describe {
        parent: SheetId,
    },
    /// `vd.push_sheet` from a Lua binding run on `parent`.
    Lua {
        parent: SheetId,
    },
    /// A text listing vdrs made: `z<C-h>` bindings, `:jumps`.
    Listing,
    /// `g"` / `:dup`: copy of `parent`'s view.
    Copy {
        parent: SheetId,
    },
    /// Worksheets / tables of a multi-part xlsx or sqlite file; `<CR>` opens one.
    Parts {
        path: std::path::PathBuf,
        kind: SourceKind,
    },
    /// `:join` of `left`'s view with another sheet or file.
    Join {
        left: SheetId,
        right_name: String,
    },
}

impl Origin {
    /// Lowercase kind, as `vd.on("push")` handlers see it.
    pub fn name(&self) -> &'static str {
        match self {
            Origin::File => "file",
            Origin::Freq { .. } => "freq",
            Origin::Pivot { .. } => "pivot",
            Origin::Unpivot { .. } => "unpivot",
            Origin::Unfurl { .. } => "unfurl",
            Origin::SheetSwitcher => "sheets",
            Origin::UndoTree { .. } => "undotree",
            Origin::Columns { .. } => "columns",
            Origin::Diff { .. } => "diff",
            Origin::Tail { .. } => "tail",
            Origin::Selected { .. } => "selected",
            Origin::Hits { .. } => "hits",
            Origin::Plot { .. } => "plot",
            Origin::Bars { .. } => "bars",
            Origin::Sql { .. } => "sql",
            Origin::Describe { .. } => "describe",
            Origin::Lua { .. } => "lua",
            Origin::Listing => "list",
            Origin::Copy { .. } => "copy",
            Origin::Parts { .. } => "parts",
            Origin::Join { .. } => "join",
        }
    }
}

/// Sheet stack = all open sheets, last = active. Modeled on VisiData's `vd.sheets`.
pub struct Sheets {
    stack: Vec<Sheet>,
    next_id: SheetId,
}

impl Sheets {
    pub fn new(initial: Sheet) -> Self {
        let next_id = initial.id + 1;
        Self {
            stack: vec![initial],
            next_id,
        }
    }

    #[expect(clippy::expect_used, reason = "`pop` never removes the last sheet")]
    pub fn active(&self) -> &Sheet {
        self.stack
            .last()
            .expect("sheet stack never empty while running")
    }
    #[expect(clippy::expect_used, reason = "`pop` never removes the last sheet")]
    pub fn active_mut(&mut self) -> &mut Sheet {
        self.stack
            .last_mut()
            .expect("sheet stack never empty while running")
    }
    pub fn len(&self) -> usize {
        self.stack.len()
    }
    #[allow(clippy::missing_const_for_fn)]
    pub fn is_empty(&self) -> bool {
        self.stack.is_empty()
    }
    /// The id `alloc_id` hands out next.
    pub fn next_id(&self) -> SheetId {
        self.next_id
    }
    pub fn alloc_id(&mut self) -> SheetId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
    pub fn push(&mut self, s: Sheet) {
        self.stack.push(s);
    }
    /// Pop the active sheet. Returns true if a sheet was popped. Last sheet is left in place
    /// so that App's `quit` flag is the only path to actually ending the session.
    pub fn pop(&mut self) -> bool {
        if self.stack.len() > 1 {
            self.stack.pop();
            true
        } else {
            false
        }
    }
    /// Iterate sheets by `(id, name, kind)` for the sheet-switcher view.
    pub fn summaries(&self) -> impl Iterator<Item = (SheetId, &str, SourceKind)> + '_ {
        self.stack.iter().map(|s| (s.id, s.name.as_str(), s.kind))
    }
    pub fn iter(&self) -> impl Iterator<Item = &Sheet> {
        self.stack.iter()
    }
    pub fn find_by_id_mut(&mut self, id: SheetId) -> Option<&mut Sheet> {
        self.stack.iter_mut().find(|s| s.id == id)
    }
    /// Move the sheet with `id` to the top. Noop if already active or missing.
    pub fn switch_to(&mut self, id: SheetId) -> bool {
        let Some(pos) = self.stack.iter().position(|s| s.id == id) else {
            return false;
        };
        if pos == self.stack.len() - 1 {
            return true;
        }
        let s = self.stack.remove(pos);
        self.stack.push(s);
        true
    }
}

/// What the user sees — cursor, scroll, column order. Mutating view is cheap
/// and never triggers a re-materialize.
#[derive(Debug, Clone)]
pub struct View {
    pub row_cursor: usize, // row index within `window`
    pub col_cursor: usize, // column index within `visible`
    pub row_offset: usize, // first visible row of `window`
    pub col_offset: usize, // first non-frozen visible column
    pub frozen_left: usize,
    pub window_offset: u64, // offset into `plan` that `window` starts at
    pub window_rows: u32,
    pub search: Option<SearchState>,
    pub filter: Option<FilterState>,
    /// `:filter-expr`: keep rows where this SQL predicate holds (S-21 B).
    pub filter_expr: Option<String>,
    pub sort: Vec<SortKey>,
    /// Selected rows by [`ROW_ID`]; survive sort and filter. Shared: every
    /// undo snapshot and job clones the view; only a selection change copies.
    // ponytail: selection-changing steps each keep their own copy in the undo
    // stack, so 100 `s` on a million-row selection hold 100 sets. A delta log
    // fixes that if it ever shows up.
    pub selected: Arc<std::collections::BTreeSet<u64>>,
    /// Hidden columns — projected out at window_plan time.
    pub hidden: std::collections::BTreeSet<String>,
    /// Key columns (VisiData `!`). Reordered to the front of the visible output.
    pub keys: Vec<String>,
    /// Pending type casts, applied before serialize. Last-write-wins per column.
    pub type_casts: Vec<(String, DataType)>,
    /// `+` (S-25 P4): (column, aggregator name), in the order added.
    pub aggregators: Vec<(String, String)>,
    /// `Alt++` / `Alt+-`, `:precision` (S-32): decimals a float column shows.
    pub precision: Vec<(String, u8)>,
    /// Row count of the filtered view; `None` while counting or after a failed count.
    total_rows: Option<u64>,
    /// `Sheet::count_key` that `total_rows` belongs to (or is being counted for).
    /// `None` forces a recount on the next materialize.
    count_key: Option<String>,
    /// `G` pending: put the cursor on the last data row once window + count land.
    pub jump_bottom: bool,
    /// `:color` rules (S-27), applied in order (the last one wins).
    pub formats: Vec<FormatRule>,
    /// `:scale` / `:databar` rules (S-27 B), painted over the `:color` ones.
    pub scales: Vec<ScaleRule>,
}

/// `:scale` / `:databar` on `column`; `range` is its min/max when the rule
/// was made (values outside it clamp).
#[derive(Debug, Clone, PartialEq)]
pub struct ScaleRule {
    pub column: String,
    /// `Some((low, high))`: a color scale; `None`: a data bar.
    pub colors: Option<(String, String)>,
    pub range: [f64; 2],
}

/// `:color <col|*> <pred> <style>` (S-27 A): paint the cell of `column`
/// (`None`: the whole row) where the SQL predicate holds. Display only.
#[derive(Debug, Clone, PartialEq)]
pub struct FormatRule {
    pub column: Option<String>,
    pub pred: String,
    /// As typed (`red`, `bg=yellow,bold`); `theme::parse_style` reads it.
    pub style: String,
}

/// Window columns carrying each `:color` rule's predicate; split off on landing.
const FMT_PREFIX: &str = "__vdrs_fmt_";

impl Default for View {
    fn default() -> Self {
        Self {
            row_cursor: 0,
            col_cursor: 0,
            row_offset: 0,
            col_offset: 0,
            frozen_left: 0,
            window_offset: 0,
            window_rows: DEFAULT_WINDOW_ROWS,
            search: None,
            filter: None,
            filter_expr: None,
            sort: Vec::new(),
            selected: Default::default(),
            hidden: std::collections::BTreeSet::new(),
            keys: Vec::new(),
            type_casts: Vec::new(),
            aggregators: Vec::new(),
            precision: Vec::new(),
            total_rows: None,
            count_key: None,
            jump_bottom: false,
            formats: Vec::new(),
            scales: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SearchState {
    /// Regex (case-insensitive), or a SQL predicate when `expr`.
    pub pattern: String,
    /// `g/` / `g?`: every visible column; else the cursor column at each `n`.
    pub all_columns: bool,
    /// `?` / `z?`: `n` walks up, `N` down.
    pub backward: bool,
    /// `z/` / `z?`: `pattern` is a SQL predicate (S-21).
    pub expr: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FilterState {
    pub column: String,
    pub regex: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SortKey {
    pub column: String,
    pub order: SortOrder,
}

pub const DEFAULT_WINDOW_ROWS: u32 = 1024;
/// CSVs at most this big move into RAM after opening.
// ponytail: fixed cap, no look at free memory; `/proc/meminfo` or a setting if
// it ever pushes a small machine into swap.
const CSV_MEMORY_BYTES: u64 = 1 << 30;

/// Stable row identity: the row's position in the sheet's base `plan`. Sorting,
/// filtering and casts don't change it, so selections key on it.
pub const ROW_ID: &str = "__vdrs_rid";

/// `data/x.csv` → `x.csv`, for the other side in a join/diff title: the
/// title has little room and the path was just typed.
fn file_name(name: &str) -> &str {
    name.rsplit('/').next().unwrap_or(name)
}

/// `:join … extend` names the other sheet's columns `<prefix>_<column>`, as
/// VisiData: its file name without the extension.
pub(crate) fn extend_prefix(right: &Sheet) -> &str {
    let name = file_name(&right.name);
    name.split_once('.').map_or(name, |(stem, _)| stem)
}

impl Sheet {
    pub fn from_source(id: SheetId, loaded: LoadedSource) -> Self {
        let name = loaded.display_name.clone();
        let kind = loaded.kind;
        let plan = with_rids(loaded.lazy.clone());
        Self {
            id,
            name,
            kind,
            source: Arc::new(loaded),
            plan,
            view: View::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin: Origin::File,
        }
    }

    /// A sheet derived from `parent` with a fresh view.
    pub fn child(
        parent: &Sheet,
        id: SheetId,
        name: String,
        plan: LazyFrame,
        origin: Origin,
    ) -> Self {
        Sheet {
            id,
            name,
            kind: parent.kind,
            source: parent.source.clone(),
            plan,
            view: View::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin,
        }
    }

    /// Already-computed rows, so the sheet draws before its first materialize.
    fn with_window(mut self, df: DataFrame) -> Self {
        self.window = Some(df);
        self
    }

    // ---- Window (S-24 R2) ----

    /// The materialized rows on screen; `None` before the first lands.
    pub fn window(&self) -> Option<&DataFrame> {
        self.window.as_ref()
    }

    /// [`ROW_ID`] of each `window` row (empty for pre-built windows).
    pub fn window_rids(&self) -> &[u64] {
        &self.state.window_rids
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The revision `window` was made for.
    pub fn loaded_revision(&self) -> u64 {
        self.state.loaded_rev
    }

    /// The latest materialize (or its error) has landed.
    pub fn is_loaded(&self) -> bool {
        self.state.loaded_rev == self.revision
    }

    pub fn is_counting(&self) -> bool {
        self.state.counting
    }

    /// Rows in the filtered view, once counted.
    pub fn row_count(&self) -> Option<u64> {
        self.view.total_rows
    }

    /// Show `df` as is (previews, tests): no row ids, counts as loaded.
    pub fn set_window(&mut self, df: DataFrame) {
        self.window = Some(df);
        self.state.window_rids.clear();
        self.state.loaded_rev = self.revision;
    }

    /// A materialize starts: results of older revisions won't land.
    pub fn begin_window(&mut self) -> u64 {
        self.revision = self.revision.wrapping_add(1);
        self.revision
    }

    /// The rows may differ: recount. `Plan`: the plan was replaced under the
    /// view, so the window goes too and in-flight results are dropped.
    pub fn invalidate(&mut self, why: Reason) {
        self.view.count_key = None;
        self.state.count_gen += 1;
        if why == Reason::Plan {
            self.window = None;
            self.begin_window();
        }
    }

    /// A materialize of revision `rev` finished. Applied only if `rev` is
    /// current; its [`ROW_ID`] column is split off into `window_rids`.
    pub fn land_window(&mut self, rev: u64, df: DataFrame) -> bool {
        if rev != self.revision {
            return false;
        }
        self.state.loaded_rev = rev;
        let (rids, mut df) = match rid_values(&df) {
            Ok(r) => (r, df.drop(ROW_ID).unwrap_or(df)),
            Err(_) => (Vec::new(), df), // pre-built windows carry no ids
        };
        let fmt: Vec<String> = (0..self.view.formats.len())
            .map(|i| format!("{FMT_PREFIX}{i}"))
            .collect();
        self.state.window_fmt = fmt
            .iter()
            .map(|n| {
                df.column(n)
                    .ok()
                    .and_then(|c| {
                        c.bool()
                            .ok()
                            .map(|b| b.iter().map(|v| v.unwrap_or(false)).collect())
                    })
                    .unwrap_or_default()
            })
            .collect();
        df = df.drop_many(fmt.iter().map(String::as_str));
        // Polars 0.44 returns every scan column when only the row index
        // is selected, i.e. when all columns are hidden.
        self.window = Some(df.drop_many(self.view.hidden.iter().map(String::as_str)));
        self.state.window_rids = rids;
        true
    }

    /// A materialize of revision `rev` failed; `false` if it was stale.
    pub fn land_error(&mut self, rev: u64) -> bool {
        let current = rev == self.revision;
        if current {
            self.state.loaded_rev = rev;
        }
        current
    }

    /// The count key to count for, if the row count is stale; marks it in flight.
    pub fn start_count(&mut self) -> Option<String> {
        let key = self.count_key();
        if self.view.count_key.as_ref() == Some(&key) {
            return None;
        }
        self.view.count_key = Some(key.clone());
        self.view.total_rows = None;
        self.state.counting = true;
        Some(key)
    }

    /// A count for `key` finished (`None` = failed); `false` if it was stale.
    pub fn land_count(&mut self, key: &str, total: Option<u64>) -> bool {
        if self.view.count_key.as_deref() != Some(key) {
            return false;
        }
        self.state.counting = false;
        self.view.total_rows = total;
        true
    }

    /// Record how `self` was derived from `parent` (snapshot without history).
    pub fn derived(mut self, parent: &Sheet, how: Derivation) -> Self {
        let mut p = parent.clone();
        p.window = None;
        p.state.undo = UndoTree::default();
        self.state.lineage = Some(Box::new(Lineage { parent: p, how }));
        self
    }

    /// Build the lazy query for the current `window`: `view_plan` + slice.
    /// A CSV can't seek, so deep windows and sorts rescan it. This plan reads
    /// the source into memory; `None` when that doesn't apply. Edited sheets
    /// stay on disk, and so do compressed files, whose size says nothing.
    pub fn memory_plan(&self) -> Option<LazyFrame> {
        let path = self.source.path.as_ref()?;
        let plain_csv = matches!(self.kind, SourceKind::Csv | SourceKind::Tsv)
            && matches!(self.origin, Origin::File)
            && path.extension().is_none_or(|e| e != "gz");
        let unedited = self.state.steps.is_empty()
            && self.state.tail.is_none()
            && self.state.tail_stamp.is_none();
        let small = std::fs::metadata(path).is_ok_and(|m| m.len() <= CSV_MEMORY_BYTES);
        (plain_csv && unedited && small).then(|| self.source.lazy.clone())
    }

    pub fn window_plan(&self) -> LazyFrame {
        self.view_plan()
            .slice(self.view.window_offset as i64, self.view.window_rows)
    }

    /// `window_plan` plus a [`ROW_ID`] column (split off by the app
    /// into `state.window_rids` when the window lands).
    pub fn window_plan_with_rids(&self) -> LazyFrame {
        self.view_plan_fmt(true, true)
            .slice(self.view.window_offset as i64, self.view.window_rows)
    }

    /// Min and max of numeric `column` over the whole view (`:scale`).
    pub fn column_range(&self, column: &str) -> Result<[f64; 2], String> {
        let lf = self.view_plan();
        let dtype = lf
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?
            .get(column)
            .cloned()
            .ok_or_else(|| format!("no column: {column}"))?;
        if !dtype.is_primitive_numeric() {
            return Err(format!("{column} is not numeric ({dtype})"));
        }
        let c = || col(column).cast(DataType::Float64);
        let df = lf
            .select([c().min().alias("lo"), c().max().alias("hi")])
            .collect()
            .map_err(|e| e.to_string())?;
        let get = |n: &str| df.column(n).ok()?.f64().ok()?.get(0);
        match (get("lo"), get("hi")) {
            (Some(lo), Some(hi)) => Ok([lo, hi]),
            _ => Err(format!("{column} has no numbers")),
        }
    }

    /// Whether `:color` rule `i` holds on window row `r`.
    pub fn format_holds(&self, i: usize, r: usize) -> bool {
        self.state
            .window_fmt
            .get(i)
            .and_then(|v| v.get(r))
            .copied()
            .unwrap_or(false)
    }

    /// Full (unsliced) query for what the user sees. Applies, in order:
    /// type_casts → filter → sort → visible-column projection.
    pub fn view_plan(&self) -> LazyFrame {
        self.view_plan_rid(false)
    }

    /// `view_plan`, optionally carrying each row's [`ROW_ID`] (first when the
    /// view projects nothing, last when it does; callers find it by name).
    fn view_plan_rid(&self, with_rid: bool) -> LazyFrame {
        self.view_plan_fmt(with_rid, false)
    }

    /// `fmt`: plus a boolean column per `:color` rule (the window only).
    fn view_plan_fmt(&self, with_rid: bool, fmt: bool) -> LazyFrame {
        let mut lf = self.sorted_plan(with_rid);
        // the projection below keeps them: they're in the schema, not hidden
        if fmt {
            let base = lf.clone();
            for (i, r) in self.view.formats.iter().enumerate() {
                // a rule gone bad (its column dropped) paints nothing
                let e = self.predicate(&base, &r.pred).unwrap_or(lit(false));
                lf = lf.with_column(e.alias(format!("{FMT_PREFIX}{i}")));
            }
        }
        // Visible-column projection: keys first (in key-col order), then schema
        // order for the rest; drop hidden. Skipped when nothing to project.
        if (!self.view.hidden.is_empty() || !self.view.keys.is_empty())
            && let Ok(schema) = lf.clone().collect_schema()
        {
            let mut all: Vec<String> = schema.iter_names().map(|n| n.to_string()).collect();
            all.retain(|n| n != ROW_ID);
            let mut order: Vec<String> = Vec::with_capacity(all.len());
            for k in &self.view.keys {
                if all.iter().any(|n| n == k) && !self.view.hidden.contains(k) {
                    order.push(k.clone());
                }
            }
            for n in &all {
                if !order.contains(n) && !self.view.hidden.contains(n) {
                    order.push(n.clone());
                }
            }
            if order != all {
                if with_rid {
                    order.push(ROW_ID.to_string());
                }
                lf = lf.select(
                    order
                        .into_iter()
                        .map(|s| col(s.as_str()))
                        .collect::<Vec<_>>(),
                );
            }
        }
        lf
    }

    /// `view_plan` before the visible-column projection: the view's rows in
    /// view order, hidden columns included (expression predicates may name them).
    fn sorted_plan(&self, with_rid: bool) -> LazyFrame {
        self.sorted_plan_with(with_rid, false)
    }

    /// `errs`: each cast column `c` also gets a boolean `err_col(c)`, true where
    /// the cast failed (a value before it, null after). For describe's `errors`.
    pub(crate) fn sorted_plan_with(&self, with_rid: bool, errs: bool) -> LazyFrame {
        let mut lf = self.filtered_plan_with(with_rid, errs);
        if !self.view.sort.is_empty() {
            let by: Vec<&str> = self.view.sort.iter().map(|k| k.column.as_str()).collect();
            let desc: Vec<bool> = self
                .view
                .sort
                .iter()
                .map(|k| k.order == SortOrder::Desc)
                .collect();
            // VisiData: a null is the smallest value, so it leads an
            // ascending sort and trails a descending one
            let opts = SortMultipleOptions::default()
                .with_order_descending_multi(desc.clone())
                .with_nulls_last_multi(desc)
                .with_maintain_order(true);
            lf = lf.sort(by, opts);
        }
        lf
    }

    /// Base plan + type_casts + filter: decides which rows exist, not their order.
    /// `with_rid` numbers base rows first, so ids survive filter and sort.
    fn filtered_plan(&self, with_rid: bool) -> LazyFrame {
        self.filtered_plan_with(with_rid, false)
    }

    fn filtered_plan_with(&self, with_rid: bool, errs: bool) -> LazyFrame {
        let mut lf = self.plan.clone();
        let deleted = self.deleted_rids();
        let any_deleted = !deleted.is_empty();
        // Source sheets carry their ids; derived ones number their rows here.
        let carried = self.has_rids();
        if !carried && (with_rid || any_deleted) {
            lf = lf.with_row_index(ROW_ID, None);
        }
        if any_deleted {
            lf = lf.filter(rid_in(&deleted).not());
        }
        if !with_rid && (carried || any_deleted) {
            lf = lf.drop(cols([ROW_ID]));
        }
        // Type casts first so downstream filter/sort see the typed values.
        if !self.view.type_casts.is_empty() {
            let base = self.plan.clone().collect_schema().ok();
            let is_text = |n: &str| base.as_ref().and_then(|s| s.get(n)) == Some(&DataType::String);
            let exprs: Vec<Expr> = self
                .view
                .type_casts
                .iter()
                .map(|(name, dt)| match dt {
                    DataType::Date if is_text(name) => lenient_date(name),
                    _ => col(name.as_str()).cast(dt.clone()).alias(name.as_str()),
                })
                .collect();
            let names = || self.view.type_casts.iter().map(|(n, _)| n.as_str());
            if errs {
                lf = lf.with_columns(
                    names()
                        .map(|n| col(n).is_not_null().alias(err_col(n)))
                        .collect::<Vec<_>>(),
                );
            }
            lf = lf.with_columns(exprs);
            if errs {
                lf = lf.with_columns(
                    names()
                        .map(|n| col(err_col(n)).and(col(n).is_null()).alias(err_col(n)))
                        .collect::<Vec<_>>(),
                );
            }
        }
        if let Some(f) = &self.view.filter {
            // Cast so numeric/date columns filter on their display text. Strict:
            // a bad regex errors loudly instead of filtering every row out.
            lf = lf.filter(
                col(f.column.as_str())
                    .cast(DataType::String)
                    .str()
                    .contains(lit(f.regex.as_str()), true),
            );
        }
        // Checked by `set_filter_expr`; an expression that stopped parsing
        // can't happen (rename refuses to orphan it).
        if let Some(e) = self
            .view
            .filter_expr
            .as_deref()
            .and_then(|p| polars::sql::sql_expr(p).ok())
        {
            lf = lf.filter(e.fill_null(false));
        }
        lf
    }

    /// `:filter-expr <pred>`: replace the predicate filter; `None` clears it.
    pub fn set_filter_expr(&mut self, pred: Option<&str>) -> Result<(), String> {
        if let Some(p) = pred {
            self.check_predicate(p)?;
        }
        self.view.filter_expr = pred.map(String::from);
        self.invalidate(Reason::Rows);
        Ok(())
    }

    /// One-row `len` query over `filtered_plan`. Sort/projection don't change
    /// the row count, so they're skipped to keep the count cheap.
    pub fn count_plan(&self) -> LazyFrame {
        self.filtered_plan(false).select([len()])
    }

    /// Identifies the row count `count_plan` yields; when it changes, recount.
    /// Plan replacements (tail refresh, currency cast) reset `view.count_key` instead.
    pub fn count_key(&self) -> String {
        format!("{}|{}", self.state.count_gen, self.view_key())
    }

    /// What the view filters and casts by: the same rows and columns while
    /// it holds, edits or not (column widths only widen).
    pub fn view_key(&self) -> String {
        format!(
            "{:?}|{:?}|{:?}",
            self.view.filter, self.view.filter_expr, self.view.type_casts
        )
    }

    /// `:derive name = <sql expr>` on the base plan. Validated against the schema.
    pub fn derive(&mut self, name: &str, expr: &str) -> Result<(), String> {
        if name == ROW_ID {
            return Err(format!("{ROW_ID} is reserved"));
        }
        let step = PlanStep::Derive {
            name: name.to_string(),
            expr: expr.to_string(),
            replaces: self.has_column(name),
        };
        let plan = step.apply(self.plan.clone()).map_err(|e| e.to_string())?;
        plan.clone().collect_schema().map_err(|e| e.to_string())?;
        self.checkpoint();
        self.plan = plan;
        self.state.steps.push(step);
        self.invalidate(Reason::Rows); // a filter on `name` may match differently now
        Ok(())
    }

    /// `^` / `:rename`: a `Rename` step, and the view follows the column
    /// (hidden, keys, casts, filter, sort). Unchanged on error; undo is the caller's.
    pub fn rename_col(&mut self, old: &str, new: &str) -> Result<(), String> {
        if new == ROW_ID {
            return Err(format!("{ROW_ID} is reserved"));
        }
        if new != old && self.has_column(new) {
            return Err(format!("column already exists: {new}"));
        }
        let reads = |p: &str| {
            polars::sql::sql_expr(p).is_ok_and(|e| e.meta().root_names().iter().any(|n| n == old))
        };
        if new != old && self.view.filter_expr.as_deref().is_some_and(reads) {
            return Err(format!(
                ":filter-expr reads {old}; clear it (:unfilter) first"
            ));
        }
        if new != old && self.view.formats.iter().any(|f| reads(&f.pred)) {
            return Err(format!(
                "a :color rule reads {old}; clear it (:nocolor) first"
            ));
        }
        let step = PlanStep::Rename {
            old: old.to_string(),
            new: new.to_string(),
        };
        self.apply_step(step).map_err(|e| format!("rename: {e}"))?;
        let v = &mut self.view;
        if v.hidden.remove(old) {
            v.hidden.insert(new.to_string());
        }
        let names = v
            .keys
            .iter_mut()
            .chain(v.type_casts.iter_mut().map(|(c, _)| c))
            .chain(v.aggregators.iter_mut().map(|(c, _)| c))
            .chain(v.precision.iter_mut().map(|(c, _)| c));
        let names = names
            .chain(v.filter.as_mut().map(|f| &mut f.column))
            .chain(v.sort.iter_mut().map(|k| &mut k.column))
            .chain(v.formats.iter_mut().filter_map(|f| f.column.as_mut()))
            .chain(v.scales.iter_mut().map(|f| &mut f.column));
        for c in names.filter(|c| *c == old) {
            *c = new.to_string();
        }
        Ok(())
    }

    /// `# % ~ @`: show `name` as `dt` (a view cast; the base plan is untouched).
    pub fn set_type_cast(&mut self, name: String, dt: DataType) {
        match self.view.type_casts.iter_mut().find(|(c, _)| *c == name) {
            Some(entry) => entry.1 = dt,
            None => self.view.type_casts.push((name, dt)),
        }
    }
}

/// Layouts `@` tries, each with the exact shape it applies to.
pub const DATE_LAYOUTS: [(&str, &str); 12] = [
    ("%Y-%m-%d", r"^\d{4}-\d{1,2}-\d{1,2}$"),
    ("%Y/%m/%d", r"^\d{4}/\d{1,2}/\d{1,2}$"),
    ("%m/%d/%Y", r"^\d{1,2}/\d{1,2}/\d{4}$"),
    ("%m/%d/%y", r"^\d{1,2}/\d{1,2}/\d{2}$"),
    ("%d.%m.%Y", r"^\d{1,2}\.\d{1,2}\.\d{4}$"),
    ("%d.%m.%y", r"^\d{1,2}\.\d{1,2}\.\d{2}$"),
    ("%Y%m%d", r"^\d{8}$"),
    ("%d %b %Y", r"^\d{1,2} [A-Za-z]{3} \d{4}$"),
    ("%b %d %Y", r"^[A-Za-z]{3} \d{1,2} \d{4}$"),
    ("%b %d, %Y", r"^[A-Za-z]{3} \d{1,2}, \d{4}$"),
    ("%B %d, %Y", r"^[A-Za-z]+ \d{1,2}, \d{4}$"),
    ("%d %B %Y", r"^\d{1,2} [A-Za-z]+ \d{4}$"),
];
/// A trailing time part, dropped before parsing.
pub const DATE_TIME_PART: &str = r"(T| +)\d{1,2}:\d{2}.*$";

/// Text → Date trying common layouts in turn; a trailing time part is ignored
/// (`7/3/2018 1:47p` → 2018-07-03). Unparsable values become null.
/// chrono's `%Y` takes any digit count, so numeric layouts only apply to
/// strings of their exact shape (regex guard): `7/3/18` isn't year 7.
fn lenient_date(name: &str) -> Expr {
    // drop a time part: "2018-07-03T10:00", "7/3/2018 1:47p"
    let day =
        col(name)
            .str()
            .strip_chars(lit(NULL))
            .str()
            .replace(lit(DATE_TIME_PART), lit(""), false);
    let tries: Vec<Expr> = DATE_LAYOUTS
        .iter()
        .map(|(layout, shape)| {
            let parsed = day.clone().str().to_date(StrptimeOptions {
                format: Some((*layout).into()),
                strict: false,
                exact: true,
                cache: true,
            });
            when(day.clone().str().contains(lit(*shape), true))
                .then(parsed)
                .otherwise(lit(NULL).cast(DataType::Date))
        })
        .collect();
    coalesce(&tries).alias(name)
}

fn tail_stamp(path: &std::path::Path) -> Option<(u64, Option<std::time::SystemTime>)> {
    let m = std::fs::metadata(path).ok()?;
    Some((m.len(), m.modified().ok()))
}

/// `column` equals one of `values`, compared as text (how freq groups print);
/// a null value matches nulls.
fn values_predicate(column: &str, values: &[AnyValue]) -> Expr {
    let text: Vec<String> = values
        .iter()
        .filter(|v| !v.is_null())
        .map(any_text)
        .collect();
    let mut pred = col(column)
        .cast(DataType::String)
        .is_in(lit(Series::new("v".into(), &text)).implode(false), false);
    if values.iter().any(AnyValue::is_null) {
        pred = pred.or(col(column).is_null());
    }
    pred
}

/// Freq sheet stat columns; renamed when the grouped column already has the name.
/// Eager pivot (Polars dropped `pivot_stable`): `on` values become columns in
/// first-seen order; `agg` reduces `element()`. A missing index×on pair is
/// null, not the aggregate of nothing (`sum` would say 0).
pub fn pivot_df(
    df: &DataFrame,
    on: &str,
    index: &[String],
    values: &str,
    agg: Expr,
) -> PolarsResult<DataFrame> {
    let on_columns = df
        .select([on])?
        .unique_stable(None, UniqueKeepStrategy::First, None)?;
    df.clone()
        .lazy()
        .pivot(
            cols([on]),
            Arc::new(on_columns),
            cols(index.iter().map(String::as_str)),
            cols([values]),
            when(len().gt(lit(0))).then(agg).otherwise(lit(NULL)),
            true,
            "_".into(),
            Default::default(),
        )
        .collect()
}

/// Names of a freq sheet's count, percent and histogram columns; prefixed
/// when the grouped column already uses one.
pub fn freq_stat_names(column: &str) -> (&'static str, &'static str, &'static str) {
    if ["count", "percent", "histogram"].contains(&column) {
        ("freq_count", "freq_percent", "freq_histogram")
    } else {
        ("count", "percent", "histogram")
    }
}

/// Describe's statistic columns after `sheet`, `column`, `type`.
pub const DESCRIBE_STATS: [&str; 10] = [
    "errors", "nulls", "distinct", "mode", "min", "max", "sum", "median", "mean", "stdev",
];

/// Describe's per-column cast-failure flag (`Sheet::sorted_plan_with`).
pub fn err_col(column: &str) -> String {
    format!("\u{1f}err:{column}")
}

/// VisiData leaves a numeric (or date) freq table in first-seen order and
/// sorts the rest by count, desc (`freqtbl.py`, `numeric_binning` off).
pub fn freq_keeps_order(parent: &Sheet, column: &str) -> bool {
    parent
        .view_plan()
        .collect_schema()
        .ok()
        .and_then(|s| s.get(column).map(|d| d.is_numeric() || d.is_temporal()))
        .unwrap_or(false)
}

/// Bar length of the most frequent value (VisiData: twice its default column
/// width, less 2); others scale down by count, rounding down.
pub const HISTOGRAM_WIDTH: usize = 38;

/// Row `r`'s `keys` values as text: what a note on that row keys on.
pub fn key_text(df: &DataFrame, keys: &[String], r: usize) -> Vec<String> {
    keys.iter()
        .map(|k| {
            df.column(k)
                .ok()
                .and_then(|c| c.get(r).ok())
                .map_or_else(String::new, |v| any_text(&v))
        })
        .collect()
}

/// A value as `cast(String)` prints it: the text `values_predicate` compares.
/// (`AnyValue`'s `Display` rounds floats and shortens datetimes.)
pub fn any_text(v: &AnyValue) -> String {
    let cast = Series::from_any_values("v".into(), std::slice::from_ref(v), true)
        .and_then(|s| s.cast(&DataType::String));
    let text = cast
        .ok()
        .and_then(|s| s.str().ok()?.get(0).map(String::from));
    text.unwrap_or_else(|| v.to_string())
}

/// A source sheet's base with its row ids: a source row's id is its
/// position in the source (S-18 §2.3). Every step carries the column; the
/// view drops it.
pub(super) fn with_rids(lf: LazyFrame) -> LazyFrame {
    lf.with_row_index(ROW_ID, None)
        .with_column(col(ROW_ID).cast(DataType::UInt64))
}

/// `c`'s values as text, null as `None`.
pub(crate) fn column_texts(c: &Column) -> PolarsResult<Vec<Option<String>>> {
    let c = c.cast(&DataType::String)?;
    Ok(c.str()?.iter().map(|v| v.map(String::from)).collect())
}

/// `1234567` as `1,234,567`.
pub fn grouped(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// A stat for the status line: digits grouped, at most 2 decimals.
pub fn stat_num(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    let int = grouped(r.trunc().abs() as u64);
    let sign = if r < 0.0 { "-" } else { "" };
    let frac = format!("{:.2}", r.fract().abs());
    match frac.trim_end_matches('0').trim_end_matches('.') {
        "0" | "" => format!("{sign}{int}"),
        f => format!("{sign}{int}{}", &f[1..]),
    }
}

/// The row's [`ROW_ID`] is one of `rids`.
pub(crate) fn rid_in(rids: &[u64]) -> Expr {
    let ids = lit(Series::new("ids".into(), rids)).implode(false);
    col(ROW_ID).cast(DataType::UInt64).is_in(ids, false)
}

/// The [`ROW_ID`] column of `df` as u64s.
pub fn rid_values(df: &DataFrame) -> PolarsResult<Vec<u64>> {
    let c = df.column(ROW_ID)?.cast(&DataType::UInt64)?;
    Ok(c.u64()?.into_no_null_iter().collect())
}

/// What [`Sheet::invalidate`] drops.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// Rows may differ (an edit, a derive, a filter): the count.
    Rows,
    /// The plan was replaced from outside: the count and the window.
    Plan,
}
