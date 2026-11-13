use super::*;

impl Sheet {
    /// Build a frequency-table sheet grouping by `col_name`, values in
    /// first-seen order, then by count, desc, ties keeping that order (as
    /// VisiData's stable sort); numeric and date keys stay in first-seen
    /// order (`freq_keeps_order`).
    /// `percent` (count / total * 100, 2 decimals) and `histogram` (a `■` bar)
    /// follow `count`, as in VisiData's freq output.
    pub fn freq_from(parent: &Sheet, id: SheetId, col_name: &str) -> Self {
        let (count, percent, histogram) = freq_stat_names(col_name);
        let keep_order = freq_keeps_order(parent, col_name);
        let grouped = parent.view_plan().group_by_stable([col(col_name)]);
        let aggs = columns::freq_aggregators(parent);
        let exprs = std::iter::once(len().alias(count));
        let exprs = exprs.chain(aggs.iter().map(|(c, a)| columns::agg_expr(c, a)));
        let plan = grouped
            .agg(exprs.collect::<Vec<_>>())
            .with_columns([(col(count).cast(DataType::Float64) * lit(100.0f64)
                / col(count).cast(DataType::Float64).sum())
            .round(2, RoundMode::HalfAwayFromZero)
            .alias(percent)])
            .with_columns([lit("■".repeat(HISTOGRAM_WIDTH))
                .str()
                .head((col(count) * lit(HISTOGRAM_WIDTH as i64)).floor_div(col(count).max()))
                .alias(histogram)]);
        // Stable on a stable group-by: row order repeats across collects,
        // since row ids (selection, freq dive) are positions in this plan.
        let plan = if keep_order {
            plan
        } else {
            plan.sort(
                [count],
                SortMultipleOptions::default()
                    .with_order_descending(true)
                    .with_maintain_order(true),
            )
        };
        let mut freq = Sheet::child(
            parent,
            id,
            format!("freq({}): {}", col_name, parent.name),
            plan,
            Origin::Freq {
                parent: parent.id,
                column: col_name.to_string(),
            },
        )
        .derived(
            parent,
            Derivation::Freq {
                column: col_name.to_string(),
            },
        );
        // VisiData hides percent and histogram once aggregators add columns.
        if !aggs.is_empty() {
            freq.view
                .hidden
                .extend([percent.to_string(), histogram.to_string()]);
        }
        freq
    }

    /// Build a pivot sheet. Eager: collects parent, then pivots.
    /// Direction §8: Polars 2.x may spill on group-bys but pivots stay in-memory.
    /// ponytail: eager pivot, document OOM risk, swap to streaming when a user complains.
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
            "count" | "len" => len(),
            "max" => e.max(),
            "min" => e.min(),
            "first" => e.first(),
            "last" => e.last(),
            other => return Err(format!("unknown agg: {other}")),
        };
        let df = parent.view_plan().collect().map_err(|e| e.to_string())?;
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

    /// Freq dive: `parent`'s rows whose `column` equals one of `values`
    /// (compared as text, the way the freq sheet grouped them; null matches null).
    pub fn rows_with_values(
        parent: &Sheet,
        id: SheetId,
        column: &str,
        values: &[AnyValue],
    ) -> Self {
        let pred = values_predicate(column, values);
        let label = match values {
            [one] => format!("{column}={}", any_text(one)),
            many => format!("{column} in {} values", many.len()),
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
        let text = values
            .iter()
            .map(|v| (!v.is_null()).then(|| any_text(v)))
            .collect();
        Self::child(
            parent,
            id,
            format!("{label}: {}", parent.name),
            parent.view_plan().filter(pred),
            origin,
        )
        .inherit_attrs(parent)
        .derived(
            parent,
            Derivation::Values {
                column: column.into(),
                values: text,
            },
        )
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
                Some(e) => e.clone().sum().cast(DataType::UInt64),
                None => lit(0u64).cast(DataType::UInt64),
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
                (c().null_count().cast(DataType::UInt64) - errors).alias(stat(i, "nulls")),
                valid()
                    .n_unique()
                    .cast(DataType::UInt64)
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

    /// `W`: count pivot of the current column `on`, rows keyed by `keys`.
    /// No keys → one row of counts, one column per distinct value (first-seen order).
    pub fn pivot_current_from(
        parent: &Sheet,
        id: SheetId,
        keys: &[String],
        on: &str,
    ) -> Result<Self, String> {
        let name = format!("pivot({on}): {}", parent.name);
        let origin = Origin::Pivot { parent: parent.id };
        // The cursor may sit on a key column; a null and the text "null" are
        // one column header, so they are one group.
        // Hidden key columns are out of the view too.
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
        let value_text = col(on).cast(DataType::String).fill_null(lit("null"));
        let counted = parent
            .view_plan()
            .with_column(value_text)
            .group_by_stable(
                keys.iter()
                    .chain([&on.to_string()])
                    .map(|k| col(k.as_str()))
                    .collect::<Vec<_>>(),
            )
            .agg([len().alias("__vdrs_n")])
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
            pivot_df(&counted, on, keys, "__vdrs_n", element().first())
                .map_err(|e| e.to_string())?
        };
        let how = Derivation::PivotCount {
            keys: keys.to_vec(),
            on: on.to_string(),
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
        let l = left.view_plan();
        let (right, right_name) = (right_sheet.view_plan(), right_sheet.name.as_str());
        let tag =
            |p: LazyFrame, name: &str| p.with_column(lit(name.to_string()).alias("origin_sheet"));
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
            concat([tag(l, &left.name), tag(renamed, right_name)], args)
                .map_err(|e| e.to_string())?
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
                    .filter(|n| !keys.iter().any(|k| k == n.as_str()))
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

    /// Build an unpivot (melt) sheet. `on` = columns to unpivot; `index` = keep columns.
    pub fn unpivot_from(parent: &Sheet, id: SheetId, on: &[String], index: &[String]) -> Self {
        use polars::prelude::UnpivotArgsDSL;
        let args = UnpivotArgsDSL {
            on: (!on.is_empty()).then(|| cols(on.iter().map(String::as_str))),
            index: cols(index.iter().map(String::as_str)),
            variable_name: None,
            value_name: None,
        };
        let plan = parent.view_plan().unpivot(args);
        Sheet::child(
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
        )
    }
}

/// `join_from` order helpers (also written by `:export`).
pub(crate) const JL: &str = "__vdrs_l";
pub(crate) const JLK: &str = "__vdrs_lk";
pub(crate) const JR: &str = "__vdrs_r";
pub(crate) const JRK: &str = "__vdrs_rk";
/// Right-only keys sort after every left key.
pub(crate) const JOIN_RIGHT_KEYS: i64 = 1 << 40;
