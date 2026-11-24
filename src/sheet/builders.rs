//! Column builders (S-31): `za` empty column, `i` / `gi` running numbers,
//! `f` fill down, `;` regex capture. Each is a `Derive`, a `SetCells` or a
//! `Capture` step, so undo, `:save`, `:export` and sessions take them as is.

use super::*;

impl Sheet {
    /// `:addcol-aggregate` (VisiData addcol-aggregate, S-46 B4): a
    /// `<column>_<agg>` column per name in `aggs` (as `+` takes them, plus
    /// `rank`), each `agg` within the key columns' groups; after `column`.
    /// Their names.
    pub fn addcol_aggregate(&mut self, column: &str, aggs: &str) -> Result<Vec<String>, String> {
        let mut names = Vec::new();
        for agg in aggs.split_whitespace() {
            match agg {
                "rank" => names.push("rank".to_string()),
                a => names.extend(self.aggregator_names(column, a)?),
            }
        }
        if names.is_empty() {
            return Err("addcol-aggregate: no aggregator given".into());
        }
        // ponytail: an error row (S-44 A) groups with the null keys; VisiData
        // gives each its own group. Add `ERR_COL` to `over` if that bites
        let keys = self.view.keys.clone();
        if names.iter().any(|n| n == "rank") && keys.is_empty() {
            return Err("ranking requires one or more key columns".into());
        }
        // VisiData ranks within a group descending when the column sorts so
        let desc =
            (self.view.sort.iter()).any(|k| k.column == column && k.order == SortOrder::Desc);
        let new: Vec<String> = (names.iter())
            .map(|a| crate::sheet::columns::agg_name(column, a))
            .collect();
        let by = [(column.to_string(), desc)];
        let casts = self.casts_of(&by);
        self.undoably(|s| {
            for (agg, name) in names.iter().zip(&new) {
                s.group_agg_step(name, agg, &by, &keys, &casts)?;
            }
            s.place_cols(&new, Some(column))
        })?;
        Ok(new)
    }

    /// `:addcol-rank-sheet` (VisiData addcol-rank-sheet, S-46 B4):
    /// `<sheet>_sheetrank`, the rank of each row's key values: sorted key
    /// columns first, in their sort direction, then the others ascending.
    /// After column `after`. Its name.
    pub fn addcol_rank_sheet(&mut self, after: Option<&str>) -> Result<String, String> {
        let keys = &self.view.keys;
        if keys.is_empty() {
            return Err("ranking requires one or more key columns".into());
        }
        let sorted = (self.view.sort.iter()).filter(|k| keys.contains(&k.column));
        let mut by: Vec<(String, bool)> = sorted
            .map(|k| (k.column.clone(), k.order == SortOrder::Desc))
            .collect();
        for k in keys {
            if !by.iter().any(|(c, _)| c == k) {
                by.push((k.clone(), false));
            }
        }
        let name = format!("{}_sheetrank", self.table_name());
        let casts = self.casts_of(&by);
        self.undoably(|s| {
            s.group_agg_step(&name, "rank", &by, &[], &casts)?;
            s.place_col(&name, after)
        })?;
        Ok(name)
    }

    /// `:addcol-agg` (S-46 B4): the [`PlanStep::GroupAgg`] the two above make,
    /// spelled out: `casts` as given, not the view's. After column `after`.
    pub fn group_agg(
        &mut self,
        name: &str,
        agg: &str,
        (by, over): (&[(String, bool)], &[String]),
        casts: &[(String, DataType)],
        after: Option<&str>,
    ) -> Result<(), String> {
        self.undoably(|s| {
            s.group_agg_step(name, agg, by, over, casts)?;
            s.place_col(name, after)
        })
    }

    /// The view's casts of the `by` columns.
    fn casts_of(&self, by: &[(String, bool)]) -> Vec<(String, DataType)> {
        (self.view.type_casts.iter())
            .filter(|(c, _)| by.iter().any(|(b, _)| b == c))
            .cloned()
            .collect()
    }

