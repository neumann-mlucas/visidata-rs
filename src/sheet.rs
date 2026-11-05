//! Sheet = Source + LazyFrame plan + View + materialized `window`. See S-01.
//!
//! Phase 0 keeps this narrow: no derived columns, no filters-as-expressions,
//! no sort state yet. Those land as the UI needs them.

use std::sync::Arc;

use polars::prelude::*;

use crate::reader::{LoadedSource, SourceKind};

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
}

/// Incremental tail: only bytes past `consumed` are parsed on refresh.
#[derive(Clone)]
pub struct TailCache {
    pub consumed: u64,
    pub df: DataFrame,
}

/// A change made to a sheet's base `plan` (as opposed to `View` state).
#[derive(Debug, Clone, PartialEq)]
pub enum PlanStep {
    Rename {
        old: String,
        new: String,
    },
    /// VisiData `$`: strip `[^0-9.-]`, cast to f64.
    Currency {
        column: String,
    },
    /// `:derive name = <sql expr>`.
    Derive {
        name: String,
        expr: String,
    },
    /// `:setcol`: `column` := `value` on rows `rids`.
    SetCol {
        column: String,
        value: String,
        dtype: DataType,
        rids: Vec<u64>,
    },
}

/// Everything `U` restores.
#[derive(Clone)]
pub struct Snapshot {
    pub plan: LazyFrame,
    pub view: View,
    pub steps: Vec<PlanStep>,
}

/// Undo depth per sheet.
const UNDO_MAX: usize = 100;

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
    pub sort: Vec<SortKey>,
    /// Selected rows by [`ROW_ID`]; survive sort and filter.
    pub selected: std::collections::BTreeSet<u64>,
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
            sort: Vec::new(),
            selected: std::collections::BTreeSet::new(),
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
    pub needle: String,
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

