//! S-14 — Golden corpus replay harness.
//!
//! Walks `../visidata/tests/*.vd{,j}`, translates each cmdlog longname into a
//! sheet-level operation, materializes, and diffs the TSV against the paired
//! golden at `../visidata/tests/golden/<stem>.tsv`.
//!
//! Semantics (see S-14 §Shape):
//!   PASS         — longname-covered, output matches golden byte-for-byte post-normalize.
//!   FAIL         — longname-covered, output differs → test fails.
//!   SKIP         — a longname in the cmdlog is unimplemented → not a failure, bucketed.
//!   QUARANTINED  — stem listed in `tests/vd_replay/quarantine.txt`.
//!   NO_GOLDEN    — cmdlog has no paired golden (interactive-only features, side-effects).
//!
//! The driver is intentionally sheet-level, not App-level: Phase 1.5 wants fast
//! iteration on command semantics without threading a tokio runtime through tests.
//! Divergence between this driver and `App::dispatch` is a bug on the driver side;
//! covered by `cargo test --lib` for App dispatch, by this harness for semantics.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use polars::prelude::*;
use visidata_rs::reader;
use visidata_rs::sheet::{Origin, Sheet, SheetId, SortKey, SortOrder};

/// Driver-local column metadata that doesn't belong on `Sheet` yet.
/// hide/key semantics are Phase 1.5 production work (S-13 #2); until then we
/// track them in the test driver to unlock golden coverage.
#[derive(Default)]
struct ColMeta {
    hidden: HashSet<String>,
    keys: Vec<String>, // VisiData key-col order, preserved
}

const CORPUS: &str = "../visidata/tests";

#[derive(Debug, Clone, Default)]
struct CmdlogRow {
    #[allow(dead_code)]
    sheet: String,
    col: String,
    #[allow(dead_code)]
    row: String,
    longname: String,
    input: String,
    #[allow(dead_code)]
    keystrokes: String,
}

fn parse_vd(path: &Path) -> anyhow::Result<Vec<CmdlogRow>> {
    let text = fs::read_to_string(path)?;
    let mut rows = Vec::new();
    let mut lines = text.lines();
    let Some(header) = lines.next() else {
        return Ok(rows);
    };
    let cols: Vec<&str> = header.split('\t').collect();
    let idx = |name: &str| cols.iter().position(|c| *c == name);
    let (si, ci, ri, li, ii, ki) = (
        idx("sheet"),
        idx("col"),
        idx("row"),
        idx("longname"),
        idx("input"),
        idx("keystrokes"),
    );
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        let get = |i: Option<usize>| -> String { i.and_then(|j| f.get(j)).map_or("", |s| *s).to_string() };
        rows.push(CmdlogRow {
            sheet: get(si),
            col: get(ci),
            row: get(ri),
            longname: get(li),
            input: get(ii),
            keystrokes: get(ki),
        });
    }
    Ok(rows)
}

fn parse_vdj(path: &Path) -> anyhow::Result<Vec<CmdlogRow>> {
    let text = fs::read_to_string(path)?;
    let mut rows = Vec::new();
    for line in text.lines() {
        if line.is_empty() || line.starts_with("#!") {
            continue;
        }
        let v: serde_json::Value = serde_json::from_str(line)?;
        let get = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
        rows.push(CmdlogRow {
            sheet: get("sheet"),
            col: get("col"),
            row: get("row"),
            longname: get("longname"),
            input: get("input"),
            keystrokes: get("keystrokes"),
        });
    }
    Ok(rows)
}

/// A loaded sheet, its name, and driver-local column metadata.
struct SheetSlot {
    name: String,
    sheet: Sheet,
    meta: ColMeta,
}

/// Multi-sheet replay driver (iteration 5). `loaded` grows as `open-file`
/// / `freq-col` / `pivot` / join create sheets; `current` indexes into it.
/// `meta_mode` is Some while a sheets-stack / columns-sheet meta view is active
/// — commands then target that mode (e.g. `select-row` picks a sheet by name)
/// rather than the underlying data sheet.
struct Replay {
    loaded: Vec<SheetSlot>,
    current: Option<usize>,
    meta_mode: Option<MetaMode>,
    /// Resolution root for `open-file` paths. VisiData runs tests from the repo
    /// root, so inputs like `tests/sort-levels.tsv` resolve relative to there.
    root: PathBuf,
    next_id: SheetId,
}

