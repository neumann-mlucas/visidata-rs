//! [`PlanStep`]: an edit baked into the sheet's plan (rename, derive, setcol, …), and how it applies.

use super::*;

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
    /// `:setcol`, `e`: `column` := `value` (None = null) on rows `rids`.
    /// `rids` behind `Arc` so undo snapshots share them.
    SetCol {
        column: String,
        value: Option<String>,
        dtype: DataType,
        rids: Arc<[u64]>,
    },
    /// `:setexpr`, `g=`: `column` := SQL `expr` on rows `rids`, cast to the
    /// column's type `dtype` (S-21 B).
    SetExpr {
        column: String,
        expr: String,
        dtype: DataType,
        rids: Arc<[u64]>,
    },
    /// `gzp`: a value per row. `patch` holds `__vdrs_rid` (u64) and
    /// `__vdrs_v` (the column's type); applied as one left join (S-18 §2.1).
    SetCells {
        column: String,
        patch: Arc<DataFrame>,
    },
    /// `d`, `gd`, `x`, `gx`: rows `rids` are gone. Not applied in the step
    /// chain: every step keeps rows, so a row id stays its position in the
    /// base; `filtered_plan` drops these rows after all steps (S-18 §2.3).
    DeleteRows {
        rids: Arc<[u64]>,
    },
    /// `a`, `ga`, `p`, `P`: new rows. `rows` holds [`ROW_ID`] (ids from
    /// [`ADDED_RID`]), [`POS`] (where they go in base order, §2.3) and the
    /// pasted values under the base's column names and types. Kept at the
    /// front of the steps: `rebuild_plan` adds them to the base, so every
    /// later step (derives, edits by id) sees them.
    AddRows {
        rows: Arc<DataFrame>,
    },
    /// `:s`, `*`, `g*` (S-28): regex `pattern` → `replacement` (Rust regex
    /// syntax, `$1`) in text `column`; `all`: every match, else the first.
    /// `rids` `None`: every row.
    Replace {
        column: String,
        pattern: String,
        replacement: String,
        all: bool,
        rids: Option<Arc<[u64]>>,
    },
    /// `:type json` (S-22 C): JSON text in `column` decoded as `dtype`, the
    /// type inferred from every value when typed.
    Json {
        column: String,
        dtype: DataType,
    },
    /// `(` (S-22): struct `column`'s fields as columns right after it.
    /// `fields`: (path into the struct, new column name); `after`: the
    /// columns that followed `column` when expanded.
    Expand {
        column: String,
        fields: Vec<(Vec<String>, String)>,
        after: Vec<String>,
    },
    /// `;` (S-31): new struct column `name` holding `pattern`'s capture
    /// groups in `column`'s text, one field per group (its name, else its
    /// number); null where the pattern doesn't match.
    Capture {
        column: String,
        pattern: String,
        name: String,
    },
    /// `:lookup` (S-36 A): new column `name` = `table`'s `__v` where its
    /// `__k` (text, unique) equals `key`'s value as text; null without a
    /// match. `link`: where `table` came from, for refreshing it (A4).
    Lookup {
        name: String,
        key: String,
        table: Arc<DataFrame>,
        link: Option<LookupLink>,
    },
}

/// A live lookup's source: `column` of sheet `sheet` by `their_key`, as
/// collected when that sheet's [`Sheet::lookup_stamp`] was `stamp`.
#[derive(Debug, Clone, PartialEq)]
pub struct LookupLink {
    pub sheet: SheetId,
    pub their_key: String,
    pub column: String,
    pub stamp: String,
}

/// First id of an added row; source rows' ids are positions, far below.
pub const ADDED_RID: u64 = 1 << 62;
/// Position key column of added rows (source rows: their id).
pub const POS: &str = "__vdrs_pos";

