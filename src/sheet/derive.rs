//! Derived sheets from a parent: frequency, pivot, describe, copies, selected and dive rows, dedupe, `:sql`, `:join`, `:unpivot`.

use super::*;

impl Sheet {
    /// Build a frequency-table sheet grouping by `col_name`, values in
    /// first-seen order, then by count, desc, ties keeping that order (as
    /// VisiData's stable sort); numeric and date keys stay in first-seen
    /// order (`freq_keeps_order`).
    /// `percent` (count / total * 100, 2 decimals) and `histogram` (a `■` bar)
    /// follow `count`, as in VisiData's freq output.
    pub fn freq_from(parent: &Sheet, id: SheetId, col_name: &str) -> Self {
        Self::freq_binned(parent, id, &[col_name.to_string()], None)
    }

    /// [`Self::freq_from`] over `columns` together (`gF` groups by the key
    /// columns, S-52), one column's numeric values grouped by `bins` (S-35
    /// C1; [`Self::numeric_bins`]).
    pub fn freq_binned(
        parent: &Sheet,
        id: SheetId,
        columns: &[String],
        bins: Option<Binning>,
    ) -> Self {
        let col_name = columns.first().map_or("", String::as_str);
        let (count, percent, histogram) = freq_stat_names(columns);
        let keep_order = freq_keeps_order(parent, columns);
        let aggs = columns::freq_aggregators(parent);
        let exprs = std::iter::once(len().cast(DataType::Int64).alias(count));
        let exprs: Vec<Expr> = exprs
            .chain(aggs.iter().map(|(c, a)| columns::agg_expr(c, a)))
            .collect();
        let base = parent.view_plan_keeping(false, columns);
        // a date `fmtstr` or a number format the user set groups by the text
        // it shows (S-35 C2, S-36 B K3)
        let keys: Vec<Expr> = (columns.iter())
            .map(|c| {
                let key = match (parent.date_fmt(c), parent.num_fmt(c)) {
                    (Some(f), _) => col(c.as_str()).dt().strftime(f),
                    (None, Some(n)) => n.text_expr(col(c.as_str())),
                    (None, None) => col(c.as_str()),
                };
                parent.err_key(key).alias(c.as_str())
            })
            .collect();
        let grouped = match &bins {
            None => base.group_by_stable(keys).agg(exprs),
            Some(Binning::Values { .. }) => base.group_by_stable([col(col_name)]).agg(exprs).sort(
                [col_name],
                SortMultipleOptions::default()
                    .with_nulls_last(true)
                    .with_maintain_order(true),
            ),
            Some(Binning::Ranges { min, width, n }) => {
                const B: &str = "__vdrs_bin";
                let by_bin = base
                    .with_column(Binning::index(*min, *width, *n, col_name).alias(B))
                    .group_by_stable([col(B)])
                    .agg(exprs);
                // every bin is a row, then the null bin (dropped when empty)
                let ids: Vec<Option<i64>> = (0..*n as i64).map(Some).chain([None]).collect();
                let labels: Vec<Option<String>> = Binning::labels(*min, *width, *n)
                    .into_iter()
                    .map(Some)
                    .chain([None])
                    .collect();
                let all = Series::new(B.into(), ids)
                    .into_frame()
                    .lazy()
                    .with_column(lit(Series::new(col_name.into(), labels)));
                let args = JoinArgs {
                    maintain_order: MaintainOrderJoin::Left,
                    nulls_equal: true,
                    ..JoinArgs::new(JoinType::Left)
                };
                // a bin with no values counts (and sums) 0
                let zero = aggs
                    .iter()
                    .filter(|(_, a)| columns::zero_on_empty(a))
                    .map(|(c, a)| columns::agg_name(c, a));
                let fill: Vec<Expr> = std::iter::once(count.to_string())
                    .chain(zero)
                    .map(|c| col(c.as_str()).fill_null(lit(0)))
                    .collect();
                let names: Vec<Expr> = [col_name, count]
                    .into_iter()
                    .map(String::from)
                    .chain(aggs.iter().map(|(c, a)| columns::agg_name(c, a)))
                    .map(|c| col(c.as_str()))
                    .collect();
                all.join(by_bin, [col(B)], [col(B)], args)
                    .with_columns(fill)
                    .filter(col(B).is_not_null().or(col(count).gt(lit(0))))
                    .select(names)
            }
        };
        let plan = freq_stats(grouped, (count, percent, histogram), keep_order);
        let mut freq = Sheet::child(
            parent,
            id,
            format!("freq({}): {}", columns.join(","), parent.name),
            plan,
            Origin::Freq {
                parent: parent.id,
                columns: columns.to_vec(),
            },
        )
        .derived(
            parent,
            Derivation::Freq {
                columns: columns.to_vec(),
                bins,
            },
        );
        freq.view.keys = columns.to_vec(); // as VisiData
        // VisiData hides percent and histogram once aggregators add columns.
        if !aggs.is_empty() {
            freq.view
                .hidden
                .extend([percent.to_string(), histogram.to_string()]);
        }
        freq
    }

    /// `numeric_binning` (S-35 C1): how `F` bins `column` of `parent`'s view,
    /// `None` when it isn't numeric. `hist_bins` 0: ⌊√rows⌋ bins (VisiData
    /// `histogram_bins`). Reads the column's min and max.
    pub fn numeric_bins(
        parent: &Sheet,
        column: &str,
        hist_bins: usize,
    ) -> Result<Option<Binning>, String> {
        let e = |e: PolarsError| e.to_string();
        let plan = parent.view_plan_keeping(false, &[column.to_string()]);
        let dt = (plan.clone().collect_schema().ok()).and_then(|s| s.get(column).cloned());
        let Some(dt) = dt.filter(DataType::is_primitive_numeric) else {
            return Ok(None);
        };
        let c = col(column).cast(DataType::Float64);
        let row = plan
            .select([
                len().alias("n"),
                c.clone().min().alias("lo"),
                c.max().alias("hi"),
            ])
            .collect()
            .map_err(e)?;
        let get = |n: &str| {
            row.column(n)
                .ok()
                .and_then(|c| c.get(0).ok()?.extract::<f64>())
        };
        let rows = get("n").unwrap_or(0.0);
        let (Some(min), Some(max)) = (get("lo"), get("hi")) else {
            return Ok(Some(Binning::Values { hist_bins }));
        };
        let n = if hist_bins > 0 {
            hist_bins
        } else {
            rows.sqrt() as usize
        }
        .max(1);
        let width = (max - min) / n as f64;
        Ok(Some(
            if width == 0.0 || (dt.is_integer() && n as f64 > max - min) || width == 1.0 {
                Binning::Values { hist_bins }
            } else {
                Binning::Ranges { min, width, n }
            },
        ))
    }