    /// `f` as one undo step; on an error, the sheet as it was.
    fn undoably(&mut self, f: impl FnOnce(&mut Self) -> Result<(), String>) -> Result<(), String> {
        self.make_editable()?;
        let before = self.snapshot();
        match f(self) {
            Ok(()) => {
                self.push_undo(before);
                Ok(())
            }
            Err(e) => {
                self.restore(before);
                Err(e)
            }
        }
    }

    fn group_agg_step(
        &mut self,
        name: &str,
        agg: &str,
        by: &[(String, bool)],
        over: &[String],
        casts: &[(String, DataType)],
    ) -> Result<(), String> {
        if self.has_column(name) {
            return Err(format!("column already exists: {name}"));
        }
        // ponytail: a date or len cast reads differently in each export; cast
        // to int, float or str, or :derive the value first
        let plain = |dt: &DataType| {
            crate::sheet::columns::AGG_CASTS
                .iter()
                .any(|(_, t)| t == dt)
        };
        if let Some((c, _)) = casts.iter().find(|(_, dt)| !plain(dt)) {
            return Err(format!(
                "{agg}: {c} has a date or len type; only int, float, str"
            ));
        }
        let step = PlanStep::GroupAgg {
            agg: agg.into(),
            by: by.to_vec(),
            over: over.to_vec(),
            casts: casts.to_vec(),
            name: name.into(),
        };
        let e = |e: PolarsError| format!("{agg}: {e}");
        self.apply_step(step).map_err(e)?;
        // a type the aggregator can't take fails here, not on the next collect
        self.plan.clone().collect_schema().map(drop).map_err(e)
    }

    /// `:join-cols` (VisiData join-cols, S-46 B3): a `Derive` of `columns`'
    /// values as text with `values` between them (null as empty), named
    /// after them joined by `names`, placed after column `after`. Its name.
    pub fn join_cols(
        &mut self,
        columns: &[&str],
        (names, values): (&str, &str),
        after: Option<&str>,
    ) -> Result<String, String> {
        let name = columns.join(names);
        let sep = format!(" || {} || ", crate::duckdb::lit(values));
        let expr = (columns.iter())
            .map(|c| format!("COALESCE(CAST({} AS VARCHAR), '')", crate::duckdb::ident(c)))
            .collect::<Vec<_>>()
            .join(&sep);
        self.derive(&name, &expr)?;
        self.place_col(&name, after)?;
        Ok(name)
    }

    /// `za` / `:addcol`: an empty text column `name`.
    pub fn add_empty_col(&mut self, name: &str) -> Result<(), String> {
        self.make_editable()?;
        if name.trim().is_empty() {
            return Err("addcol: a column needs a name".into());
        }
        if self.has_column(name) {
            return Err(format!("column already exists: {name}"));
        }
        self.derive(name, "CAST(NULL AS VARCHAR)")
    }

    /// `'` (VisiData `freeze-col`, S-46 A): `<column>_frozen`, a copy of
    /// `column` with its type, aggregators and format. Its name.
    // ponytail: a `Derive` copy, so an edit of `column` placed before it (no
    // earlier writer) shows in the copy; VisiData's keeps the old values.
    // Freeze the collected values (a `SetCells` patch) if that bites someone
    pub fn freeze_col(&mut self, column: &str) -> Result<String, String> {
        let name = format!("{column}_frozen");
        if self.has_column(&name) {
            return Err(format!("column already exists: {name}"));
        }
        self.derive(&name, &format!("\"{}\"", column.replace('"', "\"\"")))?;
        fn copy<T: Clone>(l: &mut Vec<(String, T)>, from: &str, to: &str) {
            let found: Vec<T> = l
                .iter()
                .filter(|(c, _)| c == from)
                .map(|(_, x)| x.clone())
                .collect();
            l.extend(found.into_iter().map(|x| (to.to_string(), x)));
        }
        // as wide as the original, as VisiData's copy keeps its width
        let at = self.window().and_then(|w| w.get_column_index(column));
        if let Some(&w) = at.and_then(|i| self.state.widths.get(i)) {
            self.state.width_floor.insert(name.clone(), w);
        }
        let v = &mut self.view;
        copy(&mut v.type_casts, column, &name);
        copy(&mut v.aggregators, column, &name);
        copy(&mut v.fmtstrs, column, &name);
        Ok(name)
    }

