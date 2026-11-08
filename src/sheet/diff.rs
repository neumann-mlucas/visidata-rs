use super::*;

impl Sheet {
    /// Build a schema-diff sheet comparing `parent` with `other`.
    ///
    /// Columns: `column`, `dtype_a`, `dtype_b`, `status` (same | dtype_mismatch | only_a | only_b).
    /// Row diff (full-outer join on keys) is tracked separately — see tasks.md Phase 5.
    pub fn schema_diff_from(
        parent: &Sheet,
        id: SheetId,
        other: &LoadedSource,
    ) -> Result<Self, String> {
        let schema_a = parent
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        let schema_b = other
            .lazy
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?;

        // Column union, preserving A order then B-only
        let mut names: Vec<String> = schema_a.iter_names().map(|n| n.to_string()).collect();
        for n in schema_b.iter_names() {
            if schema_a.get(n.as_str()).is_none() {
                names.push(n.to_string());
            }
        }

        let mut col_names: Vec<String> = Vec::with_capacity(names.len());
        let mut dtypes_a: Vec<String> = Vec::with_capacity(names.len());
        let mut dtypes_b: Vec<String> = Vec::with_capacity(names.len());
        let mut statuses: Vec<String> = Vec::with_capacity(names.len());
        for name in &names {
            let a = schema_a.get(name.as_str()).map(|d| format!("{d}"));
            let b = schema_b.get(name.as_str()).map(|d| format!("{d}"));
            let status = match (&a, &b) {
                (Some(_), None) => "only_a",
                (None, Some(_)) => "only_b",
                (Some(ad), Some(bd)) if ad == bd => "same",
                _ => "dtype_mismatch",
            };
            col_names.push(name.clone());
            dtypes_a.push(a.unwrap_or_default());
            dtypes_b.push(b.unwrap_or_default());
            statuses.push(status.to_string());
        }
        let df = DataFrame::new_infer_height(vec![
            Column::new("column".into(), col_names),
            Column::new("dtype_a".into(), dtypes_a),
            Column::new("dtype_b".into(), dtypes_b),
            Column::new("status".into(), statuses),
        ])
        .map_err(|e| e.to_string())?;
        let plan = df.clone().lazy();
        let other_name = other.display_name.clone();
        let name = format!("schemadiff: {} ↔ {}", parent.name, file_name(&other_name));
        let origin = Origin::Diff {
            left: parent.id,
            right_name: other_name,
        };
        Ok(Sheet::child(parent, id, name, plan, origin).with_window(df))
    }

    /// Row-diff sheet. Full outer join on `keys`; a `__diff` column tags each row.
    /// Rows marked "same" are filtered out — only mismatches surface.
    pub fn row_diff_from(
        parent: &Sheet,
        id: SheetId,
        other: &Sheet,
        keys: &[String],
    ) -> Result<Self, String> {
        if keys.is_empty() {
            return Err("row_diff needs at least one key column".into());
        }
        let key_exprs: Vec<Expr> = keys.iter().map(|k| col(k.as_str())).collect();
        let a = parent.view_plan().with_column(lit(true).alias(DIFF_LEFT));
        let b = other.view_plan().with_column(lit(true).alias(DIFF_RIGHT));
        let joined = a.join(
            b,
            key_exprs.clone(),
            key_exprs,
            JoinArgs::new(JoinType::Full).with_coalesce(JoinCoalesce::CoalesceColumns),
        );
        let schema_a = parent
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        let schema_b = other
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?;

        let only_a = col(DIFF_RIGHT).is_null();
        let only_b = col(DIFF_LEFT).is_null();
        let diff_col = when(only_a.clone())
            .then(lit("only_a"))
            .when(only_b.clone())
            .then(lit("only_b"))
            .otherwise(lit("changed"))
            .alias("__diff");

        // A shared column differs, null vs value included.
        let shared: Vec<String> = schema_a
            .iter_names()
            .filter(|n| schema_b.get(n.as_str()).is_some() && !keys.iter().any(|k| k == n.as_str()))
            .map(|n| n.to_string())
            .collect();
        let mismatch_expr = shared
            .iter()
            .map(|c| col(c.as_str()).neq_missing(col(format!("{c}_right").as_str())))
            .reduce(|a, b| a.or(b))
            .unwrap_or(lit(false));

        // Final: only_a OR only_b OR mismatch
        let plan = joined
            .with_column(diff_col)
            .filter(only_a.or(only_b).or(mismatch_expr))
            .drop(cols([DIFF_LEFT, DIFF_RIGHT]));

        let other_name = other.name.clone();
        let how = Derivation::Diff {
            other: Box::new(other.clone()),
            keys: keys.to_vec(),
        };
        Ok(Sheet::child(
            parent,
            id,
            format!(
                "rowdiff({}): {} ↔ {}",
                keys.join(","),
                parent.name,
                file_name(&other_name)
            ),
            plan,
            Origin::Diff {
                left: parent.id,
                right_name: other_name,
            },
        )
        .derived(parent, how))
    }
}