    /// Build a pivot sheet. Eager: collects the parent's `index`, `on` and
    /// `values` columns, then pivots.
    /// Direction §8: Polars 2.x may spill on group-bys but pivots stay in-memory.
    pub fn pivot_from(
        parent: &Sheet,
        id: SheetId,
        index: &str,
        on: &str,
        values: &str,
        agg: &str,
    ) -> Result<Self, String> {
        let e = element();
        let pivot_agg = match agg.to_ascii_lowercase().as_str() {
            "sum" => e.sum(),
            "mean" | "avg" | "average" => e.mean(),
            "count" | "len" => len().cast(DataType::Int64),
            "max" => e.max(),
            "min" => e.min(),
            "first" => e.first(),
            "last" => e.last(),
            other => return Err(format!("unknown agg: {other}")),
        };
        let mut need = vec![index, on, values];
        need.sort_unstable();
        need.dedup();
        let keep: Vec<String> = need.iter().map(|c| c.to_string()).collect();
        let df = parent
            .view_plan_keeping(false, &keep)
            .select(need.into_iter().map(col).collect::<Vec<_>>())
            .collect()
            .map_err(|e| e.to_string())?;
        let out = pivot_df(&df, on, &[index.to_string()], values, pivot_agg)
            .map_err(|e| e.to_string())?;
        let plan = out.clone().lazy();
        Ok(Sheet::child(
            parent,
            id,
            format!("pivot({index}×{on}→{values}): {}", parent.name),
            plan,
            Origin::Pivot { parent: parent.id },
        )
        .with_window(out)
        .derived(
            parent,
            Derivation::Pivot {
                index: index.into(),
                on: on.into(),
                values: values.into(),
                agg: agg.to_ascii_lowercase(),
            },
        ))
    }

    /// Build a sheet containing only the parent's selected rows.
    /// Eager: collects parent, filters by row index. VisiData `"` semantic.
    pub fn dup_selected_from(parent: &Sheet, id: SheetId) -> Result<Self, String> {
        if parent.view.selected.is_empty() {
            return Err("no rows selected".into());
        }
        let name = format!(
            "selected ({} rows): {}",
            parent.view.selected.len(),
            parent.name
        );
        let df = parent
            .view_plan_rid(true)
            .collect()
            .map_err(|e| e.to_string())?;
        Self::rows_from(parent, id, df, &parent.view.selected, name)
    }

    /// VisiData `dedupe-rows`: the first row of each key (each distinct row
    /// when there are no key columns), in view order.
    pub fn dedupe_from(parent: &Sheet, id: SheetId) -> Result<Self, String> {
        let df = parent
            .view_plan_rid(true)
            .collect()
            .map_err(|e| e.to_string())?;
        let mut by: Vec<String> = parent
            .view
            .keys
            .iter()
            .filter(|k| df.column(k).is_ok())
            .cloned()
            .collect();
        if by.is_empty() {
            by = df
                .get_column_names()
                .into_iter()
                .filter(|n| n.as_str() != ROW_ID)
                .map(|n| n.to_string())
                .collect();
        }
        let firsts = df
            .unique_stable(Some(&by), UniqueKeepStrategy::First, None)
            .and_then(|d| rid_values(&d))
            .map_err(|e| e.to_string())?;
        let keep: std::collections::BTreeSet<u64> = firsts.into_iter().collect();
        let name = format!("dedupe ({} rows): {}", keep.len(), parent.name);
        Self::rows_from(parent, id, df, &keep, name)
    }

    /// Rows of `df` (the parent's view with row ids) whose id is in `keep`,
    /// as a sheet rebuilt by row id.
    pub(super) fn rows_from(
        parent: &Sheet,
        id: SheetId,
        df: DataFrame,
        keep: &std::collections::BTreeSet<u64>,
        name: String,
    ) -> Result<Self, String> {
        let rids = rid_values(&df).map_err(|e| e.to_string())?;
        let bc: BooleanChunked = rids.iter().map(|r| keep.contains(r)).collect();
        let out = df
            .filter(&bc)
            .and_then(|d| d.drop(ROW_ID))
            .map_err(|e| e.to_string())?;
        let plan = out.clone().lazy();
        Ok(Sheet::child(
            parent,
            id,
            name,
            plan,
            Origin::Selected { parent: parent.id },
        )
        .with_window(out)
        .inherit_attrs(parent)
        .derived(parent, Derivation::Rids(keep.clone())))
    }

    /// What a freq dive from this sheet compares: what the `F` it descends
    /// from grouped by, as it ran (the parent's fmtstr may change since). A
    /// dive from a freq sheet keeps its origin: rows of that freq sheet.
    pub fn freq_dive_key(&self) -> DiveKey {
        let Some(l) = self.state.lineage.as_deref() else {
            return DiveKey::Value;
        };
        match &l.how {
            Derivation::Freq {
                bins: Some(Binning::Ranges { min, width, n }),
                ..
            } => DiveKey::Bin {
                min: *min,
                width: *width,
                n: *n,
            },
            Derivation::Freq { columns, .. } => match columns.as_slice() {
                [column] => l.parent.dive_key(column),
                _ => DiveKey::Value,
            },
            Derivation::Values { .. } if matches!(l.parent.origin, Origin::Freq { .. }) => {
                l.parent.freq_dive_key()
            }
            _ => DiveKey::Value,
        }
    }

    /// What a freq sheet of this sheet groups `column` by: the text its date
    /// `fmtstr` or number format shows, else its value.
    pub fn dive_key(&self, column: &str) -> DiveKey {
        match self.date_fmt(column) {
            Some(f) => DiveKey::Fmt(f.into()),
            None => (self.num_fmt(column)).map_or(DiveKey::Value, |n| DiveKey::Num(n.text())),
        }
    }

    /// A freq sheet itself, not rows picked from one (`rows_with_values`).
    pub fn is_freq(&self) -> bool {
        let how = self.state.lineage.as_deref().map(|l| &l.how);
        matches!(how, Some(Derivation::Freq { .. }))
    }

