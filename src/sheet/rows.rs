//! Row lookups: matches, duplicates, predicates, search hits, selection stats.

use super::*;

impl Sheet {
    /// Status-line stats of `column` over the selected rows (S-30 C), as
    /// (name, value) pairs: numbers get sum avg min max, other types count
    /// and distinct. View casts apply; filtered-out selected rows count too.
    pub fn selection_stats(&self, column: &str) -> PolarsResult<Vec<(&'static str, String)>> {
        let rids: Vec<u64> = self.view.selected.iter().copied().collect();
        let c = || col(column);
        let lf = self.filtered_plan(true).filter(rid_in(&rids)).select([c()]);
        let dtype = lf
            .clone()
            .collect_schema()?
            .get(column)
            .cloned()
            .unwrap_or(DataType::Null);
        let mut out = vec![("sel", grouped(rids.len() as u64))];
        if dtype.is_primitive_numeric() {
            let df = lf
                .select([
                    c().sum().cast(DataType::Float64).alias("sum"),
                    c().mean().alias("avg"),
                    c().min().cast(DataType::Float64).alias("min"),
                    c().max().cast(DataType::Float64).alias("max"),
                ])
                .collect()?;
            for name in ["sum", "avg", "min", "max"] {
                let v = df.column(name)?.f64()?.get(0);
                out.push((name, v.map_or("·".into(), stat_num)));
            }
        } else {
            let df = lf
                .select([
                    c().count().alias("count"),
                    c().drop_nulls().n_unique().alias("distinct"),
                ])
                .collect()?;
            for name in ["count", "distinct"] {
                let v = df
                    .column(name)?
                    .cast(&DataType::UInt64)?
                    .u64()?
                    .get(0)
                    .unwrap_or(0);
                out.push((name, grouped(v)));
            }
        }
        Ok(out)
    }

    /// Whether the base plan has `name` (hidden columns count: filter/sort may use them).
    pub fn has_column(&self, name: &str) -> bool {
        self.plan
            .clone()
            .collect_schema()
            .is_ok_and(|s| s.contains(name))
    }

    /// Row ids of view rows whose `column` (as text) matches `pattern`.
    pub fn rows_matching(&self, column: &str, pattern: &str) -> PolarsResult<Vec<u64>> {
        self.rids_matching(column, pattern, false)
    }

    /// `rows_matching`, or (`invert`, `:v`) the rows that don't match, empty
    /// cells among them.
    pub fn rids_matching(
        &self,
        column: &str,
        pattern: &str,
        invert: bool,
    ) -> PolarsResult<Vec<u64>> {
        let m = col(column)
            .cast(DataType::String)
            .str()
            .contains(lit(pattern), true);
        let m = match invert {
            true => m.fill_null(lit(false)).not(),
            false => m,
        };
        let df = self
            .filtered_plan(true)
            .filter(m)
            .select([col(ROW_ID)])
            .collect()?;
        rid_values(&df)
    }

    /// Row ids of view rows `from..from + n` (from 0), in view order.
    pub fn rids_in_rows(&self, from: u64, n: u64) -> PolarsResult<Vec<u64>> {
        let df = self
            .sorted_plan(true)
            .slice(from as i64, n as IdxSize)
            .select([col(ROW_ID)])
            .collect()?;
        rid_values(&df)
    }

    /// Row ids of view rows whose `column` equals one of `values` (`,`).
    pub fn rids_with_values(&self, column: &str, values: &[AnyValue]) -> PolarsResult<Vec<u64>> {
        let df = self
            .filtered_plan(true)
            .filter(values_predicate(column, values))
            .select([col(ROW_ID)])
            .collect()?;
        rid_values(&df)
    }

    /// Row ids of view rows whose `column` repeats a value seen above (view
    /// order; nulls repeat each other). VisiData `select-col-duplicates`.
    pub fn rids_col_duplicates(&self, column: &str) -> PolarsResult<Vec<u64>> {
        let df = self
            .view_plan_rid(true)
            .select([col(ROW_ID), col(column)])
            .collect()?;
        let firsts: std::collections::HashSet<u64> = rid_values(&df.unique_stable(
            Some(&[column.to_string()]),
            UniqueKeepStrategy::First,
            None,
        )?)?
        .into_iter()
        .collect();
        Ok(rid_values(&df)?
            .into_iter()
            .filter(|r| !firsts.contains(r))
            .collect())
    }

    /// Row ids of every row in the (filtered) view.
    pub fn view_rids(&self) -> PolarsResult<Vec<u64>> {
        rid_values(&self.filtered_plan(true).select([col(ROW_ID)]).collect()?)
    }