/// Stable row identity: the row's position in the sheet's base `plan`. Sorting,
/// filtering and casts don't change it, so selections key on it.
pub const ROW_ID: &str = "__vdrs_rid";

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
        let df = DataFrame::new(vec![
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

    /// Build a frequency-table sheet grouping by `col_name` with counts, desc.
    /// `percent` column added alongside `count` to match VisiData's freq output
    /// (count / total * 100, 2-decimal). No `histogram` column — users who need
    /// one can hide-col the extras.
    pub fn freq_from(parent: &Sheet, id: SheetId, col_name: &str) -> Self {
        let plan = parent
            .view_plan()
            .group_by([col(col_name)])
            .agg([len().alias("count")])
            .with_columns([(col("count").cast(DataType::Float64) * lit(100.0f64)
                / col("count").cast(DataType::Float64).sum())
            .round(2)
            .alias("percent")])
            // Value breaks count ties: row order must repeat across collects,
            // since row ids (selection, freq dive) are positions in this plan.
            .sort(
                ["count", col_name],
                SortMultipleOptions::default()
                    .with_order_descending_multi([true, false])
                    .with_nulls_last(true),
            );
        Sheet {
            id,
            name: format!("freq({}): {}", col_name, parent.name),
            kind: parent.kind,
            source: parent.source.clone(),
            plan,
            view: View::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin: Origin::Freq {
                parent: parent.id,
                column: col_name.to_string(),
            },
        }
    }

    /// Build a pivot sheet. Eager: collects parent, then pivots.
    /// Direction §8: Polars 2.x may spill on group-bys but pivots stay in-memory.
    /// ponytail: eager pivot, document OOM risk, swap to streaming when a user complains.
    pub fn pivot_from(
        parent: &Sheet,
        id: SheetId,
        index: &str,
        on: &str,
        values: &str,
        agg: &str,
    ) -> Result<Self, String> {
        use polars_ops::pivot::{PivotAgg, pivot_stable};
        let pivot_agg = match agg.to_ascii_lowercase().as_str() {
            "sum" => PivotAgg::Sum,
            "mean" | "avg" | "average" => PivotAgg::Mean,
            "count" | "len" => PivotAgg::Count,
            "max" => PivotAgg::Max,
            "min" => PivotAgg::Min,
            "first" => PivotAgg::First,
            "last" => PivotAgg::Last,
            other => return Err(format!("unknown agg: {other}")),
        };
        let df = parent.view_plan().collect().map_err(|e| e.to_string())?;
        let out = pivot_stable(
            &df,
            [on],
            Some([index]),
            Some([values]),
            false,
            Some(pivot_agg),
            None,
        )
        .map_err(|e| e.to_string())?;
        let plan = out.clone().lazy();
        Ok(Sheet {
            id,
            name: format!("pivot({index}×{on}→{values}): {}", parent.name),
            kind: parent.kind,
            source: parent.source.clone(),
            plan,
            view: View::default(),
            window: Some(out),
            revision: 0,
            state: Default::default(),
            origin: Origin::Pivot { parent: parent.id },
        })
    }

    /// Build a sheet containing only the parent's selected rows.
    /// Eager: collects parent, filters by row index. VisiData `"` semantic.
    pub fn dup_selected_from(parent: &Sheet, id: SheetId) -> Result<Self, String> {
        if parent.view.selected.is_empty() {
            return Err("no rows selected".into());
        }
        // Selected row ids, in the parent's current view order and shape.
        let df = parent
            .view_plan_rid(true)
            .collect()
            .map_err(|e| e.to_string())?;
        let rids = rid_values(&df).map_err(|e| e.to_string())?;
        let bc: BooleanChunked = rids
            .iter()
            .map(|r| parent.view.selected.contains(r))
            .collect();
        let out = df
            .filter(&bc)
            .and_then(|d| d.drop(ROW_ID))
            .map_err(|e| e.to_string())?;
        let plan = out.clone().lazy();
        Ok(Sheet {
            id,
            name: format!(
                "selected ({} rows): {}",
                parent.view.selected.len(),
                parent.name
            ),
            kind: parent.kind,
            source: parent.source.clone(),
            plan,
            view: View::default(),
            window: Some(out),
            revision: 0,
            state: Default::default(),
            origin: Origin::Selected { parent: parent.id },
        })
    }

    /// Open a tail-mode sheet. The path is re-scanned on each tail tick to pick up appended rows.
    /// Phase 4 minimum: full re-read each tick (ok for small/medium files; incremental read later).
    pub fn tail_from(
        id: SheetId,
        loaded: LoadedSource,
        follow: bool,
    ) -> Result<Self, crate::reader::OpenError> {
        let Some(path) = loaded.path.clone() else {
            return Err(crate::reader::OpenError::UnknownFormat {
                path: std::path::PathBuf::from(&loaded.display_name),
            });
        };
        let kind = loaded.kind;
        let plan = loaded.lazy.clone();
        let display = loaded.display_name.clone();
        let mut sheet = Sheet {
            id,
            name: format!("tail: {display}"),
            kind,
            source: Arc::new(loaded),
            plan,
            view: View::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin: Origin::Tail { path, follow },
        };
        if sheet.tails_incrementally() {
            sheet.reload_tail_cache()?;
        }
        Ok(sheet)
    }

    /// Plain (not gzipped) CSV/TSV/JSONL files grow by appending lines, so a
    /// refresh can parse just the new bytes. Others (Parquet dirs, …) re-scan.
    fn tails_incrementally(&self) -> bool {
        let Origin::Tail { path, .. } = &self.origin else {
            return false;
        };
        matches!(
            self.kind,
            SourceKind::Csv | SourceKind::Tsv | SourceKind::Jsonl
        ) && path.is_file()
            && !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("gz"))
    }

    /// Bytes `[from..]` of the tail file up to its last newline (a partial
    /// last line waits for the next refresh).
    fn tail_bytes(&self, from: u64) -> std::io::Result<Vec<u8>> {
        use std::io::{Read, Seek, SeekFrom};
        let Origin::Tail { path, .. } = &self.origin else {
            return Ok(Vec::new());
        };
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(from))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        buf.truncate(buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1));
        Ok(buf)
    }

    /// Read the whole tail file into the cache (first load, or after truncation).
    fn reload_tail_cache(&mut self) -> Result<(), crate::reader::OpenError> {
        let bytes = self.tail_bytes(0)?;
        let consumed = bytes.len() as u64;
        let df = crate::reader::parse_chunk(self.kind, bytes, None)?;
        self.plan = df.clone().lazy();
        self.state.tail = Some(TailCache { consumed, df });
        Ok(())
    }

    /// Pick up appended rows. Returns whether anything changed (an unchanged
    /// file isn't re-materialized or recounted).
    pub fn refresh_tail(&mut self) -> Result<bool, crate::reader::OpenError> {
        let Origin::Tail { path, .. } = &self.origin else {
            return Ok(false);
        };
        let Some(cache) = &self.state.tail else {
            let loaded = crate::reader::open(path.as_os_str(), Some(self.kind))?;
            self.plan = loaded.lazy;
            self.view.count_key = None;
            return Ok(true);
        };
        let len = std::fs::metadata(path)?.len();
        if len < cache.consumed {
            self.reload_tail_cache()?; // truncated or rotated: start over
        } else {
            let new = self.tail_bytes(cache.consumed)?;
            if new.is_empty() {
                return Ok(false);
            }
            let added = new.len() as u64;
            let schema = Arc::new(cache.df.schema());
            match crate::reader::parse_chunk(self.kind, new, Some(schema)) {
                Ok(more) => {
                    let cache = self.state.tail.as_mut().expect("checked above");
                    cache.df.vstack_mut(&more)?;
                    cache.consumed += added;
                    self.plan = cache.df.clone().lazy();
                }
                Err(_) => self.reload_tail_cache()?, // new rows don't fit the schema
            }
        }
        self.view.count_key = None;
        Ok(true)
    }

    /// List sheet for a multi-part xlsx/sqlite file.
    pub fn parts_list(id: SheetId, loaded: LoadedSource, parts: Vec<String>) -> Self {
        let path = loaded.path.clone().unwrap_or_default();
        let kind = loaded.kind;
        let what = if kind == SourceKind::Sqlite {
            "table"
        } else {
            "sheet"
        };
        let df = DataFrame::new(vec![Column::new(what.into(), parts)]).expect("one column");
        Sheet {
            id,
            name: format!("{}: {what}s", loaded.display_name),
            kind,
            source: Arc::new(loaded),
            plan: df.lazy(),
            view: View::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin: Origin::Parts { path, kind },
        }
    }

    /// Build a schema-diff sheet comparing `parent` with `other`.
    ///
    /// Columns: `column`, `dtype_a`, `dtype_b`, `status` (same | dtype_mismatch | only_a | only_b).
    /// Row diff (full-outer join on keys) is tracked separately — see tasks.md Phase 5.
    pub fn schema_diff_from(
        parent: &Sheet,
        id: SheetId,
        other: &LoadedSource,
    ) -> Result<Self, String> {
        let schema_a = parent
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        let schema_b = other
            .lazy
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?;

        // Column union, preserving A order then B-only
        let mut names: Vec<String> = schema_a.iter_names().map(|n| n.to_string()).collect();
        for n in schema_b.iter_names() {
            if schema_a.get(n.as_str()).is_none() {
                names.push(n.to_string());
            }
        }

        let mut col_names: Vec<String> = Vec::with_capacity(names.len());
        let mut dtypes_a: Vec<String> = Vec::with_capacity(names.len());
        let mut dtypes_b: Vec<String> = Vec::with_capacity(names.len());
        let mut statuses: Vec<String> = Vec::with_capacity(names.len());
        for name in &names {
            let a = schema_a.get(name.as_str()).map(|d| format!("{d}"));
            let b = schema_b.get(name.as_str()).map(|d| format!("{d}"));
            let status = match (&a, &b) {
                (Some(_), None) => "only_a",
                (None, Some(_)) => "only_b",
                (Some(ad), Some(bd)) if ad == bd => "same",
                _ => "dtype_mismatch",
            };
            col_names.push(name.clone());
            dtypes_a.push(a.unwrap_or_default());
            dtypes_b.push(b.unwrap_or_default());
            statuses.push(status.to_string());
        }
        let df = DataFrame::new(vec![
            Column::new("column".into(), col_names),
            Column::new("dtype_a".into(), dtypes_a),
            Column::new("dtype_b".into(), dtypes_b),
            Column::new("status".into(), statuses),
        ])
        .map_err(|e| e.to_string())?;
        let plan = df.clone().lazy();
        let other_name = other.display_name.clone();
        Ok(Sheet {
            id,
            name: format!("schemadiff: {} ↔ {}", parent.name, other_name),
            kind: parent.kind,
            source: parent.source.clone(),
            plan,
            view: View::default(),
            window: Some(df),
            revision: 0,
            state: Default::default(),
            origin: Origin::Diff {
                left: parent.id,
                right_name: other_name,
            },
        })
    }

    /// Row-diff sheet. Full outer join on `keys`; a `__diff` column tags each row.
    /// Rows marked "same" are filtered out — only mismatches surface.
    pub fn row_diff_from(
        parent: &Sheet,
        id: SheetId,
        other: &LoadedSource,
        keys: &[String],
    ) -> Result<Self, String> {
        if keys.is_empty() {
            return Err("row_diff needs at least one key column".into());
        }
        let key_exprs: Vec<Expr> = keys.iter().map(|k| col(k.as_str())).collect();
        let a = parent.view_plan();
        let b = other.lazy.clone();
        let joined = a.join(
            b,
            key_exprs.clone(),
            key_exprs,
            JoinArgs::new(JoinType::Full).with_coalesce(JoinCoalesce::CoalesceColumns),
        );
        // Pick a non-key column from each side to detect side presence. If none, the whole
        // row is key-only; we treat that as always-matching.
        let schema_a = parent
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        let schema_b = other
            .lazy
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        let probe_a = schema_a
            .iter_names()
            .find(|n| !keys.iter().any(|k| k == n.as_str()))
            .map(|n| n.to_string());
        let probe_b = schema_b
            .iter_names()
            .find(|n| !keys.iter().any(|k| k == n.as_str()))
            .map(|n| n.to_string());

        // __diff classifier
        let only_a = match &probe_b {
            Some(c) => col(format!("{c}_right").as_str()).is_null(),
            None => lit(false),
        };
        let only_b = match &probe_a {
            Some(c) => col(c.as_str()).is_null(),
            None => lit(false),
        };
        let diff_col = when(only_a.clone())
            .then(lit("only_a"))
            .when(only_b.clone())
            .then(lit("only_b"))
            .otherwise(lit("changed"))
            .alias("__diff");

        // Build value-mismatch predicate across shared columns
        let shared: Vec<String> = schema_a
            .iter_names()
            .filter(|n| schema_b.get(n.as_str()).is_some() && !keys.iter().any(|k| k == n.as_str()))
            .map(|n| n.to_string())
            .collect();
        let mismatch_expr = if shared.is_empty() {
            lit(false)
        } else {
            shared
                .iter()
                .map(|c| col(c.as_str()).neq(col(format!("{c}_right").as_str())))
                .reduce(|a, b| a.or(b))
                .unwrap()
        };

        // Final: only_a OR only_b OR mismatch
        let plan = joined
            .with_column(diff_col)
            .filter(only_a.or(only_b).or(mismatch_expr));

        let other_name = other.display_name.clone();
        Ok(Sheet {
            id,
            name: format!(
                "rowdiff({}): {} ↔ {}",
                keys.join(","),
                parent.name,
                other_name
            ),
            kind: parent.kind,
            source: parent.source.clone(),
            plan,
            view: View::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin: Origin::Diff {
                left: parent.id,
                right_name: other_name,
            },
        })
    }

    /// A sheet derived from `parent` with a fresh view.
    fn child(parent: &Sheet, id: SheetId, name: String, plan: LazyFrame, origin: Origin) -> Self {
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

    /// Freq dive: `parent`'s rows whose `column` equals one of `values`
    /// (compared as text, the way the freq sheet grouped them; null matches null).
    pub fn rows_with_values(
        parent: &Sheet,
        id: SheetId,
        column: &str,
        values: &[AnyValue],
    ) -> Self {
        let text: Vec<String> = values
            .iter()
            .filter(|v| !v.is_null())
            .map(|v| v.get_str().map_or_else(|| v.to_string(), String::from))
            .collect();
        let c = || col(column);
        let mut pred = c()
            .cast(DataType::String)
            .is_in(lit(Series::new("v".into(), &text)));
        if values.iter().any(AnyValue::is_null) {
            pred = pred.or(c().is_null());
        }
        let label = match values {
            [one] => format!(
                "{column}={}",
                one.get_str().map_or_else(|| one.to_string(), String::from)
            ),
            many => format!("{column} in {} values", many.len()),
        };
        let origin = Origin::Selected { parent: parent.id };
        Self::child(
            parent,
            id,
            format!("{label}: {}", parent.name),
            parent.view_plan().filter(pred),
            origin,
        )
    }

    /// `g"` / `:dup`: an independent copy of what `parent` shows.
    pub fn copy_from(parent: &Sheet, id: SheetId) -> Self {
        let origin = Origin::Copy { parent: parent.id };
        Self::child(
            parent,
            id,
            format!("copy of {}", parent.name),
            parent.view_plan(),
            origin,
        )
    }

    /// `:sql <query>`: `self` is `parent`'s view; `others` are extra tables.
    pub fn sql_from(
        parent: &Sheet,
        id: SheetId,
        query: &str,
        others: Vec<(String, LazyFrame)>,
    ) -> Result<Self, String> {
        let mut ctx = polars::sql::SQLContext::new();
        ctx.register("self", parent.view_plan());
        for (name, plan) in others {
            ctx.register(&name, plan);
        }
        let mut plan = ctx.execute(query).map_err(|e| e.to_string())?;
        plan.collect_schema().map_err(|e| e.to_string())?;
        let origin = Origin::Sql { parent: parent.id };
        Ok(Self::child(
            parent,
            id,
            format!("sql: {query}"),
            plan,
            origin,
        ))
    }

    /// `I` (VisiData describe-sheet): one row per visible column with
    /// type, count, nulls, distinct, min, max, mean, median. Eager.
    pub fn describe_from(parent: &Sheet, id: SheetId) -> Result<Self, String> {
        let mut lf = parent.view_plan();
        let schema = lf.collect_schema().map_err(|e| e.to_string())?;
        let stat = |i: usize, s: &str| format!("{i}\u{1f}{s}");
        let mut aggs = Vec::new();
        for (i, (name, dt)) in schema.iter().enumerate() {
            let c = || col(name.as_str());
            aggs.push(c().count().cast(DataType::UInt64).alias(stat(i, "count")));
            aggs.push(
                c().null_count()
                    .cast(DataType::UInt64)
                    .alias(stat(i, "nulls")),
            );
            aggs.push(
                c().n_unique()
                    .cast(DataType::UInt64)
                    .alias(stat(i, "distinct")),
            );
            let ordered = !dt.is_nested();
            let text_stat = |e: Expr, s| {
                if ordered {
                    e.cast(DataType::String)
                } else {
                    lit(NULL).cast(DataType::String)
                }
                .alias(stat(i, s))
            };
            aggs.push(text_stat(c().min(), "min"));
            aggs.push(text_stat(c().max(), "max"));
            let num_stat = |e: Expr, s| {
                if dt.is_numeric() {
                    e.cast(DataType::Float64)
                } else {
                    lit(NULL).cast(DataType::Float64)
                }
                .alias(stat(i, s))
            };
            aggs.push(num_stat(c().mean(), "mean"));
            aggs.push(num_stat(c().median(), "median"));
        }
        let row = lf.select(aggs).collect().map_err(|e| e.to_string())?;
        let get = |i: usize, s: &str| {
            row.column(&stat(i, s))
                .and_then(|c| c.get(0).map(|v| v.into_static()))
        };
        let mut names = Vec::new();
        let mut types = Vec::new();
        let (mut count, mut nulls, mut distinct) = (Vec::new(), Vec::new(), Vec::new());
        let (mut min, mut max, mut mean, mut median) =
            (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        for (i, (name, dt)) in schema.iter().enumerate() {
            names.push(name.to_string());
            types.push(dt.to_string());
            let u = |s| get(i, s).ok().and_then(|v| v.extract::<u64>());
            let f = |s| get(i, s).ok().and_then(|v| v.extract::<f64>());
            let t = |s| get(i, s).ok().and_then(|v| v.get_str().map(String::from));
            count.push(u("count"));
            nulls.push(u("nulls"));
            distinct.push(u("distinct"));
            min.push(t("min"));
            max.push(t("max"));
            mean.push(f("mean"));
            median.push(f("median"));
        }
        let df = DataFrame::new(vec![
            Column::new("column".into(), names),
            Column::new("type".into(), types),
            Column::new("count".into(), count),
            Column::new("nulls".into(), nulls),
            Column::new("distinct".into(), distinct),
            Column::new("min".into(), min),
            Column::new("max".into(), max),
            Column::new("mean".into(), mean),
            Column::new("median".into(), median),
        ])
        .map_err(|e| e.to_string())?;
        let origin = Origin::Describe { parent: parent.id };
        Ok(Self::child(
            parent,
            id,
            format!("describe: {}", parent.name),
            df.lazy(),
            origin,
        ))
    }

    /// `W`: count pivot of the current column `on`, rows keyed by `keys`.
    /// No keys → one row of counts, one column per distinct value (first-seen order).
    pub fn pivot_current_from(
        parent: &Sheet,
        id: SheetId,
        keys: &[String],
        on: &str,
    ) -> Result<Self, String> {
        let name = format!("pivot({on}): {}", parent.name);
        let origin = Origin::Pivot { parent: parent.id };
        let counted = parent
            .view_plan()
            .group_by_stable(
                keys.iter()
                    .chain([&on.to_string()])
                    .map(|k| col(k.as_str()))
                    .collect::<Vec<_>>(),
            )
            .agg([len().alias("__vdrs_n")])
            .with_column(col(on).cast(DataType::String))
            .collect()
            .map_err(|e| e.to_string())?;
        let out = if keys.is_empty() {
            let vals = counted
                .column(on)
                .map_err(|e| e.to_string())?
                .str()
                .map_err(|e| e.to_string())?
                .clone();
            let ns = counted
                .column("__vdrs_n")
                .map_err(|e| e.to_string())?
                .cast(&DataType::Int64)
                .map_err(|e| e.to_string())?;
            let ns = ns.i64().map_err(|e| e.to_string())?;
            let cols: Vec<Column> = vals
                .into_iter()
                .zip(ns)
                .map(|(v, n)| Column::new(v.unwrap_or("null").into(), [n.unwrap_or(0)]))
                .collect();
            DataFrame::new(cols).map_err(|e| e.to_string())?
        } else {
            use polars_ops::pivot::{PivotAgg, pivot_stable};
            pivot_stable(
                &counted,
                [on],
                Some(keys),
                Some(["__vdrs_n"]),
                false,
                Some(PivotAgg::First),
                None,
            )
            .map_err(|e| e.to_string())?
        };
        Ok(Self::child(parent, id, name, out.lazy(), origin))
    }

    /// Columns of the view holding at most one distinct value (`:uniform`).
    pub fn uniform_columns(&self) -> PolarsResult<Vec<String>> {
        let mut lf = self.view_plan();
        let names: Vec<String> = lf
            .collect_schema()?
            .iter_names()
            .map(|n| n.to_string())
            .collect();
        let row = lf
            .select(
                names
                    .iter()
                    .map(|n| col(n.as_str()).n_unique().cast(DataType::UInt64))
                    .collect::<Vec<_>>(),
            )
            .collect()?;
        Ok(names
            .into_iter()
            .filter(|n| {
                row.column(n)
                    .ok()
                    .and_then(|c| c.get(0).ok()?.extract::<u64>())
                    .is_some_and(|d| d <= 1)
            })
            .collect())
    }

    /// `:join`: `left`'s view joined with `right` on same-named `keys`.
    /// `how`: inner | left | full | merge (full, right's non-null values win) |
    /// append (stack rows, union columns, tag each with `origin_sheet`).
    pub fn join_from(
        left: &Sheet,
        id: SheetId,
        right: LazyFrame,
        right_name: &str,
        keys: &[String],
        how: &str,
    ) -> Result<Self, String> {
        let l = left.view_plan();
        let plan = if how == "append" {
            let tag = |p: LazyFrame, name: &str| {
                p.with_column(lit(name.to_string()).alias("origin_sheet"))
            };
            let args = UnionArgs {
                to_supertypes: true,
                diagonal: true,
                ..Default::default()
            };
            concat([tag(l, &left.name), tag(right, right_name)], args).map_err(|e| e.to_string())?
        } else {
            let jt = match how {
                "inner" => JoinType::Inner,
                "left" => JoinType::Left,
                "full" | "merge" => JoinType::Full,
                other => return Err(format!("unknown join type: {other}")),
            };
            let on: Vec<Expr> = keys.iter().map(|k| col(k.as_str())).collect();
            let args = JoinArgs::new(jt).with_coalesce(JoinCoalesce::CoalesceColumns);
            let mut joined = l.join(right, on.clone(), on, args);
            if how == "merge" {
                // Polars suffixes colliding right columns `_right`; fold them in.
                let names: Vec<String> = joined
                    .collect_schema()
                    .map_err(|e| e.to_string())?
                    .iter_names()
                    .map(|n| n.to_string())
                    .collect();
                let dups: Vec<&str> = names
                    .iter()
                    .filter_map(|n| n.strip_suffix("_right"))
                    .filter(|base| names.iter().any(|n| n == base))
                    .collect();
                let merged: Vec<Expr> = dups
                    .iter()
                    .map(|b| {
                        let r = format!("{b}_right");
                        col(r.as_str()).fill_null(col(*b)).alias(*b)
                    })
                    .collect();
                let drop: Vec<String> = dups.iter().map(|b| format!("{b}_right")).collect();
                joined = joined.with_columns(merged).drop(drop);
            }
            joined
        };
        Ok(Sheet {
            id,
            name: format!("{}&{} ({how})", left.name, right_name),
            kind: left.kind,
            source: left.source.clone(),
            plan,
            view: View::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin: Origin::Join {
                left: left.id,
                right_name: right_name.to_string(),
            },
        })
    }

    /// Build an unpivot (melt) sheet. `on` = columns to unpivot; `index` = keep columns.
    pub fn unpivot_from(parent: &Sheet, id: SheetId, on: &[String], index: &[String]) -> Self {
        use polars::prelude::UnpivotArgsDSL;
        let args = UnpivotArgsDSL {
            on: on
                .iter()
                .map(|c| polars::prelude::Selector::new(col(c.as_str())))
                .collect(),
            index: index
                .iter()
                .map(|c| polars::prelude::Selector::new(col(c.as_str())))
                .collect(),
            variable_name: None,
            value_name: None,
        };
        let plan = parent.view_plan().unpivot(args);
        Sheet {
            id,
            name: format!("unpivot({}): {}", on.join(","), parent.name),
            kind: parent.kind,
            source: parent.source.clone(),
            plan,
            view: View::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin: Origin::Unpivot { parent: parent.id },
        }
    }

    /// Build the lazy query for the current `window`: `view_plan` + slice.
    pub fn window_plan(&self) -> LazyFrame {
        self.view_plan()
            .slice(self.view.window_offset as i64, self.view.window_rows)
    }

    /// `window_plan` plus a trailing [`ROW_ID`] column (split off by the app
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

    /// `view_plan`, optionally carrying each row's [`ROW_ID`] as the last column.
    fn view_plan_rid(&self, with_rid: bool) -> LazyFrame {
        let mut lf = self.filtered_plan(with_rid);
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
        // Visible-column projection: keys first (in key-col order), then schema
        // order for the rest; drop hidden. Skipped when nothing to project.
        if !self.view.hidden.is_empty() || !self.view.keys.is_empty() {
            if let Ok(schema) = lf.clone().collect_schema() {
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
        }
        lf
    }

    /// Base plan + type_casts + filter: decides which rows exist, not their order.
    /// `with_rid` numbers base rows first, so ids survive filter and sort.
    fn filtered_plan(&self, with_rid: bool) -> LazyFrame {
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
            lf = lf.with_columns(exprs);
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
        lf
    }

    /// One-row `len` query over `filtered_plan`. Sort/projection don't change
    /// the row count, so they're skipped to keep the count cheap.
    pub fn count_plan(&self) -> LazyFrame {
        self.filtered_plan(false).select([len()])
    }

    /// Identifies the row count `count_plan` yields; when it changes, recount.
    /// Plan replacements (tail refresh, currency cast) reset `view.count_key` instead.
    pub fn count_key(&self) -> String {
        format!("{:?}|{:?}", self.view.filter, self.view.type_casts)
    }

    /// Remember the current plan/view for `U`; a new change forgets redo history.
    pub fn checkpoint(&mut self) {
        let snap = self.snapshot();
        let undo = &mut self.state.undo;
        undo.push(snap);
        if undo.len() > UNDO_MAX {
            undo.remove(0);
        }
        self.state.redo.clear();
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            plan: self.plan.clone(),
            view: self.view.clone(),
            steps: self.state.steps.clone(),
        }
    }

    /// Step back (`undo = true`) or forward through history. False when empty.
    pub fn time_travel(&mut self, undo: bool) -> bool {
        let (from, to) = if undo {
            (&mut self.state.undo, &mut self.state.redo)
        } else {
            (&mut self.state.redo, &mut self.state.undo)
        };
        let Some(snap) = from.pop() else {
            return false;
        };
        to.push(Snapshot {
            plan: std::mem::replace(&mut self.plan, snap.plan),
            view: std::mem::replace(&mut self.view, snap.view),
            steps: std::mem::replace(&mut self.state.steps, snap.steps),
        });
        self.view.count_key = None; // plan may differ: recount
        true
    }

    /// `:derive name = <sql expr>` on the base plan. Validated against the schema.
    pub fn derive(&mut self, name: &str, expr: &str) -> Result<(), String> {
        let e = polars::sql::sql_expr(expr).map_err(|e| e.to_string())?;
        let plan = self.plan.clone().with_column(e.alias(name));
        plan.clone().collect_schema().map_err(|e| e.to_string())?;
        self.checkpoint();
        self.plan = plan;
        self.state.steps.push(PlanStep::Derive {
            name: name.to_string(),
            expr: expr.to_string(),
        });
        Ok(())
    }

    /// `:setcol`: write `value` into `column` on the selected rows. The value
    /// must fit the column's type (checked here, not at collect time).
    pub fn set_col(&mut self, column: &str, value: &str) -> Result<usize, String> {
        let rids: Vec<u64> = self.view.selected.iter().copied().collect();
        if rids.is_empty() {
            return Err("no rows selected".into());
        }
        let dtype = self
            .plan
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?
            .get(column)
            .cloned()
            .ok_or_else(|| format!("no column: {column}"))?;
        df!("v" => [value])
            .and_then(|d| {
                d.lazy()
                    .select([col("v").strict_cast(dtype.clone())])
                    .collect()
            })
            .map_err(|_| format!("{value:?} doesn't fit {column} ({dtype})"))?;
        let ids = Series::new("ids".into(), &rids);
        self.checkpoint();
        self.plan = self
            .plan
            .clone()
            .with_row_index("__vdrs_set", None)
            .with_column(
                when(col("__vdrs_set").cast(DataType::UInt64).is_in(lit(ids)))
                    .then(lit(value.to_string()).strict_cast(dtype.clone()))
                    .otherwise(col(column))
                    .alias(column),
            )
            .drop(["__vdrs_set"]);
        self.state.steps.push(PlanStep::SetCol {
            column: column.to_string(),
            value: value.to_string(),
            dtype: dtype.clone(),
            rids: rids.clone(),
        });
        self.view.count_key = None;
        Ok(rids.len())
    }

    /// Whether the base plan has `name` (hidden columns count: filter/sort may use them).
    pub fn has_column(&self, name: &str) -> bool {
        self.plan
            .clone()
            .collect_schema()
            .is_ok_and(|s| s.contains(name))
    }

    /// View rows whose `column` (as text) matches regex `pattern`, in view order.
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

    /// Row ids of every row in the (filtered) view.
    pub fn view_rids(&self) -> PolarsResult<Vec<u64>> {
        rid_values(&self.filtered_plan(true).select([col(ROW_ID)]).collect()?)
    }

    /// Rows of the full view holding `needle` (case-insensitive, literal) in any
    /// column, as `(view row, first matching column)`, in view order.
    pub fn search_hits(&self, needle: &str) -> PolarsResult<Vec<(u64, usize)>> {
        let plan = self.view_plan();
        let names: Vec<String> = plan
            .clone()
            .collect_schema()?
            .iter_names()
            .map(|n| n.to_string())
            .collect();
        let width = names.len();
        if width == 0 {
            return Ok(Vec::new());
        }
        let hit = |i: usize| format!("__vdrs_hit{i}");
        let mut select = vec![col("__vdrs_row")];
        select.extend(names.iter().enumerate().map(|(i, n)| {
            col(n.as_str())
                .cast(DataType::String)
                .str()
                .to_lowercase()
                .str()
                .contains_literal(lit(needle.to_lowercase()))
                .fill_null(false)
                .alias(hit(i).as_str())
        }));
        let any = (0..width)
            .map(|i| col(hit(i).as_str()))
            .reduce(|a, b| a.or(b))
            .expect("width > 0");
        let df = plan
            .with_row_index("__vdrs_row", None)
            .select(select)
            .filter(any)
            .collect()?;
        let rows = df.column("__vdrs_row")?.cast(&DataType::UInt64)?;
        let rows = rows.u64()?;
        let flags = (0..width)
            .map(|i| df.column(hit(i).as_str())?.bool().cloned())
            .collect::<PolarsResult<Vec<_>>>()?;
        Ok((0..df.height())
            .filter_map(|r| {
                let row = rows.get(r)?;
                let c = flags.iter().position(|f| f.get(r) == Some(true))?;
                Some((row, c))
            })
            .collect())
    }
}

/// Text → Date trying common layouts in turn; a trailing time part is ignored
/// (`7/3/2018 1:47p` → 2018-07-03). Unparseable values become null.
/// chrono's `%Y` takes any digit count, so numeric layouts only apply to
/// strings of their exact shape (regex guard): `7/3/18` isn't year 7.
fn lenient_date(name: &str) -> Expr {
    const LAYOUTS: [(&str, &str); 12] = [
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
    // drop a time part: "2018-07-03T10:00", "7/3/2018 1:47p"
    let day = col(name).str().strip_chars(lit(NULL)).str().replace(
        lit(r"(T| +)\d{1,2}:\d{2}.*$"),
        lit(""),
        false,
    );
    let tries: Vec<Expr> = LAYOUTS
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

/// The [`ROW_ID`] column of `df` as u64s.
pub fn rid_values(df: &DataFrame) -> PolarsResult<Vec<u64>> {
    let c = df.column(ROW_ID)?.cast(&DataType::UInt64)?;
    Ok(c.u64()?.into_no_null_iter().collect())
}

/// Write a DataFrame to disk, picking the writer by file extension.
/// Supported: csv (default), tsv/tab, parquet/pq, ipc/arrow/feather, jsonl/ndjson, json.
/// Overwrites existing files via `File::create`.
/// Unknown extensions error before the file is opened, so existing files are untouched.
pub fn write_df_by_extension(df: &mut DataFrame, path: &std::path::Path) -> PolarsResult<()> {
    let file = || std::fs::File::create(path);
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("csv") | None => CsvWriter::new(file()?).finish(df).map(|_| ()),
        Some("tsv") | Some("tab") => CsvWriter::new(file()?)
            .with_separator(b'\t')
            .finish(df)
            .map(|_| ()),
        Some("parquet") | Some("pq") => ParquetWriter::new(file()?).finish(df).map(|_| ()),
        Some("ipc") | Some("arrow") | Some("feather") => {
            IpcWriter::new(file()?).finish(df).map(|_| ())
        }
        Some("jsonl") | Some("ndjson") => JsonWriter::new(file()?)
            .with_json_format(JsonFormat::JsonLines)
            .finish(df)
            .map(|_| ()),
        Some("json") => JsonWriter::new(file()?)
            .with_json_format(JsonFormat::Json)
            .finish(df)
            .map(|_| ()),
        Some(other) => Err(PolarsError::ComputeError(
            format!("save: unknown extension '.{other}'").into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::{self, SourceKind};
    use std::io::Write;

    fn make_sheet() -> (Sheet, tempfile::NamedTempFile) {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "name,n\nalice,3\nbob,1\ncarol,2").unwrap();
        tmp.flush().unwrap();
        let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
        (Sheet::from_source(0, loaded), tmp)
    }

    #[test]
    fn window_plan_hide_key_reorders() {
        let (mut s, _tmp) = make_sheet();
        s.view.keys.push("n".into());
        s.view.hidden.insert("name".into());
        let df = s.window_plan().collect().unwrap();
        let names: Vec<&str> = df
            .get_column_names()
            .into_iter()
            .map(|s| s.as_ref())
            .collect();
        assert_eq!(names, vec!["n"]);
    }

    #[test]
    fn window_plan_type_cast_int() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "n\n42\n17\n5").unwrap();
        tmp.flush().unwrap();
        let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
        let mut s = Sheet::from_source(0, loaded);
        // Even though CSV already inferred Int64, type-cast to Int64 should be a no-op pass.
        s.view.type_casts.push(("n".into(), DataType::Int64));
        let df = s.window_plan().collect().unwrap();
        assert_eq!(df.column("n").unwrap().dtype(), &DataType::Int64);
    }

    #[test]
    fn window_plan_sort_asc() {
        let (mut s, _tmp) = make_sheet();
        s.view.sort.push(SortKey {
            column: "n".into(),
            order: SortOrder::Asc,
        });
        let df = s.window_plan().collect().unwrap();
        let col = df.column("n").unwrap();
        let got: Vec<i64> = (0..col.len())
            .map(|i| col.get(i).unwrap().try_extract::<i64>().unwrap())
            .collect();
        assert_eq!(got, vec![1, 2, 3]);
    }

    #[test]
    fn window_plan_filter_regex() {
        let (mut s, _tmp) = make_sheet();
        s.view.filter = Some(FilterState {
            column: "name".into(),
            regex: "^(alice|bob)$".into(),
        });
        let df = s.window_plan().collect().unwrap();
        assert_eq!(df.height(), 2);
    }

    #[test]
    fn freq_sheet_counts_and_sorts_desc() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "color\nred\nblue\nred\ngreen\nred\nblue").unwrap();
        tmp.flush().unwrap();
        let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
        let parent = Sheet::from_source(0, loaded);
        let freq = Sheet::freq_from(&parent, 1, "color");
        let df = freq.plan.clone().collect().unwrap();
        assert_eq!(df.width(), 3); // color + count + percent
        assert_eq!(df.height(), 3); // red/blue/green
        let counts = df.column("count").unwrap();
        let first: i64 = counts.get(0).unwrap().try_extract().unwrap();
        assert_eq!(first, 3); // red leads
        let pct = df.column("percent").unwrap();
        let first_pct: f64 = pct.get(0).unwrap().try_extract().unwrap();
        assert!((first_pct - 50.0).abs() < 0.01); // 3/6 = 50%
    }

    #[test]
    fn pivot_sum_shape() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            "region,product,qty\nwest,a,1\nwest,b,2\neast,a,3\neast,b,4\nwest,a,5"
        )
        .unwrap();
        tmp.flush().unwrap();
        let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
        let parent = Sheet::from_source(0, loaded);
        let piv = Sheet::pivot_from(&parent, 1, "region", "product", "qty", "sum").unwrap();
        let df = piv.plan.clone().collect().unwrap();
        // columns: region, a, b
        assert_eq!(df.width(), 3);
        assert_eq!(df.height(), 2); // east + west
    }

    #[test]
    fn unpivot_longform() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, "id,a,b\n1,10,20\n2,30,40").unwrap();
        tmp.flush().unwrap();
        let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
        let parent = Sheet::from_source(0, loaded);
        let on = vec!["a".to_string(), "b".to_string()];
        let index = vec!["id".to_string()];
        let s = Sheet::unpivot_from(&parent, 1, &on, &index);
        let df = s.plan.clone().collect().unwrap();
        assert_eq!(df.height(), 4); // 2 rows × 2 unpivoted columns
    }

    #[test]
    fn schema_diff_catches_missing_and_mismatched() {
        let mut ta = tempfile::NamedTempFile::new().unwrap();
        let mut tb = tempfile::NamedTempFile::new().unwrap();
        writeln!(ta, "id,name,age\n1,alice,30").unwrap();
        writeln!(tb, "id,name,salary\n1,alice,100.5").unwrap();
        ta.flush().unwrap();
        tb.flush().unwrap();
        let la = reader::open(ta.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
        let lb = reader::open(tb.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
        let parent = Sheet::from_source(0, la);
        let d = Sheet::schema_diff_from(&parent, 1, &lb).unwrap();
        let df = d.plan.clone().collect().unwrap();
        // 4 columns: id, name, age, salary
        assert_eq!(df.height(), 4);
        let statuses = df.column("status").unwrap();
        let v: Vec<String> = (0..statuses.len())
            .map(|i| match statuses.get(i).unwrap() {
                AnyValue::String(s) => s.to_string(),
                AnyValue::StringOwned(s) => s.as_str().to_string(),
                _ => String::new(),
            })
            .collect();
        assert!(v.contains(&"only_a".to_string())); // age
        assert!(v.contains(&"only_b".to_string())); // salary
        assert!(v.contains(&"same".to_string())); // id, name
    }

    #[test]
    fn row_diff_flags_only_a_only_b_changed() {
        let mut ta = tempfile::NamedTempFile::new().unwrap();
        let mut tb = tempfile::NamedTempFile::new().unwrap();
        // A: 1=alice,30 ; 2=bob,25 ; 3=carol,40
        // B: 1=alice,30 ; 2=bob,99 ; 4=dave,10
        writeln!(ta, "id,name,age\n1,alice,30\n2,bob,25\n3,carol,40").unwrap();
        writeln!(tb, "id,name,age\n1,alice,30\n2,bob,99\n4,dave,10").unwrap();
        ta.flush().unwrap();
        tb.flush().unwrap();
        let la = reader::open(ta.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
        let lb = reader::open(tb.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
        let parent = Sheet::from_source(0, la);
        let d = Sheet::row_diff_from(&parent, 1, &lb, &["id".to_string()]).unwrap();
        let df = d.plan.clone().collect().unwrap();
        // Expect 3 rows (1 unchanged suppressed): id=2 changed, id=3 only_a, id=4 only_b
        assert_eq!(df.height(), 3);
    }

    #[test]
    fn tail_refresh_picks_up_appended_rows() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(tmp, r#"{{"a":1}}"#).unwrap();
        writeln!(tmp, r#"{{"a":2}}"#).unwrap();
        tmp.flush().unwrap();
        // Re-open to force the JSONL kind
        let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Jsonl)).unwrap();
        let mut s = Sheet::tail_from(42, loaded, false).unwrap();
        let before = s.plan.clone().collect().unwrap();
        assert_eq!(before.height(), 2);
        // append more rows
        use std::io::Write as _;
        let mut handle = std::fs::OpenOptions::new()
            .append(true)
            .open(tmp.path())
            .unwrap();
        writeln!(handle, r#"{{"a":3}}"#).unwrap();
        writeln!(handle, r#"{{"a":4}}"#).unwrap();
        handle.flush().unwrap();
        drop(handle);
        assert!(s.refresh_tail().unwrap());
        let after = s.plan.clone().collect().unwrap();
        assert_eq!(after.height(), 4);
        assert!(!s.refresh_tail().unwrap(), "unchanged file → no reload");
        // a partial last line waits; truncation starts over
        let mut handle = std::fs::OpenOptions::new()
            .append(true)
            .open(tmp.path())
            .unwrap();
        write!(handle, r#"{{"a":5"#).unwrap();
        drop(handle);
        assert!(!s.refresh_tail().unwrap());
        std::fs::write(tmp.path(), "{\"a\":9}\n").unwrap();
        assert!(s.refresh_tail().unwrap());
        assert_eq!(s.plan.clone().collect().unwrap().height(), 1);
    }

    #[test]
    fn write_df_roundtrip_csv_parquet_tsv() {
        let mut df = df!(
            "a" => [1i64, 2, 3],
            "b" => ["x", "y", "z"],
        )
        .unwrap();
        for (ext, want_sep) in [
            ("csv", b','),
            ("tsv", b'\t'),
            ("parquet", 0u8),
            ("json", 0),
            ("jsonl", 0),
        ] {
            let path = tempfile::NamedTempFile::new()
                .unwrap()
                .path()
                .with_extension(ext);
            write_df_by_extension(&mut df, &path).unwrap();
            let loaded = reader::open(path.as_os_str(), None).unwrap();
            let back = loaded.lazy.collect().unwrap();
            assert_eq!(back.height(), 3, "ext={ext}");
            assert_eq!(back.width(), 2, "ext={ext}");
            // Separator sanity for text formats
            if want_sep != 0 {
                let bytes = std::fs::read(&path).unwrap();
                assert!(bytes.contains(&want_sep), "ext={ext} missing separator");
            }
            let _ = std::fs::remove_file(&path);
        }
    }

    #[test]
    fn write_df_unknown_extension_errors() {
        let mut df = df!("a" => [1i64]).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.xyz");
        std::fs::write(&path, b"keep me").unwrap();
        let err = write_df_by_extension(&mut df, &path).unwrap_err();
        assert!(err.to_string().contains("unknown extension"));
        assert_eq!(std::fs::read(&path).unwrap(), b"keep me"); // not truncated
    }

    #[test]
    fn window_plan_filter_non_string_column() {
        let (mut s, _tmp) = make_sheet();
        s.view.filter = Some(FilterState {
            column: "n".into(),
            regex: "^3$".into(),
        });
        assert_eq!(s.window_plan().collect().unwrap().height(), 1);
    }

    #[test]
    fn derived_sheets_follow_view() {
        let (mut s, _tmp) = make_sheet(); // alice,3 bob,1 carol,2
        s.view.sort.push(SortKey {
            column: "n".into(),
            order: SortOrder::Asc,
        });
        s.view.selected.insert(1); // row id 1 = bob (base order), first in the sorted view
        let dup = Sheet::dup_selected_from(&s, 1).unwrap();
        let df = dup.plan.collect().unwrap();
        let name = df.column("name").unwrap().get(0).unwrap();
        assert_eq!(name.get_str(), Some("bob"));

        s.view.filter = Some(FilterState {
            column: "name".into(),
            regex: "^a".into(),
        });
        let freq = Sheet::freq_from(&s, 2, "name").plan.collect().unwrap();
        assert_eq!(freq.height(), 1); // only alice survives the filter
    }

    #[test]
    fn count_plan_counts_filtered_rows() {
        let (mut s, _tmp) = make_sheet();
        let count = |s: &Sheet| {
            let df = s.count_plan().collect().unwrap();
            df.get_columns()[0]
                .get(0)
                .unwrap()
                .try_extract::<u64>()
                .unwrap()
        };
        assert_eq!(count(&s), 3);
        let before = s.count_key();
        s.view.filter = Some(FilterState {
            column: "name".into(),
            regex: "^(alice|bob)$".into(),
        });
        assert_ne!(s.count_key(), before);
        assert_eq!(count(&s), 2);
    }

    #[test]
    fn search_hits_scan_full_view_in_view_order() {
        let (mut s, _tmp) = make_sheet(); // alice,3 bob,1 carol,2
        s.view.window_rows = 1; // search must not be limited to the window
        s.view.sort.push(SortKey {
            column: "n".into(),
            order: SortOrder::Asc,
        });
        // sorted: bob,1 carol,2 alice,3 ; "AR" hits carol (name) only, "3" hits alice (n)
        assert_eq!(s.search_hits("AR").unwrap(), vec![(1, 0)]);
        assert_eq!(s.search_hits("3").unwrap(), vec![(2, 1)]);
        assert!(s.search_hits("zzz").unwrap().is_empty());
    }

    #[test]
    fn lenient_date_cast_reads_common_layouts() {
        let mut tmp = tempfile::NamedTempFile::new().unwrap();
        writeln!(
            tmp,
            "d\n7/3/2018 1:47p\n2024-01-02\n03.04.2020\n\"Jan 5, 2021\"\n7/3/18\nnope"
        )
        .unwrap();
        tmp.flush().unwrap();
        let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
        let mut s = Sheet::from_source(0, loaded);
        s.view.type_casts.push(("d".into(), DataType::Date));
        let df = s.view_plan().collect().unwrap();
        let got: Vec<Option<String>> = df
            .column("d")
            .unwrap()
            .cast(&DataType::String)
            .unwrap()
            .str()
            .unwrap()
            .into_iter()
            .map(|v| v.map(String::from))
            .collect();
        let want = [
            "2018-07-03",
            "2024-01-02",
            "2020-04-03",
            "2021-01-05",
            "2018-07-03",
        ];
        let want: Vec<Option<String>> = want
            .iter()
            .map(|d| Some(d.to_string()))
            .chain([None])
            .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn view_plan_is_unsliced() {
        let (mut s, _tmp) = make_sheet();
        s.view.window_rows = 1;
        assert_eq!(s.window_plan().collect().unwrap().height(), 1);
        assert_eq!(s.view_plan().collect().unwrap().height(), 3);
    }

    #[test]
    fn sheets_stack_push_pop() {
        let (parent, _tmp) = make_sheet();
        let mut sheets = Sheets::new(parent);
        assert_eq!(sheets.len(), 1);
        assert!(!sheets.pop()); // can't pop last
        let id = sheets.alloc_id();
        let child = Sheet::freq_from(sheets.active(), id, "n");
        sheets.push(child);
        assert_eq!(sheets.len(), 2);
        assert!(sheets.pop());
        assert_eq!(sheets.len(), 1);
    }
}
