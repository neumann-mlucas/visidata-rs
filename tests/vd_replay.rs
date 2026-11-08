//! S-14 — Golden corpus replay harness.
//!
//! Walks `../visidata/tests/*.vd{,j,x}`, translates each cmdlog longname into a
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
//! The driver is sheet-level but serializes through `Sheet::view_plan`, the
//! app's own query. Key-by-key behaviour is `src/app/golden.rs`'s job.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use polars::prelude::*;
use visidata_rs::reader;
use visidata_rs::sheet::{Origin, Sheet, SheetId, SortKey, SortOrder};

const CORPUS: &str = "../visidata/tests";
/// Raise when coverage grows; never lower without saying why.
const MIN_PASS: usize = 30;

#[derive(Debug, Clone, Default)]
struct CmdlogRow {
    sheet: String,
    col: String,
    row: String,
    longname: String,
    input: String,
    /// VisiData's cmdlog keystrokes column. Parsed for completeness; the harness
    /// routes on `longname` and never dispatches by keystroke.
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
        let get = |i: Option<usize>| -> String {
            i.and_then(|j| f.get(j)).map_or("", |s| *s).to_string()
        };
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

/// VisiData's minimal `.vdx` script (`visidata/loaders/vdx.py`): one
/// `longname [input]` per line; `sheet`/`col`/`row X` lines position the next
/// command only; `option scope name value` → `set-option`; `#` comments.
/// Lines starting `{` or holding tabs are `.vdj`/`.vd` rows.
fn parse_vdx(path: &Path) -> anyhow::Result<Vec<CmdlogRow>> {
    let text = fs::read_to_string(path)?;
    let mut rows = Vec::new();
    let mut ctx = CmdlogRow::default();
    for line in text.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('{') || line.contains('\t') {
            anyhow::bail!("embedded .vd/.vdj row: {line}");
        }
        let (longname, rest) = line.split_once(' ').unwrap_or((line, ""));
        match longname {
            "sheet" => ctx.sheet = rest.into(),
            "col" => ctx.col = rest.into(),
            "row" => ctx.row = rest.into(),
            "option" => {
                let mut p = rest.splitn(3, ' ');
                rows.push(CmdlogRow {
                    sheet: p.next().unwrap_or("global").into(),
                    row: p.next().unwrap_or("").into(),
                    longname: "set-option".into(),
                    input: p.next().unwrap_or("").into(),
                    ..Default::default()
                });
            }
            _ => rows.push(CmdlogRow {
                longname: longname.into(),
                input: rest.into(),
                ..std::mem::take(&mut ctx)
            }),
        }
    }
    Ok(rows)
}

/// A loaded sheet, its name, and driver-local column metadata.
struct SheetSlot {
    name: String,
    sheet: Sheet,
    /// Non-key column order after multi-sheet appends (VisiData's
    /// enumerate-reverse-selection convention). Empty = schema order.
    col_order: Vec<String>,
}

/// Multi-sheet replay driver (iteration 5). `loaded` grows as `open-file`
/// / `freq-col` / `pivot` / join create sheets; `current` indexes into it.
/// `meta_mode` is Some while a sheets-stack / columns-sheet meta view is active
/// — commands then target that mode (e.g. `select-row` picks a sheet by name)
/// rather than the underlying data sheet.
struct Replay {
    loaded: Vec<SheetSlot>,
    current: Option<usize>,
    /// The previous `current` (for VisiData `vd.sheets[:2]` MRU semantics).
    prev_current: Option<usize>,
    meta_mode: Option<MetaMode>,
    /// Resolution root for `open-file` paths. VisiData runs tests from the repo
    /// root, so inputs like `tests/sort-levels.tsv` resolve relative to there.
    root: PathBuf,
    next_id: SheetId,
    /// `.vdx` positions only the commands that need it; VisiData's cursor stays
    /// put in between. Per-sheet (col, row) fills empty fields when set.
    cursor: Option<HashMap<usize, (String, String)>>,
}

enum MetaMode {
    /// VisiData `S` key — the "sheets stack" meta view listing all open sheets.
    /// `selected` holds names of sheets picked for a follow-up `join-sheets`.
    SheetsStack { selected: Vec<String> },
    /// VisiData `C` — meta view listing current sheet's columns. We don't model
    /// its contents; commands target it as a no-op surface so parent sheet
    /// isn't mutated. `quit-sheet` returns to parent.
    ColumnsSheet,
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

/// Longnames the replay can't run: their input is a Python expression
/// (`qty*amt`, `curcol.casefold()`, `range(1, 100)`) or Python itself, and vdrs
/// has no Python runtime (direction
/// §9.5; a Python subset is S-21 C, gated on evidence), or they unset VisiData
/// options. Planned features (editing/clipboard S-18, nested data S-22, column
/// aggregators S-25 P4) are not listed: they are SKIP (missing longname) until
/// they ship, so the triage shows them as real blockers.
fn is_direction_skip_longname(ln: &str) -> bool {
    matches!(
        ln,
        // (1) Python expression payloads
        "addcol-expr"
            | "setcol-expr"
            | "select-expr"
            | "unselect-expr"
            | "addcol-iter"
            | "setcol-fake" // random values from Python `faker`
            | "assert-expr"
            | "import-python"
            // Options
            | "unset-option"
    )
}

impl Replay {
    fn new(root: PathBuf) -> Self {
        Self {
            loaded: Vec::new(),
            current: None,
            prev_current: None,
            meta_mode: None,
            root,
            next_id: 0,
            cursor: None,
        }
    }