/// `steps` on top of `plan`. Polars recurses once per plan node, and in a
/// debug build about 30 chained `with_column`s overflow a 2 MiB stack, so a
/// run of column steps goes into one `with_columns`: a step joins the run
/// unless it reads a column the run writes, and an edit of a column the run
/// already writes wraps that column's expression (nested expressions are
/// cheap, 1600 deep is fine).
pub(super) fn apply_steps(mut plan: LazyFrame, steps: &[PlanStep]) -> PolarsResult<LazyFrame> {
    let mut run: Vec<(String, Expr)> = Vec::new();
    let flush = |plan: LazyFrame, run: &mut Vec<(String, Expr)>| match run.is_empty() {
        true => plan,
        false => plan.with_columns(
            std::mem::take(run)
                .into_iter()
                .map(|(n, e)| e.alias(n))
                .collect::<Vec<_>>(),
        ),
    };
    for step in steps {
        let Some(target) = step.column_target() else {
            plan = step.apply(flush(plan, &mut run))?;
            continue;
        };
        // what it reads apart from the target's value so far, which the run supplies
        let own = step.column_value(col(CURRENT))?;
        let clash = reads(&own).is_none_or(|r| r.iter().any(|c| run.iter().any(|(n, _)| n == c)));
        if clash {
            plan = flush(plan, &mut run);
        }
        let current = run
            .iter()
            .find(|(n, _)| n == target)
            .map_or_else(|| col(target), |(_, e)| e.clone());
        let e = step.column_value(current)?;
        match run.iter_mut().find(|(n, _)| n == target) {
            Some(slot) => slot.1 = e,
            None => run.push((target.to_string(), e)),
        }
    }
    Ok(flush(plan, &mut run))
}

/// Stands for a column step's target in [`apply_steps`]; never a real column.
const CURRENT: &str = "\0vdrs_current";

/// The columns `e` reads; None when it selects them some other way (a pattern).
fn reads(e: &Expr) -> Option<Vec<PlSmallStr>> {
    let mut out = Vec::new();
    for node in e {
        match node {
            Expr::Column(n) => out.push(n.clone()),
            Expr::Selector(_) => return None,
            _ => {}
        }
    }
    Some(out)
}

impl PlanStep {
    /// The one column a column step sets, from the columns before it: such a
    /// step is `with_column(column_value(col(target)))`.
    fn column_target(&self) -> Option<&str> {
        match self {
            PlanStep::Currency { column }
            | PlanStep::SetCol { column, .. }
            | PlanStep::SetExpr { column, .. }
            | PlanStep::SetCells { column, .. }
            | PlanStep::Replace { column, .. }
            | PlanStep::Json { column, .. } => Some(column),
            PlanStep::Derive { name, .. }
            | PlanStep::Capture { name, .. }
            | PlanStep::Lookup { name, .. } => Some(name),
            PlanStep::Rename { .. }
            | PlanStep::DeleteRows { .. }
            | PlanStep::AddRows { .. }
            | PlanStep::Expand { .. } => None,
        }
    }