    /// Freq dive: `parent`'s rows whose `column`'s `key` (as text, the way the
    /// freq sheet grouped them) is one of `values` (None matches null).
    pub fn rows_with_values(
        parent: &Sheet,
        id: SheetId,
        column: &str,
        key: DiveKey,
        values: Vec<Option<String>>,
    ) -> Self {
        let float = matches!(key, DiveKey::Value) && parent.is_float_column(column);
        let values = match float {
            true => values
                .into_iter()
                .map(|v| v.map(|t| zero_unsigned(&t)))
                .collect(),
            false => values,
        };
        let text: Vec<&str> = values.iter().flatten().map(String::as_str).collect();
        let mut pred = key
            .expr(column, float)
            .is_in(lit(Series::new("v".into(), &text)).implode(false), false);
        if values.iter().any(Option::is_none) {
            pred = pred.or(key.expr(column, float).is_null());
        }
        let shown = |v: &Option<String>| match (&key, v) {
            (DiveKey::Bin { min, width, n }, Some(i)) => i
                .parse::<usize>()
                .ok()
                .and_then(|i| Binning::labels(*min, *width, *n).into_iter().nth(i))
                .unwrap_or_default(),
            (DiveKey::Bin { .. }, None) => "null bin".into(),
            (_, v) => v.clone().unwrap_or_default(),
        };
        let label = match (&key, &values[..]) {
            (DiveKey::Bin { .. }, [one]) => format!("{column} in {}", shown(one)),
            (DiveKey::Bin { .. }, many) => format!("{column} in {} bins", many.len()),
            (_, [one]) => format!("{column}={}", shown(one)),
            (_, many) => format!("{column} in {} values", many.len()),
        };
        // freq rows picked from a freq sheet still dive to the source rows, as
        // VisiData — while the value column is still first (the dive reads it)
        let first = |mut p: LazyFrame| {
            p.collect_schema()
                .ok()
                .and_then(|s| s.get_at_index(0).map(|(n, _)| n.clone()))
        };
        let origin = match &parent.origin {
            Origin::Freq { .. } if first(parent.view_plan()) == first(parent.plan.clone()) => {
                parent.origin.clone()
            }
            _ => Origin::Selected { parent: parent.id },
        };
        let mut rows = Self::child(
            parent,
            id,
            format!("{label}: {}", parent.name),
            (parent.view_plan_keeping(false, &[column.to_string()])).filter(pred),
            origin,
        )
        .inherit_attrs(parent);
        // the dive reads a hidden column but still hides it
        if parent.view.hidden.contains(column) {
            rows.view.hidden.insert(column.to_string());
        }
        rows.derived(
            parent,
            Derivation::Values {
                column: column.into(),
                key,
                values,
            },
        )
    }

    /// The columns an unpivot of `on` (empty: all but `index`) melts as text:
    /// all of them when their types differ, so `false` next to a number stays
    /// `false` (VisiData keeps each value), none when they share a type.
    pub(crate) fn melt_as_text(&self, on: &[String], index: &[String]) -> Vec<String> {
        let Ok(schema) = self.view_plan().collect_schema() else {
            return Vec::new();
        };
        let melted: Vec<(String, DataType)> = (schema.iter())
            .filter(|(n, _)| match on.is_empty() {
                true => !index.iter().any(|i| i == n.as_str()),
                false => on.iter().any(|o| o == n.as_str()),
            })
            .map(|(n, t)| (n.to_string(), t.clone()))
            .collect();
        match melted.windows(2).all(|w| w[0].1 == w[1].1) {
            true => Vec::new(),
            false => melted.into_iter().map(|(n, _)| n).collect(),
        }
    }

    /// `column` of the view (hidden or not) holds floats.
    pub(crate) fn is_float_column(&self, column: &str) -> bool {
        let keep = [column.to_string()];
        let schema = self.view_plan_keeping(false, &keep).collect_schema().ok();
        schema
            .and_then(|s| s.get(column).cloned())
            .is_some_and(|t| t.is_float())
    }

    /// `g"` / `:dup`: an independent copy of what `parent` shows.
    pub fn copy_from(parent: &Sheet, id: SheetId) -> Self {
        let origin = Origin::Copy { parent: parent.id };
        Self::child(
            parent,
            id,
            format!("copy of {}", parent.name),
            parent.view_plan(),
            origin,
        )
        .inherit_attrs(parent)
        .derived(parent, Derivation::Copy)
    }

    /// `:sql <query>`: `self` is `parent`'s view; `others` are extra tables.
    pub fn sql_from(
        parent: &Sheet,
        id: SheetId,
        query: &str,
        others: Vec<Sheet>,
    ) -> Result<Self, String> {
        let mut ctx = polars::sql::SQLContext::new();
        ctx.register("self", parent.view_plan());
        for o in &others {
            ctx.register(&format!("s{}", o.id), o.view_plan());
        }
        // Only the sheets the query names belong in the lineage.
        let words: Vec<String> = query
            .split(|c: char| !c.is_alphanumeric() && c != '_')
            .map(str::to_ascii_lowercase)
            .collect();
        let others = others
            .into_iter()
            .filter(|o| words.contains(&format!("s{}", o.id)))
            .collect();
        let mut plan = ctx.execute(query).map_err(|e| e.to_string())?;
        plan.collect_schema().map_err(|e| e.to_string())?;
        let origin = Origin::Sql { parent: parent.id };
        Ok(
            Self::child(parent, id, format!("sql: {query}"), plan, origin).derived(
                parent,
                Derivation::Sql {
                    query: query.into(),
                    others,
                },
            ),
        )
    }

