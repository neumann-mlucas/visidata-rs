//! Sheet = Source + LazyFrame plan + View + materialized `window`. See S-01.

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
    /// `:derive name = <sql expr>`; `replaces`: `name` already existed.
    Derive {
        name: String,
        expr: String,
        replaces: bool,
    },
    /// `:setcol`: `column` := `value` on rows `rids`.
    SetCol {
        column: String,
        value: String,
        dtype: DataType,
        rids: Vec<u64>,
    },
}

impl PlanStep {
    /// This edit on top of `plan`. The one definition: edits run through here
    /// when made and again when `:tail` swaps in a grown base table.
    fn apply(&self, plan: LazyFrame) -> PolarsResult<LazyFrame> {
        Ok(match self {
            PlanStep::Rename { old, new } => plan.rename([old.as_str()], [new.as_str()], true),
            PlanStep::Currency { column } => plan.with_columns([col(column.as_str())
                .cast(DataType::String)
                .str()
                .replace_all(lit(r"[^0-9.\-]"), lit(""), false)
                .cast(DataType::Float64)
                .alias(column.as_str())]),
            PlanStep::Derive { name, expr, .. } => {
                plan.with_column(polars::sql::sql_expr(expr)?.alias(name.as_str()))
            }
            PlanStep::SetCol {
                column,
                value,
                dtype,
                rids,
            } => plan
                .with_row_index("__vdrs_set", None)
                .with_column(
                    when(
                        col("__vdrs_set")
                            .cast(DataType::UInt64)
                            .is_in(lit(Series::new("ids".into(), rids)).implode(false), false),
                    )
                    .then(lit(value.to_string()).strict_cast(dtype.clone()))
                    .otherwise(col(column.as_str()))
                    .alias(column.as_str()),
                )
                .drop(cols(["__vdrs_set"])),
        })
    }
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
/// CSVs at most this big move into RAM after opening.
// ponytail: fixed cap, no look at free memory; `/proc/meminfo` or a setting if
// it ever pushes a small machine into swap.
const CSV_MEMORY_BYTES: u64 = 1 << 30;

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

    /// Build a frequency-table sheet grouping by `col_name` with counts, desc.
    /// `percent` (count / total * 100, 2 decimals) and `histogram` (a `■` bar)
    /// follow `count`, as in VisiData's freq output.
    pub fn freq_from(parent: &Sheet, id: SheetId, col_name: &str) -> Self {
        let (count, percent, histogram) = freq_stat_names(col_name);
        let plan = parent
            .view_plan()
            .group_by([col(col_name)])
            .agg([len().alias(count)])
            .with_columns([(col(count).cast(DataType::Float64) * lit(100.0f64)
                / col(count).cast(DataType::Float64).sum())
            .round(2, RoundMode::HalfAwayFromZero)
            .alias(percent)])
            .with_columns([lit("■".repeat(HISTOGRAM_WIDTH))
                .str()
                .head((col(count) * lit(HISTOGRAM_WIDTH as i64)).floor_div(col(count).max()))
                .alias(histogram)])
            // Value breaks count ties: row order must repeat across collects,
            // since row ids (selection, freq dive) are positions in this plan.
            .sort(
                [count, col_name],
                SortMultipleOptions::default()
                    .with_order_descending_multi([true, false])
                    .with_nulls_last(true),
            );
        Sheet::child(
            parent,
            id,
            format!("freq({}): {}", col_name, parent.name),
            plan,
            Origin::Freq {
                parent: parent.id,
                column: col_name.to_string(),
            },
        )
        .derived(
            parent,
            Derivation::Freq {
                column: col_name.to_string(),
            },
        )
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
        let e = element();
        let pivot_agg = match agg.to_ascii_lowercase().as_str() {
            "sum" => e.sum(),
            "mean" | "avg" | "average" => e.mean(),
            "count" | "len" => len(),
            "max" => e.max(),
            "min" => e.min(),
            "first" => e.first(),
            "last" => e.last(),
            other => return Err(format!("unknown agg: {other}")),
        };
        let df = parent.view_plan().collect().map_err(|e| e.to_string())?;
        let out = pivot_df(&df, on, &[index.to_string()], values, pivot_agg)
            .map_err(|e| e.to_string())?;
        let plan = out.clone().lazy();
        Ok(Sheet::child(
            parent,
            id,
            format!("pivot({index}×{on}→{values}): {}", parent.name),
            plan,
            Origin::Pivot { parent: parent.id },
        )
        .with_window(out)
        .derived(
            parent,
            Derivation::Pivot {
                index: index.into(),
                on: on.into(),
                values: values.into(),
                agg: agg.to_ascii_lowercase(),
            },
        ))
    }