    /// `i` / `:addcol-incr`: a new int column numbering the view's rows
    /// base, base + 1, … in view order; rows the filter hides stay null. Its name.
    pub fn add_incr_col(&mut self, base: u64) -> Result<String, String> {
        let rids = self.view_order_rids()?;
        let nums = counting(base, rids.len());
        self.add_incr_col_with(rids, nums)
    }

    /// `:addcol-incr-key` (S-41 E): [`Self::add_incr_col`], restarting at
    /// `base` wherever the key columns' values change from the row above.
    pub fn add_incr_key_col(&mut self, base: u64) -> Result<String, String> {
        let (rids, nums) = self.key_run_numbers(base)?;
        self.add_incr_col_with(rids, nums)
    }

    /// The view's row ids in order, numbered from `base` within each run of
    /// equal key values (one run without keys).
    pub fn key_run_numbers(&self, base: u64) -> Result<(Vec<u64>, Vec<i64>), String> {
        let e = |e: PolarsError| e.to_string();
        let keys = &self.view.keys;
        let mut sel = vec![col(ROW_ID)];
        sel.extend(keys.iter().map(|k| col(k.as_str()).cast(DataType::String)));
        let df = self.sorted_plan(true).select(sel).collect().map_err(e)?;
        let rids = rid_values(&df).map_err(e)?;
        let text = |i: usize| -> Vec<Option<String>> {
            (df.columns()[1..].iter())
                .map(|c| c.str().ok().and_then(|s| s.get(i)).map(String::from))
                .collect()
        };
        let mut nums = Vec::with_capacity(rids.len());
        let mut last = None;
        let mut n = base as i64;
        for i in 0..rids.len() {
            let k = text(i);
            n = if last.as_ref() == Some(&k) {
                n + 1
            } else {
                base as i64
            };
            nums.push(n);
            last = Some(k);
        }
        Ok((rids, nums))
    }

