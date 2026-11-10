//! Sheet = Source + LazyFrame plan + View + materialized `window`. See S-01.

use std::sync::Arc;

use polars::prelude::*;

use crate::reader::{LoadedSource, SourceKind};

pub(crate) mod columns;
mod derive;
mod diff;
mod nested;
mod refresh;
mod step;
mod tail;
#[cfg(test)]
mod tests;
mod undo;

pub(crate) use derive::{JL, JLK, JOIN_RIGHT_KEYS, JR, JRK};
pub use nested::{UNFURL_ROW, Unfurl, unfurl_kind};
pub use refresh::{Refreshed, refresh};
pub use step::{ADDED_RID, POS, PlanStep};
pub use undo::{Snapshot, UndoTree};

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
            Origin::Sql { .. } => "sql",
            Origin::Describe { .. } => "describe",
            Origin::Lua { .. } => "lua",
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
    /// Row count of the filtered view; `None` while counting or after a failed count.
    total_rows: Option<u64>,
    /// `Sheet::count_key` that `total_rows` belongs to (or is being counted for).
    /// `None` forces a recount on the next materialize.
    count_key: Option<String>,
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
            aggregators: Vec::new(),
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

    /// Build a tiny in-memory sheet listing every sheet in the stack.
    #[expect(clippy::expect_used, reason = "columns built here have one length")]
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
        Self::meta(id, "<sheets>".into(), df, Origin::SheetSwitcher)
    }

    /// An in-memory meta-sheet (sheet list, columns, undo tree) showing `df`.
    /// Its row ids are positions (the view numbers its rows), known now, so
    /// row keys work before the first materialize lands.
    fn meta(id: SheetId, name: String, df: DataFrame, origin: Origin) -> Self {
        let window_rids = (0..df.height() as u64).collect();
        Sheet {
            id,
            name,
            kind: SourceKind::Csv, // placeholder; this is a meta-sheet
            source: Arc::new(LoadedSource::empty()),
            plan: df.clone().lazy(),
            view: View::default(),
            window: Some(df),
            revision: 0,
            state: SheetState {
                window_rids,
                ..Default::default()
            },
            origin,
        }
    }

    /// Status-line stats of `column` over the selected rows (S-30 C), as
    /// (name, value) pairs: numbers get sum avg min max, other types count
    /// and distinct. View casts apply; filtered-out selected rows count too.
    pub fn selection_stats(&self, column: &str) -> PolarsResult<Vec<(&'static str, String)>> {
        let rids: Vec<u64> = self.view.selected.iter().copied().collect();
        let c = || col(column);
        let lf = self.filtered_plan(true).filter(rid_in(&rids)).select([c()]);
        let dtype = lf
            .clone()
            .collect_schema()?
            .get(column)
            .cloned()
            .unwrap_or(DataType::Null);
        let mut out = vec![("sel", grouped(rids.len() as u64))];
        if dtype.is_primitive_numeric() {
            let df = lf
                .select([
                    c().sum().cast(DataType::Float64).alias("sum"),
                    c().mean().alias("avg"),
                    c().min().cast(DataType::Float64).alias("min"),
                    c().max().cast(DataType::Float64).alias("max"),
                ])
                .collect()?;
            for name in ["sum", "avg", "min", "max"] {
                let v = df.column(name)?.f64()?.get(0);
                out.push((name, v.map_or("·".into(), stat_num)));
            }
        } else {
            let df = lf
                .select([
                    c().count().alias("count"),
                    c().drop_nulls().n_unique().alias("distinct"),
                ])
                .collect()?;
            for name in ["count", "distinct"] {
                let v = df
                    .column(name)?
                    .cast(&DataType::UInt64)?
                    .u64()?
                    .get(0)
                    .unwrap_or(0);
                out.push((name, grouped(v)));
            }
        }
        Ok(out)
    }

    /// `:hits needle` (S-28 B): `row` (from 1), `column`, `value` and the key
    /// columns' values of each hit of `needle` in `parent`'s view, at most
    /// `cap`. Also whether the cap cut it short.
    /// `:notes` (S-30 B): a sheet of the notes on `parent`'s cells, `row` = its
    /// view position now (empty: filtered out or gone); `Enter` jumps, as `:hits`.
    pub fn notes_from(
        parent: &Sheet,
        id: SheetId,
        notes: &crate::notes::Notes,
    ) -> Result<Self, String> {
        use crate::notes::Row;
        let e = |e: PolarsError| e.to_string();
        let view = parent.view_plan_rid(true).collect().map_err(e)?;
        let rids = rid_values(&view).map_err(e)?;
        let keys = &parent.view.keys;
        let rows: Vec<Vec<String>> = match keys.is_empty() {
            true => Vec::new(),
            false => (0..view.height())
                .map(|r| key_text(&view, keys, r))
                .collect(),
        };
        let at = |row: &Row| {
            match row {
                Row::Id(r) => rids.iter().position(|x| x == r),
                Row::Keys(k) => rows.iter().position(|v| v == k),
            }
            .map(|p| p as u64 + 1)
        };
        let ns = &notes.notes;
        let df = DataFrame::new_infer_height(vec![
            Column::new(
                "row".into(),
                ns.iter().map(|n| at(&n.row)).collect::<Vec<_>>(),
            ),
            Column::new(
                "key".into(),
                ns.iter()
                    .map(|n| match &n.row {
                        Row::Id(r) => format!("id {r}"),
                        Row::Keys(k) => k.join(" · "),
                    })
                    .collect::<Vec<_>>(),
            ),
            Column::new(
                "column".into(),
                ns.iter().map(|n| n.column.clone()).collect::<Vec<_>>(),
            ),
            Column::new(
                "note".into(),
                ns.iter().map(|n| n.text.clone()).collect::<Vec<_>>(),
            ),
            Column::new(
                "stale?".into(),
                ns.iter().map(|n| notes.stale(n)).collect::<Vec<_>>(),
            ),
            Column::new(
                "created".into(),
                ns.iter()
                    .map(|n| n.created as i64 * 1000)
                    .collect::<Vec<_>>(),
            )
            .cast(&DataType::Datetime(TimeUnit::Milliseconds, None))
            .map_err(e)?,
        ])
        .map_err(e)?;
        let name = format!("notes: {}", parent.name);
        Ok(Self::meta(id, name, df, Origin::Hits { parent: parent.id }))
    }

    pub fn hits_from(
        parent: &Sheet,
        id: SheetId,
        needle: &str,
        cap: usize,
    ) -> Result<(Self, bool), String> {
        let e = |e: PolarsError| e.to_string();
        let mut hits = parent.search_hits(needle, None).map_err(e)?;
        let capped = hits.len() > cap;
        hits.truncate(cap);
        let view = parent.view_plan().collect().map_err(e)?;
        let names: Vec<String> = view
            .get_column_names()
            .iter()
            .map(|n| n.to_string())
            .collect();
        let text = |name: &str| -> Result<Series, String> {
            Ok(view
                .column(name)
                .map_err(e)?
                .cast(&DataType::String)
                .map_err(e)?
                .as_materialized_series()
                .clone())
        };
        let texts: Vec<Series> = names.iter().map(|n| text(n)).collect::<Result<_, _>>()?;
        let at = |c: usize, r: u64| {
            texts[c]
                .str()
                .ok()
                .and_then(|s| s.get(r as usize))
                .map(String::from)
        };
        let mut cols = vec![
            Column::new(
                "row".into(),
                hits.iter().map(|(r, _)| r + 1).collect::<Vec<u64>>(),
            ),
            Column::new(
                "column".into(),
                hits.iter()
                    .map(|(_, c)| names[*c].clone())
                    .collect::<Vec<_>>(),
            ),
            Column::new(
                "value".into(),
                hits.iter().map(|(r, c)| at(*c, *r)).collect::<Vec<_>>(),
            ),
        ];
        for k in &parent.view.keys {
            let Some(i) = names.iter().position(|n| n == k) else {
                continue;
            };
            if matches!(k.as_str(), "row" | "column" | "value") {
                continue;
            }
            cols.push(Column::new(
                k.as_str().into(),
                hits.iter().map(|(r, _)| at(i, *r)).collect::<Vec<_>>(),
            ));
        }
        let df = DataFrame::new_infer_height(cols).map_err(e)?;
        let name = format!("hits({needle}): {}", parent.name);
        Ok((
            Self::meta(id, name, df, Origin::Hits { parent: parent.id }),
            capped,
        ))
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
            Column::new(what.into(), parts).into_frame().lazy()
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
        let (rids, df) = match rid_values(&df) {
            Ok(r) => (r, df.drop(ROW_ID).unwrap_or(df)),
            Err(_) => (Vec::new(), df), // pre-built windows carry no ids
        };
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
            .chain(v.aggregators.iter_mut().map(|(c, _)| c));
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
        if text.is_empty() {
            return Ok((None, self.base_type(column)?));
        }
        let base = self
            .edit_values(column, &[Some(text.to_string())])?
            .dtype()
            .clone();
        let text = match base {
            DataType::Boolean => text.to_ascii_lowercase(),
            _ => text.to_string(),
        };
        Ok((Some(text), base))
    }

    fn base_type(&self, column: &str) -> Result<DataType, String> {
        self.plan
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?
            .get(column)
            .cloned()
            .ok_or_else(|| format!("no column: {column}"))
    }

    /// `edit_value` for many texts at once (`gzp`): the values in the column's
    /// base type, or the first text that doesn't fit. None = null.
    pub fn edit_values(&self, column: &str, texts: &[Option<String>]) -> Result<Series, String> {
        let base = self.base_type(column)?;
        let refuse = |i: usize, dtype: &dyn std::fmt::Display| {
            let t = texts[i].as_deref().unwrap_or_default();
            format!("{t:?} doesn't fit {column} ({dtype})")
        };
        if base == DataType::Boolean {
            // Polars doesn't cast text to bool
            let vals = texts.iter().enumerate().map(|(i, t)| {
                match t.as_deref().map(str::to_ascii_lowercase).as_deref() {
                    None => Ok(None),
                    Some("true") => Ok(Some(true)),
                    Some("false") => Ok(Some(false)),
                    Some(_) => Err(refuse(i, &"bool: true/false")),
                }
            });
            let vals: Vec<Option<bool>> = vals.collect::<Result<_, _>>()?;
            return Ok(Series::new(column.into(), vals));
        }
        let df = DataFrame::new_infer_height(vec![Column::new(column.into(), texts)])
            .map_err(|e| e.to_string())?;
        // The first text a (non-strict) cast turns into null.
        let first_bad = |e: Expr| -> Result<Option<usize>, String> {
            let out = df
                .clone()
                .lazy()
                .select([e.alias("v")])
                .collect()
                .map_err(|e| e.to_string())?;
            let v = out.column("v").map_err(|e| e.to_string())?;
            Ok((0..texts.len())
                .find(|&i| texts[i].is_some() && v.get(i).is_ok_and(|x| x.is_null())))
        };
        let shown = self
            .view
            .type_casts
            .iter()
            .find(|(n, _)| n == column)
            .map(|(_, d)| d);
        let shown_e = match shown {
            Some(DataType::Date) if base == DataType::String => Some(lenient_date(column)),
            Some(dt) => Some(col(column).cast(dt.clone())),
            None => None,
        };
        if let (Some(e), Some(dt)) = (shown_e, shown)
            && let Some(i) = first_bad(e)?
        {
            return Err(refuse(i, dt));
        }
        if let Some(i) = first_bad(col(column).cast(base.clone()))? {
            return Err(refuse(i, &base));
        }
        let out = df
            .lazy()
            .select([col(column).cast(base)])
            .collect()
            .map_err(|e| e.to_string())?;
        Ok(out
            .column(column)
            .map_err(|e| e.to_string())?
            .as_materialized_series()
            .clone())
    }

    /// Selected row ids in view order (sort applied); selected rows the
    /// filter hides follow, in row order.
    pub fn selected_in_view_order(&self) -> PolarsResult<Vec<u64>> {
        let sel: Vec<u64> = self.view.selected.iter().copied().collect();
        let df = self
            .sorted_plan(true)
            .filter(rid_in(&sel))
            .select([col(ROW_ID)])
            .collect()?;
        let mut out = rid_values(&df)?;
        let shown: std::collections::HashSet<u64> = out.iter().copied().collect();
        out.extend(sel.into_iter().filter(|r| !shown.contains(r)));
        Ok(out)
    }

    /// The plan carries [`ROW_ID`] (source sheets, see [`with_rids`]).
    // ponytail: a schema lookup per view plan; Polars caches it, a flag would
    // go stale whenever a caller swaps `plan`.
    fn has_rids(&self) -> bool {
        self.plan
            .clone()
            .collect_schema()
            .is_ok_and(|s| s.contains(ROW_ID))
    }

    /// The plan's rows and columns as the user has them (hidden ones too, no
    /// view casts/filter/sort): deleted rows out, no [`ROW_ID`].
    pub fn data_plan(&self) -> LazyFrame {
        if !self.has_rids() {
            return self.plan.clone(); // derived sheets take no deletes
        }
        let deleted = self.deleted_rids();
        let mut lf = self.plan.clone();
        if !deleted.is_empty() {
            lf = lf.filter(rid_in(&deleted).not());
        }
        lf.drop(cols([ROW_ID]))
    }

    /// Added rows' (id, position) pairs, from every `AddRows` step.
    fn added_positions(&self) -> Vec<(u64, f64)> {
        let mut out = Vec::new();
        for s in &self.state.steps {
            if let PlanStep::AddRows { rows } = s
                && let (Ok(ids), Ok(pos)) = (rid_values(rows), rows.column(POS))
                && let Ok(pos) = pos.f64()
            {
                out.extend(ids.into_iter().zip(pos.into_no_null_iter()));
            }
        }
        out
    }

    /// `k` positions strictly between `anchor`'s row and the next one in base
    /// order (`after`), or the previous one. No anchor: after the last added
    /// row, or at the top of an empty sheet. Midpoints, so nothing renumbers;
    /// a source row's position is its id, so `:tail` rows still follow.
    /// `None`: the gap is too narrow for `k` distinct positions.
    fn insert_positions(&self, anchor: Option<u64>, after: bool, k: usize) -> Option<Vec<f64>> {
        let added = self.added_positions();
        let pos_of = |rid: u64| {
            if rid < ADDED_RID {
                rid as f64
            } else {
                added
                    .iter()
                    .find(|(r, _)| *r == rid)
                    .map_or(0.0, |(_, p)| *p)
            }
        };
        let (lo, hi) = match (anchor.map(pos_of), after) {
            (Some(p), true) => {
                let next = added.iter().map(|(_, q)| *q).filter(|q| *q > p);
                (p, next.fold(p.floor() + 1.0, f64::min))
            }
            (Some(p), false) => {
                let prev = added.iter().map(|(_, q)| *q).filter(|q| *q < p);
                (prev.fold(p.ceil() - 1.0, f64::max), p)
            }
            (None, _) => {
                let last = added.iter().map(|(_, q)| *q).fold(-1.0, f64::max);
                (last, last.floor() + 1.0)
            }
        };
        let out: Vec<f64> = (1..=k)
            .map(|i| lo + (hi - lo) * i as f64 / (k + 1) as f64)
            .collect();
        let fits = std::iter::once(lo)
            .chain(out.iter().copied())
            .chain([hi])
            .collect::<Vec<_>>()
            .windows(2)
            .all(|w| w[0] < w[1]);
        fits.then_some(out)
    }

    /// Spread the added rows evenly over each gap between source rows,
    /// keeping their order, so midpoints have room again (inserting after
    /// the row just added halves the gap every time).
    fn respace_added(&mut self) -> PolarsResult<()> {
        let mut all: Vec<(f64, usize, usize)> = Vec::new(); // (pos, step, row)
        for (i, s) in self.state.steps.iter().enumerate() {
            if let PlanStep::AddRows { rows } = s {
                let pos = rows.column(POS)?.f64()?;
                all.extend(pos.into_no_null_iter().enumerate().map(|(j, p)| (p, i, j)));
            }
        }
        // view order: position, then step and row order (stable sort)
        all.sort_by(|a, b| a.0.total_cmp(&b.0).then((a.1, a.2).cmp(&(b.1, b.2))));
        let mut new = std::collections::HashMap::new();
        for gap in all.chunk_by(|a, b| a.0.floor() == b.0.floor()) {
            let g = gap[0].0.floor();
            for (r, &(_, i, j)) in gap.iter().enumerate() {
                new.insert((i, j), g + (r + 1) as f64 / (gap.len() + 1) as f64);
            }
        }
        for (i, s) in self.state.steps.iter_mut().enumerate() {
            if let PlanStep::AddRows { rows } = s {
                let pos: Vec<f64> = (0..rows.height()).map(|j| new[&(i, j)]).collect();
                let mut df = (**rows).clone();
                df.with_column(Column::new(POS.into(), pos))?;
                *rows = Arc::new(df);
            }
        }
        Ok(())
    }

    /// `a` `ga` `p` `P`: `n` blank rows, or `values` (copied rows, matched by
    /// column name; missing columns stay null, a value that doesn't convert
    /// refuses all; columns the sheet doesn't have are added, as VisiData
    /// does, #1377), after or before `anchor` in base order. Returns the new
    /// rows' ids.
    pub fn add_rows(
        &mut self,
        anchor: Option<u64>,
        after: bool,
        values: Option<&DataFrame>,
        n: usize,
    ) -> Result<Vec<u64>, String> {
        let before = self.snapshot();
        let ids = self.insert_rows(anchor, after, values, n)?;
        self.push_undo(before);
        Ok(ids)
    }

    /// [`Self::add_rows`] without the undo step.
    fn insert_rows(
        &mut self,
        anchor: Option<u64>,
        after: bool,
        values: Option<&DataFrame>,
        n: usize,
    ) -> Result<Vec<u64>, String> {
        self.editable()?;
        let n = values.map_or(n, DataFrame::height);
        if n == 0 {
            return Err("nothing to add".into());
        }
        let next = self
            .added_positions()
            .iter()
            .map(|(r, _)| r + 1)
            .max()
            .unwrap_or(ADDED_RID);
        let ids: Vec<u64> = (next..next + n as u64).collect();
        let pos = match self.insert_positions(anchor, after, n) {
            Some(p) => p,
            None => {
                self.respace_added().map_err(|e| e.to_string())?;
                self.insert_positions(anchor, after, n)
                    .ok_or("too many rows to add here")?
            }
        };
        let mut cols = vec![
            Column::new(ROW_ID.into(), &ids),
            Column::new(POS.into(), pos),
        ];
        if let Some(values) = values {
            let shown = self
                .plan
                .clone()
                .collect_schema()
                .map_err(|e| e.to_string())?;
            for c in values.columns() {
                if !shown.contains(c.name()) && c.name() != ROW_ID {
                    cols.push(c.clone()); // a new column
                }
            }
            let mut base = (*self
                .tail_base()
                .and_then(|mut b| b.collect_schema())
                .map_err(|e| e.to_string())?)
            .clone();
            // columns an earlier paste added are base columns too
            for s in &self.state.steps {
                if let PlanStep::AddRows { rows } = s {
                    for (n, d) in rows.schema().iter() {
                        if n != ROW_ID && n != POS && !base.contains(n) {
                            base.insert(n.clone(), d.clone());
                        }
                    }
                }
            }
            for (name, dtype) in base.iter() {
                let mut now = name.to_string(); // its name after this sheet's renames
                for s in &self.state.steps {
                    if let PlanStep::Rename { old, new } = s
                        && *old == now
                    {
                        now = new.clone();
                    }
                }
                if let Ok(c) = values.column(&now) {
                    let refuse = |_| format!("{now}: {} values don't fit {dtype}", c.dtype());
                    let json = self.state.steps.iter().find_map(|s| match s {
                        PlanStep::Json { column, dtype } if *column == now => Some(dtype),
                        _ => None,
                    });
                    if let (Some(dt), Ok(text)) = (json, c.str()) {
                        // `:type json` decodes every row: refuse what it can't
                        let _decoded = text
                            .json_decode(Some(dt.clone()), None)
                            .map_err(|e| format!("{now}: {e}"))?;
                    }
                    let c = if c.dtype() == &DataType::String {
                        // text goes in as `e` takes it: bool words, the shown type
                        let texts = column_texts(c).map_err(|e| e.to_string())?;
                        let typed = self.edit_values(&now, &texts)?;
                        match typed.dtype() == dtype {
                            true => typed.into_column(),
                            false => c.strict_cast(dtype).map_err(refuse)?,
                        }
                    } else {
                        c.strict_cast(dtype).map_err(refuse)?
                    };
                    cols.push(c.with_name(name.clone()));
                }
            }
        }
        let rows = DataFrame::new_infer_height(cols).map_err(|e| e.to_string())?;
        let at = self
            .state
            .steps
            .iter()
            .take_while(|s| matches!(s, PlanStep::AddRows { .. }))
            .count();
        self.state.steps.insert(
            at,
            PlanStep::AddRows {
                rows: Arc::new(rows),
            },
        );
        if let Err(e) = self.rebuild_plan() {
            self.state.steps.remove(at);
            return Err(e.to_string());
        }
        self.invalidate(Reason::Rows);
        Ok(ids)
    }

    /// Absolute view row of row `rid`, if the view shows it.
    pub fn view_index_of(&self, rid: u64) -> PolarsResult<Option<u64>> {
        let df = self.sorted_plan(true).select([col(ROW_ID)]).collect()?;
        Ok(rid_values(&df)?
            .iter()
            .position(|r| *r == rid)
            .map(|i| i as u64))
    }

    /// `JobDone::Loaded`: the source, read into memory, becomes the base.
    pub fn set_memory_base(&mut self, df: DataFrame) {
        let base = df.lazy();
        if self.state.steps.is_empty() {
            self.plan = with_rids(base.clone());
        }
        self.state.memory_base = Some(base);
    }

    /// Row ids of deleted rows (`DeleteRows` steps), ascending.
    pub fn deleted_rids(&self) -> Vec<u64> {
        let mut out: Vec<u64> = self
            .state
            .steps
            .iter()
            .filter_map(|s| match s {
                PlanStep::DeleteRows { rids } => Some(rids.iter().copied()),
                _ => None,
            })
            .flatten()
            .collect();
        out.sort_unstable();
        out
    }

    /// `d` / `gd`: delete rows `rids`; they leave the selection too. Returns
    /// how many were deleted.
    pub fn delete_rows(&mut self, rids: Vec<u64>) -> Result<usize, String> {
        self.editable()?;
        if rids.is_empty() {
            return Err("no rows selected".into());
        }
        self.checkpoint();
        let sel = Arc::make_mut(&mut self.view.selected);
        for r in &rids {
            sel.remove(r);
        }
        let n = rids.len();
        self.state
            .steps
            .push(PlanStep::DeleteRows { rids: rids.into() });
        self.invalidate(Reason::Rows);
        Ok(n)
    }

    /// `gzp` (VisiData setcol-clipboard): copied `values` into `column` on the
    /// selected rows in view order, repeating them to fill. All of them must
    /// fit, or nothing changes.
    pub fn paste_cells(&mut self, column: &str, values: &Column) -> Result<usize, String> {
        self.editable()?;
        if values.is_empty() {
            return Err("nothing to paste".into());
        }
        let rids = self.selected_in_view_order().map_err(|e| e.to_string())?;
        if rids.is_empty() {
            return Err("no rows selected".into());
        }
        let texts = column_texts(values).map_err(|e| e.to_string())?;
        let typed = self.edit_values(column, &texts)?;
        if let [one] = &texts[..] {
            return self.set_col(column, one.as_deref().unwrap_or_default());
        }
        let n = rids.len();
        let idx = IdxCa::from_vec(
            "i".into(),
            (0..n).map(|i| (i % texts.len()) as IdxSize).collect(),
        );
        let vals = typed.take(&idx).map_err(|e| e.to_string())?;
        let patch = DataFrame::new_infer_height(vec![
            Column::new("__vdrs_rid".into(), rids),
            vals.with_name("__vdrs_v".into()).into_column(),
        ])
        .map_err(|e| e.to_string())?;
        let step = PlanStep::SetCells {
            column: column.to_string(),
            patch: Arc::new(patch),
        };
        let before = self.snapshot();
        self.insert_edit(column, step).map_err(|e| e.to_string())?;
        self.push_undo(before);
        Ok(n)
    }

    /// `zP` `gzP` (VisiData `pasteFromClipboard`): line `i` of `lines` into
    /// row `rids[i]`, its fields into `cols` left to right; a short line
    /// leaves the cells past it alone, extra fields are dropped. Lines past
    /// the last row go into new rows after the view's last row. An empty
    /// field is null, except in text columns. All of it fits, or nothing
    /// changes; one `U` takes it back. Returns cells written.
    pub fn paste_region(
        &mut self,
        cols: &[String],
        mut rids: Vec<u64>,
        lines: &[Vec<String>],
    ) -> Result<usize, String> {
        self.editable()?;
        if lines.is_empty() || cols.is_empty() {
            return Err("nothing to paste".into());
        }
        rids.truncate(lines.len());
        let before = self.snapshot();
        match self.paste_region_steps(cols, &mut rids, lines) {
            Ok(n) => {
                self.push_undo(before);
                Ok(n)
            }
            Err(e) => {
                self.restore(before);
                Err(e)
            }
        }
    }

    fn paste_region_steps(
        &mut self,
        cols: &[String],
        rids: &mut Vec<u64>,
        lines: &[Vec<String>],
    ) -> Result<usize, String> {
        if lines.len() > rids.len() {
            let last = self
                .sorted_plan(true)
                .select([col(ROW_ID)])
                .tail(1)
                .collect();
            let last = rid_values(&last.map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
                .first()
                .copied();
            rids.extend(self.insert_rows(last, true, None, lines.len() - rids.len())?);
        }
        let mut n = 0;
        for (i, column) in cols.iter().enumerate() {
            let text = self.base_type(column)? == DataType::String;
            let (ids, texts): (Vec<u64>, Vec<Option<String>>) = rids
                .iter()
                .zip(lines)
                .filter_map(|(r, l)| {
                    let v = l.get(i)?;
                    Some((*r, (text || !v.is_empty()).then(|| v.clone())))
                })
                .unzip();
            if ids.is_empty() {
                continue;
            }
            let vals = self.edit_values(column, &texts)?;
            n += ids.len();
            let patch = DataFrame::new_infer_height(vec![
                Column::new(ROW_ID.into(), ids),
                vals.with_name("__vdrs_v".into()).into_column(),
            ])
            .map_err(|e| e.to_string())?;
            let step = PlanStep::SetCells {
                column: column.clone(),
                patch: Arc::new(patch),
            };
            self.insert_edit(column, step).map_err(|e| e.to_string())?;
        }
        Ok(n)
    }

    /// Row ids of view rows `from..from + n` (fewer at the end).
    pub fn view_rids_at(&self, from: u64, n: usize) -> PolarsResult<Vec<u64>> {
        let df = self
            .sorted_plan(true)
            .select([col(ROW_ID)])
            .slice(from as i64, n as IdxSize)
            .collect()?;
        rid_values(&df)
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

    /// `:s` / `g*` (S-28): replace `pattern` in text `column` on `rids` (`None`:
    /// every row). `undo`: push an undo step (`*` made its own with the derive).
    pub fn replace(
        &mut self,
        column: &str,
        pattern: &str,
        replacement: &str,
        all: bool,
        rids: Option<Arc<[u64]>>,
        undo: bool,
    ) -> Result<(), String> {
        self.editable()?;
        regex::Regex::new(pattern).map_err(|e| format!("bad regex: {e}"))?;
        let dtype = self
            .plan
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?
            .get(column)
            .cloned()
            .ok_or_else(|| format!("no column: {column}"))?;
        if dtype != DataType::String {
            return Err(format!(
                "{column} is {dtype}: replace works on text (:type string)"
            ));
        }
        let step = PlanStep::Replace {
            column: column.to_string(),
            pattern: pattern.to_string(),
            replacement: replacement.to_string(),
            all,
            rids,
        };
        let before = self.snapshot();
        self.insert_edit(column, step).map_err(|e| e.to_string())?;
        if undo {
            self.push_undo(before);
        }
        Ok(())
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
        let Some(any) = searched
            .iter()
            .map(|&i| col(hit(i).as_str()))
            .reduce(|a, b| a.or(b))
        else {
            return Ok(Vec::new());
        };
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
    /// `base`: values as stored, not as the view's casts (`#` `%` `@`) show
    /// them, so `p` pastes the row itself (the system clipboard gets the shown text).
    pub fn copy_rows(&self, pos: Option<u64>, base: bool) -> PolarsResult<DataFrame> {
        let lf = self.sorted_plan(true);
        let mut lf = match pos {
            Some(p) => lf.slice(p as i64, 1),
            None => {
                let rids: Vec<u64> = self.view.selected.iter().copied().collect();
                lf.filter(rid_in(&rids))
            }
        };
        if base && !self.view.type_casts.is_empty() {
            let order = lf
                .collect_schema()?
                .iter_names()
                .cloned()
                .collect::<Vec<_>>();
            let cast: Vec<&str> = self
                .view
                .type_casts
                .iter()
                .map(|(c, _)| c.as_str())
                .collect();
            let mut raw = self.plan.clone();
            if !self.has_rids() {
                raw = raw.with_row_index(ROW_ID, None); // as `filtered_plan_with` numbers them
            }
            let raw = raw.select(
                std::iter::once(col(ROW_ID).cast(DataType::UInt64))
                    .chain(cast.iter().map(|c| col(*c)))
                    .collect::<Vec<_>>(),
            );
            let args = JoinArgs {
                maintain_order: MaintainOrderJoin::Left,
                ..JoinArgs::new(JoinType::Left)
            };
            lf = lf
                .with_column(col(ROW_ID).cast(DataType::UInt64))
                .drop(cols(cast))
                .join(raw, [col(ROW_ID)], [col(ROW_ID)], args)
                .select(order.into_iter().map(col).collect::<Vec<_>>());
        }
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

/// A value as `cast(String)` prints it: the text `values_predicate` compares.
/// (`AnyValue`'s `Display` rounds floats and shortens datetimes.)
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

/// The row's [`ROW_ID`] is one of `rids`.
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
fn stat_num(v: f64) -> String {
    let r = (v * 100.0).round() / 100.0;
    let int = grouped(r.trunc().abs() as u64);
    let sign = if r < 0.0 { "-" } else { "" };
    let frac = format!("{:.2}", r.fract().abs());
    match frac.trim_end_matches('0').trim_end_matches('.') {
        "0" | "" => format!("{sign}{int}"),
        f => format!("{sign}{int}{}", &f[1..]),
    }
}

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
