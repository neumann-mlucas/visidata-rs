//! Sheet = Source + LazyFrame plan + View + materialized `window`. See S-01.
//!
//! Phase 0 keeps this narrow: no derived columns, no filters-as-expressions,
//! no sort state yet. Those land as the UI needs them.

use std::sync::Arc;

use polars::prelude::*;

use crate::reader::{LoadedSource, SourceKind};

pub type SheetId = u32;

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
    /// Selected absolute row offsets (VisiData-style). Cleared on plan mutations.
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
    pub current: Option<(usize, usize)>, // (row_in_window, col)
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
            .sort(
                ["count"],
                SortMultipleOptions::default().with_order_descending(true),
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
            origin: Origin::Pivot { parent: parent.id },
        })
    }

    /// Build a sheet containing only the parent's selected rows.
    /// Eager: collects parent, filters by row index. VisiData `"` semantic.
    pub fn dup_selected_from(parent: &Sheet, id: SheetId) -> Result<Self, String> {
        if parent.view.selected.is_empty() {
            return Err("no rows selected".into());
        }
        let df = parent.view_plan().collect().map_err(|e| e.to_string())?;
        let height = df.height() as u64;
        let mask: Vec<bool> = (0..height)
            .map(|i| parent.view.selected.contains(&i))
            .collect();
        let bc: BooleanChunked = mask.into_iter().collect();
        let out = df.filter(&bc).map_err(|e| e.to_string())?;
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
        Ok(Sheet {
            id,
            name: format!("tail: {display}"),
            kind,
            source: Arc::new(loaded),
            plan,
            view: View::default(),
            window: None,
            revision: 0,
            origin: Origin::Tail { path, follow },
        })
    }

    /// Rebuild `plan` from the tail path. Called by the app loop each tail tick.
    pub fn refresh_tail(&mut self) -> Result<(), crate::reader::OpenError> {
        let Origin::Tail { path, .. } = &self.origin else {
            return Ok(());
        };
        let loaded = crate::reader::open(path.as_os_str(), Some(self.kind))?;
        self.plan = loaded.lazy;
        self.view.count_key = None; // file grew: recount
        Ok(())
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
            origin: Origin::Diff {
                left: parent.id,
                right_name: other_name,
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
            origin: Origin::Unpivot { parent: parent.id },
        }
    }

    /// Build the lazy query for the current `window`: `view_plan` + slice.
    pub fn window_plan(&self) -> LazyFrame {
        self.view_plan()
            .slice(self.view.window_offset as i64, self.view.window_rows)
    }

    /// Full (unsliced) query for what the user sees. Applies, in order:
    /// type_casts → filter → sort → visible-column projection.
    pub fn view_plan(&self) -> LazyFrame {
        let mut lf = self.filtered_plan();
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
                let all: Vec<String> = schema.iter_names().map(|n| n.to_string()).collect();
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
    fn filtered_plan(&self) -> LazyFrame {
        let mut lf = self.plan.clone();
        // Type casts first so downstream filter/sort see the typed values.
        if !self.view.type_casts.is_empty() {
            let exprs: Vec<Expr> = self
                .view
                .type_casts
                .iter()
                .map(|(name, dt)| col(name.as_str()).cast(dt.clone()).alias(name.as_str()))
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
        self.filtered_plan().select([len()])
    }

    /// Identifies the row count `count_plan` yields; when it changes, recount.
    /// Plan replacements (tail refresh, currency cast) reset `view.count_key` instead.
    pub fn count_key(&self) -> String {
        format!("{:?}|{:?}", self.view.filter, self.view.type_casts)
    }

    pub fn visible_columns(&self) -> Vec<String> {
        self.window
            .as_ref()
            .map(|df| {
                df.get_column_names()
                    .into_iter()
                    .map(|s| s.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn total_rows_hint(&self) -> Option<usize> {
        // Parquet knows; the rest return None. Phase 0 keeps it dumb.
        None
    }
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
        s.refresh_tail().unwrap();
        let after = s.plan.clone().collect().unwrap();
        assert_eq!(after.height(), 4);
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
        s.view.selected.insert(0); // row 0 of the sorted view = bob
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