    /// `I` (VisiData describe-sheet): one row per visible column, VisiData's
    /// columns: sheet, column, type (both hidden), errors (failed type casts),
    /// nulls, distinct (null is one value), mode (most common, first seen on
    /// ties), then min, max, sum, median, mean, stdev for numbers; dates get
    /// min, max and a median (an odd count's middle value; VisiData errors on
    /// the rest). Eager.
    pub fn describe_from(parent: &Sheet, id: SheetId) -> Result<Self, String> {
        let schema = parent
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        let mut lf = parent.sorted_plan_with(false, true);
        let has_err = lf.collect_schema().map_err(|e| e.to_string())?;
        let stat = |i: usize, s: &str| format!("{i}\u{1f}{s}");
        let null = |t: DataType| lit(NULL).cast(t);
        let mut aggs = Vec::new();
        for (i, (name, dt)) in schema.iter().enumerate() {
            let c = || col(name.as_str());
            let err = has_err.get(&err_col(name)).map(|_| col(err_col(name)));
            let errors = match &err {
                Some(e) => e.clone().sum().cast(DataType::Int64),
                None => lit(0i64).cast(DataType::Int64),
            };
            let valid = || match &err {
                Some(e) => c().filter(e.clone().not()),
                None => c(),
            };
            let numeric = dt.is_numeric();
            // Duration has no text cast: no text stats.
            let texty = !dt.is_nested() && !matches!(dt, DataType::Duration(_));
            let ordered = texty && (numeric || dt.is_temporal());
            let text = |e: Expr, ok: bool| {
                if ok {
                    e.cast(DataType::String)
                } else {
                    null(DataType::String)
                }
            };
            let cnt = c().count().over([c()]).map_err(|e| e.to_string())?;
            let mode = c()
                .filter(c().is_not_null().and(cnt.clone().eq(cnt.max())))
                .first();
            let n = c().count();
            let middle = c()
                .drop_nulls()
                .sort(Default::default())
                .get(n.clone() / lit(2u32), true);
            let median = when(n.clone().eq(lit(0u32)))
                .then(null(DataType::String))
                .when((n % lit(2u32)).eq(lit(1u32)))
                .then(middle.cast(DataType::String))
                .otherwise(text(c().median(), numeric));
            aggs.extend([
                errors.clone().alias(stat(i, "errors")),
                (c().null_count().cast(DataType::Int64) - errors).alias(stat(i, "nulls")),
                valid()
                    .n_unique()
                    .cast(DataType::Int64)
                    .alias(stat(i, "distinct")),
                text(mode, texty).alias(stat(i, "mode")),
                text(c().min(), ordered).alias(stat(i, "min")),
                text(c().max(), ordered).alias(stat(i, "max")),
                text(c().sum(), numeric).alias(stat(i, "sum")),
                text(median, ordered).alias(stat(i, "median")),
                if numeric {
                    c().mean().cast(DataType::Float64)
                } else {
                    null(DataType::Float64)
                }
                .alias(stat(i, "mean")),
                if numeric {
                    c().std(1).cast(DataType::Float64)
                } else {
                    null(DataType::Float64)
                }
                .alias(stat(i, "stdev")),
            ]);
        }
        let row = lf.select(aggs).collect().map_err(|e| e.to_string())?;
        let n = schema.len();
        let stat_col = |s: &str| -> Result<Column, String> {
            let parts: Vec<Column> = (0..n)
                .map(|i| row.column(&stat(i, s)).cloned().map_err(|e| e.to_string()))
                .collect::<Result<_, _>>()?;
            let mut out = parts
                .first()
                .cloned()
                .unwrap_or_else(|| Column::new_empty(s.into(), &DataType::Null));
            for p in &parts[1.min(parts.len())..] {
                out.append(p).map_err(|e| e.to_string())?;
            }
            Ok(out.with_name(s.into()))
        };
        let names: Vec<String> = schema.iter_names().map(|s| s.to_string()).collect();
        let types: Vec<String> = schema.iter().map(|(_, dt)| dt.to_string()).collect();
        let mut cols = vec![
            Column::new("sheet".into(), vec![parent.name.clone(); n]),
            Column::new("column".into(), names),
            Column::new("type".into(), types),
        ];
        for s in DESCRIBE_STATS {
            cols.push(stat_col(s)?);
        }
        let df = DataFrame::new_infer_height(cols).map_err(|e| e.to_string())?;
        let origin = Origin::Describe { parent: parent.id };
        let mut sheet = Self::child(
            parent,
            id,
            format!("describe: {}", parent.name),
            df.lazy(),
            origin,
        )
        .derived(parent, Derivation::Describe);
        sheet.view.hidden = ["sheet", "type"].iter().map(|s| s.to_string()).collect();
        Ok(sheet)
    }

    /// `zs` / `zu` on the describe sheet (VisiData select-cell, S-41 F): the
    /// view rows of this sheet that `stat` of `column` describes. errors /
    /// nulls: those cells; mode, min, max: rows equal to it; median: rows
    /// equal to the middle value, or either middle value of an even count;
    /// distinct: every row; the rest (sum, mean, stdev): rows with a value.
    pub fn described_rids(&self, column: &str, stat: &str) -> Result<Vec<u64>, String> {
        let e = |e: PolarsError| e.to_string();
        let lf = self.sorted_plan_with(true, true);
        let schema = lf.clone().collect_schema().map_err(e)?;
        if !schema.contains(column) {
            return Err(format!("no column {column}"));
        }
        let c = || col(column);
        let err = match schema.contains(err_col(column).as_str()) {
            true => col(err_col(column)),
            false => lit(false),
        };
        let valid = || c().is_not_null().and(err.clone().not());
        let sorted = || c().drop_nulls().sort(Default::default());
        let n = || c().count();
        let pred = match stat {
            "errors" => err.clone(),
            "nulls" => c().is_null().and(err.clone().not()),
            "distinct" => lit(true),
            "min" => c().eq(c().min()),
            "max" => c().eq(c().max()),
            "mode" => {
                let cnt = c().count().over([c()]).map_err(e)?;
                c().eq(c()
                    .filter(c().is_not_null().and(cnt.clone().eq(cnt.max())))
                    .first())
            }
            "median" => {
                let low = sorted().get((n() - lit(1u32)) / lit(2u32), true);
                let high = sorted().get(n() / lit(2u32), true);
                c().eq(low).or(c().eq(high))
            }
            "sum" | "mean" | "stdev" => valid(),
            _ => return Err("no rows described in this column".into()),
        };
        let df = lf
            .filter(pred.fill_null(lit(false)))
            .select([col(ROW_ID)])
            .collect()
            .map_err(e)?;
        rid_values(&df).map_err(e)
    }

    /// `W`: pivot of the current column `on`, rows keyed by `keys`: counts,
    /// or with `aggs` (the `+` aggregators, [`columns::freq_aggregators`])
    /// one column per aggregator and value, VisiData's names (S-35 B).
    /// No keys → one row, one column per distinct value (first-seen order).
    pub fn pivot_current_from(
        parent: &Sheet,
        id: SheetId,
        keys: &[String],
        on: &str,
        aggs: &[(String, String)],
    ) -> Result<Self, String> {
        let name = format!("pivot({on}): {}", parent.name);
        let origin = Origin::Pivot { parent: parent.id };
        // The cursor may sit on a key column; a null and "" are one column
        // header (VisiData's `''`), so they are one group.
        // A hidden key is no key (VisiData `keyCols`).
        let shown = parent
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        let keys: Vec<String> = keys
            .iter()
            .filter(|k| *k != on && shown.contains(k.as_str()))
            .cloned()
            .collect();
        let keys = &keys[..];
        let value_text = col(on).cast(DataType::String).fill_null(lit(""));
        let aggs: Vec<(String, String)> = aggs
            .iter()
            .filter(|(c, _)| c != on && !keys.contains(c))
            .cloned()
            .collect();
        if !aggs.is_empty() {
            let out = pivot_aggs(
                err_keys_of(parent, keys).with_column(value_text),
                keys,
                on,
                &aggs,
            )
            .map_err(|e| e.to_string())?;
            let how = Derivation::PivotCount {
                keys: keys.to_vec(),
                on: on.to_string(),
                aggs,
            };
            return Ok(Self::child(parent, id, name, out.lazy(), origin).derived(parent, how));
        }
        let counted = err_keys_of(parent, keys)
            .with_column(value_text)
            .group_by_stable(
                keys.iter()
                    .chain([&on.to_string()])
                    .map(|k| col(k.as_str()))
                    .collect::<Vec<_>>(),
            )
            .agg([len().cast(DataType::Int64).alias("__vdrs_n")])
            .collect()
            .map_err(|e| e.to_string())?;
        let out = if keys.is_empty() {
            let vals = counted
                .column(on)
                .map_err(|e| e.to_string())?
                .str()
                .map_err(|e| e.to_string())?
                .clone();
            let ns = counted
                .column("__vdrs_n")
                .map_err(|e| e.to_string())?
                .cast(&DataType::Int64)
                .map_err(|e| e.to_string())?;
            let ns = ns.i64().map_err(|e| e.to_string())?;
            let cols: Vec<Column> = vals
                .iter()
                .zip(ns.iter())
                .map(|(v, n)| Column::new(v.unwrap_or_default().into(), [n.unwrap_or(0)]))
                .collect();
            DataFrame::new_infer_height(cols).map_err(|e| e.to_string())?
        } else {
            // VisiData counts an absent pair as 0
            let df = pivot_df(&counted, on, keys, "__vdrs_n", element().first())
                .map_err(|e| e.to_string())?;
            df.lazy()
                .with_column(
                    all()
                        .exclude_cols(keys.to_vec())
                        .as_expr()
                        .fill_null(lit(0)),
                )
                .collect()
                .map_err(|e| e.to_string())?
        };
        let how = Derivation::PivotCount {
            keys: keys.to_vec(),
            on: on.to_string(),
            aggs: Vec::new(),
        };
        Ok(Self::child(parent, id, name, out.lazy(), origin).derived(parent, how))
    }

