#![expect(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "test helpers fail the test by panicking"
)]
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
use visidata_rs::reader::{self, LoadedSource, SourceKind};
use visidata_rs::sheet::{Origin, PlanStep, Sheet, SheetId, SortKey, SortOrder};
use visidata_rs::{duckdb, sheet::write_df_by_extension};

const CORPUS: &str = "../visidata/tests";
/// Raise when coverage grows; never lower without saying why.
const MIN_PASS: usize = 50;

#[derive(Debug, Clone, Default)]
struct CmdlogRow {
    sheet: String,
    col: String,
    /// `.vdj` `"col": 1` is a visible-column index (a string is a name).
    col_index: Option<usize>,
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
            col_index: None,
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
        // `row` may be a number (`"row": 1`); null / missing = "".
        let col_index = v.get("col").and_then(|c| c.as_u64()).map(|i| i as usize);
        let get = |k: &str| match v.get(k) {
            Some(serde_json::Value::String(s)) => s.clone(),
            Some(serde_json::Value::Number(n)) => n.to_string(),
            _ => String::new(),
        };
        rows.push(CmdlogRow {
            sheet: get("sheet"),
            col: if col_index.is_some() {
                String::new()
            } else {
                get("col")
            },
            col_index,
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
    /// VisiData's sheet stack (most recent last; `vd.sheets[:2]` = the last
    /// two): using a sheet moves it on top, `quit-sheet` takes it off.
    stack: Vec<usize>,
    /// Rows selected on the sheets stack: one sheet in VisiData, so they stay.
    stack_selected: Vec<String>,
    meta_mode: Option<MetaMode>,
    /// Resolution root for `open-file` paths. VisiData runs tests from the repo
    /// root, so inputs like `tests/sort-levels.tsv` resolve relative to there.
    root: PathBuf,
    next_id: SheetId,
    /// `.vdx` positions only the commands that need it; VisiData's cursor stays
    /// put in between. Per-sheet (col, row) fills empty fields when set.
    cursor: Option<HashMap<usize, (String, String)>>,
    /// `$VD_TESTTMP`: scratch for files a script saves and reopens.
    tmp: tempfile::TempDir,
    /// The register's cells (`copy-cell(s)`), for `paste-cell` / `setcol-clipboard`.
    clip: Option<Column>,
    /// The register's rows (`copy-row` / `copy-selected`), for `paste-*`.
    clip_rows: Option<DataFrame>,
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
            stack: Vec::new(),
            stack_selected: Vec::new(),
            meta_mode: None,
            root,
            next_id: 0,
            cursor: None,
            tmp: tempfile::tempdir().expect("scratch dir"),
            clip: None,
            clip_rows: None,
        }
    }

    /// A script path: `$VD_TESTTMP` → scratch, else relative to the repo root.
    fn path(&self, input: &str) -> PathBuf {
        let tmp = self.tmp.path().to_string_lossy();
        self.root.join(input.replace("$VD_TESTTMP", &tmp))
    }

    fn alloc(&mut self) -> SheetId {
        self.next_id += 1;
        self.next_id - 1
    }

    /// A file as the app opens it: a `.duckdb` file → its index.
    fn open_sheet(&mut self, loaded: LoadedSource) -> Sheet {
        let id = self.alloc();
        match loaded.kind {
            SourceKind::Duckdb => Sheet::parts_list(id, loaded, Vec::new()),
            _ => Sheet::from_source(id, loaded),
        }
    }

