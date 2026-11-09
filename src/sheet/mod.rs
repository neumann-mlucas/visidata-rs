//! Sheet = Source + LazyFrame plan + View + materialized `window`. See S-01.

use std::sync::Arc;

use polars::prelude::*;

use crate::reader::{LoadedSource, SourceKind};

mod derive;
mod diff;
mod step;
mod tail;
#[cfg(test)]
mod tests;

pub use step::{PlanStep, Snapshot};

pub type SheetId = u32;

#[derive(Clone)]
pub struct Sheet {
    pub id: SheetId,
    pub name: String,
    pub kind: SourceKind,
    pub source: Arc<LoadedSource>, // keeps tempfile anchor alive across derived sheets
    pub plan: LazyFrame,
    pub view: View,
    pub window: Option<DataFrame>, // None until first materialize lands
    pub revision: u64,
    pub origin: Origin,
    /// Bookkeeping that isn't part of what the sheet shows.
    pub state: SheetState,
}

/// Per-sheet bookkeeping. `Default` so derived-sheet constructors stay short.
#[derive(Default, Clone)]
pub struct SheetState {
    /// `revision` of the last materialize result applied (Ok or Err).
    pub loaded_rev: u64,
    /// A row count for the current `count_key` is in flight.
    pub counting: bool,
    /// [`ROW_ID`] of each row of `window`, split off when the window lands.
    pub window_rids: Vec<u64>,
    /// Edits baked into `plan`, in order — replayed by `:export`.
    pub steps: Vec<PlanStep>,
    /// Undo / redo history of (plan, view, steps), newest last.
    pub undo: Vec<Snapshot>,
    pub redo: Vec<Snapshot>,
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
    SheetSwitcher,
    Diff {
        left: SheetId,
        right_name: String,
    },
    /// Tail of a growing file. On each tail tick the plan is rebuilt via `scan_*`.
    Tail {
        path: std::path::PathBuf,
        follow: bool,
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

    pub fn active(&self) -> &Sheet {
        self.stack
            .last()
            .expect("sheet stack never empty while running")
    }
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
    /// Row count of the filtered view; `None` while counting or after a failed count.
    pub total_rows: Option<u64>,
    /// `Sheet::count_key` that `total_rows` belongs to (or is being counted for).
    /// `None` forces a recount on the next materialize.
    pub count_key: Option<String>,
    /// `G` pending: put the cursor on the last data row once window + count land.
    pub jump_bottom: bool,
}

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
            total_rows: None,
            count_key: None,
            jump_bottom: false,
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

#[derive(Debug, Clone)]
pub struct FilterState {
    pub column: String,
    pub regex: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortOrder {
    Asc,
    Desc,
}

#[derive(Debug, Clone)]
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

impl Sheet {
    pub fn from_source(id: SheetId, loaded: LoadedSource) -> Self {
        let name = loaded.display_name.clone();
        let kind = loaded.kind;
        let plan = loaded.lazy.clone();
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

    /// Build a tiny in-memory sheet listing every sheet in the stack.
    pub fn sheet_switcher(id: SheetId, summaries: Vec<(SheetId, String, SourceKind)>) -> Self {
        let ids: Vec<i64> = summaries.iter().map(|(i, _, _)| *i as i64).collect();
        let names: Vec<String> = summaries.iter().map(|(_, n, _)| n.clone()).collect();
        let kinds: Vec<String> = summaries.iter().map(|(_, _, k)| format!("{k:?}")).collect();
        let df = DataFrame::new_infer_height(vec![
            Column::new("id".into(), ids),
            Column::new("name".into(), names),
            Column::new("kind".into(), kinds),
        ])
        .expect("sheet-switcher dataframe");
        let plan = df.clone().lazy();
        Sheet {
            id,
            name: "<sheets>".into(),
            kind: SourceKind::Csv, // placeholder; this is a meta-sheet
            source: Arc::new(LoadedSource::empty()),
            plan,
            view: View::default(),
            window: Some(df),
            revision: 0,
            state: Default::default(),
            origin: Origin::SheetSwitcher,
        }
    }

    /// Table this sheet saves as in a database file, as VisiData names it:
    /// `data3.tsv` → `data3`, a table opened from `app.duckdb` → its name.
    pub fn table_name(&self) -> String {
        let part = self.source.path.as_ref().and_then(|p| {
            let shown = format!("{}:", p.display());
            self.source
                .display_name
                .strip_prefix(&shown)
                .map(String::from)
        });
        match part {
            Some(part) if matches!(self.origin, Origin::File) => {
                part.rsplit('.').next().unwrap_or(&part).to_string()
            }
            _ => std::path::Path::new(&self.name)
                .file_stem()
                .map_or_else(|| self.name.clone(), |s| s.to_string_lossy().into_owned()),
        }
    }

    /// List sheet for a multi-part xlsx/sqlite file, or a duckdb file's index
    /// (its source already is that list; `parts` is unused).
    pub fn parts_list(id: SheetId, loaded: LoadedSource, parts: Vec<String>) -> Self {
        let path = loaded.path.clone().unwrap_or_default();
        let kind = loaded.kind;
        let what = if kind == SourceKind::Xlsx {
            "sheet"
        } else {
            "table"
        };
        let plan = if kind == SourceKind::Duckdb {
            loaded.lazy.clone()
        } else {
            DataFrame::new_infer_height(vec![Column::new(what.into(), parts)])
                .expect("one column")
                .lazy()
        };
        Sheet {
            id,
            name: format!("{}: {what}s", loaded.display_name),
            kind,
            source: Arc::new(loaded),
            plan,
            view: View::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin: Origin::Parts { path, kind },
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

    /// Record how `self` was derived from `parent` (snapshot without history).
    pub fn derived(mut self, parent: &Sheet, how: Derivation) -> Self {
        let mut p = parent.clone();
        p.window = None;
        p.state.undo.clear();
        p.state.redo.clear();
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
        self.view_plan_rid(true)
            .slice(self.view.window_offset as i64, self.view.window_rows)
    }

    /// Full (unsliced) query for what the user sees. Applies, in order:
    /// type_casts → filter → sort → visible-column projection.
    pub fn view_plan(&self) -> LazyFrame {
        self.view_plan_rid(false)
    }

    /// `view_plan`, optionally carrying each row's [`ROW_ID`] (first when the
    /// view projects nothing, last when it does; callers find it by name).
    fn view_plan_rid(&self, with_rid: bool) -> LazyFrame {
        let mut lf = self.sorted_plan(with_rid);
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
            let opts = SortMultipleOptions::default()
                .with_order_descending_multi(desc)
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
        if with_rid {
            lf = lf.with_row_index(ROW_ID, None);
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
        self.view.count_key = None;
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
        format!(
            "{:?}|{:?}|{:?}",
            self.view.filter, self.view.filter_expr, self.view.type_casts
        )
    }

    /// `:derive name = <sql expr>` on the base plan. Validated against the schema.
    pub fn derive(&mut self, name: &str, expr: &str) -> Result<(), String> {
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
        self.view.count_key = None; // a filter on `name` may match differently now
        Ok(())
    }

    /// `^` / `:rename`: a `Rename` step, and the view follows the column
    /// (hidden, keys, casts, filter, sort). Unchanged on error; undo is the caller's.
    pub fn rename_col(&mut self, old: &str, new: &str) -> Result<(), String> {
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
            .chain(v.type_casts.iter_mut().map(|(c, _)| c));
        let names = names
            .chain(v.filter.as_mut().map(|f| &mut f.column))
            .chain(v.sort.iter_mut().map(|k| &mut k.column));
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

    /// Sheets read from a source take edits; derived ones (freq, pivot,
    /// join, `:sql`, selected rows, …) don't (S-18 D3).
    pub fn editable(&self) -> Result<(), String> {
        if matches!(self.origin, Origin::File | Origin::Tail { .. }) {
            return Ok(());
        }
        Err(match &self.state.lineage {
            Some(l) => format!("read-only: {} is derived from {}", self.name, l.parent.name),
            None => format!("read-only: {} is derived", self.name),
        })
    }

    /// `text` typed into `column`, as stored (None for empty = null), and the
    /// column's base type. It must fit the column as displayed (after
    /// `# % ~ @`) and its base type (S-18 §2.4).
    pub fn edit_value(
        &self,
        column: &str,
        text: &str,
    ) -> Result<(Option<String>, DataType), String> {
        let base = self
            .plan
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?
            .get(column)
            .cloned()
            .ok_or_else(|| format!("no column: {column}"))?;
        if text.is_empty() {
            return Ok((None, base));
        }
        if base == DataType::Boolean {
            // Polars doesn't cast text to bool
            let t = text.to_ascii_lowercase();
            return match t.as_str() {
                "true" | "false" => Ok((Some(t), base)),
                _ => Err(format!("{text:?} doesn't fit {column} (bool: true/false)")),
            };
        }
        let fits = |e: Expr| {
            df!(column => [text])
                .and_then(|d| d.lazy().select([e.alias("v")]).collect())
                .is_ok_and(|d| d.column("v").is_ok_and(|c| c.null_count() == 0))
        };
        let shown = self
            .view
            .type_casts
            .iter()
            .find(|(n, _)| n == column)
            .map(|(_, d)| d);
        let ok_shown = match shown {
            Some(DataType::Date) if base == DataType::String => fits(lenient_date(column)),
            Some(dt) => fits(col(column).strict_cast(dt.clone())),
            None => true,
        };
        let ok_base = fits(col(column).strict_cast(base.clone()));
        if ok_shown && ok_base {
            return Ok((Some(text.to_string()), base));
        }
        let dtype = if ok_shown { &base } else { shown.unwrap() };
        Err(format!("{text:?} doesn't fit {column} ({dtype})"))
    }

    /// `e` / `:edit`: set one cell (row id `rid`) of `column`.
    pub fn edit_cell(&mut self, column: &str, rid: u64, text: &str) -> Result<(), String> {
        self.set_rows(column, Arc::from([rid]), text)
    }

    /// `:setcol` / `ge`: write `text` into `column` on the selected rows.
    pub fn set_col(&mut self, column: &str, text: &str) -> Result<usize, String> {
        let rids: Arc<[u64]> = self.view.selected.iter().copied().collect();
        if rids.is_empty() {
            self.editable()?;
            return Err("no rows selected".into());
        }
        let n = rids.len();
        self.set_rows(column, rids, text)?;
        Ok(n)
    }

    /// `:setexpr` / `g=`: `column` := SQL `expr` on the selected rows. The
    /// expression reads the base columns (as `:derive`), not the view's casts;
    /// its type must fit the column's.
    pub fn set_expr(&mut self, column: &str, expr: &str) -> Result<usize, String> {
        self.editable()?;
        let rids: Arc<[u64]> = self.view.selected.iter().copied().collect();
        if rids.is_empty() {
            return Err("no rows selected".into());
        }
        let e = polars::sql::sql_expr(expr).map_err(|e| format!("expr: {e}"))?;
        // Steps after the insert point write neither `column` nor what `expr`
        // reads, so the current plan has their types there.
        let schema = self
            .plan
            .clone()
            .select([col(column).alias("__t"), e.alias("__v")])
            .collect_schema()
            .map_err(|e| format!("expr: {e}"))?;
        let (Some(want), Some(got)) = (schema.get("__t"), schema.get("__v")) else {
            return Err("expr: no result".into());
        };
        let fits = got == want
            || got == &DataType::Null
            || (want.is_numeric() && got.is_numeric())
            || (want.is_temporal()
                && got.is_temporal()
                && !matches!(want, DataType::Duration(_))
                && !matches!(got, DataType::Duration(_)))
            || (want == &DataType::String && !got.is_nested());
        if !fits {
            return Err(format!("expr: {got} doesn't fit {column} ({want})"));
        }
        let n = rids.len();
        let step = PlanStep::SetExpr {
            column: column.to_string(),
            expr: expr.to_string(),
            dtype: want.clone(),
            rids,
        };
        let before = self.snapshot();
        self.insert_edit(column, step).map_err(|e| e.to_string())?;
        self.push_undo(before);
        Ok(n)
    }

    fn set_rows(&mut self, column: &str, rids: Arc<[u64]>, text: &str) -> Result<(), String> {
        self.editable()?;
        let (value, dtype) = self.edit_value(column, text)?;
        let step = PlanStep::SetCol {
            column: column.to_string(),
            value,
            dtype,
            rids,
        };
        let before = self.snapshot();
        self.insert_edit(column, step).map_err(|e| e.to_string())?;
        self.push_undo(before);
        Ok(())
    }

    /// The sheet's edit steps, in plan order.
    pub fn edits(&self) -> Vec<PlanStep> {
        self.state
            .steps
            .iter()
            .filter(|s| s.is_edit())
            .cloned()
            .collect()
    }

    /// Edits not in the last `:save` (or open), plus saved ones undone since.
    pub fn unsaved_edits(&self) -> usize {
        let (now, saved) = (self.edits(), &self.state.saved_edits);
        now.iter().filter(|e| !saved.contains(e)).count()
            + saved.iter().filter(|e| !now.contains(e)).count()
    }

    /// Whether the base plan has `name` (hidden columns count: filter/sort may use them).
    pub fn has_column(&self, name: &str) -> bool {
        self.plan
            .clone()
            .collect_schema()
            .is_ok_and(|s| s.contains(name))
    }

    /// Row ids of view rows whose `column` (as text) matches `pattern`.
    pub fn rows_matching(&self, column: &str, pattern: &str) -> PolarsResult<Vec<u64>> {
        let df = self
            .filtered_plan(true)
            .filter(
                col(column)
                    .cast(DataType::String)
                    .str()
                    .contains(lit(pattern), true),
            )
            .select([col(ROW_ID)])
            .collect()?;
        rid_values(&df)
    }

    /// Row ids of view rows whose `column` equals one of `values` (`,`).
    pub fn rids_with_values(&self, column: &str, values: &[AnyValue]) -> PolarsResult<Vec<u64>> {
        let df = self
            .filtered_plan(true)
            .filter(values_predicate(column, values))
            .select([col(ROW_ID)])
            .collect()?;
        rid_values(&df)
    }

    /// Row ids of view rows whose `column` repeats a value seen above (view
    /// order; nulls repeat each other). VisiData `select-col-duplicates`.
    pub fn rids_col_duplicates(&self, column: &str) -> PolarsResult<Vec<u64>> {
        let df = self
            .view_plan_rid(true)
            .select([col(ROW_ID), col(column)])
            .collect()?;
        let firsts: std::collections::HashSet<u64> = rid_values(&df.unique_stable(
            Some(&[column.to_string()]),
            UniqueKeepStrategy::First,
            None,
        )?)?
        .into_iter()
        .collect();
        Ok(rid_values(&df)?
            .into_iter()
            .filter(|r| !firsts.contains(r))
            .collect())
    }

    /// Row ids of every row in the (filtered) view.
    pub fn view_rids(&self) -> PolarsResult<Vec<u64>> {
        rid_values(&self.filtered_plan(true).select([col(ROW_ID)]).collect()?)
    }

    /// Rows of the full view where `pattern` (regex, case-insensitive, as
    /// VisiData) matches `column`, or any visible column when `None`, as
    /// `(view row, first matching column)`, in view order.
    pub fn search_hits(
        &self,
        pattern: &str,
        column: Option<&str>,
    ) -> PolarsResult<Vec<(u64, usize)>> {
        let plan = self.view_plan();
        let names: Vec<String> = plan
            .clone()
            .collect_schema()?
            .iter_names()
            .map(|n| n.to_string())
            .collect();
        let searched: Vec<usize> = (0..names.len())
            .filter(|&i| column.is_none_or(|c| names[i] == c))
            .collect();
        if searched.is_empty() {
            return Ok(Vec::new());
        }
        let hit = |i: usize| format!("__vdrs_hit{i}");
        let pattern = format!("(?i){pattern}");
        let mut select = vec![col("__vdrs_row")];
        select.extend(searched.iter().map(|&i| {
            col(names[i].as_str())
                .cast(DataType::String)
                .str()
                .contains(lit(pattern.as_str()), true)
                .fill_null(false)
                .alias(hit(i).as_str())
        }));
        let any = searched
            .iter()
            .map(|&i| col(hit(i).as_str()))
            .reduce(|a, b| a.or(b))
            .expect("searched is not empty");
        let df = plan
            .with_row_index("__vdrs_row", None)
            .select(select)
            .filter(any)
            .collect()?;
        let rows = df.column("__vdrs_row")?.cast(&DataType::UInt64)?;
        let rows = rows.u64()?;
        let flags = searched
            .iter()
            .map(|&i| Ok((i, df.column(hit(i).as_str())?.bool()?.clone())))
            .collect::<PolarsResult<Vec<_>>>()?;
        Ok((0..df.height())
            .filter_map(|r| {
                let row = rows.get(r)?;
                let c = flags.iter().find(|(_, f)| f.get(r) == Some(true))?.0;
                Some((row, c))
            })
            .collect())
    }

    /// Rows for `y` (the view row at `pos`) or `gy` (`None`: the selected
    /// rows), in view order, every column incl. hidden ones, no row id (S-18 A).
    pub fn copy_rows(&self, pos: Option<u64>) -> PolarsResult<DataFrame> {
        let lf = self.sorted_plan(true);
        let lf = match pos {
            Some(p) => lf.slice(p as i64, 1),
            None => {
                let rids: Vec<u64> = self.view.selected.iter().copied().collect();
                lf.filter(
                    col(ROW_ID).is_in(lit(Series::new("ids".into(), rids)).implode(false), false),
                )
            }
        };
        lf.collect()?.drop(ROW_ID)
    }

    /// Schema-only check of a `:select` / `z|` predicate (no data read).
    pub fn check_predicate(&self, pred: &str) -> Result<(), String> {
        self.predicate(&self.filtered_plan(false), pred).map(|_| ())
    }

    /// `pred` (a SQL expression, S-21) as a filter over the view's rows,
    /// hidden columns included. Refused unless it is a boolean.
    fn predicate(&self, lf: &LazyFrame, pred: &str) -> Result<Expr, String> {
        let e = polars::sql::sql_expr(pred).map_err(|e| format!("expr: {e}"))?;
        let schema = lf
            .clone()
            .select([e.clone().alias("__vdrs_pred")])
            .collect_schema()
            .map_err(|e| format!("expr: {e}"))?;
        match schema.get("__vdrs_pred") {
            Some(DataType::Boolean) => Ok(e.fill_null(false)),
            _ => Err(format!("expr: not a boolean: {pred}")),
        }
    }

    /// View rows (positions in view order) where `pred` holds (`z/`).
    pub fn expr_hits(&self, pred: &str) -> Result<Vec<u64>, String> {
        let lf = self.sorted_plan(false).with_row_index("__vdrs_row", None);
        let p = self.predicate(&lf, pred)?;
        let df = lf
            .filter(p)
            .select([col("__vdrs_row").cast(DataType::UInt64)])
            .collect()
            .map_err(|e| e.to_string())?;
        Ok(df
            .column("__vdrs_row")
            .map_err(|e| e.to_string())?
            .u64()
            .map_err(|e| e.to_string())?
            .into_no_null_iter()
            .collect())
    }

    /// Row ids of view rows where `pred` holds (`:select`, `z|`).
    pub fn rids_where(&self, pred: &str) -> Result<Vec<u64>, String> {
        let lf = self.filtered_plan(true);
        let p = self.predicate(&lf, pred)?;
        let df = lf
            .filter(p)
            .select([col(ROW_ID)])
            .collect()
            .map_err(|e| e.to_string())?;
        rid_values(&df).map_err(|e| e.to_string())
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
/// (`7/3/2018 1:47p` → 2018-07-03). Unparseable values become null.
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

/// The [`ROW_ID`] column of `df` as u64s.
pub fn rid_values(df: &DataFrame) -> PolarsResult<Vec<u64>> {
    let c = df.column(ROW_ID)?.cast(&DataType::UInt64)?;
    Ok(c.u64()?.into_no_null_iter().collect())
}

/// Write a DataFrame to disk, picking the writer by file extension.
/// Supported: csv (default), tsv/tab, parquet/pq, ipc/arrow/feather, jsonl/ndjson, json.
/// An existing file is replaced by rename from a temp file beside it, so a
/// failed save (unsupported column type, full disk) leaves it untouched. Where
/// that's not allowed the data is rendered first and then written in place, so
/// only a failure during that last write can damage the file. A rename gives
/// the file to the saver and detaches hard links.
pub fn write_df_by_extension(df: &mut DataFrame, path: &std::path::Path) -> PolarsResult<()> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    let known = matches!(
        ext.as_deref(),
        None | Some(
            "csv"
                | "tsv"
                | "tab"
                | "parquet"
                | "pq"
                | "ipc"
                | "arrow"
                | "feather"
                | "jsonl"
                | "ndjson"
                | "json"
        )
    );
    if !known {
        let other = ext.unwrap_or_default();
        return Err(PolarsError::ComputeError(
            format!("save: unknown extension '.{other}'").into(),
        ));
    }
    let ext = ext.as_deref();
    let Ok(target) = std::fs::canonicalize(path) else {
        // New file: nothing to protect, and the umask sets its mode. A dangling
        // symlink lands here too: render first, so a failure creates nothing.
        if std::fs::symlink_metadata(path).is_ok() {
            let mut rendered = Vec::new();
            write_df(df, &mut rendered, ext)?;
            return Ok(std::fs::write(path, rendered)?);
        }
        let result = write_df(df, std::fs::File::create(path)?, ext);
        if result.is_err() {
            let _ = std::fs::remove_file(path);
        }
        return result;
    };
    // Same refusal as writing in place: a read-only file stays read-only.
    drop(std::fs::OpenOptions::new().write(true).open(&target)?);
    let dir = target.parent().unwrap_or(std::path::Path::new("."));
    let tmp = tempfile::Builder::new()
        .prefix(".vdrs-save")
        .tempfile_in(dir);
    let tmp = match tmp {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            let mut rendered = Vec::new();
            write_df(df, &mut rendered, ext)?;
            std::fs::write(&target, rendered)?;
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };
    tmp.as_file()
        .set_permissions(std::fs::metadata(&target)?.permissions())?;
    write_df(df, tmp.as_file(), ext)?;
    match tmp.persist(&target) {
        Ok(_) => Ok(()),
        // Sticky directory: we may write the file but not replace it.
        Err(e) if e.error.kind() == std::io::ErrorKind::PermissionDenied => {
            std::fs::copy(e.file.path(), &target)?;
            Ok(())
        }
        Err(e) => Err(e.error.into()),
    }
}

fn write_df<W: std::io::Write>(df: &mut DataFrame, file: W, ext: Option<&str>) -> PolarsResult<()> {
    match ext {
        Some("tsv" | "tab") => CsvWriter::new(file).with_separator(b'\t').finish(df),
        Some("parquet" | "pq") => ParquetWriter::new(file).finish(df).map(|_| ()),
        Some("ipc" | "arrow" | "feather") => IpcWriter::new(file).finish(df),
        Some("jsonl" | "ndjson") => JsonWriter::new(file)
            .with_json_format(JsonFormat::JsonLines)
            .finish(df),
        Some("json") => JsonWriter::new(file)
            .with_json_format(JsonFormat::Json)
            .finish(df),
        _ => CsvWriter::new(file).finish(df),
    }
}
