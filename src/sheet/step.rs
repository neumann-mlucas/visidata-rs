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
}

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
            PlanStep::DeleteRows { .. } => plan,
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
            PlanStep::DeleteRows { .. } => None,
        }
    }

    pub fn is_edit(&self) -> bool {
        matches!(
            self,
            PlanStep::SetCol { .. }
                | PlanStep::SetExpr { .. }
                | PlanStep::SetCells { .. }
                | PlanStep::DeleteRows { .. }
        )
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

/// Everything `U` restores.
#[derive(Clone)]
pub struct Snapshot {
    pub plan: LazyFrame,
    pub view: View,
    pub steps: Vec<PlanStep>,
}

/// Undo depth per sheet.
const UNDO_MAX: usize = 100;

impl Sheet {
    /// Remember the current plan/view for `U`; a new change forgets redo history.
    pub fn checkpoint(&mut self) {
        self.push_undo(self.snapshot());
    }

    /// `snap` (taken before a change) becomes the newest undo step.
    pub fn push_undo(&mut self, snap: Snapshot) {
        let undo = &mut self.state.undo;
        undo.push(snap);
        if undo.len() > UNDO_MAX {
            undo.remove(0);
        }
        self.state.redo.clear();
    }

    pub fn snapshot(&self) -> Snapshot {
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
        self.invalidate(Reason::Rows); // plan may differ: recount
        if matches!(self.origin, Origin::Tail { .. }) {
            // Snapshots predate rows that arrived since; keep the rows, undo the edit.
            if let Err(e) = self.rebuild_plan() {
                debug_assert!(false, "steps applied before: {e}");
            }
        }
        true
    }

    /// Put an edit of `column` at its [`insert_point`] and rebuild the plan.
    /// Unchanged on error; the undo step is the caller's.
    /// An expression edit also goes after the last writer of each column it reads.
    pub(super) fn insert_edit(&mut self, column: &str, step: PlanStep) -> PolarsResult<()> {
        let at = std::iter::once(column.to_string())
            .chain(step.reads())
            .map(|c| insert_point(&self.state.steps, &c))
            .max()
            .unwrap_or(0);
        self.state.steps.insert(at, step);
        if let Err(e) = self.rebuild_plan() {
            self.state.steps.remove(at);
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