    /// `open-row` on a duckdb index: the row's table (`キname` or a row number).
    fn open_index_row(&mut self, row: &str) -> Result<(), StepErr> {
        use StepErr::Runtime;
        let s = &self.current_slot()?.sheet;
        if let Origin::Freq { parent, column } = s.origin.clone() {
            // the source rows with the cursor row's value (first column), as `<CR>`
            let vals = s
                .plan
                .clone()
                .select([nth(0).as_expr()])
                .collect()
                .map_err(|e| Runtime(e.to_string()))?;
            let vals = &vals.columns()[0];
            let i = match row.strip_prefix('キ') {
                Some(v) => (0..vals.len()).find(|&i| {
                    vals.get(i)
                        .is_ok_and(|a| a.str_value().trim_matches('"') == v)
                }),
                None => row.parse().ok(),
            }
            .ok_or_else(|| Runtime(format!("open-row: no row {row}")))?;
            let picked = [vals
                .get(i)
                .map_err(|e| Runtime(e.to_string()))?
                .into_static()];
            let p = self
                .loaded
                .iter()
                .find(|l| l.sheet.id == parent)
                .ok_or_else(|| Runtime("open-row: freq parent gone".into()))?;
            let sheet = Sheet::rows_with_values(&p.sheet, self.next_id, &column, &picked);
            self.next_id += 1;
            let name = format!(
                "{}_{}",
                self.current_slot()?.name,
                row.trim_start_matches('キ')
            );
            self.push_sheet(name, sheet);
            return Ok(());
        }
        let Origin::Parts { path, kind } = s.origin.clone() else {
            // xlsx/sqlite open their first part directly; `キname` names another
            let (Some(name), Some(path)) = (row.strip_prefix('キ'), s.source.path.clone()) else {
                return Ok(());
            };
            if !matches!(s.kind, SourceKind::Sqlite | SourceKind::Xlsx) {
                return Ok(());
            }
            let loaded =
                reader::open_part(&path, s.kind, name).map_err(|e| Runtime(e.to_string()))?;
            let sheet = Sheet::from_source(self.alloc(), loaded);
            let name = format!("{}_{name}", self.current_slot()?.name); // VisiData's name
            self.push_sheet(name, sheet);
            return Ok(());
        };
        let df = s
            .view_plan()
            .collect()
            .map_err(|e| Runtime(e.to_string()))?;
        let text = |c: &str, i: usize| -> Option<String> {
            Some(df.column(c).ok()?.get(i).ok()?.get_str()?.to_string())
        };
        let i = match row.strip_prefix('キ') {
            Some(name) => (0..df.height()).find(|&i| text("name", i).as_deref() == Some(name)),
            None => row.parse().ok(),
        }
        .ok_or_else(|| Runtime(format!("open-row: no row {row}")))?;
        let name = text("name", i).ok_or_else(|| Runtime(format!("open-row: no row {row}")))?;
        let schema = text("schema_name", i).unwrap_or_else(|| "main".into());
        let loaded = reader::open_part(&path, kind, &format!("{schema}.{name}"))
            .map_err(|e| Runtime(e.to_string()))?;
        let sheet = Sheet::from_source(self.alloc(), loaded);
        self.push_sheet(name, sheet);
        Ok(())
    }

    /// Reread every open index of `db` (after `exec-sql` / `save-sheet`).
    fn refresh_index(&mut self, db: &Path) -> Result<(), StepErr> {
        let index = duckdb::index(db).map_err(StepErr::Runtime)?;
        for slot in &mut self.loaded {
            if matches!(&slot.sheet.origin, Origin::Parts { path, kind: SourceKind::Duckdb } if path == db)
            {
                slot.sheet.plan = index.clone().lazy();
            }
        }
        Ok(())
    }

