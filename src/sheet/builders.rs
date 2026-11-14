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
        let rids = self.view_order_rids()?;
        self.add_incr_col_with(rids)
    }

    /// [`Self::add_incr_col`] with the view's row ids already read (S-33).
    pub fn add_incr_col_with(&mut self, rids: Vec<u64>) -> Result<String, String> {
        self.editable()?;
        let before = self.snapshot();
        let name = self.next_colname();
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
        let rids = self.selected_in_view_order().map_err(|e| e.to_string())?;
        self.set_incr_with(column, rids)
    }

    /// [`Self::set_incr`] with the selected rows in view order (S-33).
    pub fn set_incr_with(&mut self, column: &str, rids: Vec<u64>) -> Result<usize, String> {
        self.editable()?;
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
        let (ids, filled) = self.fill_scan(column)?;
        self.fill_apply(column, ids, filled)
    }

    /// [`Self::fill_down`]'s read (S-33): the rows to fill and their values.
    pub fn fill_scan(&self, column: &str) -> Result<(Vec<u64>, Series), String> {
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
        let idx = IdxCa::from_vec("i".into(), from);
        let filled = vals.take(&idx).map_err(|e| e.to_string())?;
        Ok((ids, filled))
    }

    /// [`Self::fill_down`]'s write: `filled` into rows `ids`. The count.
    pub fn fill_apply(
        &mut self,
        column: &str,
        ids: Vec<u64>,
        filled: Series,
    ) -> Result<usize, String> {
        self.editable()?;
        let n = ids.len();
        if n == 0 {
            return Ok(0);
        }
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
    /// rules, column order (S-34), widths (S-38 B), for the columns it still
    /// has (S-31 C).
    pub(super) fn inherit_attrs(mut self, parent: &Sheet) -> Self {
        let shown = parent.window().map(|df| df.get_column_names_owned());
        let measured = shown.iter().flatten().zip(&parent.state.widths);
        self.state.width_floor = measured
            .map(|(c, w)| (c.to_string(), *w))
            .chain(parent.state.width_floor.iter().cloned())
            .collect();
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
        v.precision = p
            .precision
            .iter()
            .filter(|(c, _)| has(c))
            .cloned()
            .collect();
        v.date_fmts = p
            .date_fmts
            .iter()
            .filter(|(c, _)| has(c))
            .cloned()
            .collect();
        v.order = p.order.iter().filter(|c| has(c)).cloned().collect();
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

    /// `fmtstr` (columns sheet, `:fmtstr`; S-35 C2): `%.Nf` / `{:.Nf}` sets a
    /// float column's decimals, a strftime format a date or datetime
    /// column's text; empty clears either. The toast.
    pub fn set_fmtstr(&mut self, column: &str, fmt: &str) -> Result<String, String> {
        let dtype = self
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?
            .get(column)
            .cloned()
            .ok_or_else(|| format!("no column {column}"))?;
        let fmt = fmt.trim();
        let dated = matches!(dtype, DataType::Date | DataType::Datetime(..));
        if fmt.is_empty() || dated {
            self.view.date_fmts.retain(|(c, _)| c != column);
            self.state.widths_key = None;
        }
        if fmt.is_empty() {
            if dtype.is_float() {
                self.set_precision(column, None, 0, 2)?;
            }
            return Ok(format!("{column}: default format"));
        }
        if dtype.is_float() {
            let d = crate::plugins::float_fmt_digits(fmt)
                .ok_or_else(|| format!("fmtstr: {fmt:?} isn't %.Nf or {{:.Nf}}"))?;
            self.set_precision(column, Some(d), 0, 2)?;
            return Ok(format!("{column}: {d} decimals"));
        }
        if !dated {
            return Err(format!(
                "fmtstr: {column} is {} (float, date or datetime columns only)",
                crate::widget::dtype_badge(&dtype)
            ));
        }
        // a format chrono can't write fails on any value: try one
        let probe = Series::new("".into(), [0i64])
            .cast(&dtype)
            .map_err(|e| e.to_string())?;
        let ok = match &dtype {
            DataType::Date => probe.date().map(|d| d.strftime(fmt).is_ok()),
            _ => probe.datetime().map(|d| d.strftime(fmt).is_ok()),
        };
        if !ok.unwrap_or(false) {
            return Err(format!("fmtstr: {fmt:?} isn't a strftime format"));
        }
        self.view
            .date_fmts
            .push((column.to_string(), fmt.to_string()));
        Ok(format!("{column}: {fmt}"))
    }

    /// `Alt++` / `Alt+-` (`delta` ±1) and `:precision` (`set`): the decimals
    /// float `column` shows, from `start` when it has none (S-32). `set`
    /// `None` with `delta` 0 clears it. The decimals, or `None` when cleared.
    pub fn set_precision(
        &mut self,
        column: &str,
        set: Option<u8>,
        delta: i8,
        start: u8,
    ) -> Result<Option<u8>, String> {
        let dtype = self
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?
            .get(column)
            .cloned()
            .ok_or_else(|| format!("no column {column}"))?;
        if !matches!(dtype, DataType::Float32 | DataType::Float64) {
            return Err(format!(
                "precision: {column} is {} (float columns only)",
                crate::widget::dtype_badge(&dtype)
            ));
        }
        let v = &mut self.view;
        let old = v.precision.iter().position(|(c, _)| c == column);
        let cur = old.map_or(start, |i| v.precision[i].1);
        let new = match (set, delta) {
            (Some(d), _) => Some(d.min(15)),
            (None, 0) => None,
            (None, d) => Some(cur.saturating_add_signed(d).min(15)),
        };
        if let Some(i) = old {
            v.precision.remove(i);
        }
        if let Some(d) = new {
            v.precision.push((column.to_string(), d));
        }
        self.state.widths_key = None;
        Ok(new)
    }

    /// Row ids of the filtered, sorted view, in order.
    pub fn view_order_rids(&self) -> Result<Vec<u64>, String> {
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