    /// A column step's new value of its target, whose value so far is `current`.
    fn column_value(&self, current: Expr) -> PolarsResult<Expr> {
        let on_rows =
            |rids: &[u64], v: Expr, current: Expr| when(rid_in(rids)).then(v).otherwise(current);
        Ok(match self {
            PlanStep::Currency { .. } => current
                .cast(DataType::String)
                .str()
                .replace_all(lit(r"[^0-9.\-]"), lit(""), false)
                .cast(DataType::Float64),
            PlanStep::Derive { expr, .. } => polars::sql::sql_expr(expr)?,
            PlanStep::SetCol {
                value, dtype, rids, ..
            } => on_rows(
                rids,
                match value {
                    Some(v) if *dtype == DataType::Boolean => lit(v == "true"),
                    Some(v) => lit(v.to_string()).strict_cast(dtype.clone()),
                    None => lit(Null {}).cast(dtype.clone()),
                },
                current,
            ),
            PlanStep::SetExpr {
                expr, dtype, rids, ..
            } => on_rows(
                rids,
                polars::sql::sql_expr(expr)?.cast(dtype.clone()),
                current,
            ),
            // A value lookup, not a join: a join per pasted or numbered
            // column nests the plan until collecting it overflows the stack.
            PlanStep::SetCells { patch, .. } => {
                let rids = patch.column(ROW_ID)?.as_materialized_series().clone();
                let vals = patch.column("__vdrs_v")?.as_materialized_series().clone();
                let dtype = vals.dtype().clone();
                col(ROW_ID).replace_strict(lit(rids), lit(vals), Some(current), Some(dtype))
            }
            PlanStep::Replace {
                pattern,
                replacement,
                all,
                rids,
                ..
            } => {
                let s = current.clone().str();
                let (p, r) = (lit(pattern.as_str()), lit(replacement.as_str()));
                let v = match all {
                    true => s.replace_all(p, r, false),
                    false => s.replace(p, r, false),
                };
                match rids {
                    Some(rids) => on_rows(rids, v, current),
                    None => v,
                }
            }
            PlanStep::Json { dtype, .. } => current.str().json_decode(dtype.clone()),
            PlanStep::Lookup { key, table, .. } => {
                let keys = table.column("__k")?.as_materialized_series().clone();
                let vals = table.column("__v")?.as_materialized_series().clone();
                let dtype = vals.dtype().clone();
                col(key.as_str()).cast(DataType::String).replace_strict(
                    lit(keys),
                    lit(vals),
                    Some(lit(NULL).cast(dtype.clone())),
                    Some(dtype),
                )
            }
            PlanStep::Capture {
                column, pattern, ..
            } => {
                let text = col(column.as_str()).cast(DataType::String);
                let fields = capture_fields(pattern)?
                    .into_iter()
                    .enumerate()
                    .map(|(i, f)| {
                        let p = lit(pattern.as_str());
                        text.clone().str().extract(p, i + 1).alias(f)
                    });
                when(text.clone().str().contains(lit(pattern.as_str()), true))
                    .then(as_struct(fields.collect()))
                    .otherwise(lit(NULL))
            }
            PlanStep::Rename { .. }
            | PlanStep::DeleteRows { .. }
            | PlanStep::AddRows { .. }
            | PlanStep::Expand { .. } => unreachable!("not a column step"),
        })
    }

    /// This edit on top of `plan`. The one definition: edits run through here
    /// when made and again when `:tail` swaps in a grown base table.
    pub(super) fn apply(&self, plan: LazyFrame) -> PolarsResult<LazyFrame> {
        if let Some(t) = self.column_target() {
            return Ok(plan.with_column(self.column_value(col(t))?.alias(t)));
        }
        Ok(match self {
            PlanStep::Rename { old, new } => plan.rename([old.as_str()], [new.as_str()], true),
            PlanStep::Expand {
                column,
                fields,
                after,
            } => {
                let kids = fields.iter().map(|(path, name)| {
                    path.iter()
                        .fold(col(column.as_str()), |e, f| e.struct_().field_by_name(f))
                        .alias(name.as_str())
                });
                let head = all().exclude_cols(after.clone()).as_expr();
                let tail = cols(after.clone()).as_expr();
                plan.select(
                    std::iter::once(head)
                        .chain(kids)
                        .chain([tail])
                        .collect::<Vec<_>>(),
                )
            }
            _ => plan, // DeleteRows, AddRows: see their docs
        })
    }
}

impl PlanStep {
    /// The column this step writes, if any (S-18 §2.2).
    pub(super) fn writes(&self) -> Option<&str> {
        match self {
            PlanStep::Rename { new, .. } => Some(new),
            PlanStep::Currency { column }
            | PlanStep::Json { column, .. }
            | PlanStep::SetCol { column, .. }
            | PlanStep::SetExpr { column, .. }
            | PlanStep::Replace { column, .. }
            | PlanStep::SetCells { column, .. } => Some(column),
            PlanStep::Derive { name, .. }
            | PlanStep::Capture { name, .. }
            | PlanStep::Lookup { name, .. } => Some(name),
            PlanStep::DeleteRows { .. } | PlanStep::AddRows { .. } | PlanStep::Expand { .. } => {
                None
            }
        }
    }

    /// The rows an edit writes; `None`: every row (or not an edit).
    fn edited_rids(&self) -> PolarsResult<Option<Vec<u64>>> {
        Ok(match self {
            PlanStep::SetCol { rids, .. } | PlanStep::SetExpr { rids, .. } => Some(rids.to_vec()),
            PlanStep::Replace { rids, .. } => rids.as_deref().map(<[u64]>::to_vec),
            PlanStep::SetCells { patch, .. } => Some(rid_values(patch)?),
            _ => None,
        })
    }

