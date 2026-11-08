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

impl Sheet {
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

    pub(super) fn snapshot(&self) -> Snapshot {
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
}