    /// Build a sheet containing only the parent's selected rows.
    /// Eager: collects parent, filters by row index. VisiData `"` semantic.
    pub fn dup_selected_from(parent: &Sheet, id: SheetId) -> Result<Self, String> {
        if parent.view.selected.is_empty() {
            return Err("no rows selected".into());
        }
        let name = format!(
            "selected ({} rows): {}",
            parent.view.selected.len(),
            parent.name
        );
        let df = parent
            .view_plan_rid(true)
            .collect()
            .map_err(|e| e.to_string())?;
        Self::rows_from(parent, id, df, &parent.view.selected, name)
    }

    /// VisiData `dedupe-rows`: the first row of each key (each distinct row
    /// when there are no key columns), in view order.
    pub fn dedupe_from(parent: &Sheet, id: SheetId) -> Result<Self, String> {
        let df = parent
            .view_plan_rid(true)
            .collect()
            .map_err(|e| e.to_string())?;
        let mut by: Vec<String> = parent
            .view
            .keys
            .iter()
            .filter(|k| df.column(k).is_ok())
            .cloned()
            .collect();
        if by.is_empty() {
            by = df
                .get_column_names()
                .into_iter()
                .filter(|n| n.as_str() != ROW_ID)
                .map(|n| n.to_string())
                .collect();
        }
        let firsts = df
            .unique_stable(Some(&by), UniqueKeepStrategy::First, None)
            .and_then(|d| rid_values(&d))
            .map_err(|e| e.to_string())?;
        let keep: std::collections::BTreeSet<u64> = firsts.into_iter().collect();
        let name = format!("dedupe ({} rows): {}", keep.len(), parent.name);
        Self::rows_from(parent, id, df, &keep, name)
    }

    /// Rows of `df` (the parent's view with row ids) whose id is in `keep`,
    /// as a sheet rebuilt by row id.
    fn rows_from(
        parent: &Sheet,
        id: SheetId,
        df: DataFrame,
        keep: &std::collections::BTreeSet<u64>,
        name: String,
    ) -> Result<Self, String> {
        let rids = rid_values(&df).map_err(|e| e.to_string())?;
        let bc: BooleanChunked = rids.iter().map(|r| keep.contains(r)).collect();
        let out = df
            .filter(&bc)
            .and_then(|d| d.drop(ROW_ID))
            .map_err(|e| e.to_string())?;
        let plan = out.clone().lazy();
        Ok(Sheet::child(
            parent,
            id,
            name,
            plan,
            Origin::Selected { parent: parent.id },
        )
        .with_window(out)
        .derived(parent, Derivation::Rids(keep.clone())))
    }