    /// Columns of the view holding at most one distinct value (`:uniform`).
    pub fn uniform_columns(&self) -> PolarsResult<Vec<String>> {
        let mut lf = self.view_plan();
        let names: Vec<String> = lf
            .collect_schema()?
            .iter_names()
            .map(|n| n.to_string())
            .collect();
        let row = lf
            .select(
                names
                    .iter()
                    .map(|n| col(n.as_str()).n_unique().cast(DataType::UInt64))
                    .collect::<Vec<_>>(),
            )
            .collect()?;
        Ok(names
            .into_iter()
            .filter(|n| {
                row.column(n)
                    .ok()
                    .and_then(|c| c.get(0).ok()?.extract::<u64>())
                    .is_some_and(|d| d <= 1)
            })
            .collect())
    }

    /// `:join`: `left`'s view joined with `right` on same-named `keys`.
    /// `how`: inner | left | full | merge (full, right's non-null values win) |
    /// append (stack rows, union columns by name, tag each with a hidden
    /// `origin_sheet`) | concat (stack rows, columns by position, `left`'s names,
    /// same hidden `origin_sheet`).
    pub fn join_from(
        left: &Sheet,
        id: SheetId,
        right_sheet: &Sheet,
        keys: &[String],
        how: &str,
    ) -> Result<Self, String> {
        let plan = join_plan(left, right_sheet, keys, how, false)?;
        let right_name = right_sheet.name.as_str();
        let mut joined = Sheet::child(
            left,
            id,
            format!("{}&{} ({how})", left.name, file_name(right_name)),
            plan,
            Origin::Join {
                left: left.id,
                right_name: right_name.to_string(),
            },
        )
        .derived(
            left,
            Derivation::Join {
                right: Box::new(right_sheet.clone()),
                keys: keys.to_vec(),
                how: how.into(),
            },
        );
        if how == "append" || how == "concat" {
            // VisiData's `origin_sheet` starts at width 0; `g-` shows it.
            joined.view.hidden.insert("origin_sheet".into());
        }
        Ok(joined)
    }

    /// A join's source rows (S-45 B), from its lineage: for each of its rows,
    /// the left and right sheets' row ids, and which of their columns it
    /// shows. `None` for an append or concat, or a sheet that isn't a join.
    pub(super) fn join_sources(&self) -> Result<Option<JoinSources>, String> {
        let Some(Lineage {
            parent,
            how: Derivation::Join { right, keys, how },
            ..
        }) = self.state.lineage.as_deref()
        else {
            return Ok(None);
        };
        if how == "append" || how == "concat" {
            return Ok(None);
        }
        let df = join_plan(parent, right, keys, how, true)?
            .select([col(LRID), col(RRID)])
            .collect()
            .map_err(|e| e.to_string())?;
        let ids = |c: &str| -> Result<Vec<Option<u64>>, String> {
            let c = df.column(c).and_then(|c| c.cast(&DataType::UInt64));
            Ok(c.map_err(|e| e.to_string())?
                .u64()
                .map_err(|e| e.to_string())?
                .iter()
                .collect())
        };
        let names = |s: &Sheet| -> Result<Vec<String>, String> {
            let schema = s.view_plan().collect_schema().map_err(|e| e.to_string())?;
            Ok(schema.iter_names().map(|n| n.to_string()).collect())
        };
        let both = join_collisions(parent, right, keys, how)?;
        let shown = |s: &Sheet| -> Result<Vec<(String, String)>, String> {
            Ok(names(s)?
                .into_iter()
                .map(|n| match both.contains(&n) {
                    true => (format!("{}_{n}", extend_prefix(s)), n),
                    false => (n.clone(), n),
                })
                .collect())
        };
        let mut right_cols = shown(right)?;
        if how == "extend" {
            let prefix = extend_prefix(right);
            for (shown, n) in &mut right_cols {
                if !keys.contains(n) {
                    *shown = format!("{prefix}_{n}");
                }
            }
        }
        // `g&`'s inner joins carry the result's own id: their cells come
        // from the sheets they joined
        let inner = match parent.id == self.id {
            true => parent.join_sources()?.map(Arc::new),
            false => None,
        };
        Ok(Some(JoinSources {
            left: parent.id,
            right: right.id,
            left_cols: shown(parent)?,
            right_cols,
            rows: ids(LRID)?.into_iter().zip(ids(RRID)?).collect(),
            inner,
        }))
    }