    pub fn is_edit(&self) -> bool {
        matches!(
            self,
            PlanStep::SetCol { .. }
                | PlanStep::SetExpr { .. }
                | PlanStep::Replace { .. }
                | PlanStep::SetCells { .. }
                | PlanStep::DeleteRows { .. }
                | PlanStep::AddRows { .. }
        )
    }

    /// A literal edit (`SetCol`, `SetCells`) as its `__vdrs_rid`/`__vdrs_v` patch.
    fn patch(&self) -> Option<PolarsResult<DataFrame>> {
        Some(match self {
            PlanStep::SetCells { patch, .. } => Ok((**patch).clone()),
            PlanStep::SetCol {
                value, dtype, rids, ..
            } => {
                let n = rids.len();
                let v = match value {
                    Some(v) if *dtype == DataType::Boolean => {
                        Ok(Series::new("__vdrs_v".into(), vec![v == "true"; n]))
                    }
                    Some(v) => {
                        Series::new("__vdrs_v".into(), vec![v.as_str(); n]).strict_cast(dtype)
                    }
                    None => Ok(Series::full_null("__vdrs_v".into(), n, dtype)),
                };
                v.and_then(|v| {
                    DataFrame::new_infer_height(vec![
                        Column::new(ROW_ID.into(), &rids[..]),
                        v.into_column(),
                    ])
                })
            }
            _ => return None,
        })
    }

    /// `self` then `next`, both literal edits of one column, as one
    /// `SetCells`: `next`'s rows win. Keeps the plan one join deep however
    /// many cells are typed in.
    fn merged(&self, next: &PlanStep) -> Option<PolarsResult<PlanStep>> {
        let column = self.writes()?;
        if next.writes() != Some(column) {
            return None;
        }
        let (old, new) = (self.patch()?, next.patch()?);
        Some((|| {
            let (old, new) = (old?, new?);
            let ids = lit(new.column(ROW_ID)?.as_materialized_series().clone()).implode(false);
            let kept = old
                .lazy()
                .filter(col(ROW_ID).is_in(ids, false).not())
                .collect()?;
            Ok(PlanStep::SetCells {
                column: column.to_string(),
                patch: Arc::new(kept.vstack(&new)?),
            })
        })())
    }

    /// A few words for `:undotree`.
    pub fn summary(&self) -> String {
        let rows = |n: usize| format!("{n} row{}", if n == 1 { "" } else { "s" });
        match self {
            PlanStep::Rename { old, new } => format!("rename {old} → {new}"),
            PlanStep::Currency { column } => format!("type {column} currency"),
            PlanStep::Json { column, .. } => format!("type {column} json"),
            PlanStep::Derive { name, expr, .. } => format!("derive {name} = {expr}"),
            PlanStep::SetCol {
                column,
                value,
                rids,
                ..
            } => match (rids.len(), value) {
                (1, Some(v)) => format!("edit {column} = {v}"),
                (n, Some(v)) => format!("setcol {column} = {v} on {}", rows(n)),
                (n, None) => format!("empty {column} on {}", rows(n)),
            },
            PlanStep::SetExpr {
                column, expr, rids, ..
            } => {
                format!("setexpr {column} = {expr} on {}", rows(rids.len()))
            }
            PlanStep::SetCells { column, patch } => {
                format!("paste into {column} on {}", rows(patch.height()))
            }
            PlanStep::Replace {
                column,
                pattern,
                replacement,
                rids,
                ..
            } => match rids {
                Some(r) => format!(
                    "s/{pattern}/{replacement}/ in {column} on {}",
                    rows(r.len())
                ),
                None => format!("s/{pattern}/{replacement}/ in {column}"),
            },
            PlanStep::DeleteRows { rids } => format!("delete {}", rows(rids.len())),
            PlanStep::AddRows { rows: r } => format!("add {}", rows(r.height())),
            PlanStep::Expand { column, .. } => format!("expand {column}"),
            PlanStep::Capture {
                column, pattern, ..
            } => format!("capture {column} {pattern}"),
            PlanStep::Lookup { name, key, .. } => format!("lookup {name} by {key}"),
        }
    }