    /// Open a tail-mode sheet; `refresh_tail` picks up what the file gains.
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
        if let Origin::Tail { path, .. } = &sheet.origin {
            sheet.state.tail_stamp = tail_stamp(path);
        }
        if sheet.tails_incrementally() {
            sheet.reload_tail_cache()?;
            sheet.rebuild_tail_plan()?;
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

    /// Bytes `[from..]` of the tail file: `(lines, text after the last newline)`.
    fn tail_bytes(&self, from: u64) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
        use std::io::{Read, Seek, SeekFrom};
        let Origin::Tail { path, .. } = &self.origin else {
            return Ok(Default::default());
        };
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(from))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        let cut = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let rest = buf.split_off(cut);
        Ok((buf, rest))
    }

    /// `partial` bytes as a row of `df`'s shape; None when they don't parse yet.
    fn parse_partial(&self, mut partial: Vec<u8>, df: &DataFrame) -> Option<DataFrame> {
        if partial.is_empty() {
            return None;
        }
        partial.push(b'\n');
        crate::reader::parse_chunk(self.kind, partial, Some(df.schema().clone()), false).ok()
    }

    /// Read the whole tail file into the cache (first load, or after truncation).
    fn reload_tail_cache(&mut self) -> Result<(), crate::reader::OpenError> {
        let (mut lines, mut rest) = self.tail_bytes(0)?;
        // `consumed` 0 = a lone header without its newline: reread it all later.
        let consumed = lines.len() as u64;
        if lines.is_empty() {
            lines = std::mem::take(&mut rest);
            lines.push(b'\n');
        }
        let df = crate::reader::parse_chunk(self.kind, lines, None, true)?;
        let partial = self.parse_partial(rest, &df);
        self.state.tail = Some(TailCache {
            consumed,
            df,
            partial,
        });
        Ok(())
    }

    /// The table `state.steps` apply to: rows read so far, or the reopened source.
    fn tail_base(&self) -> PolarsResult<LazyFrame> {
        match &self.state.tail {
            Some(c) => {
                let mut df = c.df.clone();
                if let Some(p) = &c.partial {
                    df.vstack_mut(p)?;
                }
                Ok(df.lazy())
            }
            None => Ok(self.source.lazy.clone()),
        }
    }

    /// Plan = current tail rows + this sheet's edits.
    fn rebuild_tail_plan(&mut self) -> PolarsResult<()> {
        let mut plan = self.tail_base()?;
        for step in &self.state.steps {
            plan = step.apply(plan)?;
        }
        self.plan = plan;
        Ok(())
    }

    /// A tail that can't append (Parquet dir, gzip) whose file changed: the
    /// path to reopen off the UI thread, then hand to `set_tail_source`.
    pub fn tail_needs_reopen(&mut self) -> Option<std::path::PathBuf> {
        let Origin::Tail { path, .. } = &self.origin else {
            return None;
        };
        let path = path.clone();
        let stamp = tail_stamp(&path);
        if self.state.tail.is_some() || stamp == self.state.tail_stamp {
            return None;
        }
        self.state.tail_stamp = stamp;
        Some(path)
    }

    /// The reopened source of a tail that can't append. Holding it keeps a
    /// decompressed tempfile alive.
    pub fn set_tail_source(&mut self, loaded: LoadedSource) -> PolarsResult<()> {
        self.source = Arc::new(loaded);
        self.rebuild_tail_plan()?;
        self.view.count_key = None;
        Ok(())
    }

    /// Pick up rows appended to a CSV/TSV/JSONL tail. Returns whether anything
    /// changed (an unchanged file isn't re-materialized or recounted).
    pub fn refresh_tail(&mut self) -> Result<bool, crate::reader::OpenError> {
        let Origin::Tail { path, .. } = &self.origin else {
            return Ok(false);
        };
        let Some(cache) = &self.state.tail else {
            return Ok(false);
        };
        let stamp = tail_stamp(path);
        if stamp == self.state.tail_stamp {
            return Ok(false);
        }
        self.state.tail_stamp = stamp;
        let header_only = cache.consumed == 0 || cache.df.height() == 0;
        if std::fs::metadata(path)?.len() < cache.consumed || header_only {
            self.reload_tail_cache()?; // truncated, rotated, or types still unknown
        } else {
            let (new, rest) = self.tail_bytes(cache.consumed)?;
            let added = new.len() as u64;
            let schema = cache.df.schema().clone();
            let more = (!new.is_empty())
                .then(|| crate::reader::parse_chunk(self.kind, new, Some(schema), false))
                .transpose();
            match more {
                Ok(more) => {
                    let cache = self.state.tail.as_mut().expect("checked above");
                    if let Some(more) = more {
                        cache.df.vstack_mut(&more)?;
                        cache.consumed += added;
                    }
                    let df = cache.df.clone();
                    let partial = self.parse_partial(rest, &df);
                    self.state.tail.as_mut().expect("checked above").partial = partial;
                }
                Err(_) => self.reload_tail_cache()?, // new rows don't fit the schema
            }
        }
        self.rebuild_tail_plan()?;
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
        let df =
            DataFrame::new_infer_height(vec![Column::new(what.into(), parts)]).expect("one column");
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
        let df = DataFrame::new_infer_height(vec![
            Column::new("column".into(), col_names),
            Column::new("dtype_a".into(), dtypes_a),
            Column::new("dtype_b".into(), dtypes_b),
            Column::new("status".into(), statuses),
        ])
        .map_err(|e| e.to_string())?;
        let plan = df.clone().lazy();
        let other_name = other.display_name.clone();
        let name = format!("schemadiff: {} ↔ {}", parent.name, other_name);
        let origin = Origin::Diff {
            left: parent.id,
            right_name: other_name,
        };
        Ok(Sheet::child(parent, id, name, plan, origin).with_window(df))
    }

    /// Row-diff sheet. Full outer join on `keys`; a `__diff` column tags each row.
    /// Rows marked "same" are filtered out — only mismatches surface.
    pub fn row_diff_from(
        parent: &Sheet,
        id: SheetId,
        other: &Sheet,
        keys: &[String],
    ) -> Result<Self, String> {
        if keys.is_empty() {
            return Err("row_diff needs at least one key column".into());
        }
        let key_exprs: Vec<Expr> = keys.iter().map(|k| col(k.as_str())).collect();
        let a = parent.view_plan().with_column(lit(true).alias(DIFF_LEFT));
        let b = other.view_plan().with_column(lit(true).alias(DIFF_RIGHT));
        let joined = a.join(
            b,
            key_exprs.clone(),
            key_exprs,
            JoinArgs::new(JoinType::Full).with_coalesce(JoinCoalesce::CoalesceColumns),
        );
        let schema_a = parent
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        let schema_b = other
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?;

        let only_a = col(DIFF_RIGHT).is_null();
        let only_b = col(DIFF_LEFT).is_null();
        let diff_col = when(only_a.clone())
            .then(lit("only_a"))
            .when(only_b.clone())
            .then(lit("only_b"))
            .otherwise(lit("changed"))
            .alias("__diff");

        // A shared column differs, null vs value included.
        let shared: Vec<String> = schema_a
            .iter_names()
            .filter(|n| schema_b.get(n.as_str()).is_some() && !keys.iter().any(|k| k == n.as_str()))
            .map(|n| n.to_string())
            .collect();
        let mismatch_expr = shared
            .iter()
            .map(|c| col(c.as_str()).neq_missing(col(format!("{c}_right").as_str())))
            .reduce(|a, b| a.or(b))
            .unwrap_or(lit(false));

        // Final: only_a OR only_b OR mismatch
        let plan = joined
            .with_column(diff_col)
            .filter(only_a.or(only_b).or(mismatch_expr))
            .drop(cols([DIFF_LEFT, DIFF_RIGHT]));

        let other_name = other.name.clone();
        let how = Derivation::Diff {
            other: Box::new(other.clone()),
            keys: keys.to_vec(),
        };
        Ok(Sheet::child(
            parent,
            id,
            format!(
                "rowdiff({}): {} ↔ {}",
                keys.join(","),
                parent.name,
                other_name
            ),
            plan,
            Origin::Diff {
                left: parent.id,
                right_name: other_name,
            },
        )
        .derived(parent, how))
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

    /// Freq dive: `parent`'s rows whose `column` equals one of `values`
    /// (compared as text, the way the freq sheet grouped them; null matches null).
    pub fn rows_with_values(
        parent: &Sheet,
        id: SheetId,
        column: &str,
        values: &[AnyValue],
    ) -> Self {
        let pred = values_predicate(column, values);
        let label = match values {
            [one] => format!("{column}={}", any_text(one)),
            many => format!("{column} in {} values", many.len()),
        };
        let origin = Origin::Selected { parent: parent.id };
        let text = values
            .iter()
            .map(|v| (!v.is_null()).then(|| any_text(v)))
            .collect();
        Self::child(
            parent,
            id,
            format!("{label}: {}", parent.name),
            parent.view_plan().filter(pred),
            origin,
        )
        .derived(
            parent,
            Derivation::Values {
                column: column.into(),
                values: text,
            },
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
        .derived(parent, Derivation::Copy)
    }

    /// `:sql <query>`: `self` is `parent`'s view; `others` are extra tables.
    pub fn sql_from(
        parent: &Sheet,
        id: SheetId,
        query: &str,
        others: Vec<Sheet>,
    ) -> Result<Self, String> {
        let mut ctx = polars::sql::SQLContext::new();
        ctx.register("self", parent.view_plan());
        for o in &others {
            ctx.register(&format!("s{}", o.id), o.view_plan());
        }
        // Only the sheets the query names belong in the lineage.
        let words: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .map(str::to_ascii_lowercase)
            .collect();
        let others = others
            .into_iter()
            .filter(|o| words.contains(&format!("s{}", o.id)))
            .collect();
        let mut plan = ctx.execute(query).map_err(|e| e.to_string())?;
        plan.collect_schema().map_err(|e| e.to_string())?;
        let origin = Origin::Sql { parent: parent.id };
        Ok(
            Self::child(parent, id, format!("sql: {query}"), plan, origin).derived(
                parent,
                Derivation::Sql {
                    query: query.into(),
                    others,
                },
            ),
        )
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
        let df = DataFrame::new_infer_height(vec![
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
        )
        .derived(parent, Derivation::Describe))
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
        // The cursor may sit on a key column; a null and the text "null" are
        // one column header, so they are one group.
        // Hidden key columns are out of the view too.
        let shown = parent
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        let keys: Vec<String> = keys
            .iter()
            .filter(|k| *k != on && shown.contains(k.as_str()))
            .cloned()
            .collect();
        let keys = &keys[..];
        let value_text = col(on).cast(DataType::String).fill_null(lit("null"));
        let counted = parent
            .view_plan()
            .with_column(value_text)
            .group_by_stable(
                keys.iter()
                    .chain([&on.to_string()])
                    .map(|k| col(k.as_str()))
                    .collect::<Vec<_>>(),
            )
            .agg([len().alias("__vdrs_n")])
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
                .iter()
                .zip(ns.iter())
                .map(|(v, n)| Column::new(v.unwrap_or_default().into(), [n.unwrap_or(0)]))
                .collect();
            DataFrame::new_infer_height(cols).map_err(|e| e.to_string())?
        } else {
            pivot_df(&counted, on, keys, "__vdrs_n", element().first())
                .map_err(|e| e.to_string())?
        };
        let how = Derivation::PivotCount {
            keys: keys.to_vec(),
            on: on.to_string(),
        };
        Ok(Self::child(parent, id, name, out.lazy(), origin).derived(parent, how))
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
    /// append (stack rows, union columns by name, tag each with a hidden
    /// `origin_sheet`) | concat (stack rows, columns by position, `left`'s names,
    /// same hidden `origin_sheet`).
    pub fn join_from(
        left: &Sheet,
        id: SheetId,
        right_sheet: &Sheet,
        keys: &[String],
        how: &str,
    ) -> Result<Self, String> {
        let l = left.view_plan();
        let (right, right_name) = (right_sheet.view_plan(), right_sheet.name.as_str());
        let tag =
            |p: LazyFrame, name: &str| p.with_column(lit(name.to_string()).alias("origin_sheet"));
        let plan = if how == "append" {
            let args = UnionArgs {
                to_supertypes: true,
                diagonal: true,
                ..Default::default()
            };
            concat([tag(l, &left.name), tag(right, right_name)], args).map_err(|e| e.to_string())?
        } else if how == "concat" {
            let names = |p: &LazyFrame| -> Result<Vec<String>, String> {
                let schema = p.clone().collect_schema().map_err(|e| e.to_string())?;
                Ok(schema.iter_names().map(|n| n.to_string()).collect())
            };
            let (ln, rn) = (names(&l)?, names(&right)?);
            if ln.len() != rn.len() {
                return Err(format!(
                    "concat: {} has {} columns, {right_name} has {}",
                    left.name,
                    ln.len(),
                    rn.len()
                ));
            }
            let renamed = right.select(
                rn.iter()
                    .zip(&ln)
                    .map(|(r, l)| col(r.as_str()).alias(l.as_str()))
                    .collect::<Vec<_>>(),
            );
            let args = UnionArgs {
                to_supertypes: true,
                ..Default::default()
            };
            concat([tag(l, &left.name), tag(renamed, right_name)], args)
                .map_err(|e| e.to_string())?
        } else {
            let jt = match how {
                "inner" => JoinType::Inner,
                "left" => JoinType::Left,
                "full" | "merge" => JoinType::Full,
                other => return Err(format!("unknown join type: {other}")),
            };
            let on: Vec<Expr> = keys.iter().map(|k| col(k.as_str())).collect();
            let mut args = JoinArgs::new(jt).with_coalesce(JoinCoalesce::CoalesceColumns);
            // Left rows in their order, as VisiData; pivots of the join then
            // order their columns the same way every run (and in the export).
            args.maintain_order = MaintainOrderJoin::LeftRight;
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
                joined = joined.with_columns(merged).drop(cols(drop));
            }
            joined
        };
        let mut joined = Sheet::child(
            left,
            id,
            format!("{}&{} ({how})", left.name, right_name),
            plan,
            Origin::Join {
                left: left.id,
                right_name: right_name.to_string(),
            },
        )
        .derived(
            left,
            Derivation::Join {
                right: Box::new(right_sheet.clone()),
                keys: keys.to_vec(),
                how: how.into(),
            },
        );
        if how == "append" || how == "concat" {
            // VisiData's `origin_sheet` starts at width 0; `g-` shows it.
            joined.view.hidden.insert("origin_sheet".into());
        }
        Ok(joined)
    }

    /// Build an unpivot (melt) sheet. `on` = columns to unpivot; `index` = keep columns.
    pub fn unpivot_from(parent: &Sheet, id: SheetId, on: &[String], index: &[String]) -> Self {
        use polars::prelude::UnpivotArgsDSL;
        let args = UnpivotArgsDSL {
            on: (!on.is_empty()).then(|| cols(on.iter().map(String::as_str))),
            index: cols(index.iter().map(String::as_str)),
            variable_name: None,
            value_name: None,
        };
        let plan = parent.view_plan().unpivot(args);
        Sheet::child(
            parent,
            id,
            format!("unpivot({}): {}", on.join(","), parent.name),
            plan,
            Origin::Unpivot { parent: parent.id },
        )
        .derived(
            parent,
            Derivation::Unpivot {
                on: on.to_vec(),
                index: index.to_vec(),
            },
        )
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
        if matches!(self.origin, Origin::Tail { .. }) {
            // Snapshots predate rows that arrived since; keep the rows, undo the edit.
            if let Err(e) = self.rebuild_tail_plan() {
                debug_assert!(false, "steps applied before: {e}");
            }
        }
        true
    }

    /// Bake `step` into the plan and remember it for `:export` and `:tail`.
    pub fn apply_step(&mut self, step: PlanStep) -> PolarsResult<()> {
        self.plan = step.apply(self.plan.clone())?;
        self.state.steps.push(step);
        self.view.count_key = None; // a filter on the column may match differently
        Ok(())
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
        let step = PlanStep::SetCol {
            column: column.to_string(),
            value: value.to_string(),
            dtype,
            rids,
        };
        self.checkpoint();
        let n = self.view.selected.len();
        self.apply_step(step).map_err(|e| e.to_string())?;
        Ok(n)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reader::{self, SourceKind};

    fn sheet_of(csv: &str) -> (Sheet, tempfile::NamedTempFile) {
        let t = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        std::fs::write(t.path(), csv).unwrap();
        let loaded = reader::open(t.path().as_os_str(), None).unwrap();
        (Sheet::from_source(0, loaded), t)
    }
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
        assert_eq!(df.width(), 4); // color + count + percent + histogram
        assert_eq!(df.height(), 3); // red/blue/green
        let counts = df.column("count").unwrap();
        let first: i64 = counts.get(0).unwrap().try_extract().unwrap();
        assert_eq!(first, 3); // red leads
        let pct = df.column("percent").unwrap();
        let first_pct: f64 = pct.get(0).unwrap().try_extract().unwrap();
        assert!((first_pct - 50.0).abs() < 0.01); // 3/6 = 50%
        // bars scale by count, rounding down: red 3 → 38, blue 2 → 25, green 1 → 12
        let bars: Vec<usize> = df
            .column("histogram")
            .unwrap()
            .str()
            .unwrap()
            .iter()
            .map(|b| b.unwrap().chars().count())
            .collect();
        assert_eq!(bars, [38, 25, 12]);
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
        let d = Sheet::row_diff_from(&parent, 1, &Sheet::from_source(2, lb), &["id".to_string()])
            .unwrap();
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
        // an unparsable partial line waits; truncation starts over
        let mut handle = std::fs::OpenOptions::new()
            .append(true)
            .open(tmp.path())
            .unwrap();
        write!(handle, r#"{{"a":5"#).unwrap();
        drop(handle);
        s.refresh_tail().unwrap();
        assert_eq!(s.plan.clone().collect().unwrap().height(), 4);
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
        Arc::make_mut(&mut s.view.selected).insert(1); // row id 1 = bob (base order), first in the sorted view
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
        // freq of a freq's `count` column: stat columns step aside
        let ff = Sheet::freq_from(&Sheet::freq_from(&s, 3, "name"), 4, "count");
        let names = ff.plan.collect().unwrap().get_column_names_owned();
        assert_eq!(
            names,
            ["count", "freq_count", "freq_percent", "freq_histogram"]
        );
    }

    #[test]
    fn count_plan_counts_filtered_rows() {
        let (mut s, _tmp) = make_sheet();
        let count = |s: &Sheet| {
            let df = s.count_plan().collect().unwrap();
            df.columns()[0]
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
            .iter()
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

    #[test]
    fn failed_save_keeps_the_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("keep.csv");
        std::fs::write(&p, "precious\n").unwrap();
        let list = Series::new("l".into(), [1i32, 2]).implode().unwrap();
        let mut df = DataFrame::new_infer_height(vec![list.into_series().into()]).unwrap();
        assert!(write_df_by_extension(&mut df, &p).is_err());
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "precious\n");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no temp left"
        );
        let mut ok = df!("a" => [1, 2]).unwrap();
        write_df_by_extension(&mut ok, &p).unwrap();
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\n1\n2\n");
    }

    #[test]
    fn save_keeps_mode_and_refuses_read_only_files() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("t.csv");
        let mode = |m| std::fs::Permissions::from_mode(m);
        std::fs::write(&p, "old\n").unwrap();
        std::fs::set_permissions(&p, mode(0o664)).unwrap();
        let mut df = df!("a" => [1]).unwrap();
        write_df_by_extension(&mut df, &p).unwrap();
        let got = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(got & 0o777, 0o664);
        std::fs::set_permissions(&p, mode(0o444)).unwrap();
        if !std::fs::OpenOptions::new().write(true).open(&p).is_ok() {
            assert!(write_df_by_extension(&mut df, &p).is_err());
        } // root ignores modes
    }

    #[test]
    fn failed_save_to_a_new_path_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("new.csv");
        let list = Series::new("l".into(), [1i32]).implode().unwrap();
        let mut df = DataFrame::new_infer_height(vec![list.into_series().into()]).unwrap();
        assert!(write_df_by_extension(&mut df, &p).is_err());
        assert!(!p.exists());
    }

    #[test]
    fn w_ignores_hidden_key_columns() {
        let (mut s, _t) = sheet_of("a,b\nx,1\ny,2\n");
        s.view.hidden.insert("a".into());
        let keys = vec!["a".to_string()];
        let w = Sheet::pivot_current_from(&s, 1, &keys, "b").unwrap();
        assert_eq!(w.plan.clone().collect().unwrap().height(), 1);
    }

    #[test]
    fn failed_save_through_a_dangling_symlink_keeps_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("out.csv");
        std::os::unix::fs::symlink(dir.path().join("later.csv"), &link).unwrap();
        let list = Series::new("l".into(), [1i32]).implode().unwrap();
        let mut df = DataFrame::new_infer_height(vec![list.into_series().into()]).unwrap();
        assert!(write_df_by_extension(&mut df, &link).is_err());
        assert!(std::fs::symlink_metadata(&link).is_ok());
        assert!(!dir.path().join("later.csv").exists());
    }
}