    /// `gF` dive: `parent`'s rows whose `keys` hold one of `wanted`'s rows,
    /// each compared as the text its [`DiveKey`] gives (nulls match nulls).
    pub fn rows_with_keys(
        parent: &Sheet,
        id: SheetId,
        keys: &[(String, DiveKey)],
        wanted: DataFrame,
        name: String,
    ) -> Result<Self, String> {
        let df = parent
            .view_plan_rid(true)
            .collect()
            .map_err(|e| e.to_string())?;
        let on: Vec<Expr> = keys.iter().map(|(k, _)| col(k.as_str())).collect();
        let args = JoinArgs {
            nulls_equal: true,
            ..JoinArgs::new(JoinType::Inner)
        };
        // a float `-0.0` matches as `0.0` on both sides (`DiveKey::expr`)
        let float = |k: &str, d: &DiveKey| matches!(d, DiveKey::Value) && parent.is_float_column(k);
        let shown = keys
            .iter()
            .map(|(k, d)| d.expr(k, float(k, d)).alias(k.as_str()));
        let wanted = wanted.lazy().select(
            keys.iter()
                .map(|(k, d)| match float(k, d) {
                    true => DiveKey::Value.expr(k, true),
                    false => col(k.as_str()).cast(DataType::String),
                })
                .collect::<Vec<_>>(),
        );
        let hit = (df.clone().lazy())
            .select([col(ROW_ID)].into_iter().chain(shown).collect::<Vec<_>>())
            .join(
                wanted.unique(None, UniqueKeepStrategy::First),
                on.clone(),
                on,
                args,
            )
            .select([col(ROW_ID)])
            .collect()
            .map_err(|e| e.to_string())?;
        let keep = rid_values(&hit)
            .map_err(|e| e.to_string())?
            .into_iter()
            .collect();
        Self::rows_from(parent, id, df, &keep, name)
    }

    /// `T` (VisiData transpose): a row per visible non-key column, a column
    /// per row named by its key values joined with `_` (its row number without
    /// keys), the first column holding the old names (named after the keys).
    /// Every cell as text, as VisiData's transposed sheet loses types (#2575).
    /// Built in memory, so refused past [`MAX_CELLS`].
    pub fn transpose_from(parent: &Sheet, id: SheetId) -> Result<Self, String> {
        let e = |e: PolarsError| e.to_string();
        let mut plan = parent.view_plan();
        let width = plan.collect_schema().map_err(e)?.len();
        let rows = (plan.clone().select([len()]).collect().map_err(e)?)
            .column("len")
            .ok()
            .and_then(|c| c.get(0).ok()?.extract::<usize>())
            .unwrap_or(0);
        if rows * width > MAX_CELLS {
            return Err(format!(
                "transpose: {} cells is over the {} limit",
                grouped((rows * width) as u64),
                grouped(MAX_CELLS as u64)
            ));
        }
        let df = plan.collect().map_err(e)?;
        let keys: Vec<&Column> = (parent.view.keys.iter())
            .filter_map(|k| df.column(k).ok())
            .collect();
        let others: Vec<&Column> = (df.columns().iter())
            .filter(|c| !parent.view.keys.iter().any(|k| k == c.name().as_str()))
            .filter(|c| c.name().as_str() != crate::reader::ERR_COL)
            .collect();
        // one cast per column; per cell where it can't (lists, structs)
        let text = |c: &Column| -> Vec<Option<String>> {
            column_texts(c).unwrap_or_else(|_| {
                (c.as_materialized_series().iter())
                    .map(|v| (!v.is_null()).then(|| any_text(&v)))
                    .collect()
            })
        };
        let key_text: Vec<Vec<Option<String>>> = keys.iter().map(|c| text(c)).collect();
        let names = (0..df.height()).map(|i| match key_text.is_empty() {
            true => i.to_string(),
            false => (key_text.iter())
                .map(|k| k[i].clone().unwrap_or_default())
                .collect::<Vec<_>>()
                .join("_"),
        });
        let first = match keys.is_empty() {
            true => "column".to_string(),
            false => (keys.iter())
                .map(|c| c.name().to_string())
                .collect::<Vec<_>>()
                .join("_"),
        };
        let names = crate::reader::unique_headers(std::iter::once(first).chain(names));
        let mut cells: Vec<Vec<Option<String>>> = others.iter().map(|c| text(c)).collect();
        let mut cols = vec![Column::new(
            names[0].as_str().into(),
            others
                .iter()
                .map(|c| c.name().to_string())
                .collect::<Vec<_>>(),
        )];
        for (i, name) in names.iter().enumerate().skip(1) {
            let row: Vec<Option<String>> = cells.iter_mut().map(|c| c[i - 1].take()).collect();
            cols.push(Column::new(name.as_str().into(), row));
        }
        let out = DataFrame::new_infer_height(cols).map_err(|e| e.to_string())?;
        let mut sheet = Self::child(
            parent,
            id,
            format!("{}_T", parent.name),
            out.lazy(),
            Origin::Transpose { parent: parent.id },
        )
        .derived(parent, Derivation::Transpose);
        sheet.view.keys = vec![names[0].clone()];
        Ok(sheet)
    }

    /// Build an unpivot (melt) sheet. `on` = columns to unpivot; `index` = keep columns.
    pub fn unpivot_from(parent: &Sheet, id: SheetId, on: &[String], index: &[String]) -> Self {
        use polars::prelude::UnpivotArgsDSL;
        // a bad JSON line's flag column (S-44) isn't data to melt
        let on: Vec<String> = on
            .iter()
            .filter(|c| *c != crate::reader::ERR_COL)
            .cloned()
            .collect();
        let on = on.as_slice();
        // a hidden key is no key (VisiData `keyCols`): it isn't kept
        let shown = parent.view_plan().collect_schema().ok();
        let index: Vec<String> = (index.iter())
            .filter(|c| shown.as_ref().is_none_or(|s| s.contains(c.as_str())))
            .cloned()
            .collect();
        let index = index.as_slice();
        let args = UnpivotArgsDSL {
            on: (!on.is_empty()).then(|| cols(on.iter().map(String::as_str))),
            index: cols(index.iter().map(String::as_str).chain([MELT_ROW])),
            variable_name: Some(MELT_VAR.into()),
            value_name: Some(MELT_VAL.into()),
        };
        // VisiData's names and row-major order; it leaves out null values
        let text: Vec<Expr> = (parent.melt_as_text(on, index).iter())
            .map(|c| col(c.as_str()).cast(DataType::String))
            .collect();
        let plan = parent
            .view_plan()
            .with_columns(text)
            .with_row_index(MELT_ROW, None)
            .unpivot(args)
            .filter(col(MELT_VAL).is_not_null())
            .sort(
                [MELT_ROW],
                SortMultipleOptions::default().with_maintain_order(true),
            )
            .drop(cols([MELT_ROW]))
            .rename([MELT_VAR, MELT_VAL], ["Variable", "Value"], true);
        let mut s = Sheet::child(
            parent,
            id,
            format!("unpivot({}): {}", on.join(","), parent.name),
            plan,
            Origin::Unpivot { parent: parent.id },
        )
        .derived(
            parent,
            Derivation::Unpivot {
                on: on.to_vec(),
                index: index.to_vec(),
            },
        );
        // VisiData's melt keeps its index columns as keys
        s.view.keys = index.to_vec();
        s
    }
}