    /// Columns this step reads besides the one it writes.
    fn reads(&self) -> Vec<String> {
        match self {
            PlanStep::SetExpr { expr, .. } => polars::sql::sql_expr(expr)
                .map(|e| {
                    e.meta()
                        .root_names()
                        .iter()
                        .map(|n| n.to_string())
                        .collect()
                })
                .unwrap_or_default(),
            PlanStep::Lookup { key, .. } => vec![key.clone()],
            _ => Vec::new(),
        }
    }
}

/// `;`'s struct fields: each group's name, else its index from 0 (VisiData
/// expands a match tuple as `col[0]`, `col[1]`…). A pattern without a group is
/// refused with VisiData's message.
pub fn capture_fields(pattern: &str) -> PolarsResult<Vec<String>> {
    let re = regex::Regex::new(pattern).map_err(|e| polars_err!(ComputeError: "bad regex: {e}"))?;
    let names: Vec<String> = re
        .capture_names()
        .skip(1)
        .enumerate()
        .map(|(i, n)| n.map_or_else(|| i.to_string(), str::to_string))
        .collect();
    polars_ensure!(!names.is_empty(), ComputeError: "specify a capture group using parentheses");
    Ok(names)
}

/// Where an edit of `column` goes: right after the last step that writes it,
/// so later derives that read `column` see the edit (S-18 §2.2).
pub(super) fn insert_point(steps: &[PlanStep], column: &str) -> usize {
    steps
        .iter()
        .rposition(|s| match s {
            // `(` makes its field columns: their edits go after it
            PlanStep::Expand { fields, .. } => fields.iter().any(|(_, n)| n == column),
            s => s.writes() == Some(column),
        })
        .map_or(0, |i| i + 1)
}

impl Sheet {
    /// Put an edit of `column` at its [`insert_point`] and rebuild the plan.
    /// Unchanged on error; the undo step is the caller's.
    /// An expression edit also goes after the last writer of each column it reads.
    pub(super) fn insert_edit(&mut self, column: &str, step: PlanStep) -> PolarsResult<()> {
        self.check_writable(column)
            .map_err(|e| PolarsError::ComputeError(e.into()))?;
        let rids = step.edited_rids()?;
        let at = std::iter::once(column.to_string())
            .chain(step.reads())
            .map(|c| insert_point(&self.state.steps, &c))
            .max()
            .unwrap_or(0);
        let saved = |s: &PlanStep| self.state.saved_edits.contains(s);
        let merge = match at.checked_sub(1).map(|i| &self.state.steps[i]) {
            Some(prev) if !saved(prev) => prev.merged(&step).transpose()?,
            _ => None,
        };
        let undo = match merge {
            Some(m) => Some(std::mem::replace(&mut self.state.steps[at - 1], m)),
            None => {
                self.state.steps.insert(at, step);
                None
            }
        };
        let checked = self.rebuild_plan().and_then(|()| {
            self.check_edit(&[column], rids.as_deref())
                .map_err(|e| PolarsError::ComputeError(e.into()))
        });
        if let Err(e) = checked {
            match undo {
                Some(prev) => self.state.steps[at - 1] = prev,
                None => drop(self.state.steps.remove(at)),
            }
            self.rebuild_plan()?;
            return Err(e);
        }
        self.invalidate(Reason::Rows);
        Ok(())
    }

    /// `:patch` (S-29 sessions): pasted cells into `column`, or (`None`)
    /// added rows, as `:mksession` saved their step's data.
    pub fn restore_patch(&mut self, column: Option<&str>, df: DataFrame) -> Result<(), String> {
        self.make_editable()?;
        let df = Arc::new(df);
        match column {
            Some(c) => self
                .insert_edit(
                    c,
                    PlanStep::SetCells {
                        column: c.to_string(),
                        patch: df,
                    },
                )
                .map_err(|e| e.to_string()),
            None => {
                let at = self
                    .state
                    .steps
                    .iter()
                    .take_while(|s| matches!(s, PlanStep::AddRows { .. }))
                    .count();
                self.state.steps.insert(at, PlanStep::AddRows { rows: df });
                if let Err(e) = self.rebuild_plan() {
                    self.state.steps.remove(at);
                    return Err(e.to_string());
                }
                self.invalidate(Reason::Rows);
                Ok(())
            }
        }
    }