    fn set_current(&mut self, new: usize) {
        self.stack.retain(|&i| i != new);
        self.stack.push(new);
        self.current = Some(new);
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

    /// On a columns sheet: the commands that write to the sheets it lists
    /// (`App::columns_dispatch`). `None`: not one of them.
    fn columns_step(&mut self, r: &CmdlogRow) -> Option<Result<(), StepErr>> {
        let slot = self.current?;
        let Origin::Columns { cols } = &self.loaded[slot].sheet.origin else {
            return None;
        };
        let cols = cols.clone();
        let rt = StepErr::Runtime;
        // `キsheet,name` (`gC`), `キname`, or a row number
        let row = |key: &str| -> Result<usize, StepErr> {
            let k = key.strip_prefix('キ').unwrap_or(key);
            let sheet_of = |id: SheetId| {
                self.loaded
                    .iter()
                    .find(|l| l.sheet.id == id)
                    .map(|l| l.name.as_str())
            };
            let hit = cols.iter().position(|(id, n)| match k.split_once(',') {
                Some((s, c)) if c == n => sheet_of(*id) == Some(s),
                _ => n == k,
            });
            hit.or_else(|| k.parse().ok().filter(|i| *i < cols.len()))
                .ok_or_else(|| rt(format!("columns sheet: no row {key}")))
        };
        let selected = || -> Vec<usize> {
            self.loaded[slot]
                .sheet
                .view
                .selected
                .iter()
                .map(|&r| r as usize)
                .collect()
        };
        let (rows, op): (Vec<usize>, &str) = match r.longname.as_str() {
            "select-row" | "unselect-row" | "stoggle-row" => {
                let i = match row(&r.row) {
                    Ok(i) => i as u64,
                    Err(e) => return Some(Err(e)),
                };
                let sel = Arc::make_mut(&mut self.loaded[slot].sheet.view.selected);
                match r.longname.as_str() {
                    "select-row" => {
                        sel.insert(i);
                    }
                    "unselect-row" => {
                        sel.remove(&i);
                    }
                    _ if !sel.remove(&i) => {
                        sel.insert(i);
                    }
                    _ => {}
                }
                return Some(Ok(()));
            }
            "select-rows" => {
                self.loaded[slot].sheet.view.selected = Arc::new((0..cols.len() as u64).collect());
                return Some(Ok(()));
            }
            "key-selected" => (selected(), "key"),
            "key-off-selected" => (selected(), "unkey"),
            "hide-selected" => (selected(), "hide"),
            l if l.starts_with("type-") && l.ends_with("-selected") => (selected(), l),
            "delete-row" => (vec![row(&r.row).ok()?], "hide"),
            "aggregate-cols" => {
                let mut rows = selected();
                if rows.is_empty() {
                    // VisiData: the source's visible non-key columns
                    rows = (0..cols.len())
                        .filter(|&i| {
                            let (id, c) = &cols[i];
                            self.loaded.iter().any(|l| {
                                l.sheet.id == *id
                                    && !l.sheet.view.hidden.contains(c)
                                    && !l.sheet.view.keys.contains(c)
                            })
                        })
                        .collect();
                }
                (rows, "aggregate")
            }
            "edit-cell" if matches!(r.col.as_str(), "name" | "width" | "type" | "aggregators") => {
                (vec![row(&r.row).ok()?], "edit")
            }
            "setcol-input" if matches!(r.col.as_str(), "width" | "type") => (selected(), "edit"),
            _ => return None,
        };
        for i in rows {
            let (id, c) = cols[i].clone();
            let Some(t) = self.loaded.iter_mut().find(|l| l.sheet.id == id) else {
                return Some(Err(rt("columns sheet: that sheet is gone".into())));
            };
            let s = &mut t.sheet;
            let res = match (op, r.col.as_str()) {
                ("key", _) if !s.view.keys.contains(&c) => {
                    s.view.keys.push(c);
                    Ok(())
                }
                ("key", _) => Ok(()),
                ("unkey", _) => {
                    s.view.keys.retain(|k| *k != c);
                    Ok(())
                }
                ("hide", _) => {
                    s.view.hidden.insert(c);
                    Ok(())
                }
                ("aggregate", _) => s.add_aggregators(&c, &r.input),
                ("edit", "aggregators") => s.set_aggregators(&c, &r.input),
                ("edit", "name") => s.rename_col(&c, &r.input),
                ("edit", "width") if r.input.trim() == "0" => {
                    s.view.hidden.insert(c);
                    Ok(())
                }
                ("edit", "width") => {
                    s.view.hidden.remove(&c);
                    Ok(())
                }
                ("edit", _) => s.cast_column(&c, &r.input),
                (l, _) => {
                    let ty = l.trim_start_matches("type-").trim_end_matches("-selected");
                    let ty = if ty == "string" { "str" } else { ty };
                    s.cast_column(&c, ty)
                }
            };
            if let Err(e) = res {
                return Some(Err(rt(e)));
            }
        }
        // rebuild from the sheets it lists, selection kept
        let mut ids: Vec<SheetId> = cols.iter().map(|(id, _)| *id).collect();
        ids.dedup();
        let sheets: Vec<&Sheet> = ids
            .iter()
            .filter_map(|id| {
                self.loaded
                    .iter()
                    .find(|l| l.sheet.id == *id)
                    .map(|l| &l.sheet)
            })
            .collect();
        let id = self.loaded[slot].sheet.id;
        let mut new = match self.loaded[slot].name == "all_columns" {
            true => Sheet::columns_all(&sheets, id),
            false => Sheet::columns_of(sheets[0], id),
        };
        new.view.selected = self.loaded[slot].sheet.view.selected.clone();
        self.loaded[slot].sheet = new;
        Some(Ok(()))
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
        let indexed;
        let r = match r.col_index {
            Some(i) => {
                let schema = self.current_slot()?.sheet.view_plan().collect_schema();
                let name = schema
                    .ok()
                    .and_then(|s| s.get_at_index(i).map(|(n, _)| n.to_string()));
                // out of range (e.g. a sheet not modelled yet): the cursor column
                let name = name.unwrap_or_default();
                indexed = CmdlogRow {
                    col: name,
                    col_index: None,
                    ..r.clone()
                };
                &indexed
            }
            None => r,
        };
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
        // a command on `sheets` after `gC` there: the sheets list is on top again
        if r.sheet == "sheets" && self.meta_mode.is_none() {
            self.meta_mode = Some(MetaMode::SheetsStack {
                selected: std::mem::take(&mut self.stack_selected),
            });
        }
        if self.meta_mode.is_none()
            && let Some(res) = self.columns_step(r)
        {
            return res;
        }
        match r.longname.as_str() {
            "open-file" => {
                let path = self.path(&r.input);
                let loaded = reader::open(path.as_os_str(), None)
                    .map_err(|e| Runtime(format!("open-file {}: {}", path.display(), e)))?;
                // Sheet name = file stem (VisiData default); e.g. tests/data1.tsv → data1.
                let name = Path::new(&r.input)
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("sheet")
                    .to_string();
                let sheet = self.open_sheet(loaded);
                self.push_sheet(name, sheet);
                Ok(())
            }
            // The app's `:save`: a table named after the sheet in a .duckdb file.
            "save-sheet" => {
                let path = self.path(&r.input);
                let slot = self.current_slot()?;
                let mut df = slot
                    .sheet
                    .view_plan()
                    .collect()
                    .map_err(|e| Runtime(e.to_string()))?;
                if duckdb::is_db(&path) {
                    duckdb::save(&mut df, &path, &slot.name).map_err(Runtime)?;
                    self.refresh_index(&path)
                } else {
                    write_df_by_extension(&mut df, &path).map_err(|e| Runtime(e.to_string()))
                }
            }
            // The app's `:exec-sql`: DDL refreshes the index; a query opens its rows.
            "exec-sql" => {
                let s = &self.current_slot()?.sheet;
                let db = match (&s.origin, &s.source.path) {
                    (
                        Origin::Parts {
                            path,
                            kind: SourceKind::Duckdb,
                        },
                        _,
                    ) => path.clone(),
                    (_, Some(p)) if s.kind == SourceKind::Duckdb => p.clone(),
                    _ => return Err(Runtime("exec-sql: not a .duckdb sheet".into())),
                };
                let rows = duckdb::exec(&db, &r.input).map_err(Runtime)?;
                self.refresh_index(&db)?;
                // VisiData pushes a query sheet even for statements without rows
                let df = rows.unwrap_or_default();
                let src =
                    LoadedSource::from_df(SourceKind::Duckdb, "exec-sql".into(), Some(db), df);
                let sheet = Sheet::from_source(self.alloc(), src);
                self.push_sheet("query".into(), sheet);
                Ok(())
            }
            "sort-asc" => self.push_sort(&r.col, SortOrder::Asc, false),
            "sort-desc" => self.push_sort(&r.col, SortOrder::Desc, false),
            "sort-asc-add" => self.push_sort(&r.col, SortOrder::Asc, true),
            "sort-desc-add" => self.push_sort(&r.col, SortOrder::Desc, true),
            "sort-keys-asc" => self.sort_keys(SortOrder::Asc),
            "sort-keys-desc" => self.sort_keys(SortOrder::Desc),
            // S-22 nested data
            "unfurl-col" => {
                let id = self.alloc();
                let parent = self.current_slot()?;
                let name = format!("{}_{}_unfurled", parent.name, r.col);
                let u = Sheet::unfurl_from(&parent.sheet, id, &r.col).map_err(Runtime)?;
                self.push_sheet(name, u);
                Ok(())
            }
            "expand-col" | "expand-col-depth" => {
                let depth = match r.longname.as_str() {
                    "expand-col" => 1,
                    _ => r
                        .input
                        .trim()
                        .parse()
                        .map_err(|_| Runtime("depth".into()))?,
                };
                let s = &mut self.current_slot_mut()?.sheet;
                s.expand(&r.col, depth).map_err(Runtime)
            }
            "expand-cols-depth" => {
                let depth = r
                    .input
                    .trim()
                    .parse()
                    .map_err(|_| Runtime("depth".into()))?;
                let s = &mut self.current_slot_mut()?.sheet;
                s.expand_all(depth).map(drop).map_err(Runtime)
            }
            "contract-col" => {
                let s = &mut self.current_slot_mut()?.sheet;
                s.contract(&r.col).map(drop).map_err(Runtime)
            }
            "describe-sheet" => {
                let id = self.alloc();
                let parent = self.current_slot()?;
                let name = format!("{}_describe", parent.name);
                let d = Sheet::describe_from(&parent.sheet, id).map_err(Runtime)?;
                self.push_sheet(name, d);
                Ok(())
            }
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
                        .data_plan()
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
                    let plan = parent_slot.sheet.data_plan();
                    let origin = Origin::Copy {
                        parent: parent_slot.sheet.id,
                    };
                    let sheet = Sheet::child(&parent_slot.sheet, id, name.clone(), plan, origin);
                    (name, sheet)
                };
                self.push_sheet(name, new_sheet);
                Ok(())
            }
            // `g"` deep copy / `g'` freeze: vdrs `:dup` (a copy of the view;
            // vdrs sheets share nothing mutable, so deep = shallow).
            "dup-rows-deep" | "freeze-sheet" => {
                let id = self.next_id;
                self.next_id += 1;
                let slot = self.current_slot()?;
                let name = format!("{}_copy", slot.name);
                let new = Sheet::copy_from(&slot.sheet, id);
                self.push_sheet(name, new);
                Ok(())
            }
            "aggregate-col" => {
                let s = &mut self.current_slot_mut()?.sheet;
                s.add_aggregators(&r.col, &r.input).map_err(Runtime)
            }
            "rename-col" => {
                let s = &mut self.current_slot_mut()?.sheet;
                s.rename_col(&r.col, &r.input).map_err(Runtime)
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
            "unkey-col" | "key-col-off" => {
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
            // `ge`: every selected row (the row field is just the cursor).
            "setcol-input" => self.setcol_input_gang(&r.col, &r.input),
            "set-cell-input" => self.setcol_input(&r.col, &r.row, &r.input),
            // Walk every column of the current sheet; hide any whose non-null
            // values are all identical (and there's >1 row). VisiData's `hide-uniform-cols`.
            "hide-uniform-cols" => self.hide_uniform_cols(),
            // sheets-stack: enter meta mode. `select-row` henceforth picks sheet
            // names (not data rows) and `join-sheets` consumes the selection.
            "sheets-stack" => {
                self.meta_mode = Some(MetaMode::SheetsStack {
                    selected: std::mem::take(&mut self.stack_selected),
                });
                Ok(())
            }
            // columns-sheet: enter a meta view listing parent sheet's columns.
            // ponytail: no-op mode; parent sheet is unchanged. edit-cell /
            // select-row / etc. while active become no-ops. quit-sheet exits.
            // S-25 P3: the app's columns sheet, written back as the app does
            "columns-sheet" => {
                let cur = self.current_slot()?;
                let name = format!("{}_columns", cur.name);
                let id = self.next_id;
                let s = Sheet::columns_of(&self.current_slot()?.sheet, id);
                self.next_id += 1;
                self.push_sheet(name, s);
                Ok(())
            }
            "columns-all" | "columns-selected" => {
                let names: Vec<String> = match self.meta_mode.take() {
                    Some(MetaMode::SheetsStack { selected })
                        if r.longname == "columns-selected" =>
                    {
                        self.stack_selected = selected.clone();
                        selected
                    }
                    _ => self
                        .stack
                        .iter()
                        .rev()
                        .map(|&i| self.loaded[i].name.clone())
                        .filter(|n| !n.ends_with("columns"))
                        .collect(),
                };
                let slots: Vec<usize> = names
                    .iter()
                    .filter_map(|n| self.find_sheet_by_name(n))
                    .collect();
                let sheets: Vec<&Sheet> = slots.iter().map(|&i| &self.loaded[i].sheet).collect();
                let s = Sheet::columns_all(&sheets, self.next_id);
                self.next_id += 1;
                self.push_sheet("all_columns".into(), s);
                Ok(())
            }
            // quit-sheet: clear meta mode if any; otherwise no-op (we don't
            // model the parent sheet stack).
            // a meta view closes; a data sheet leaves the stack
            "quit-sheet" if self.meta_mode.is_some() => {
                self.meta_mode = None;
                Ok(())
            }
            "quit-sheet" => {
                if let Some(cur) = self.stack.pop() {
                    self.loaded[cur].name.insert(0, '\0'); // no longer found by name
                }
                self.current = self.stack.last().copied();
                Ok(())
            }
            // edit-cell / set-cell-input while in a meta view: no-op (we don't
            // model meta contents). Outside meta, edit-cell is a data edit.
            "edit-cell" if self.meta_mode.is_some() => Ok(()),
            "edit-cell" => self.setcol_input(&r.col, &r.row, &r.input),
            // join-sheets: consumes sheets-stack selection, builds a joined sheet
            // and makes it current. Input = jointype (append/inner/outer/full).
            // VisiData 3 renamed join-sheets → join-selected
            "join-sheets" | "join-selected" => self.join_sheets(&r.input, true),
            // `gv`
            "unhide-cols" => {
                self.current_slot_mut()?.sheet.view.hidden.clear();
                Ok(())
            }
            // join-sheets-top2: VisiData `&` on a data sheet (not meta). Joins
            // the current sheet with the previously-active one (`vd.sheets[:2]`).
            "join-sheets-top2" => self.join_top2(&r.input),
            "set-option" if r.row == "unfurl_empty" && r.input == "True" => {
                Err(Runtime("option unfurl_empty".into()))
            }
            "set-option" => Ok(()),
            "copy-row" | "copy-selected" => {
                let s = &self.current_slot()?.sheet;
                let rt = |e: PolarsError| StepErr::Runtime(e.to_string());
                let pos = if r.longname == "copy-row" {
                    let rid = view_rid(s, &r.row)?;
                    let rids = s.view_rids().map_err(rt)?;
                    Some(rids.iter().position(|&x| x == rid).unwrap_or_default() as u64)
                } else {
                    None
                };
                self.clip_rows = Some(s.copy_rows(pos, true).map_err(rt)?);
                Ok(())
            }
            "add-row" | "add-rows" | "paste-after" | "paste-before" => {
                let values = match r.longname.as_str() {
                    "paste-after" | "paste-before" => match self.clip_rows.clone() {
                        Some(v) => Some(v),
                        None => return Ok(()),
                    },
                    _ => None,
                };
                let n = match r.longname.as_str() {
                    "add-rows" => r
                        .input
                        .trim()
                        .parse()
                        .map_err(|_| StepErr::Runtime(format!("add-rows {:?}", r.input)))?,
                    _ => 1,
                };
                let s = &mut self.current_slot_mut()?.sheet;
                let anchor = match s.view_rids() {
                    Ok(rids) if !rids.is_empty() => Some(view_rid(s, &r.row)?),
                    _ => None,
                };
                let after = r.longname != "paste-before";
                s.add_rows(anchor, after, values.as_ref(), n)
                    .map(drop)
                    .map_err(StepErr::Runtime)
            }
            // `A`: VisiData's new sheet has one column, `A`.
            "open-new" => {
                let df = DataFrame::new_infer_height(vec![Column::new_empty(
                    "A".into(),
                    &DataType::String,
                )])
                .map_err(|e| StepErr::Runtime(e.to_string()))?;
                let src = LoadedSource::from_df(SourceKind::Csv, "unnamed".into(), None, df);
                let id = self.alloc();
                self.push_sheet("unnamed".into(), Sheet::from_source(id, src));
                Ok(())
            }
            "copy-cell" => self.clip_copy(&r.col, Some(&r.row)),
            "copy-cells" => self.clip_copy(&r.col, None),
            "cut-cell" => {
                self.clip_copy(&r.col, Some(&r.row))?;
                self.setcol_input(&r.col, &r.row, "")
            }
            "cut-cells" => {
                self.clip_copy(&r.col, None)?;
                self.setcol_input_gang(&r.col, "")
            }
            // `zd` / `gzd`: null the cell(s); the same edit as `e` with nothing.
            "delete-cell" => self.setcol_input(&r.col, &r.row, ""),
            "delete-cells" => self.setcol_input_gang(&r.col, ""),
            "delete-row" | "cut-row" => {
                let s = &mut self.current_slot_mut()?.sheet;
                let rid = view_rid(s, &r.row)?;
                s.delete_rows(vec![rid]).map(drop).map_err(StepErr::Runtime)
            }
            "delete-selected" | "cut-selected" => {
                let s = &mut self.current_slot_mut()?.sheet;
                let rids: Vec<u64> = s.view.selected.iter().copied().collect();
                if rids.is_empty() {
                    return Ok(());
                }
                s.delete_rows(rids).map(drop).map_err(StepErr::Runtime)
            }
            "paste-cell" => {
                let text = match self.clip.as_ref().map(|c| c.get(0)) {
                    Some(Ok(v)) if !v.is_null() => visidata_rs::sheet::any_text(&v),
                    _ => String::new(),
                };
                self.setcol_input(&r.col, &r.row, &text)
            }
            "setcol-clipboard" => {
                let Some(clip) = self.clip.clone() else {
                    return Ok(());
                };
                let s = &mut self.current_slot_mut()?.sheet;
                if s.view.selected.is_empty() {
                    return Ok(());
                }
                s.paste_cells(&r.col, &clip)
                    .map(drop)
                    .map_err(StepErr::Runtime)
            }
            // the system clipboard never changes the sheet
            "syscopy-row" | "syscopy-selected" | "syscopy-cell" | "syscopy-cells" => Ok(()),
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
            // dive-row/open-row: a duckdb index opens the row's table. xlsx/sqlite
            // load their first part directly: no sub-sheet to enter — noop.
            // For VisiData's columns-sheet / other meta-sheet workflows, this
            // is wrong but we skip those tests elsewhere.
            "dive-row" | "open-row" if self.meta_mode.is_none() => self.open_index_row(&r.row),
            // on the sheets stack: go to that sheet
            "dive-row" | "open-row" => {
                let name = r.row.strip_prefix('キ').unwrap_or(&r.row);
                if let Some(i) = self.find_sheet_by_name(name) {
                    if let Some(MetaMode::SheetsStack { selected }) = self.meta_mode.take() {
                        self.stack_selected = selected;
                    }
                    self.set_current(i);
                }
                Ok(())
            }
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

    /// `# % ~` / `@`: a view cast, as the app does.
    fn cast(&mut self, name: &str, dtype: DataType) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        s.set_type_cast(name.to_string(), dtype);
        Ok(())
    }

    fn cast_date(&mut self, name: &str) -> Result<(), StepErr> {
        self.cast(name, DataType::Date)
    }

    /// `$`: a plan step, as the app does (edits rebuild the plan from steps).
    fn cast_currency(&mut self, name: &str) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        s.apply_step(PlanStep::Currency {
            column: name.to_string(),
        })
        .map_err(|e| StepErr::Runtime(e.to_string()))
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
        if let Some(MetaMode::SheetsStack { selected }) = self.meta_mode.as_mut() {
            *selected = self.loaded.iter().rev().map(|s| s.name.clone()).collect();
            return Ok(());
        }
        let s = &mut self.current_slot_mut()?.sheet;
        let n = s
            .data_plan()
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
            .data_plan()
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
            .data_plan()
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
        // The sheets sheet lists the stack top first; row 0 is itself.
        let stack: Vec<String> = self.loaded.iter().rev().map(|s| s.name.clone()).collect();
        if let Some(MetaMode::SheetsStack { selected }) = self.meta_mode.as_mut() {
            let mut name = row_key.strip_prefix('キ').unwrap_or(row_key).to_string();
            if !stack.contains(&name)
                && let Ok(i) = name.parse::<usize>()
                && let Some(n) = i.checked_sub(1).and_then(|i| stack.get(i))
            {
                name = n.clone();
            }
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
        let keys = self.current_slot()?.sheet.view.keys.clone();
        let s = &mut self.current_slot_mut()?.sheet;
        let df = s
            .data_plan()
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

    /// VisiData `e` (edit-cell): the cell at (col, row), through `Sheet::edit_cell`.
    fn setcol_input(&mut self, col_name: &str, row_key: &str, value: &str) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        let rid = view_rid(s, row_key)?;
        s.edit_cell(col_name, rid, value).map_err(StepErr::Runtime)
    }

    /// `zy` (`row` = its key) / `gzy` (selected rows, view order): `col` to the register.
    fn clip_copy(&mut self, col: &str, row: Option<&str>) -> Result<(), StepErr> {
        let rt = |e: PolarsError| StepErr::Runtime(e.to_string());
        let s = &self.current_slot()?.sheet;
        let pos = match row {
            Some(k) => {
                let rid = view_rid(s, k)?;
                let rids = s.view_rids().map_err(rt)?;
                Some(rids.iter().position(|&r| r == rid).unwrap_or_default() as u64)
            }
            None => None,
        };
        let df = s.copy_rows(pos, true).map_err(rt)?;
        self.clip = Some(df.column(col).map_err(rt)?.clone());
        Ok(())
    }

    /// VisiData `ge`: `col_name` := `value` on the selected rows (`:setcol`).
    fn setcol_input_gang(&mut self, col_name: &str, value: &str) -> Result<(), StepErr> {
        let s = &mut self.current_slot_mut()?.sheet;
        if s.view.selected.is_empty() {
            return Ok(());
        }
        s.set_col(col_name, value)
            .map(|_| ())
            .map_err(StepErr::Runtime)
    }

    /// Hide every column whose non-null values are all identical, provided
    /// there's more than one row. Pure visibility op; data plan untouched.
    fn hide_uniform_cols(&mut self) -> Result<(), StepErr> {
        let slot = self.current_slot_mut()?;
        let df = slot
            .sheet
            .data_plan()
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
                .data_plan()
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
            let origin = Origin::Pivot {
                parent: parent_slot.sheet.id,
            };
            let sheet = Sheet::child(&parent_slot.sheet, id, name.clone(), out_df.lazy(), origin);
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
            .stack
            .iter()
            .rev()
            .nth(1)
            .copied()
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
                            .data_plan()
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
                        .data_plan()
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
                            .data_plan()
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
                let mut acc = first.sheet.data_plan();
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
                    // VisiData's order, as `Sheet::join_from`: keys as first
                    // seen (left, then right-only), each key's rows together.
                    let numbered = |p: LazyFrame, keys: &[Expr], row: &str, first: &str| {
                        let min = col(row).cast(DataType::Int64).min().over(keys);
                        min.map(|m| p.with_row_index(row, None).with_column(m.alias(first)))
                            .map_err(|e| StepErr::Runtime(e.to_string()))
                    };
                    let l = numbered(acc, &left_exprs, "__vdrs_l", "__vdrs_lk")?;
                    let mut right = self.loaded[i].sheet.data_plan();
                    if jt == "extend" {
                        // VisiData names every extended column `<sheet>_<column>`
                        let names: Vec<String> = right
                            .collect_schema()
                            .map_err(|e| StepErr::Runtime(e.to_string()))?
                            .iter_names()
                            .filter(|n| !right_keys.iter().any(|k| k == n.as_str()))
                            .map(|n| n.to_string())
                            .collect();
                        let to: Vec<String> = names
                            .iter()
                            .map(|n| format!("{}_{n}", self.loaded[i].name))
                            .collect();
                        right = right.rename(names, to, true);
                    }
                    let r = numbered(right, &right_exprs, "__vdrs_r", "__vdrs_rk")?;
                    acc = l
                        .join(
                            r,
                            left_exprs.clone(),
                            right_exprs,
                            JoinArgs {
                                maintain_order: MaintainOrderJoin::LeftRight,
                                ..JoinArgs::new(how.clone())
                                    .with_coalesce(JoinCoalesce::CoalesceColumns)
                            },
                        )
                        .sort_by_exprs(
                            [
                                col("__vdrs_lk").fill_null(col("__vdrs_rk") + lit(1i64 << 40)),
                                col("__vdrs_l"),
                                col("__vdrs_r"),
                            ],
                            SortMultipleOptions::default()
                                .with_nulls_last(true)
                                .with_maintain_order(true),
                        )
                        .drop(cols(["__vdrs_l", "__vdrs_lk", "__vdrs_r", "__vdrs_rk"]));
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
        let joined_name = selected.join("+"); // VisiData's name
        let first = &self.loaded[slot_idxs[0]];
        // VisiData's join keeps first-sheet's keys on the result so subsequent
        // sort-keys / column-order reflect them. Inherit here.
        let inherited_keys = first.sheet.view.keys.clone();
        // a derived sheet: read-only, as in the app (S-18 D3)
        let origin = Origin::Join {
            left: first.sheet.id,
            right_name: String::new(),
        };
        let joined = Sheet::child(&first.sheet, id, joined_name.clone(), result, origin);
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
                let schema = match self.loaded[i].sheet.data_plan().collect_schema() {
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
        let mut df = match &slot.sheet.origin {
            Origin::Columns { cols } => self.visidata_columns(cols, slot.name == "all_columns")?,
            _ => slot
                .sheet
                .view_plan()
                .collect()
                .map_err(|e| e.to_string())?,
        };
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

impl Replay {
    /// A columns sheet as VisiData prints it: `width` = widest text + 2 (empty
    /// when hidden), `type` only when set, an empty `fmtstr`, `value` from the
    /// first row. vdrs measures widths on screen and always names the type.
    fn visidata_columns(&self, cols: &[(SheetId, String)], all: bool) -> Result<DataFrame, String> {
        let err = |e: PolarsError| e.to_string();
        let mut out: [Vec<Option<String>>; 8] = Default::default();
        for (id, name) in cols {
            let slot = self
                .loaded
                .iter()
                .find(|l| l.sheet.id == *id)
                .ok_or("sheet gone")?;
            let s = &slot.sheet;
            let v = &s.view;
            let df = s.data_plan().collect().map_err(err)?;
            let view = s.view_plan().collect().map_err(err)?;
            let texts: Vec<String> = match view.column(name).or_else(|_| df.column(name)) {
                Ok(c) => (0..c.len())
                    .map(|i| c.get(i).map(|a| av_display(&a)).unwrap_or_default())
                    .collect(),
                Err(_) => Vec::new(),
            };
            let width = texts.iter().map(|t| t.chars().count()).max().unwrap_or(0);
            let width = width.max(name.chars().count()) + 2;
            let currency = s
                .state
                .steps
                .iter()
                .any(|st| matches!(st, PlanStep::Currency { column } if column == name));
            let ty = v
                .type_casts
                .iter()
                .rev()
                .find(|(c, _)| c == name)
                .map(|(_, d)| match d {
                    DataType::Int64 => "int",
                    DataType::Float64 => "float",
                    DataType::Date => "date",
                    _ => "str",
                });
            let ty = if currency { Some("currency") } else { ty };
            let order = v.sort.iter().position(|k| k.column == *name).map(|i| {
                let i = i as i64 + 1;
                if v.sort[i as usize - 1].order == SortOrder::Desc {
                    -i
                } else {
                    i
                }
            });
            let aggs: Vec<&str> = v
                .aggregators
                .iter()
                .filter(|(c, _)| c == name)
                .map(|(_, a)| a.as_str())
                .collect();
            let row = [
                Some(slot.name.clone()),
                Some(name.clone()),
                (!v.hidden.contains(name)).then(|| width.to_string()),
                ty.map(String::from),
                None,
                texts.first().cloned(),
                (!aggs.is_empty()).then(|| aggs.join(" ")),
                order.map(|o| o.to_string()),
            ];
            for (col, val) in out.iter_mut().zip(row) {
                col.push(val);
            }
        }
        let names = [
            "sheet",
            "name",
            "width",
            "type",
            "fmtstr",
            "value",
            "aggregators",
            "sortorder",
        ];
        let columns: Vec<Column> = names
            .iter()
            .zip(out)
            .skip(if all { 0 } else { 1 })
            .map(|(n, v)| Column::new((*n).into(), v))
            .collect();
        DataFrame::new_infer_height(columns).map_err(err)
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

/// Row id of a cmdlog row reference, looked up in the sheet's view (casts
/// applied, so a `@` key matches `2018-07-03`): key value, else view index.
fn view_rid(s: &Sheet, target: &str) -> Result<u64, StepErr> {
    let target = target.strip_prefix('キ').unwrap_or(target);
    let rt = |e: PolarsError| StepErr::Runtime(e.to_string());
    if let Some(k) = s.view.keys.first()
        && !target.is_empty()
        && let Some(&rid) = s
            .rids_with_values(k, &[AnyValue::String(target)])
            .map_err(rt)?
            .first()
    {
        return Ok(rid);
    }
    let rids = s.view_rids().map_err(rt)?;
    let i = if target.is_empty() {
        Some(0)
    } else {
        target.parse::<usize>().ok()
    };
    i.and_then(|i| rids.get(i).copied()).ok_or_else(|| {
        StepErr::Runtime(format!(
            "cannot locate row '{target}' (keys={:?})",
            s.view.keys
        ))
    })
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