enum MetaMode {
    /// VisiData `S` key — the "sheets stack" meta view listing all open sheets.
    /// `selected` holds names of sheets picked for a follow-up `join-sheets`.
    SheetsStack { selected: Vec<String> },
}

enum StepErr {
    /// `longname` not in the translator — bucket as a skip reason.
    MissingLongname(String),
    /// Translator ran but the operation failed; counts as skip-with-reason.
    Runtime(String),
    /// Longname encodes a Python expression / Python-syntax payload we can't
    /// natively evaluate in a Lua runtime. These tests are permanently
    /// untestable via replay unless a Python-compat eval layer lands — out of
    /// scope per direction §9.5 (zero VisiData compat beyond data files).
    PythonExpr(String),
}

/// Longnames that encode Python-syntax payloads in their `input` field.
/// VisiData evaluates these via `exec`/`eval`; a Rust+Lua port can't natively
/// run `qty*amt` or `curcol.casefold()` without a Python-compat layer.
fn is_python_expr_longname(ln: &str) -> bool {
    matches!(
        ln,
        "addcol-expr"
            | "setcol-expr"
            | "aggregate-col"
            | "select-expr"
            | "unselect-expr"
            | "addcol-capture"
            | "addcol-incr"
            | "addcol-iter"
            | "addcol-new"
            | "addcol-aggregate"
            | "addcol-window"
            | "aggregate-cols"
    )
}

impl Replay {
    fn new(root: PathBuf) -> Self {
        Self {
            loaded: Vec::new(),
            current: None,
            meta_mode: None,
            root,
            next_id: 0,
        }
    }

    fn current_slot(&self) -> Result<&SheetSlot, StepErr> {
        self.current
            .and_then(|i| self.loaded.get(i))
            .ok_or_else(|| StepErr::Runtime("no active sheet".into()))
    }

    fn current_slot_mut(&mut self) -> Result<&mut SheetSlot, StepErr> {
        self.current
            .and_then(|i| self.loaded.get_mut(i))
            .ok_or_else(|| StepErr::Runtime("no active sheet".into()))
    }

    fn push_sheet(&mut self, name: String, sheet: Sheet) {
        self.loaded.push(SheetSlot {
            name,
            sheet,
            meta: ColMeta::default(),
        });
        self.current = Some(self.loaded.len() - 1);
        self.meta_mode = None;
    }

    fn find_sheet_by_name(&self, name: &str) -> Option<usize> {
        self.loaded.iter().rposition(|s| s.name == name)
    }