    /// Bake `step` into the plan and remember it for `:export` and `:tail`.
    pub fn apply_step(&mut self, step: PlanStep) -> PolarsResult<()> {
        match self.rebuilds() {
            // rebuilt so the step joins the run of column steps before it
            true => {
                self.state.steps.push(step);
                if let Err(e) = self.rebuild_plan() {
                    self.state.steps.pop();
                    return Err(e);
                }
            }
            false => {
                self.plan = step.apply(self.plan.clone())?;
                self.state.steps.push(step);
            }
        }
        self.invalidate(Reason::Rows); // a filter on the column may match differently
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hundreds of column steps collect on a 2 MiB stack in a debug build,
    /// and the batched plan gives what applying them one by one gives.
    #[test]
    fn many_column_steps_batch_into_few_nodes() {
        let set = |c: &str, v: &str, r: u64| PlanStep::SetCol {
            column: c.into(),
            value: Some(v.into()),
            dtype: DataType::String,
            rids: Arc::from([r]),
        };
        let derive = |n: &str, e: &str| PlanStep::Derive {
            name: n.into(),
            expr: e.into(),
            replaces: false,
        };
        let mut steps = vec![
            derive("c", "n + 1"),
            set("name", "x", 0),
            derive("d", "c * 2"), // reads c: a new run
            set("name", "y", 0),
            PlanStep::Replace {
                column: "name".into(),
                pattern: "y".into(),
                replacement: "z".into(),
                all: true,
                rids: None,
            },
            PlanStep::Rename {
                old: "d".into(),
                new: "e".into(),
            },
            set("name", "w", 1),
        ];
        for i in 0..200 {
            steps.push(derive(&format!("k{i}"), "n"));
            steps.push(set("name", &format!("v{i}"), 2));
        }
        let base = || {
            df!("name" => ["a", "b", "c"], "n" => [1i64, 2, 3], ROW_ID => [0u64, 1, 2])
                .unwrap()
                .lazy()
        };
        let one_by_one = steps.iter().try_fold(base(), |p, s| s.apply(p)).unwrap();
        let batched = apply_steps(base(), &steps).unwrap();
        let got = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || batched.collect().unwrap())
            .unwrap()
            .join()
            .unwrap();
        let want = std::thread::Builder::new()
            .stack_size(64 << 20)
            .spawn(move || one_by_one.collect().unwrap())
            .unwrap()
            .join()
            .unwrap();
        assert_eq!(got, want);
        let names: Vec<_> = got.column("name").unwrap().str().unwrap().iter().collect();
        assert_eq!(names, [Some("z"), Some("w"), Some("v199")]);
        assert_eq!(got.column("e").unwrap().i64().unwrap().get(0), Some(4));
    }

    #[test]
    fn insert_point_follows_the_last_writer() {
        let set = |c: &str| PlanStep::SetCol {
            column: c.into(),
            value: None,
            dtype: DataType::String,
            rids: Arc::from([0u64]),
        };
        let derive = |n: &str| PlanStep::Derive {
            name: n.into(),
            expr: "qty * 2".into(),
            replaces: false,
        };
        let rename = PlanStep::Rename {
            old: "a".into(),
            new: "qty".into(),
        };
        assert_eq!(insert_point(&[], "qty"), 0);
        // no writer: front, so `total = qty * 2` sees the edit
        assert_eq!(insert_point(&[derive("total")], "qty"), 0);
        assert_eq!(insert_point(&[rename.clone(), derive("total")], "qty"), 1);
        assert_eq!(insert_point(&[set("qty"), derive("total")], "qty"), 1);
        // a derived column itself: after its Derive (pins the cell)
        assert_eq!(insert_point(&[derive("total"), set("x")], "total"), 1);
        let cur = PlanStep::Currency {
            column: "qty".into(),
        };
        assert_eq!(insert_point(&[cur, derive("total"), set("qty")], "qty"), 3);
    }
}
