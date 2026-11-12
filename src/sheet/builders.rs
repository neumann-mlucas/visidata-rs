//! Column builders (S-31): `za` empty column, `i` / `gi` running numbers,
//! `f` fill down, `;` regex capture. Each is a `Derive`, a `SetCells` or a
//! `Capture` step, so undo, `:save`, `:export` and sessions take them as is.

use super::*;

impl Sheet {
    /// `za` / `:addcol`: an empty text column `name`.
    pub fn add_empty_col(&mut self, name: &str) -> Result<(), String> {
        self.editable()?;
        if name.trim().is_empty() {
            return Err("addcol: a column needs a name".into());
        }
        if self.has_column(name) {
            return Err(format!("column already exists: {name}"));
        }
        self.derive(name, "CAST(NULL AS VARCHAR)")
    }

    /// `i` / `:addcol-incr`: a new int column numbering the view's rows
    /// 1…n in view order; rows the filter hides stay null. Its name.
    pub fn add_incr_col(&mut self) -> Result<String, String> {
        self.editable()?;
        let name = (1..)
            .map(|i| match i {
                1 => "incr".to_string(),
                i => format!("incr{i}"),
            })
            .find(|n| !self.has_column(n))
            .unwrap_or_default();
        let rids = self.view_order_rids()?;
        let before = self.snapshot();
        let made = self
            .apply_step(PlanStep::Derive {
                name: name.clone(),
                expr: "CAST(NULL AS BIGINT)".into(),
                replaces: false,
            })
            .map_err(|e| e.to_string())
            .and_then(|()| self.number_rows(&name, rids));
        match made {
            Ok(()) => {
                self.push_undo(before);
                Ok(name)
            }
            Err(e) => {
                self.restore(before);
                Err(e)
            }
        }
    }

    /// `gi` / `:setcol-incr`: the selected rows of `column` get 1…k in view
    /// order. The count.
    pub fn set_incr(&mut self, column: &str) -> Result<usize, String> {
        self.editable()?;
        let rids = self.selected_in_view_order().map_err(|e| e.to_string())?;
        if rids.is_empty() {
            return Err("no rows selected".into());
        }
        let n = rids.len();
        let before = self.snapshot();
        self.number_rows(column, rids)?;
        self.push_undo(before);
        Ok(n)
    }

    /// `f` / `:setcol-fill`: null cells of `column` take the last non-null
    /// value above them, in view order; only selected rows change, or every
    /// row with no selection. The count filled (no step when 0).
    pub fn fill_down(&mut self, column: &str) -> Result<usize, String> {
        self.editable()?;
        let df = self
            .sorted_plan(true)
            .select([col(ROW_ID), col(column)])
            .collect()
            .map_err(|e| e.to_string())?;
        let rids = rid_values(&df).map_err(|e| e.to_string())?;
        let vals = df
            .column(column)
            .map_err(|e| e.to_string())?
            .as_materialized_series()
            .clone();
        let sel = &self.view.selected;
        let (mut ids, mut from) = (Vec::new(), Vec::new());
        let mut last = None;
        for (i, rid) in rids.iter().enumerate() {
            if vals.get(i).is_ok_and(|v| !v.is_null()) {
                last = Some(i as IdxSize);
            } else if let Some(l) = last
                && (sel.is_empty() || sel.contains(rid))
            {
                ids.push(*rid);
                from.push(l);
            }
        }
        let n = ids.len();
        if n == 0 {
            return Ok(0);
        }
        let idx = IdxCa::from_vec("i".into(), from);
        let filled = vals.take(&idx).map_err(|e| e.to_string())?;
        let before = self.snapshot();
        self.set_cells(column, ids, filled)?;
        self.push_undo(before);
        Ok(n)
    }

    /// `;` / `:capture`: a struct column `<column>_re` of `pattern`'s
    /// groups (`(` expands it). Its name.
    pub fn capture(&mut self, column: &str, pattern: &str) -> Result<String, String> {
        self.editable()?;
        capture_fields(pattern).map_err(|e| e.to_string())?;
        let name = format!("{column}_re");
        if self.has_column(&name) {
            return Err(format!("column already exists: {name}"));
        }
        let step = PlanStep::Capture {
            column: column.into(),
            pattern: pattern.into(),
            name: name.clone(),
        };
        let mut plan = step.apply(self.plan.clone()).map_err(|e| e.to_string())?;
        plan.collect_schema().map_err(|e| e.to_string())?;
        self.checkpoint();
        self.apply_step(step).map_err(|e| e.to_string())?;
        Ok(name)
    }

    /// A copy of `parent`'s rows (`"`, `g"`, `g'`, dedupe, freq dive) keeps
    /// its column attributes: types, aggregators, `:color` and `:scale`
    /// rules, for the columns it still has (S-31 C).
    pub(super) fn inherit_attrs(mut self, parent: &Sheet) -> Self {
        let have: std::collections::HashSet<String> = self
            .plan
            .collect_schema()
            .map(|s| s.iter_names().map(|n| n.to_string()).collect())
            .unwrap_or_default();
        let has = |c: &String| have.contains(c);
        let reads_kept = |pred: &str| {
            polars::sql::sql_expr(pred)
                .is_ok_and(|e| e.meta().root_names().iter().all(|n| has(&n.to_string())))
        };
        let p = &parent.view;
        let v = &mut self.view;
        v.type_casts = p
            .type_casts
            .iter()
            .filter(|(c, _)| has(c))
            .cloned()
            .collect();
        v.aggregators = p
            .aggregators
            .iter()
            .filter(|(c, _)| has(c))
            .cloned()
            .collect();
        v.scales = p
            .scales
            .iter()
            .filter(|r| has(&r.column))
            .cloned()
            .collect();
        v.formats = p
            .formats
            .iter()
            .filter(|f| f.column.as_ref().is_none_or(has) && reads_kept(&f.pred))
            .cloned()
            .collect();
        self
    }

    /// Row ids of the filtered, sorted view, in order.
    fn view_order_rids(&self) -> Result<Vec<u64>, String> {
        let df = self.sorted_plan(true).select([col(ROW_ID)]).collect();
        rid_values(&df.map_err(|e| e.to_string())?).map_err(|e| e.to_string())
    }

    /// `rids[i]` of `column` := i + 1, as the column's type.
    fn number_rows(&mut self, column: &str, rids: Vec<u64>) -> Result<(), String> {
        let texts: Vec<Option<String>> = (1..=rids.len()).map(|i| Some(i.to_string())).collect();
        let vals = self.edit_values(column, &texts)?;
        self.set_cells(column, rids, vals)
    }

    /// One `SetCells` step: `vals[i]` into row `rids[i]` of `column`.
    fn set_cells(&mut self, column: &str, rids: Vec<u64>, vals: Series) -> Result<(), String> {
        let patch = DataFrame::new_infer_height(vec![
            Column::new(ROW_ID.into(), rids),
            vals.with_name("__vdrs_v".into()).into_column(),
        ])
        .map_err(|e| e.to_string())?;
        let step = PlanStep::SetCells {
            column: column.to_string(),
            patch: Arc::new(patch),
        };
        self.insert_edit(column, step).map_err(|e| e.to_string())
    }
}