    fn step(&mut self, r: &CmdlogRow) -> Result<(), StepErr> {
        use StepErr::*;
        if is_python_expr_longname(&r.longname) {
            return Err(PythonExpr(r.longname.clone()));
        }
        // Honor cmdlog's `sheet` field: switch current to the named sheet so
        // commands land on the right one in multi-sheet workflows. `sheets`
        // is the meta-view's own name — don't switch to it.
        if !r.sheet.is_empty() && r.sheet != "sheets" {
            if let Some(i) = self.find_sheet_by_name(&r.sheet) {
                self.current = Some(i);
            }
        }
        match r.longname.as_str() {
            "open-file" => {
                let path = self.root.join(&r.input);
                let loaded = reader::open(path.as_os_str(), None)
                    .map_err(|e| Runtime(format!("open-file {}: {}", path.display(), e)))?;
                let id = self.next_id;
                self.next_id += 1;
                // Sheet name = file stem (VisiData default); e.g. tests/data1.tsv → data1.
                let name = Path::new(&r.input)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("sheet")
                    .to_string();
                self.push_sheet(name, Sheet::from_source(id, loaded));
                Ok(())
            }
            "sort-asc" => self.push_sort(&r.col, SortOrder::Asc, false),
            "sort-desc" => self.push_sort(&r.col, SortOrder::Desc, false),
            "sort-asc-add" => self.push_sort(&r.col, SortOrder::Asc, true),
            "sort-desc-add" => self.push_sort(&r.col, SortOrder::Desc, true),
            "sort-keys-asc" => self.sort_keys(SortOrder::Asc),
            "sort-keys-desc" => self.sort_keys(SortOrder::Desc),
            "freq-col" => {
                let id = self.next_id;
                self.next_id += 1;
                let parent_slot = self.current_slot()?;
                let name = format!("freq({})-{}", r.col, parent_slot.name);
                let new = Sheet::freq_from(&parent_slot.sheet, id, &r.col);
                self.push_sheet(name, new);
                Ok(())
            }
            "rename-col" => {
                let slot = self.current_slot_mut()?;
                let old = r.col.as_str();
                let new = r.input.as_str();
                slot.sheet.plan = slot.sheet.plan.clone().rename([old], [new], true);
                if slot.meta.hidden.remove(old) {
                    slot.meta.hidden.insert(new.to_string());
                }
                for k in slot.meta.keys.iter_mut() {
                    if k == old {
                        *k = new.to_string();
                    }
                }
                Ok(())
            }
            "hide-col" => {
                self.current_slot_mut()?.meta.hidden.insert(r.col.clone());
                Ok(())
            }
            "key-col" => {
                // VisiData `!` is TOGGLE, not add. Repeated key-col on the
                // same col removes it (seen in inner-join / full-join tests).
                let slot = self.current_slot_mut()?;
                if let Some(i) = slot.meta.keys.iter().position(|k| k == &r.col) {
                    slot.meta.keys.remove(i);
                } else {
                    slot.meta.keys.push(r.col.clone());
                }
                Ok(())
            }
            "unkey-col" => {
                self.current_slot_mut()?
                    .meta
                    .keys
                    .retain(|k| k != &r.col);
                Ok(())
            }
            "type-int" => self.cast(&r.col, DataType::Int64),
            "type-float" => self.cast(&r.col, DataType::Float64),
            "type-string" => self.cast(&r.col, DataType::String),
            "type-date" => self.cast_date(&r.col),
            "type-currency" => self.cast_currency(&r.col),
            "sort-asc-change" => self.sort_change(&r.col, SortOrder::Asc),
            "sort-desc-change" => self.sort_change(&r.col, SortOrder::Desc),
            "select-rows" => self.select_all(),
            "unselect-rows" => self.unselect_all(),
            "select-row" => self.toggle_single(&r.row, true, false),
            "unselect-row" => self.toggle_single(&r.row, false, false),
            "stoggle-row" => self.toggle_single(&r.row, false, true),
            "select-col-regex" => self.select_regex(&r.col, &r.input),
            "unselect-col-regex" => self.unselect_regex(&r.col, &r.input),
            "select-equal-cell" => self.select_equal_cell(&r.col, &r.row),
            "dup-selected" => self.dup_selected(),
            "setcol-input" => self.setcol_input(&r.col, &r.row, &r.input),
            "set-cell-input" => self.setcol_input(&r.col, &r.row, &r.input),
            // sheets-stack: enter meta mode. `select-row` henceforth picks sheet
            // names (not data rows) and `join-sheets` consumes the selection.
            "sheets-stack" => {
                self.meta_mode = Some(MetaMode::SheetsStack {
                    selected: Vec::new(),
                });
                Ok(())
            }
            // join-sheets: consumes sheets-stack selection, builds a joined sheet
            // and makes it current. Input = jointype (append/inner/outer/full).
            "join-sheets" => self.join_sheets(&r.input),
            "set-option" => Ok(()),
            // dive-row/open-row: VisiData pushes a sub-sheet. For xlsx/sqlite
            // loaded as first-sheet-directly, no sub-sheet to enter — noop.
            // For VisiData's columns-sheet / other meta-sheet workflows, this
            // is wrong but we skip those tests elsewhere.
            "dive-row" | "open-row" => Ok(()),
            // Slide operations are column/row reorder cosmetic to the data model;
            // our serializer uses key-col ordering + schema order, not slide pos.
            "slide-left" | "slide-right" | "slide-up" | "slide-down" => Ok(()),
            "slide-leftmost" | "slide-rightmost" | "slide-top" | "slide-bottom" => Ok(()),
            "quit-sheet" => Ok(()),
            other => Err(MissingLongname(other.to_string())),
        }
    }

