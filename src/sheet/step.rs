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
}

/// First id of an added row; source rows' ids are positions, far below.
pub const ADDED_RID: u64 = 1 << 62;
/// Position key column of added rows (source rows: their id).
pub const POS: &str = "__vdrs_pos";

/// `when(row ∈ rids).then(v).otherwise(column)`: the edit steps' row mask,
/// on the [`ROW_ID`] column source sheets carry.
fn on_rows(plan: LazyFrame, column: &str, rids: &[u64], v: Expr) -> LazyFrame {
    plan.with_column(
        when(col(ROW_ID).is_in(lit(Series::new("ids".into(), rids)).implode(false), false))
            .then(v)
            .otherwise(col(column))
            .alias(column),
    )
}

impl PlanStep {
    /// This edit on top of `plan`. The one definition: edits run through here
    /// when made and again when `:tail` swaps in a grown base table.
    pub(super) fn apply(&self, plan: LazyFrame) -> PolarsResult<LazyFrame> {
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
            } => on_rows(
                plan,
                column,
                rids,
                match value {
                    Some(v) if *dtype == DataType::Boolean => lit(v == "true"),
                    Some(v) => lit(v.to_string()).strict_cast(dtype.clone()),
                    None => lit(Null {}).cast(dtype.clone()),
                },
            ),
            PlanStep::SetExpr {
                column,
                expr,
                dtype,
                rids,
            } => on_rows(
                plan,
                column,
                rids,
                polars::sql::sql_expr(expr)?.cast(dtype.clone()),
            ),
            PlanStep::SetCells { column, patch } => plan
                .join(
                    (**patch)
                        .clone()
                        .lazy()
                        .rename([ROW_ID], ["__vdrs_set"], true)
                        .with_column(lit(true).alias("__vdrs_hit")),
                    [col(ROW_ID)],
                    [col("__vdrs_set")],
                    JoinArgs {
                        maintain_order: MaintainOrderJoin::Left,
                        ..JoinArgs::new(JoinType::Left)
                    },
                )
                .with_column(
                    when(col("__vdrs_hit").is_not_null())
                        .then(col("__vdrs_v"))
                        .otherwise(col(column.as_str()))
                        .alias(column.as_str()),
                )
                .drop(cols(["__vdrs_v", "__vdrs_hit"])),
            PlanStep::DeleteRows { .. } | PlanStep::AddRows { .. } => plan,
        })
    }
}

impl PlanStep {
    /// The column this step writes, if any (S-18 §2.2).
    fn writes(&self) -> Option<&str> {
        match self {
            PlanStep::Rename { new, .. } => Some(new),
            PlanStep::Currency { column }
            | PlanStep::SetCol { column, .. }
            | PlanStep::SetExpr { column, .. }
            | PlanStep::SetCells { column, .. } => Some(column),
            PlanStep::Derive { name, .. } => Some(name),
            PlanStep::DeleteRows { .. } | PlanStep::AddRows { .. } => None,
        }
    }

    pub fn is_edit(&self) -> bool {
        matches!(
            self,
            PlanStep::SetCol { .. }
                | PlanStep::SetExpr { .. }
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
            PlanStep::DeleteRows { rids } => format!("delete {}", rows(rids.len())),
            PlanStep::AddRows { rows: r } => format!("add {}", rows(r.height())),
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
            _ => Vec::new(),
        }
    }
}

/// Where an edit of `column` goes: right after the last step that writes it,
/// so later derives that read `column` see the edit (S-18 §2.2).
pub(super) fn insert_point(steps: &[PlanStep], column: &str) -> usize {
    steps
        .iter()
        .rposition(|s| s.writes() == Some(column))
        .map_or(0, |i| i + 1)
}

impl Sheet {
    /// Put an edit of `column` at its [`insert_point`] and rebuild the plan.
    /// Unchanged on error; the undo step is the caller's.
    /// An expression edit also goes after the last writer of each column it reads.
    pub(super) fn insert_edit(&mut self, column: &str, step: PlanStep) -> PolarsResult<()> {
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
        if let Err(e) = self.rebuild_plan() {
            match undo {
                Some(prev) => self.state.steps[at - 1] = prev,
                None => drop(self.state.steps.remove(at)),
            }
            return Err(e);
        }
        self.invalidate(Reason::Rows);
        Ok(())
    }

    /// Bake `step` into the plan and remember it for `:export` and `:tail`.
    pub fn apply_step(&mut self, step: PlanStep) -> PolarsResult<()> {
        self.plan = step.apply(self.plan.clone())?;
        self.state.steps.push(step);
        self.invalidate(Reason::Rows); // a filter on the column may match differently
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