    /// Rows of the full view where `pattern` (regex, case-insensitive, as
    /// VisiData) matches `column`, or any visible column when `None`, as
    /// `(view row, first matching column)`, in view order.
    pub fn search_hits(
        &self,
        pattern: &str,
        column: Option<&str>,
    ) -> PolarsResult<Vec<(u64, usize)>> {
        let plan = self.view_plan();
        let names: Vec<String> = plan
            .clone()
            .collect_schema()?
            .iter_names()
            .map(|n| n.to_string())
            .collect();
        let searched: Vec<usize> = (0..names.len())
            .filter(|&i| column.is_none_or(|c| names[i] == c))
            .collect();
        if searched.is_empty() {
            return Ok(Vec::new());
        }
        let hit = |i: usize| format!("__vdrs_hit{i}");
        let pattern = format!("(?i){pattern}");
        let mut select = vec![col("__vdrs_row")];
        select.extend(searched.iter().map(|&i| {
            col(names[i].as_str())
                .cast(DataType::String)
                .str()
                .contains(lit(pattern.as_str()), true)
                .fill_null(false)
                .alias(hit(i).as_str())
        }));
        let Some(any) = searched
            .iter()
            .map(|&i| col(hit(i).as_str()))
            .reduce(|a, b| a.or(b))
        else {
            return Ok(Vec::new());
        };
        let df = plan
            .with_row_index("__vdrs_row", None)
            .select(select)
            .filter(any)
            .collect()?;
        let rows = df.column("__vdrs_row")?.cast(&DataType::UInt64)?;
        let rows = rows.u64()?;
        let flags = searched
            .iter()
            .map(|&i| Ok((i, df.column(hit(i).as_str())?.bool()?.clone())))
            .collect::<PolarsResult<Vec<_>>>()?;
        Ok((0..df.height())
            .filter_map(|r| {
                let row = rows.get(r)?;
                let c = flags.iter().find(|(_, f)| f.get(r) == Some(true))?.0;
                Some((row, c))
            })
            .collect())
    }

    /// Rows for `y` (the view row at `pos`) or `gy` (`None`: the selected
    /// rows), in view order, every column incl. hidden ones, no row id (S-18 A).
    /// `base`: values as stored, not as the view's casts (`#` `%` `@`) show
    /// them, so `p` pastes the row itself (the system clipboard gets the shown text).
    pub fn copy_rows(&self, pos: Option<u64>, base: bool) -> PolarsResult<DataFrame> {
        let lf = self.sorted_plan(true);
        let mut lf = match pos {
            Some(p) => lf.slice(p as i64, 1),
            None => {
                let rids: Vec<u64> = self.view.selected.iter().copied().collect();
                lf.filter(rid_in(&rids))
            }
        };
        if base && !self.view.type_casts.is_empty() {
            let order = lf
                .collect_schema()?
                .iter_names()
                .cloned()
                .collect::<Vec<_>>();
            let cast: Vec<&str> = self
                .view
                .type_casts
                .iter()
                .map(|(c, _)| c.as_str())
                .collect();
            let mut raw = self.plan.clone();
            if !self.has_rids() {
                raw = raw.with_row_index(ROW_ID, None); // as `filtered_plan_with` numbers them
            }
            let raw = raw.select(
                std::iter::once(col(ROW_ID).cast(DataType::UInt64))
                    .chain(cast.iter().map(|c| col(*c)))
                    .collect::<Vec<_>>(),
            );
            let args = JoinArgs {
                maintain_order: MaintainOrderJoin::Left,
                ..JoinArgs::new(JoinType::Left)
            };
            lf = lf
                .with_column(col(ROW_ID).cast(DataType::UInt64))
                .drop(cols(cast))
                .join(raw, [col(ROW_ID)], [col(ROW_ID)], args)
                .select(order.into_iter().map(col).collect::<Vec<_>>());
        }
        lf.collect()?.drop(ROW_ID)
    }

    /// Schema-only check of a `:select` / `z|` predicate (no data read).
    pub fn check_predicate(&self, pred: &str) -> Result<(), String> {
        self.predicate(&self.filtered_plan(false), pred).map(|_| ())
    }

    /// `pred` (a SQL expression, S-21) as a filter over the view's rows,
    /// hidden columns included. Refused unless it is a boolean.
    pub(super) fn predicate(&self, lf: &LazyFrame, pred: &str) -> Result<Expr, String> {
        let e = polars::sql::sql_expr(pred).map_err(|e| format!("expr: {e}"))?;
        let schema = lf
            .clone()
            .select([e.clone().alias("__vdrs_pred")])
            .collect_schema()
            .map_err(|e| format!("expr: {e}"))?;
        match schema.get("__vdrs_pred") {
            Some(DataType::Boolean) => Ok(e.fill_null(false)),
            _ => Err(format!("expr: not a boolean: {pred}")),
        }
    }

    /// View rows (positions in view order) where `pred` holds (`z/`).
    pub fn expr_hits(&self, pred: &str) -> Result<Vec<u64>, String> {
        let lf = self.sorted_plan(false).with_row_index("__vdrs_row", None);
        let p = self.predicate(&lf, pred)?;
        let df = lf
            .filter(p)
            .select([col("__vdrs_row").cast(DataType::UInt64)])
            .collect()
            .map_err(|e| e.to_string())?;
        Ok(df
            .column("__vdrs_row")
            .map_err(|e| e.to_string())?
            .u64()
            .map_err(|e| e.to_string())?
            .into_no_null_iter()
            .collect())
    }

    /// Row ids of view rows where `pred` holds (`:select`, `z|`).
    pub fn rids_where(&self, pred: &str) -> Result<Vec<u64>, String> {
        let lf = self.filtered_plan(true);
        let p = self.predicate(&lf, pred)?;
        let df = lf
            .filter(p)
            .select([col(ROW_ID)])
            .collect()
            .map_err(|e| e.to_string())?;
        rid_values(&df).map_err(|e| e.to_string())
    }
}