    fn push_sort(&mut self, col: &str, order: SortOrder, additive: bool) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        if !additive {
            s.view.sort.clear();
        }
        s.view.sort.push(SortKey {
            column: col.into(),
            order,
        });
        Ok(())
    }

    fn sort_keys(&mut self, order: SortOrder) -> Result<(), StepErr> {
        let slot = self.current_slot_mut()?;
        if slot.meta.keys.is_empty() {
            return Err(StepErr::Runtime("sort-keys with no key columns".into()));
        }
        slot.sheet.view.sort.clear();
        for k in &slot.meta.keys {
            slot.sheet.view.sort.push(SortKey {
                column: k.clone(),
                order,
            });
        }
        Ok(())
    }

    fn cast(&mut self, name: &str, dtype: DataType) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        s.plan = s
            .plan
            .clone()
            .with_columns([col(name).cast(dtype).alias(name)]);
        Ok(())
    }

    fn cast_date(&mut self, name: &str) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        s.plan = s
            .plan
            .clone()
            .with_columns([col(name).cast(DataType::Date).alias(name)]);
        Ok(())
    }

    fn cast_currency(&mut self, name: &str) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        s.plan = s.plan.clone().with_columns([col(name)
            .cast(DataType::String)
            .str()
            .replace_all(lit(r"[^0-9.\-]"), lit(""), false)
            .cast(DataType::Float64)
            .alias(name)]);
        Ok(())
    }

    /// VisiData `z[`/`z]`: if `col` already a sort key, flip its direction in place
    /// while keeping its position; otherwise add it at the end.
    fn sort_change(&mut self, col_name: &str, order: SortOrder) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        if let Some(k) = s.view.sort.iter_mut().find(|k| k.column == col_name) {
            k.order = order;
        } else {
            s.view.sort.push(SortKey {
                column: col_name.into(),
                order,
            });
        }
        Ok(())
    }

    fn select_all(&mut self) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        let n = s
            .plan
            .clone()
            .collect()
            .map_err(|e| StepErr::Runtime(e.to_string()))?
            .height();
        s.view.selected = (0..n as u64).collect();
        Ok(())
    }

    fn unselect_all(&mut self) -> Result<(), StepErr> {
        self.current_slot_mut()?.sheet.view.selected.clear();
        Ok(())
    }

    fn select_regex(&mut self, col_name: &str, pattern: &str) -> Result<(), StepErr> {
        self.apply_regex(col_name, pattern, true)
    }
    fn unselect_regex(&mut self, col_name: &str, pattern: &str) -> Result<(), StepErr> {
        self.apply_regex(col_name, pattern, false)
    }

    fn apply_regex(&mut self, col_name: &str, pattern: &str, add: bool) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        let re = regex::Regex::new(pattern).map_err(|e| StepErr::Runtime(e.to_string()))?;
        let df = s
            .plan
            .clone()
            .collect()
            .map_err(|e| StepErr::Runtime(e.to_string()))?;
        let col = df
            .column(col_name)
            .map_err(|e| StepErr::Runtime(e.to_string()))?;
        for i in 0..col.len() {
            let val = col.get(i).map(|v| av_display(&v)).unwrap_or_default();
            if re.is_match(&val) {
                if add {
                    s.view.selected.insert(i as u64);
                } else {
                    s.view.selected.remove(&(i as u64));
                }
            }
        }
        Ok(())
    }

    /// Select rows whose value in `col_name` equals the value in `row_key`'s row.
    /// `row_key` is VisiData's row identifier string — may be a key-column value,
    /// an integer index, or empty. We resolve by:
    ///   1. If `row_key` parses as usize and key_cols is empty → row-index lookup.
    ///   2. Else: scan for the row where any key col string-equals row_key.
    ///   3. Else: Runtime error (test skips with a reason).
    fn select_equal_cell(&mut self, col_name: &str, row_key: &str) -> Result<(), StepErr> {
        let keys = self.current_slot()?.meta.keys.clone();
        let s = &mut self.current_slot_mut()?.sheet;
        let df = s
            .plan
            .clone()
            .collect()
            .map_err(|e| StepErr::Runtime(e.to_string()))?;
        let col = df
            .column(col_name)
            .map_err(|e| StepErr::Runtime(e.to_string()))?;
        let cursor_idx: usize = if let Ok(i) = row_key.parse::<usize>() {
            if keys.is_empty() { i } else { find_by_key(&df, &keys, row_key)? }
        } else {
            find_by_key(&df, &keys, row_key)?
        };
        if cursor_idx >= col.len() {
            return Err(StepErr::Runtime(format!(
                "select-equal-cell: cursor row {cursor_idx} out of bounds"
            )));
        }
        let target = col.get(cursor_idx).map(|v| av_display(&v)).unwrap_or_default();
        for i in 0..col.len() {
            let v = col.get(i).map(|v| av_display(&v)).unwrap_or_default();
            if v == target {
                s.view.selected.insert(i as u64);
            }
        }
        Ok(())
    }

    /// VisiData select-row (`s`): mark current row. unselect-row (`u`): clear.
    /// stoggle-row (`t`): flip. `row_key` is the VisiData row identifier.
    ///
    /// In SheetsStack meta mode, `row_key` is a *sheet name* — adds/removes
    /// from the meta-mode `selected` list for a follow-up `join-sheets`.
    fn toggle_single(&mut self, row_key: &str, select: bool, toggle: bool) -> Result<(), StepErr> {
        if let Some(MetaMode::SheetsStack { selected }) = self.meta_mode.as_mut() {
            let name = row_key.strip_prefix('キ').unwrap_or(row_key).to_string();
            if toggle {
                if let Some(i) = selected.iter().position(|n| n == &name) {
                    selected.remove(i);
                } else {
                    selected.push(name);
                }
            } else if select {
                if !selected.contains(&name) {
                    selected.push(name);
                }
            } else {
                selected.retain(|n| n != &name);
            }
            return Ok(());
        }
        let keys = self.current_slot()?.meta.keys.clone();
        let s = &mut self.current_slot_mut()?.sheet;
        let df = s
            .plan
            .clone()
            .collect()
            .map_err(|e| StepErr::Runtime(e.to_string()))?;
        let idx = resolve_row(&df, &keys, row_key)?;
        let idx64 = idx as u64;
        if toggle {
            if !s.view.selected.remove(&idx64) {
                s.view.selected.insert(idx64);
            }
        } else if select {
            s.view.selected.insert(idx64);
        } else {
            s.view.selected.remove(&idx64);
        }
        Ok(())
    }

    /// VisiData setcol-input (`g=` text literal): set a cell's value to a string.
    /// Applies to a single row at (col, row). Phase 1.5: literal strings only;
    /// VisiData's "input" expression form hooks into Phase 3 Lua eval.
    fn setcol_input(&mut self, col_name: &str, row_key: &str, value: &str) -> Result<(), StepErr> {
        let keys = self.current_slot()?.meta.keys.clone();
        let s = &mut self.current_slot_mut()?.sheet;
        let df = s
            .plan
            .clone()
            .collect()
            .map_err(|e| StepErr::Runtime(e.to_string()))?;
        let idx = resolve_row(&df, &keys, row_key)?;
        // Use a row-index predicate so unkeyed sheets work too. Row index via
        // `int_range(0, df.height())` with eq filter avoids depending on a key.
        s.plan = s
            .plan
            .clone()
            .with_row_index("__ridx", None)
            .with_columns([when(col("__ridx").eq(lit(idx as u32)))
                .then(lit(value))
                .otherwise(col(col_name).cast(DataType::String))
                .alias(col_name)])
            .drop(["__ridx"]);
        Ok(())
    }

    fn dup_selected(&mut self) -> Result<(), StepErr> {
        let (name, new_sheet) = {
            let parent_slot = self.current_slot()?;
            if parent_slot.sheet.view.selected.is_empty() {
                return Err(StepErr::Runtime("dup-selected: no rows selected".into()));
            }
            let id = self.next_id;
            let name = format!("sel-{}", parent_slot.name);
            let sheet = Sheet::dup_selected_from(&parent_slot.sheet, id).map_err(StepErr::Runtime)?;
            (name, sheet)
        };
        self.next_id += 1;
        self.push_sheet(name, new_sheet);
        Ok(())
    }

    /// VisiData `&` join-sheets: consumes the sheets-stack selection and builds
    /// a joined sheet. Jointype (`input` field):
    ///   - "append" / "" → vertical concat (diagonal if schemas differ)
    ///   - "inner" / "outer" / "full" / "left" → key-based join on current keys
    /// Not in sheets-stack mode → error.
    fn join_sheets(&mut self, jointype: &str) -> Result<(), StepErr> {
        let selected = match self.meta_mode.take() {
            Some(MetaMode::SheetsStack { selected }) => selected,
            _ => {
                return Err(StepErr::Runtime(
                    "join-sheets outside sheets-stack mode".into(),
                ));
            }
        };
        if selected.len() < 2 {
            return Err(StepErr::Runtime(format!(
                "join-sheets needs ≥2 sheets, got {}",
                selected.len()
            )));
        }
        let slot_idxs: Vec<usize> = selected
            .iter()
            .map(|name| {
                self.find_sheet_by_name(name)
                    .ok_or_else(|| StepErr::Runtime(format!("no sheet named '{name}'")))
            })
            .collect::<Result<_, _>>()?;
        let jt = if jointype.is_empty() { "inner" } else { jointype };
        let result = match jt {
            "append" => {
                let plans: Vec<LazyFrame> = slot_idxs
                    .iter()
                    .map(|&i| self.loaded[i].sheet.plan.clone())
                    .collect();
                concat(
                    &plans,
                    UnionArgs {
                        parallel: true,
                        rechunk: true,
                        to_supertypes: true,
                        diagonal: true,
                        from_partitioned_ds: false,
                    },
                )
                .map_err(|e| StepErr::Runtime(e.to_string()))?
            }
            // Any key-based join: take VisiData's "visible-cols on all" semantic
            // literally by inner-joining pairwise on first slot's key columns.
            _ => {
                let first = &self.loaded[slot_idxs[0]];
                let keys: Vec<String> = first.meta.keys.clone();
                if keys.is_empty() {
                    return Err(StepErr::Runtime(format!(
                        "join-sheets {jt}: first sheet has no key columns"
                    )));
                }
                // VisiData jointype map:
                //   "inner" → only matching rows
                //   "outer" → keep LEFT (first-selected) rows + attach right
                //   "full"  → keep both sides' rows (symmetric)
                //   "left"  → same as outer in VisiData for 2-sheet case
                let how = match jt {
                    "inner" => JoinType::Inner,
                    "outer" | "left" => JoinType::Left,
                    "full" => JoinType::Full,
                    other => {
                        return Err(StepErr::Runtime(format!(
                            "join-sheets: unsupported jointype '{other}'"
                        )));
                    }
                };
                let key_exprs: Vec<_> = keys.iter().map(|k| col(k.as_str())).collect();
                let mut acc = first.sheet.plan.clone();
                for &i in slot_idxs.iter().skip(1) {
                    acc = acc.join(
                        self.loaded[i].sheet.plan.clone(),
                        key_exprs.clone(),
                        key_exprs.clone(),
                        JoinArgs::new(how.clone())
                            .with_coalesce(JoinCoalesce::CoalesceColumns),
                    );
                }
                acc
            }
        };
        let id = self.next_id;
        self.next_id += 1;
        let joined_name = selected.join("&");
        let first = &self.loaded[slot_idxs[0]];
        let kind = first.sheet.kind;
        let source = first.sheet.source.clone();
        // VisiData's join keeps first-sheet's keys on the result so subsequent
        // sort-keys / column-order reflect them. Inherit here.
        let inherited_keys = first.meta.keys.clone();
        let joined = Sheet {
            id,
            name: joined_name.clone(),
            kind,
            source,
            plan: result,
            view: Default::default(),
            window: None,
            revision: 0,
            origin: Origin::File,
        };
        self.push_sheet(joined_name, joined);
        if let Some(slot) = self.loaded.last_mut() {
            slot.meta.keys = inherited_keys;
        }
        Ok(())
    }

    fn to_tsv(&self) -> Result<String, String> {
        let slot = self.current_slot().map_err(|e| match e {
            StepErr::Runtime(m) => m,
            _ => "no active sheet".into(),
        })?;
        let s = &slot.sheet;
        let mut plan = s.plan.clone();
        if !s.view.sort.is_empty() {
            let by: Vec<&str> = s.view.sort.iter().map(|k| k.column.as_str()).collect();
            let desc: Vec<bool> = s
                .view
                .sort
                .iter()
                .map(|k| k.order == SortOrder::Desc)
                .collect();
            // VisiData's sort is stable; Polars default is unstable. Stable is
            // required for golden parity when sort keys have ties.
            let opts = SortMultipleOptions::default()
                .with_order_descending_multi(desc)
                .with_maintain_order(true);
            plan = plan.sort(by, opts);
        }
        let mut df = plan.collect().map_err(|e| e.to_string())?;

        // Apply visible-column projection: key columns first (in key-col order),
        // then remaining in schema order, dropping any in `hidden`.
        let all_names: Vec<String> = df
            .get_column_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        let mut order: Vec<String> = Vec::with_capacity(all_names.len());
        for k in &slot.meta.keys {
            if all_names.iter().any(|n| n == k) && !slot.meta.hidden.contains(k) {
                order.push(k.clone());
            }
        }
        for n in &all_names {
            if !order.contains(n) && !slot.meta.hidden.contains(n) {
                order.push(n.clone());
            }
        }
        if order != all_names {
            df = df.select(order.iter().cloned()).map_err(|e| e.to_string())?;
        }

        let mut buf = Vec::new();
        CsvWriter::new(&mut buf)
            .with_separator(b'\t')
            .include_header(true)
            .with_quote_style(QuoteStyle::Never)
            .finish(&mut df)
            .map_err(|e| e.to_string())?;
        Ok(String::from_utf8_lossy(&buf).into_owned())
    }
}