    fn set_current(&mut self, new: usize) {
        if self.current != Some(new) {
            self.prev_current = self.current;
            self.current = Some(new);
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
            col_order: Vec::new(),
        });
        self.set_current(self.loaded.len() - 1);
        self.meta_mode = None;
    }

    fn find_sheet_by_name(&self, name: &str) -> Option<usize> {
        self.loaded.iter().rposition(|s| s.name == name)
    }

    fn step(&mut self, r: &CmdlogRow) -> Result<(), StepErr> {
        use StepErr::*;
        if is_direction_skip_longname(&r.longname) {
            return Err(PythonExpr(r.longname.clone()));
        }
        // Honor cmdlog's `sheet` field: switch current to the named sheet so
        // commands land on the right one in multi-sheet workflows. `sheets`
        // is the meta-view's own name — don't switch to it.
        if !r.sheet.is_empty()
            && r.sheet != "sheets"
            && let Some(i) = self.find_sheet_by_name(&r.sheet)
        {
            self.set_current(i);
        }
        let filled;
        // A sheet's cursor starts on its first column.
        let first_col = self.current_slot().ok().and_then(|s| {
            let schema = s.sheet.view_plan().collect_schema().ok()?;
            schema.iter_names().next().map(|n| n.to_string())
        });
        let r = match (&mut self.cursor, self.current) {
            (Some(cursor), Some(cur)) => {
                let (col, row) = cursor
                    .entry(cur)
                    .or_insert_with(|| (first_col.unwrap_or_default(), String::new()));
                let mut f = r.clone();
                for (field, pos) in [(&mut f.col, col), (&mut f.row, row)] {
                    if field.is_empty() {
                        field.clone_from(pos);
                    } else {
                        pos.clone_from(field);
                    }
                }
                filled = f;
                &filled
            }
            _ => r,
        };
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
            // VisiData `W` pivot with no keys = 1-row transposed count: distinct
            // values of current col become headers, cell = count of that value.
            // First-seen order (not sorted). With keys we'd use real pivot but
            // the one upstream test using `pivot` has no keys.
            "pivot" => self.pivot_no_keys_count(&r.col),
            // VisiData `M` melt: unpivot all non-key columns.
            "melt" => {
                let id = self.next_id;
                self.next_id += 1;
                let (name, new) = {
                    let parent_slot = self.current_slot()?;
                    let keys = parent_slot.sheet.view.keys.clone();
                    if keys.is_empty() {
                        return Err(Runtime("melt: no key columns on parent".into()));
                    }
                    // non-key cols = all cols minus keys
                    let schema = parent_slot
                        .sheet
                        .plan
                        .clone()
                        .collect_schema()
                        .map_err(|e| Runtime(e.to_string()))?;
                    let on: Vec<String> = schema
                        .iter_names()
                        .map(|n| n.to_string())
                        .filter(|n| !keys.contains(n))
                        .collect();
                    let new = Sheet::unpivot_from(&parent_slot.sheet, id, &on, &keys);
                    (format!("{}_melted", parent_slot.name), new)
                };
                self.push_sheet(name, new);
                Ok(())
            }
            // VisiData `g"` dup-rows: new sheet copy with all rows. In replay
            // this is equivalent to pushing a sheet with the same plan.
            "dup-rows" => {
                let id = self.next_id;
                self.next_id += 1;
                let (name, new_sheet) = {
                    let parent_slot = self.current_slot()?;
                    let name = format!("{}_copy", parent_slot.name);
                    let plan = parent_slot.sheet.plan.clone();
                    let sheet = Sheet {
                        id,
                        name: name.clone(),
                        kind: parent_slot.sheet.kind,
                        source: parent_slot.sheet.source.clone(),
                        plan,
                        view: Default::default(),
                        window: None,
                        revision: 0,
                        state: Default::default(),
                        origin: Origin::File,
                    };
                    (name, sheet)
                };
                self.push_sheet(name, new_sheet);
                Ok(())
            }
            "rename-col" => {
                let slot = self.current_slot_mut()?;
                let old = r.col.as_str();
                let new = r.input.as_str();
                slot.sheet.plan = slot.sheet.plan.clone().rename([old], [new], true);
                if slot.sheet.view.hidden.remove(old) {
                    slot.sheet.view.hidden.insert(new.to_string());
                }
                for k in slot.sheet.view.keys.iter_mut() {
                    if k == old {
                        *k = new.to_string();
                    }
                }
                Ok(())
            }
            "hide-col" => {
                self.current_slot_mut()?
                    .sheet
                    .view
                    .hidden
                    .insert(r.col.clone());
                Ok(())
            }
            "key-col" => {
                // VisiData `!` is TOGGLE, not add. Repeated key-col on the
                // same col removes it (seen in inner-join / full-join tests).
                let slot = self.current_slot_mut()?;
                if let Some(i) = slot.sheet.view.keys.iter().position(|k| k == &r.col) {
                    slot.sheet.view.keys.remove(i);
                } else {
                    slot.sheet.view.keys.push(r.col.clone());
                }
                Ok(())
            }
            "key-col-on" => {
                let keys = &mut self.current_slot_mut()?.sheet.view.keys;
                if !keys.contains(&r.col) {
                    keys.push(r.col.clone());
                }
                Ok(())
            }
            "select-col-duplicates" => {
                let s = &mut self.current_slot_mut()?.sheet;
                let rids = s
                    .rids_col_duplicates(&r.col)
                    .map_err(|e| Runtime(e.to_string()))?;
                Arc::make_mut(&mut s.view.selected).extend(rids);
                Ok(())
            }
            "dedupe-rows" => {
                let id = self.next_id;
                self.next_id += 1;
                let slot = self.current_slot()?;
                let name = format!("{}_deduped", slot.name);
                let sheet = Sheet::dedupe_from(&slot.sheet, id).map_err(Runtime)?;
                self.push_sheet(name, sheet);
                Ok(())
            }
            "unkey-col" => {
                self.current_slot_mut()?
                    .sheet
                    .view
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
            // `ge` (gang mode) sets the col for every selected row; `e` single-row.
            // Keystroke isn't on the cmdlog's `row` field, so we gang-set when there
            // IS a selection AND the row field is empty / "0" (VisiData's cursor is
            // row 0 at gang time) — otherwise single-row via `row` key.
            "setcol-input" => {
                let has_selection = !self.current_slot()?.sheet.view.selected.is_empty();
                if has_selection && (r.row.is_empty() || r.row == "0") {
                    self.setcol_input_gang(&r.col, &r.input)
                } else {
                    self.setcol_input(&r.col, &r.row, &r.input)
                }
            }
            "set-cell-input" => self.setcol_input(&r.col, &r.row, &r.input),
            // Walk every column of the current sheet; hide any whose non-null
            // values are all identical (and there's >1 row). VisiData's `hide-uniform-cols`.
            "hide-uniform-cols" => self.hide_uniform_cols(),
            // sheets-stack: enter meta mode. `select-row` henceforth picks sheet
            // names (not data rows) and `join-sheets` consumes the selection.
            "sheets-stack" => {
                self.meta_mode = Some(MetaMode::SheetsStack {
                    selected: Vec::new(),
                });
                Ok(())
            }
            // columns-sheet: enter a meta view listing parent sheet's columns.
            // ponytail: no-op mode; parent sheet is unchanged. edit-cell /
            // select-row / etc. while active become no-ops. quit-sheet exits.
            "columns-sheet" | "columns-all" => {
                self.meta_mode = Some(MetaMode::ColumnsSheet);
                Ok(())
            }
            // quit-sheet: clear meta mode if any; otherwise no-op (we don't
            // model the parent sheet stack).
            "quit-sheet" => {
                self.meta_mode = None;
                Ok(())
            }
            // edit-cell / set-cell-input while in a meta view: no-op (we don't
            // model meta contents). Outside meta, edit-cell is a data edit.
            // Columns sheet: a column's width 0 hides it, any other shows it.
            "edit-cell"
                if matches!(self.meta_mode, Some(MetaMode::ColumnsSheet)) && r.col == "width" =>
            {
                let key = r.row.strip_prefix('キ').unwrap_or(&r.row);
                // `columns-all` rows are keyed `sheet,column`.
                let (slot, name) = match key.split_once(',') {
                    Some((sheet, c)) if self.find_sheet_by_name(sheet).is_some() => {
                        (self.find_sheet_by_name(sheet).unwrap(), c.to_string())
                    }
                    _ => (
                        self.current
                            .ok_or_else(|| Runtime("no active sheet".into()))?,
                        key.to_string(),
                    ),
                };
                let hidden = &mut self.loaded[slot].sheet.view.hidden;
                if r.input.trim() == "0" {
                    hidden.insert(name);
                } else {
                    hidden.remove(&name);
                }
                Ok(())
            }
            "edit-cell" if self.meta_mode.is_some() => Ok(()),
            "edit-cell" => self.setcol_input(&r.col, &r.row, &r.input),
            // join-sheets: consumes sheets-stack selection, builds a joined sheet
            // and makes it current. Input = jointype (append/inner/outer/full).
            "join-sheets" => self.join_sheets(&r.input, true),
            // join-sheets-top2: VisiData `&` on a data sheet (not meta). Joins
            // the current sheet with the previously-active one (`vd.sheets[:2]`).
            "join-sheets-top2" => self.join_top2(&r.input),
            "set-option" => Ok(()),
            // Copy to the register (S-18 A) leaves the sheet unchanged; paste is S-18 D/F.
            "copy-row" | "copy-selected" | "copy-cell" | "copy-cells" => Ok(()),
            // Widening a column shows a hidden (width 0) one.
            "resize-col-max" | "resize-col-input" => {
                let col = r.col.clone();
                self.current_slot_mut()?.sheet.view.hidden.remove(&col);
                Ok(())
            }
            "jump-sheet" => {
                let i = self
                    .find_sheet_by_name(&r.input)
                    .ok_or_else(|| Runtime(format!("jump-sheet: no sheet {}", r.input)))?;
                self.set_current(i);
                Ok(())
            }
            // dive-row/open-row: VisiData pushes a sub-sheet. For xlsx/sqlite
            // loaded as first-sheet-directly, no sub-sheet to enter — noop.
            // For VisiData's columns-sheet / other meta-sheet workflows, this
            // is wrong but we skip those tests elsewhere.
            "dive-row" | "open-row" => Ok(()),
            // Slide operations are column/row reorder cosmetic to the data model;
            // our serializer uses key-col ordering + schema order, not slide pos.
            "slide-left" | "slide-right" | "slide-up" | "slide-down" => Ok(()),
            "slide-leftmost" | "slide-rightmost" | "slide-top" | "slide-bottom" => Ok(()),
            // reload-sheet / reload-selected: data on disk is unchanged in replay.
            "reload-sheet" | "reload-selected" => Ok(()),
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
        if slot.sheet.view.keys.is_empty() {
            return Err(StepErr::Runtime("sort-keys with no key columns".into()));
        }
        slot.sheet.view.sort.clear();
        for k in &slot.sheet.view.keys {
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
        s.view.selected = Arc::new((0..n as u64).collect());
        Ok(())
    }

    fn unselect_all(&mut self) -> Result<(), StepErr> {
        self.current_slot_mut()?.sheet.view.selected = Default::default();
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
                    Arc::make_mut(&mut s.view.selected).insert(i as u64);
                } else {
                    Arc::make_mut(&mut s.view.selected).remove(&(i as u64));
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
        let keys = self.current_slot()?.sheet.view.keys.clone();
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
            if keys.is_empty() {
                i
            } else {
                find_by_key(&df, &keys, row_key)?
            }
        } else {
            find_by_key(&df, &keys, row_key)?
        };
        if cursor_idx >= col.len() {
            return Err(StepErr::Runtime(format!(
                "select-equal-cell: cursor row {cursor_idx} out of bounds"
            )));
        }
        let target = col
            .get(cursor_idx)
            .map(|v| av_display(&v))
            .unwrap_or_default();
        for i in 0..col.len() {
            let v = col.get(i).map(|v| av_display(&v)).unwrap_or_default();
            if v == target {
                Arc::make_mut(&mut s.view.selected).insert(i as u64);
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
        // columns-sheet meta mode: select-row targets a column (by name).
        // We don't model that selection yet; treat as no-op so parent isn't
        // polluted and the chain can continue.
        if matches!(self.meta_mode, Some(MetaMode::ColumnsSheet)) {
            return Ok(());
        }
        let keys = self.current_slot()?.sheet.view.keys.clone();
        let s = &mut self.current_slot_mut()?.sheet;
        let df = s
            .plan
            .clone()
            .collect()
            .map_err(|e| StepErr::Runtime(e.to_string()))?;
        let idx = resolve_row(&df, &keys, row_key)?;
        let idx64 = idx as u64;
        if toggle {
            if !Arc::make_mut(&mut s.view.selected).remove(&idx64) {
                Arc::make_mut(&mut s.view.selected).insert(idx64);
            }
        } else if select {
            Arc::make_mut(&mut s.view.selected).insert(idx64);
        } else {
            Arc::make_mut(&mut s.view.selected).remove(&idx64);
        }
        Ok(())
    }

    /// VisiData setcol-input (`g=` text literal): set a cell's value to a string.
    /// Applies to a single row at (col, row). Phase 1.5: literal strings only;
    /// VisiData's "input" expression form hooks into Phase 3 Lua eval.
    fn setcol_input(&mut self, col_name: &str, row_key: &str, value: &str) -> Result<(), StepErr> {
        let keys = self.current_slot()?.sheet.view.keys.clone();
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
            .drop(cols(["__ridx"]));
        Ok(())
    }

    /// VisiData `ge`: for every row in `view.selected`, overwrite `col_name` with `value`.
    fn setcol_input_gang(&mut self, col_name: &str, value: &str) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        if s.view.selected.is_empty() {
            return Ok(());
        }
        // Build a chain of eq-OR predicates over the row index for each selected row.
        // Ponytail: when selection is huge this is O(n·|sel|); good enough for tests.
        let idxs: Vec<u32> = s.view.selected.iter().map(|i| *i as u32).collect();
        let mut predicate: Option<Expr> = None;
        for i in idxs {
            let p = col("__ridx").eq(lit(i));
            predicate = Some(match predicate {
                None => p,
                Some(acc) => acc.or(p),
            });
        }
        let predicate = predicate.unwrap();
        s.plan = s
            .plan
            .clone()
            .with_row_index("__ridx", None)
            .with_columns([when(predicate)
                .then(lit(value))
                .otherwise(col(col_name).cast(DataType::String))
                .alias(col_name)])
            .drop(cols(["__ridx"]));
        Ok(())
    }

    /// Hide every column whose non-null values are all identical, provided
    /// there's more than one row. Pure visibility op; data plan untouched.
    fn hide_uniform_cols(&mut self) -> Result<(), StepErr> {
        let slot = self.current_slot_mut()?;
        let df = slot
            .sheet
            .plan
            .clone()
            .collect()
            .map_err(|e| StepErr::Runtime(e.to_string()))?;
        if df.height() <= 1 {
            return Ok(());
        }
        for name in df
            .get_column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
        {
            let col = match df.column(&name) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let unique_count = col.unique().map(|c| c.len()).unwrap_or(usize::MAX);
            // n_unique counts null as one value; treat all-null + 1 real = 2.
            // Fast path: ≤1 distinct value means hide.
            if unique_count <= 1 {
                slot.sheet.view.hidden.insert(name);
            }
        }
        Ok(())
    }

    /// VisiData `W` with no key columns: distinct values of `col_name` as headers,
    /// counts as the single data row. First-seen order preserved.
    fn pivot_no_keys_count(&mut self, col_name: &str) -> Result<(), StepErr> {
        use polars::prelude::NamedFrom;
        let (name, new_sheet) = {
            let parent_slot = self.current_slot()?;
            let df = parent_slot
                .sheet
                .plan
                .clone()
                .collect()
                .map_err(|e| StepErr::Runtime(e.to_string()))?;
            let col = df
                .column(col_name)
                .map_err(|e| StepErr::Runtime(e.to_string()))?;
            let mut order: Vec<String> = Vec::new();
            let mut counts: std::collections::HashMap<String, i64> =
                std::collections::HashMap::new();
            for i in 0..col.len() {
                let v = col.get(i).map(|v| av_display(&v)).unwrap_or_default();
                counts
                    .entry(v.clone())
                    .and_modify(|c| *c += 1)
                    .or_insert_with(|| {
                        order.push(v.clone());
                        1
                    });
            }
            let series: Vec<Column> = order
                .iter()
                .map(|val| Series::new(val.as_str().into(), &[counts[val]]).into_column())
                .collect();
            let out_df =
                DataFrame::new_infer_height(series).map_err(|e| StepErr::Runtime(e.to_string()))?;
            let id = self.next_id;
            let name = format!("pivot({col_name})-{}", parent_slot.name);
            let sheet = Sheet {
                id,
                name: name.clone(),
                kind: parent_slot.sheet.kind,
                source: parent_slot.sheet.source.clone(),
                plan: out_df.lazy(),
                view: Default::default(),
                window: None,
                revision: 0,
                state: Default::default(),
                origin: Origin::File,
            };
            (name, sheet)
        };
        self.next_id += 1;
        self.push_sheet(name, new_sheet);
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
            let sheet =
                Sheet::dup_selected_from(&parent_slot.sheet, id).map_err(StepErr::Runtime)?;
            (name, sheet)
        };
        self.next_id += 1;
        self.push_sheet(name, new_sheet);
        Ok(())
    }

    /// VisiData `&` on a data sheet: join current + previous via same core as join-sheets.
    fn join_top2(&mut self, jointype: &str) -> Result<(), StepErr> {
        let cur = self
            .current
            .ok_or_else(|| StepErr::Runtime("no current".into()))?;
        let prev = self
            .prev_current
            .ok_or_else(|| StepErr::Runtime("join-sheets-top2: no previous sheet".into()))?;
        let selected = vec![
            self.loaded[cur].name.clone(),
            self.loaded[prev].name.clone(),
        ];
        self.meta_mode = Some(MetaMode::SheetsStack { selected });
        self.join_sheets(jointype, false)
    }

    /// VisiData `&` join-sheets: consumes the sheets-stack selection and builds
    /// a joined sheet.
    ///
    /// Jointype (`input` field):
    ///   - "append" / "" → vertical concat (diagonal if schemas differ)
    ///   - "inner" / "outer" / "full" / "left" → key-based join on current keys
    ///
    /// Not in sheets-stack mode → error.
    /// `from_stack`: sheets picked on the sheets stack, whose appended columns
    /// VisiData lists in reverse; `join-sheets-top2` keeps [current, previous].
    fn join_sheets(&mut self, jointype: &str, from_stack: bool) -> Result<(), StepErr> {
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
        let jt = if jointype.is_empty() {
            "inner"
        } else {
            jointype
        };
        let result = match jt {
            "append" => {
                // VisiData tags each row with origin_sheet. Prepend an
                // `origin_sheet` lit() column before concat so each slice
                // keeps its source name.
                let plans: Vec<LazyFrame> = slot_idxs
                    .iter()
                    .map(|&i| {
                        let name = self.loaded[i].name.clone();
                        self.loaded[i]
                            .sheet
                            .plan
                            .clone()
                            .with_column(lit(name).alias("origin_sheet"))
                    })
                    .collect();
                concat(
                    &plans,
                    UnionArgs {
                        parallel: true,
                        rechunk: true,
                        to_supertypes: true,
                        diagonal: true,
                        from_partitioned_ds: false,
                        strict: false,
                        maintain_order: true,
                    },
                )
                .map_err(|e| StepErr::Runtime(e.to_string()))?
                // ponytail: col_order hint set after push below; see outer scope
            }
            // VisiData `concat` (#3208): rows stacked, columns paired by
            // position, names from the first sheet.
            "concat" => {
                let names = |i: usize| -> Result<Vec<String>, StepErr> {
                    let schema = self.loaded[i]
                        .sheet
                        .plan
                        .clone()
                        .collect_schema()
                        .map_err(|e| StepErr::Runtime(e.to_string()))?;
                    Ok(schema.iter_names().map(|n| n.to_string()).collect())
                };
                let first = names(slot_idxs[0])?;
                let mut plans = Vec::new();
                for &i in &slot_idxs {
                    let these = names(i)?;
                    if these.len() != first.len() {
                        return Err(StepErr::Runtime("concat: column counts differ".into()));
                    }
                    let renamed: Vec<Expr> = these
                        .iter()
                        .zip(&first)
                        .map(|(t, f)| col(t.as_str()).alias(f.as_str()))
                        .collect();
                    let origin = lit(self.loaded[i].name.clone()).alias("origin_sheet");
                    plans.push(
                        self.loaded[i]
                            .sheet
                            .plan
                            .clone()
                            .select(renamed)
                            .with_column(origin),
                    );
                }
                let args = UnionArgs {
                    to_supertypes: true,
                    ..Default::default()
                };
                concat(&plans, args).map_err(|e| StepErr::Runtime(e.to_string()))?
            }
            // VisiData joins each sheet on its *own* key columns, pairing the
            // ith key of sheet N with the ith key of sheet N+1. Allows joining
            // sheets whose key columns have different names.
            _ => {
                let first = &self.loaded[slot_idxs[0]];
                let left_keys = first.sheet.view.keys.clone();
                if left_keys.is_empty() {
                    return Err(StepErr::Runtime(format!(
                        "join-sheets {jt}: first sheet has no key columns"
                    )));
                }
                let how = match jt {
                    "inner" => JoinType::Inner,
                    "outer" | "left" | "extend" => JoinType::Left,
                    "full" | "merge" => JoinType::Full,
                    other => {
                        return Err(StepErr::Runtime(format!(
                            "join-sheets: unsupported jointype '{other}'"
                        )));
                    }
                };
                let left_exprs: Vec<_> = left_keys.iter().map(|k| col(k.as_str())).collect();
                let mut acc = first.sheet.plan.clone();
                for &i in slot_idxs.iter().skip(1) {
                    let right_keys = &self.loaded[i].sheet.view.keys;
                    if right_keys.len() < left_keys.len() {
                        return Err(StepErr::Runtime(format!(
                            "join-sheets {jt}: sheet '{}' has {} key(s), need ≥{}",
                            self.loaded[i].name,
                            right_keys.len(),
                            left_keys.len()
                        )));
                    }
                    let right_exprs: Vec<_> = right_keys
                        .iter()
                        .take(left_keys.len())
                        .map(|k| col(k.as_str()))
                        .collect();
                    acc = acc.join(
                        self.loaded[i].sheet.plan.clone(),
                        left_exprs.clone(),
                        right_exprs,
                        JoinArgs::new(how.clone()).with_coalesce(JoinCoalesce::CoalesceColumns),
                    );
                    // VisiData `merge`: for each non-key col colliding with the
                    // right side (suffixed `_right` by Polars), prefer the right
                    // value, fall back to left when right is null. Then drop the
                    // `_right` variant. Keys already coalesced above.
                    if jt == "merge" {
                        let schema = acc
                            .clone()
                            .collect_schema()
                            .map_err(|e| StepErr::Runtime(e.to_string()))?;
                        let names: Vec<String> =
                            schema.iter_names().map(|n| n.to_string()).collect();
                        let right_dups: Vec<String> = names
                            .iter()
                            .filter_map(|n| n.strip_suffix("_right").map(String::from))
                            .filter(|base| names.iter().any(|n| n == base))
                            .collect();
                        if !right_dups.is_empty() {
                            let coalesce_exprs: Vec<Expr> = right_dups
                                .iter()
                                .map(|base| {
                                    let right_name = format!("{base}_right");
                                    when(col(right_name.as_str()).is_null())
                                        .then(col(base.as_str()))
                                        .otherwise(col(right_name.as_str()))
                                        .alias(base.as_str())
                                })
                                .collect();
                            let drop_cols: Vec<String> =
                                right_dups.iter().map(|b| format!("{b}_right")).collect();
                            acc = acc.with_columns(coalesce_exprs).drop(cols(drop_cols));
                        }
                    }
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
        let inherited_keys = first.sheet.view.keys.clone();
        let joined = Sheet {
            id,
            name: joined_name.clone(),
            kind,
            source,
            plan: result,
            view: Default::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin: Origin::File,
        };
        self.push_sheet(joined_name, joined);
        if let Some(slot) = self.loaded.last_mut() {
            slot.sheet.view.keys = inherited_keys;
            if jt == "append" || jt == "concat" {
                slot.sheet.view.hidden.insert("origin_sheet".into()); // width 0 (#2929)
            }
        }
        // For append: enumerate non-key cols in REVERSE selection order to
        // match VisiData output (data1's cols appear before data2's when
        // selected=[data2, data1]).
        if jt == "append" {
            let mut col_order: Vec<String> = Vec::new();
            let keys: HashSet<&str> = self
                .loaded
                .last()
                .unwrap()
                .sheet
                .view
                .keys
                .iter()
                .map(String::as_str)
                .collect();
            let order: Vec<usize> = if from_stack {
                slot_idxs.iter().rev().copied().collect()
            } else {
                slot_idxs.clone()
            };
            for &i in &order {
                let schema = match self.loaded[i].sheet.plan.clone().collect_schema() {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                for name in schema.iter_names() {
                    let n = name.to_string();
                    if !keys.contains(n.as_str()) && n != "origin_sheet" && !col_order.contains(&n)
                    {
                        col_order.push(n);
                    }
                }
            }
            if let Some(slot) = self.loaded.last_mut() {
                slot.col_order = col_order;
            }
        }
        Ok(())
    }

    fn to_tsv(&self) -> Result<String, String> {
        let slot = self.current_slot().map_err(|e| match e {
            StepErr::Runtime(m) => m,
            _ => "no active sheet".into(),
        })?;
        // The app's own query: casts, filter, stable sort, hide, keys first.
        let mut df = slot
            .sheet
            .view_plan()
            .collect()
            .map_err(|e| e.to_string())?;
        let shown: Vec<String> = df
            .get_column_names()
            .into_iter()
            .map(|s| s.to_string())
            .collect();
        // VisiData's join columns: keys, then `origin_sheet`, then the
        // col_order hint, then the rest.
        let keys = slot.sheet.view.keys.iter();
        let mut order: Vec<String> = Vec::with_capacity(shown.len());
        for n in keys
            .chain(std::iter::once(&"origin_sheet".to_string()))
            .chain(&slot.col_order)
            .chain(&shown)
        {
            if shown.contains(n) && !order.contains(n) {
                order.push(n.clone());
            }
        }
        if order != shown {
            df = df.select(order).map_err(|e| e.to_string())?;
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
    // Cast the lookup column to String so Date/Int/Float columns match their
    // ISO/decimal cmdlog representation (`2018-07-03`, `42`, `3.14`).
    let col_s = df
        .column(keys[0].as_str())
        .map_err(|e| StepErr::Runtime(e.to_string()))?
        .cast(&DataType::String)
        .map_err(|e| StepErr::Runtime(e.to_string()))?;
    for i in 0..col_s.len() {
        let v = col_s.get(i).map(|v| av_display(&v)).unwrap_or_default();
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
    if !keys.is_empty()
        && let Ok(i) = find_by_key(df, keys, target)
    {
        return Ok(i);
    }
    if let Ok(i) = target.parse::<usize>()
        && i < df.height()
    {
        return Ok(i);
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
            (Ok(xf), Ok(yf)) => {
                xf == yf || (xf - yf).abs() <= 1e-9 * xf.abs().max(yf.abs()).max(1.0)
            }
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
        .map(|l| l.split('#').next().unwrap_or("").trim())
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Why a file has no `<stem>.tsv` golden: VisiData runs `-nosave`, `-manual`,
/// `-broken` and `quit-perf` for their assertions only; the others save to
/// another format (`golden/<stem>.csv`, `.json`, …) the replay doesn't compare yet.
fn no_golden_reason(corpus: &Path, stem: &str) -> String {
    let mut exts: Vec<String> = fs::read_dir(corpus.join("golden"))
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.file_stem().and_then(|s| s.to_str()) == Some(stem))
        .filter_map(|p| p.extension().and_then(|x| x.to_str()).map(str::to_string))
        .collect();
    exts.sort();
    if exts.is_empty() {
        "assertion-only (no golden in VisiData)".into()
    } else {
        format!("golden is .{}", exts.join(" ."))
    }
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
    // Triage: run quarantined files too, record every file's first blocker in
    // `tests/vd_replay/triage.txt`, assert nothing (S-25 P0).
    let triage = std::env::var("VDRS_REPLAY_TRIAGE").is_ok();
    let quarantine = if triage {
        HashSet::new()
    } else {
        load_quarantine()
    };
    let mut outcomes: BTreeMap<String, String> = BTreeMap::new();

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
                Some("vd") | Some("vdj") | Some("vdx")
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
            outcomes.insert(
                stem.clone(),
                format!("NO_GOLDEN {}", no_golden_reason(corpus, &stem)),
            );
            continue;
        }

        let ext = cmd_path.extension().and_then(|x| x.to_str());
        let rows = match ext {
            Some("vdj") => parse_vdj(&cmd_path),
            Some("vdx") => parse_vdx(&cmd_path),
            _ => parse_vd(&cmd_path),
        };
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                outcomes.insert(stem.clone(), format!("FAIL parse: {e}"));
                fail.push((stem.clone(), format!("parse: {e}")));
                continue;
            }
        };

        let mut rp = Replay::new(visidata_root.clone());
        if ext == Some("vdx") {
            rp.cursor = Some(HashMap::new());
        }
        let mut blocked = None;
        for row in &rows {
            match rp.step(row) {
                Ok(()) => {}
                Err(StepErr::PythonExpr(ln)) => {
                    blocked = Some(format!("UNTESTABLE {ln}"));
                    *untestable.entry(ln).or_default() += 1;
                    break;
                }
                Err(StepErr::MissingLongname(ln)) => {
                    blocked = Some(format!("SKIP {ln}"));
                    skip.entry(ln).or_default().push(stem.clone());
                    break;
                }
                Err(StepErr::Runtime(e)) => {
                    let e = format!("runtime: {e}");
                    blocked = Some(format!("SKIP {e}"));
                    skip.entry(e).or_default().push(stem.clone());
                    break;
                }
            }
        }
        if let Some(b) = blocked {
            outcomes.insert(stem, b);
            continue;
        }

        let got = match rp.to_tsv() {
            Ok(s) => normalize(&s),
            Err(e) => {
                let e = format!("serialize: {e}");
                outcomes.insert(stem.clone(), format!("SKIP {e}"));
                skip.entry(e).or_default().push(stem.clone());
                continue;
            }
        };
        // Lossy: `invalid_unicode_sqlite`'s golden holds invalid UTF-8, which the
        // sqlite reader decodes the same way.
        let Ok(want_raw) = fs::read(&golden).map(|b| String::from_utf8_lossy(&b).into_owned())
        else {
            no_golden += 1;
            outcomes.insert(stem, "NO_GOLDEN unreadable".into());
            continue;
        };
        let want = normalize(&want_raw);

        if tsv_eq(&want, &got) {
            outcomes.insert(stem.clone(), "PASS".into());
            pass += 1;
            if std::env::var("VDRS_REPLAY_VERBOSE").is_ok() {
                eprintln!("PASS: {stem}");
            }
        } else {
            let d = short_diff(&want, &got);
            let first = d.lines().nth(1).unwrap_or(&d).trim().to_string();
            outcomes.insert(stem.clone(), format!("FAIL {first}"));
            fail.push((stem, d));
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
    eprintln!("skip buckets:");
    let mut buckets: Vec<_> = skip.iter().collect();
    buckets.sort_by(|a, b| b.1.len().cmp(&a.1.len()).then(a.0.cmp(b.0)));
    let verbose = std::env::var("VDRS_REPLAY_VERBOSE").is_ok();
    for (ln, files) in buckets.iter() {
        eprintln!("  {ln:40}  {} files", files.len());
        if verbose {
            for f in files.iter() {
                eprintln!("      → {f}");
            }
        }
    }
    if !fail.is_empty() {
        eprintln!("\nFAIL details:");
        for (name, msg) in &fail {
            eprintln!("  {name}:\n    {msg}");
        }
    }
    if triage {
        let mut out = String::from(
            "# S-25 P0 triage: every corpus file's first blocker, quarantine ignored.\n\
             # Regenerate: VDRS_REPLAY_TRIAGE=1 cargo test --test vd_replay\n",
        );
        for (stem, o) in &outcomes {
            let o: String = o.lines().next().unwrap_or("").chars().take(160).collect();
            out.push_str(&format!("{stem}\t{o}\n"));
        }
        fs::write("tests/vd_replay/triage.txt", out).unwrap();
        eprintln!("triage written to tests/vd_replay/triage.txt");
        return;
    }
    assert!(fail.is_empty(), "{} replay regressions", fail.len());
    // A runtime error buckets a file as SKIP, so FAIL=0 alone can hide regressions.
    assert!(pass >= MIN_PASS, "PASS fell to {pass}, floor {MIN_PASS}");
}