/// Melt's source row, for VisiData's row-major order (also written by `:export`).
pub(crate) const MELT_ROW: &str = "__vdrs_melt";
/// Melt's output columns until the melted ones are gone: a melt of a melt
/// melts columns named `Variable` and `Value` (also written by `:export`).
pub(crate) const MELT_VAR: &str = "__vdrs_var";
pub(crate) const MELT_VAL: &str = "__vdrs_val";

/// `join_from` order helpers (also written by `:export`).
pub(crate) const JL: &str = "__vdrs_l";
pub(crate) const JLK: &str = "__vdrs_lk";
pub(crate) const JR: &str = "__vdrs_r";
pub(crate) const JRK: &str = "__vdrs_rk";
/// Right-only keys sort after every left key.
pub(crate) const JOIN_RIGHT_KEYS: i64 = 1 << 40;

/// `W` with aggregators (S-35 B): each aggregate's column name prefix
/// (VisiData's: `prefix` + `on` value).
pub(crate) fn pivot_prefixes(aggs: &[(String, String)]) -> Vec<String> {
    let several_cols = aggs.iter().any(|(c, _)| *c != aggs[0].0);
    aggs.iter()
        .map(|(c, a)| match (several_cols, aggs.len()) {
            (true, _) => format!("{}_", columns::agg_name(c, a)),
            (false, 1) => String::new(),
            _ => format!("{a}_"),
        })
        .collect()
}

/// [`Sheet::pivot_current_from`] with aggregators: group by `keys` and `on`,
/// aggregate, then a pivot per aggregate. Columns per `on` value, its
/// aggregates in `aggs` order; a pair with no rows counts and sums to 0.
fn pivot_aggs(
    plan: LazyFrame,
    keys: &[String],
    on: &str,
    aggs: &[(String, String)],
) -> PolarsResult<DataFrame> {
    const K: &str = "__vdrs_k";
    let index: Vec<String> = match keys {
        [] => vec![K.to_string()],
        k => k.to_vec(),
    };
    let plan = if keys.is_empty() {
        plan.with_column(lit(0).alias(K))
    } else {
        plan
    };
    let by: Vec<Expr> = index
        .iter()
        .chain([&on.to_string()])
        .map(|k| col(k.as_str()))
        .collect();
    let grouped = plan
        .group_by_stable(by)
        .agg(
            aggs.iter()
                .map(|(c, a)| columns::agg_expr(c, a))
                .collect::<Vec<_>>(),
        )
        .collect()?;
    let pivots: Vec<DataFrame> = aggs
        .iter()
        .map(|(c, a)| {
            pivot_df(
                &grouped,
                on,
                &index,
                &columns::agg_name(c, a),
                element().first(),
            )
        })
        .collect::<PolarsResult<_>>()?;
    let prefixes = pivot_prefixes(aggs);
    let mut out: Vec<Column> = if keys.is_empty() {
        Vec::new()
    } else {
        index
            .iter()
            .map(|k| pivots[0].column(k).cloned())
            .collect::<PolarsResult<_>>()?
    };
    let values: Vec<String> = pivots[0]
        .get_column_names()
        .iter()
        .map(|n| n.to_string())
        .filter(|n| !index.contains(n))
        .collect();
    for v in &values {
        for (((_, a), p), pre) in aggs.iter().zip(&pivots).zip(&prefixes) {
            let mut s = p.column(v)?.clone();
            if columns::zero_on_empty(a) {
                s = s.fill_null(FillNullStrategy::Zero)?;
            }
            out.push(s.with_name(format!("{pre}{v}").into()));
        }
    }
    DataFrame::new_infer_height(out)
}

/// `parent`'s view with `keys` as `#ERR` on error rows (S-44 A).
fn err_keys_of(parent: &Sheet, keys: &[String]) -> LazyFrame {
    let lf = parent.view_plan();
    match parent.has_error_rows() {
        true => lf.with_columns(
            keys.iter()
                .map(|k| parent.err_key(col(k.as_str())).alias(k.as_str()))
                .collect::<Vec<_>>(),
        ),
        false => lf,
    }
}