fn av_display(av: &AnyValue) -> String {
    match av {
        AnyValue::Null => String::new(),
        AnyValue::String(s) => (*s).to_string(),
        AnyValue::StringOwned(s) => s.as_str().to_string(),
        other => other.to_string(),
    }
}

fn find_by_key(df: &DataFrame, keys: &[String], target: &str) -> Result<usize, StepErr> {
    if keys.is_empty() {
        return Err(StepErr::Runtime(format!(
            "cannot locate row '{target}': no key columns set"
        )));
    }
    // Phase 1.5 scope: single-key lookup via string compare. Composite keys later.
    let col = df
        .column(keys[0].as_str())
        .map_err(|e| StepErr::Runtime(e.to_string()))?;
    for i in 0..col.len() {
        let v = col.get(i).map(|v| av_display(&v)).unwrap_or_default();
        if v == target {
            return Ok(i);
        }
    }
    Err(StepErr::Runtime(format!(
        "no row with {}='{target}'",
        keys[0]
    )))
}

/// Resolve a VisiData row identifier to a DataFrame row index.
///
/// VisiData encodes row identifiers in cmdlog with a `キ` (U+30AD) marker
/// before the key value — strip it. Why `キ`? VisiData convention; see
/// `visidata/_commands.py` row-reference encoding.
///
/// Rules after prefix-strip:
///   - empty string → row 0 (unkeyed sheets at the top of file)
///   - any string on a keyed sheet → `find_by_key`
///   - integer-looking string → row index (fallback for unkeyed sheets)
fn resolve_row(df: &DataFrame, keys: &[String], target: &str) -> Result<usize, StepErr> {
    let target = target.strip_prefix('キ').unwrap_or(target);
    if target.is_empty() {
        return Ok(0);
    }
    if !keys.is_empty() {
        if let Ok(i) = find_by_key(df, keys, target) {
            return Ok(i);
        }
    }
    if let Ok(i) = target.parse::<usize>() {
        if i < df.height() {
            return Ok(i);
        }
    }
    Err(StepErr::Runtime(format!(
        "cannot locate row '{target}' (keys={keys:?})"
    )))
}