    /// [`Self::add_incr_col`] with the view's row ids and their numbers
    /// already worked out (S-33).
    pub fn add_incr_col_with(&mut self, rids: Vec<u64>, nums: Vec<i64>) -> Result<String, String> {
        self.make_editable()?;
        let before = self.snapshot();
        let name = self.next_colname();
        let made = self
            .apply_step(PlanStep::Derive {
                name: name.clone(),
                expr: "CAST(NULL AS BIGINT)".into(),
                replaces: false,
            })
            .map_err(|e| e.to_string())
            .and_then(|()| self.number_rows(&name, rids, &nums));
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

    /// `gi` / `:setcol-incr`: the selected rows of `column` get base…
    /// in view order. The count.
    pub fn set_incr(&mut self, column: &str, base: u64) -> Result<usize, String> {
        let rids = self.selected_in_view_order().map_err(|e| e.to_string())?;
        self.set_incr_with(column, rids, base)
    }

    /// [`Self::set_incr`] with the selected rows in view order (S-33).
    pub fn set_incr_with(
        &mut self,
        column: &str,
        rids: Vec<u64>,
        base: u64,
    ) -> Result<usize, String> {
        self.make_editable()?;
        if rids.is_empty() {
            return Err("no rows selected".into());
        }
        let n = rids.len();
        let before = self.snapshot();
        let nums = counting(base, n);
        self.number_rows(column, rids, &nums)?;
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
        self.make_editable()?;
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
        self.make_editable()?;
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
        let mut floor = parent.state.width_floor.clone();
        for (c, w) in measured {
            let f = floor.entry(c.to_string()).or_default();
            *f = (*f).max(*w);
        }
        self.state.width_floor = floor;
        let have: std::collections::HashSet<String> = self
            .plan
            .collect_schema()
            .map(|s| s.iter_names().map(|n| n.to_string()).collect())
            .unwrap_or_default();
        let has = |c: &String| have.contains(c);
        let reads_kept = |pred: &str| {
            crate::sqlexpr::parse(pred)
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
        v.fmtstrs = p.fmtstrs.iter().filter(|(c, _)| has(c)).cloned().collect();
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
            .view_dtype(column)
            .ok_or_else(|| format!("no column {column}"))?;
        let fmt = fmt.trim();
        let dated = matches!(dtype, DataType::Date | DataType::Datetime(..));
        let number = dtype.is_primitive_numeric();
        if fmt.is_empty() || dated || number {
            self.view.fmtstrs.retain(|(c, _)| c != column);
            self.state.widths_key = None;
        }
        if fmt.is_empty() {
            if dtype.is_float() {
                self.set_precision(column, None, 0, 2)?;
            }
            return Ok(format!("{column}: default format"));
        }
        if number {
            if let Some(d) = crate::plugins::float_fmt_digits(fmt).filter(|_| dtype.is_float()) {
                self.set_precision(column, Some(d), 0, 2)?;
                return Ok(format!("{column}: {d} decimals"));
            }
            let f = crate::numfmt::NumFmt::parse(fmt)?;
            if dtype.is_float() && !f.fits_float() {
                return Err(format!(
                    "fmtstr: {fmt:?} is for integers ({column} is float)"
                ));
            }
            self.view.precision.retain(|(c, _)| c != column);
            self.view
                .fmtstrs
                .push((column.to_string(), fmt.to_string()));
            return Ok(format!("{column}: {fmt}"));
        }
        if !dated {
            return Err(format!(
                "fmtstr: {column} is {} (number, date or datetime columns only)",
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
            .fmtstrs
            .push((column.to_string(), fmt.to_string()));
        Ok(format!("{column}: {fmt}"))
    }

    /// A number format the user set on `column` (K3): its rich `fmtstr`, else
    /// its precision as `%.Nf`. `disp_float_fmt` isn't one.
    pub fn num_fmt(&self, column: &str) -> Option<crate::numfmt::NumFmt> {
        crate::sheet::num_fmt_of(&self.view, column).filter(|_| {
            self.view_dtype(column)
                .is_some_and(|d| d.is_primitive_numeric())
        })
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
            .view_dtype(column)
            .ok_or_else(|| format!("no column {column}"))?;
        // a rich number format: change its decimals (K2)
        let rich = (self.view.fmtstrs.iter().rposition(|(c, _)| c == column))
            .filter(|_| dtype.is_primitive_numeric())
            .and_then(|i| {
                Some((
                    i,
                    crate::numfmt::NumFmt::parse(&self.view.fmtstrs[i].1).ok()?,
                ))
            });
        if let Some((i, f)) = rich {
            let cur = f
                .digits()
                .ok_or_else(|| format!("precision: {} has no decimals", f.text()))?;
            let new = match (set, delta) {
                (Some(d), _) => d,
                (None, 0) => {
                    self.view.fmtstrs.remove(i);
                    self.state.widths_key = None;
                    return Ok(None);
                }
                (None, d) => cur.saturating_add_signed(d),
            };
            let f = f.with_digits(new);
            self.view.fmtstrs[i].1 = f.text();
            self.state.widths_key = None;
            return Ok(f.digits());
        }
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

    /// `rids[i]` of `column` := `nums[i]`, as the column's type.
    fn number_rows(&mut self, column: &str, rids: Vec<u64>, nums: &[i64]) -> Result<(), String> {
        let texts: Vec<Option<String>> = nums.iter().map(|i| Some(i.to_string())).collect();
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

/// `base`, `base + 1`, … (`n` of them).
fn counting(base: u64, n: usize) -> Vec<i64> {
    (0..n as i64).map(|i| base as i64 + i).collect()
}