/// [`Sheet::join_from`]'s rows. `rids`: plus each side's row id, [`LRID`]
/// and [`RRID`] (S-45 B).
fn join_plan(
    left: &Sheet,
    right_sheet: &Sheet,
    keys: &[String],
    how: &str,
    rids: bool,
) -> Result<LazyFrame, String> {
    let side = |s: &Sheet, rid: &str| match rids {
        true => s.view_plan_rid(true).rename([ROW_ID], [rid], true),
        false => s.view_plan(),
    };
    let l = side(left, LRID);
    let (right, right_name) = (side(right_sheet, RRID), right_sheet.name.as_str());
    let tag = |p: LazyFrame, name: &str| p.with_column(lit(name.to_string()).alias("origin_sheet"));
    let plan = if how == "append" {
        let args = UnionArgs {
            to_supertypes: true,
            diagonal: true,
            ..Default::default()
        };
        concat([tag(l, &left.name), tag(right, right_name)], args).map_err(|e| e.to_string())?
    } else if how == "concat" {
        let names = |p: &LazyFrame| -> Result<Vec<String>, String> {
            let schema = p.clone().collect_schema().map_err(|e| e.to_string())?;
            Ok(schema.iter_names().map(|n| n.to_string()).collect())
        };
        let (ln, rn) = (names(&l)?, names(&right)?);
        if ln.len() != rn.len() {
            return Err(format!(
                "concat: {} has {} columns, {right_name} has {}",
                left.name,
                ln.len(),
                rn.len()
            ));
        }
        let renamed = right.select(
            rn.iter()
                .zip(&ln)
                .map(|(r, l)| col(r.as_str()).alias(l.as_str()))
                .collect::<Vec<_>>(),
        );
        let args = UnionArgs {
            to_supertypes: true,
            ..Default::default()
        };
        concat([tag(l, &left.name), tag(renamed, right_name)], args).map_err(|e| e.to_string())?
    } else {
        let jt = match how {
            "inner" => JoinType::Inner,
            "left" | "extend" => JoinType::Left,
            "full" | "merge" => JoinType::Full,
            other => return Err(format!("unknown join type: {other}")),
        };
        let on: Vec<Expr> = keys.iter().map(|k| col(k.as_str())).collect();
        let mut args = JoinArgs::new(jt).with_coalesce(JoinCoalesce::CoalesceColumns);
        args.maintain_order = MaintainOrderJoin::LeftRight;
        // VisiData reads an empty cell as "", and "" matches "": null keys join
        args.nulls_equal = true;
        // VisiData's order: keys as first seen (left rows, then right-only
        // keys), each key's rows together, left row then right row. Pivots
        // of the join then order their columns the same way every run.
        let numbered = |p: LazyFrame, row: &str, first: &str| -> Result<LazyFrame, String> {
            let min = col(row).cast(DataType::Int64).min();
            let min = min.over(on.clone()).map_err(|e| e.to_string())?;
            Ok(p.with_row_index(row, None).with_column(min.alias(first)))
        };
        let right = if how == "extend" {
            let names: Vec<String> = right
                .clone()
                .collect_schema()
                .map_err(|e| e.to_string())?
                .iter_names()
                .filter(|n| !keys.iter().any(|k| k == n.as_str()) && n.as_str() != RRID)
                .map(|n| n.to_string())
                .collect();
            let to: Vec<String> = names
                .iter()
                .map(|n| format!("{}_{n}", extend_prefix(right_sheet)))
                .collect();
            right.rename(names, to, true)
        } else {
            right
        };
        // a non-key column on both sides: `<sheet>_<column>` on each, as VisiData
        let both = join_collisions(left, right_sheet, keys, how)?;
        let (l, right) = match both.is_empty() {
            true => (l, right),
            false => {
                let named = |p: &Sheet| -> Vec<String> {
                    both.iter()
                        .map(|c| format!("{}_{c}", extend_prefix(p)))
                        .collect()
                };
                (
                    l.rename(&both, named(left), true),
                    right.rename(&both, named(right_sheet), true),
                )
            }
        };
        let l = numbered(l, JL, JLK)?;
        let right = numbered(right, JR, JRK)?;
        let mut joined = l
            .join(right, on.clone(), on, args)
            .sort_by_exprs(
                [
                    col(JLK).fill_null(col(JRK) + lit(JOIN_RIGHT_KEYS)),
                    col(JL),
                    col(JR),
                ],
                SortMultipleOptions::default()
                    .with_nulls_last(true)
                    .with_maintain_order(true),
            )
            .drop(cols([JL, JLK, JR, JRK]));
        if how == "merge" {
            // Polars suffixes colliding right columns `_right`; fold them in.
            let names: Vec<String> = joined
                .collect_schema()
                .map_err(|e| e.to_string())?
                .iter_names()
                .map(|n| n.to_string())
                .collect();
            let dups: Vec<&str> = names
                .iter()
                .filter_map(|n| n.strip_suffix("_right"))
                .filter(|base| names.iter().any(|n| n == base))
                .collect();
            let merged: Vec<Expr> = dups
                .iter()
                .map(|b| {
                    let r = format!("{b}_right");
                    col(r.as_str()).fill_null(col(*b)).alias(*b)
                })
                .collect();
            let drop: Vec<String> = dups.iter().map(|b| format!("{b}_right")).collect();
            joined = joined.with_columns(merged).drop(cols(drop));
        }
        joined
    };
    Ok(plan)
}

/// A join row's left and right source row ids (S-45 B).
pub(crate) const LRID: &str = "__vdrs_lrid";
pub(crate) const RRID: &str = "__vdrs_rrid";

/// What [`Sheet::join_sources`] finds.
#[derive(Debug, Clone)]
pub struct JoinSources {
    pub left: SheetId,
    pub right: SheetId,
    /// `(shown in the join, name in the source)`
    pub left_cols: Vec<(String, String)>,
    pub right_cols: Vec<(String, String)>,
    pub rows: Vec<(Option<u64>, Option<u64>)>,
    /// The left side is `g&`'s join of the sheets before (no live sheet).
    pub inner: Option<Arc<JoinSources>>,
}

impl JoinSources {
    /// The source cells behind cell (`rid`, `column`), as `(sheet, row id,
    /// column)`: a key's on both sides, another column's on the side that
    /// has it, through `g&`'s inner joins.
    pub fn cells(&self, rid: u64, column: &str) -> Vec<(SheetId, u64, String)> {
        let Some(&(l, r)) = self.rows.get(rid as usize) else {
            return Vec::new();
        };
        let name = |cols: &[(String, String)]| {
            let c = cols.iter().find(|(shown, _)| shown == column)?;
            Some(c.1.clone())
        };
        let left = match (l, name(&self.left_cols)) {
            (Some(l), Some(n)) => match &self.inner {
                Some(inner) => inner.cells(l, &n),
                None => vec![(self.left, l, n)],
            },
            _ => Vec::new(),
        };
        let right = r
            .zip(name(&self.right_cols))
            .map(|(r, n)| (self.right, r, n));
        left.into_iter().chain(right).collect()
    }
}

/// `F` and `gF` after the group-by: `percent` and `histogram` of `count`,
/// by count descending unless `keep_order`. Stable on a stable group-by: row
/// order repeats across collects, since row ids (selection, freq dive) are
/// positions in this plan.
fn freq_stats(
    grouped: LazyFrame,
    (count, percent, histogram): (&str, &str, &str),
    keep_order: bool,
) -> LazyFrame {
    let plan = grouped
        .with_columns([(col(count).cast(DataType::Float64) * lit(100.0f64)
            / col(count).cast(DataType::Float64).sum())
        .round(2, RoundMode::HalfAwayFromZero)
        .alias(percent)])
        .with_columns([lit("■".repeat(HISTOGRAM_WIDTH))
            .str()
            .head((col(count) * lit(HISTOGRAM_WIDTH as i64)).floor_div(col(count).max()))
            .alias(histogram)]);
    match keep_order {
        true => plan,
        false => plan.sort(
            [count],
            SortMultipleOptions::default()
                .with_order_descending(true)
                .with_maintain_order(true),
        ),
    }
}

/// Non-key columns both sides of an inner, left or full join show: VisiData
/// names each `<sheet>_<column>` (merge folds them, extend prefixes the right).
fn join_collisions(
    left: &Sheet,
    right: &Sheet,
    keys: &[String],
    how: &str,
) -> Result<Vec<String>, String> {
    if !matches!(how, "inner" | "left" | "full") {
        return Ok(Vec::new());
    }
    let names = |s: &Sheet| -> Result<Vec<String>, String> {
        let schema = s.view_plan().collect_schema().map_err(|e| e.to_string())?;
        Ok(schema.iter_names().map(|n| n.to_string()).collect())
    };
    let r = names(right)?;
    Ok(names(left)?
        .into_iter()
        .filter(|n| !keys.contains(n) && r.contains(n))
        .collect())
}

/// A float's text with `-0.0` read as `0.0`, the way a freq dive matches it.
pub(crate) fn zero_unsigned(text: &str) -> String {
    match text.parse::<f64>() {
        Ok(f) if f == 0.0 && text.starts_with('-') => text[1..].to_string(),
        _ => text.to_string(),
    }
}