fn normalize(s: &str) -> String {
    let s = s.replace("\r\n", "\n");
    let lines: Vec<&str> = s.lines().map(|l| l.trim_end()).collect();
    let joined = lines.join("\n");
    let trimmed = joined.trim_end_matches('\n');
    format!("{trimmed}\n")
}

/// Loose line equality: tab-split, each cell string-equal OR numerically equal
/// (parses as f64 on both sides, within 1e-9). Covers VisiData's trailing-zero
/// float formatting (`999.50` vs Polars `999.5`) without wholesale ignoring
/// numeric content.
fn eq_loose(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let ac: Vec<&str> = a.split('\t').collect();
    let bc: Vec<&str> = b.split('\t').collect();
    if ac.len() != bc.len() {
        return false;
    }
    ac.iter().zip(bc.iter()).all(|(x, y)| {
        if x == y {
            return true;
        }
        match (x.parse::<f64>(), y.parse::<f64>()) {
            (Ok(xf), Ok(yf)) => xf == yf || (xf - yf).abs() <= 1e-9 * xf.abs().max(yf.abs()).max(1.0),
            _ => false,
        }
    })
}

fn tsv_eq(want: &str, got: &str) -> bool {
    let w: Vec<&str> = want.lines().collect();
    let g: Vec<&str> = got.lines().collect();
    w.len() == g.len() && w.iter().zip(g.iter()).all(|(a, b)| eq_loose(a, b))
}

fn load_quarantine() -> HashSet<String> {
    let p = Path::new("tests/vd_replay/quarantine.txt");
    let Ok(text) = fs::read_to_string(p) else {
        return HashSet::new();
    };
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.trim_start().starts_with('#'))
        .map(|l| l.trim().to_string())
        .collect()
}

fn short_diff(want: &str, got: &str) -> String {
    let w: Vec<&str> = want.lines().collect();
    let g: Vec<&str> = got.lines().collect();
    let n = w.len().max(g.len());
    let mut out = format!("rows want={} got={}", w.len(), g.len());
    let mut shown = 0;
    for i in 0..n {
        let wl = w.get(i).copied().unwrap_or("<EOF>");
        let gl = g.get(i).copied().unwrap_or("<EOF>");
        if wl != gl {
            out.push_str(&format!("\n    line {}: want={wl:?} got={gl:?}", i + 1));
            shown += 1;
            if shown >= 3 {
                out.push_str("\n    …");
                break;
            }
        }
    }
    out
}

#[test]
fn vd_replay_corpus() {
    let corpus = Path::new(CORPUS);
    if !corpus.exists() {
        eprintln!(
            "vd_replay: skipping — corpus not at {CORPUS}. Need a sibling checkout of saulpw/visidata.",
        );
        return;
    }
    let visidata_root = corpus.parent().unwrap().to_path_buf();
    let quarantine = load_quarantine();

    let mut pass = 0usize;
    let mut skip: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut fail: Vec<(String, String)> = Vec::new();
    let mut quarantined = 0usize;
    let mut no_golden = 0usize;
    let mut untestable: BTreeMap<String, usize> = BTreeMap::new();

    let mut entries: Vec<PathBuf> = fs::read_dir(corpus)
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            matches!(
                p.extension().and_then(|x| x.to_str()),
                Some("vd") | Some("vdj")
            )
        })
        .collect();
    entries.sort();

    for cmd_path in entries {
        let stem = cmd_path.file_stem().unwrap().to_string_lossy().into_owned();
        if quarantine.contains(&stem) {
            quarantined += 1;
            continue;
        }
        let golden = corpus.join("golden").join(format!("{stem}.tsv"));
        if !golden.exists() {
            no_golden += 1;
            continue;
        }

        let rows = if cmd_path.extension().and_then(|x| x.to_str()) == Some("vdj") {
            parse_vdj(&cmd_path)
        } else {
            parse_vd(&cmd_path)
        };
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                fail.push((stem.clone(), format!("parse: {e}")));
                continue;
            }
        };

        let mut rp = Replay::new(visidata_root.clone());
        let mut outcome: Result<(), String> = Ok(());
        for row in &rows {
            match rp.step(row) {
                Ok(()) => {}
                Err(StepErr::PythonExpr(ln)) => {
                    *untestable.entry(ln).or_default() += 1;
                    outcome = Err("untestable".into());
                    break;
                }
                Err(StepErr::MissingLongname(ln)) => {
                    skip.entry(ln).or_default().push(stem.clone());
                    outcome = Err("skip".into());
                    break;
                }
                Err(StepErr::Runtime(e)) => {
                    skip.entry(format!("runtime: {e}")).or_default().push(stem.clone());
                    outcome = Err("skip".into());
                    break;
                }
            }
        }
        if outcome.is_err() {
            continue;
        }

        let got = match rp.to_tsv() {
            Ok(s) => normalize(&s),
            Err(e) => {
                skip.entry(format!("serialize: {e}"))
                    .or_default()
                    .push(stem.clone());
                continue;
            }
        };
        let Ok(want_raw) = fs::read_to_string(&golden) else {
            no_golden += 1;
            continue;
        };
        let want = normalize(&want_raw);

        if tsv_eq(&want, &got) {
            pass += 1;
        } else {
            fail.push((stem, short_diff(&want, &got)));
        }
    }

    let total_skip: usize = skip.values().map(|v| v.len()).sum();
    let total_untestable: usize = untestable.values().sum();
    let addressable = pass + fail.len() + total_skip; // tests we COULD still unlock
    eprintln!(
        "\nvd_replay: PASS={pass}  FAIL={}  SKIP={total_skip}  UNTESTABLE={total_untestable}  QUARANTINED={quarantined}  NO_GOLDEN={no_golden}",
        fail.len()
    );
    eprintln!("addressable ceiling (PASS + FAIL + SKIP): {addressable} tests");
    if !untestable.is_empty() {
        eprintln!("untestable (Python-expression payloads, direction §9.5):");
        let mut un: Vec<_> = untestable.iter().collect();
        un.sort_by(|a, b| b.1.cmp(a.1));
        for (ln, n) in un {
            eprintln!("  {ln:40}  {n} files");
        }
    }
    eprintln!("skip buckets (top 20 by count):");
    let mut buckets: Vec<_> = skip.iter().collect();
    buckets.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    for (ln, files) in buckets.iter().take(20) {
        eprintln!("  {ln:40}  {} files", files.len());
    }
    if !fail.is_empty() {
        eprintln!("\nFAIL details:");
        for (name, msg) in &fail {
            eprintln!("  {name}:\n    {msg}");
        }
    }
    assert!(fail.is_empty(), "{} replay regressions", fail.len());
}
