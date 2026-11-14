//! Columns sheet (S-25 P3): `C` lists a sheet's columns, one row each, as
//! VisiData's; edits there write back to the sheet (`App`, `columns.rs`).

use super::*;

/// Column type as the columns sheet names it (VisiData's words).
pub fn type_name(dt: &DataType) -> String {
    match dt {
        d if d.is_integer() => "int".into(),
        d if d.is_float() => "float".into(),
        DataType::String => "str".into(),
        DataType::Boolean => "bool".into(),
        DataType::Date => "date".into(),
        DataType::Datetime(..) => "datetime".into(),
        DataType::List(_) => "list".into(),
        DataType::Struct(_) => "struct".into(),
        d => d.to_string(),
    }
}

/// Aggregators `+` offers (S-25 P4), VisiData's names.
pub const AGGREGATORS: &[&str] = &[
    "count", "distinct", "sum", "mean", "avg", "median", "mode", "min", "max", "stdev",
];

/// Aggregators that are 0, not null, over no rows (VisiData's on an
/// empty list).
pub fn zero_on_empty(agg: &str) -> bool {
    matches!(agg, "count" | "distinct" | "sum")
}

/// Aggregators that need numbers.
const NUMERIC: &[&str] = &["sum", "mean", "avg", "median", "stdev"];

/// `agg` of `column`'s non-null values, named `{column}_{agg}` (VisiData's
/// `aggname`). `agg` is one of [`AGGREGATORS`].
pub fn agg_expr(column: &str, agg: &str) -> Expr {
    let c = col(column);
    let e = match agg {
        "count" => c.count().cast(DataType::Int64),
        "distinct" => c.drop_nulls().n_unique().cast(DataType::Int64),
        "sum" => c.sum(),
        "mean" | "avg" => c.mean(),
        "median" => c.median(),
        // ties: the first seen, as VisiData's `statistics.mode`
        "mode" => c.drop_nulls().mode(true).first(),
        "min" => c.min(),
        "max" => c.max(),
        _ => c.std(1),
    };
    e.alias(format!("{column}_{agg}"))
}

/// The aggregators `F` on `parent` adds: (column, aggregator), in the
/// order of `parent`'s visible columns (VisiData's).
pub fn freq_aggregators(parent: &Sheet) -> Vec<(String, String)> {
    if parent.view.aggregators.is_empty() {
        return Vec::new();
    }
    let Ok(schema) = parent.view_plan().collect_schema() else {
        return Vec::new();
    };
    schema
        .iter_names()
        .flat_map(|n| {
            parent
                .view
                .aggregators
                .iter()
                .filter(move |(c, _)| c == n.as_str())
        })
        .cloned()
        .collect()
}

impl Sheet {
    /// The columns sheet's `aggregators` cell: space-separated names for
    /// `column`, replacing its list. Unchanged on error.
    pub fn set_aggregators(&mut self, column: &str, names: &str) -> Result<(), String> {
        let names = self.aggregator_names(column, names)?;
        self.view.aggregators.retain(|(c, _)| c != column);
        let new = names.into_iter().map(|n| (column.to_string(), n));
        self.view.aggregators.extend(new);
        Ok(())
    }

    /// `+`: add the space-separated `names` to `column`'s aggregators
    /// (ones it has stay put). Unchanged on error.
    pub fn add_aggregators(&mut self, column: &str, names: &str) -> Result<(), String> {
        for n in self.aggregator_names(column, names)? {
            let a = (column.to_string(), n);
            if !self.view.aggregators.contains(&a) {
                self.view.aggregators.push(a);
            }
        }
        Ok(())
    }

    /// `names` split and checked against [`AGGREGATORS`] and `column`'s type.
    fn aggregator_names(&self, column: &str, names: &str) -> Result<Vec<String>, String> {
        let dt = self
            .all_columns()
            .into_iter()
            .find(|(n, _)| n == column)
            .map(|(_, d)| d)
            .ok_or_else(|| format!("no column {column:?}"))?;
        names
            .split_whitespace()
            .map(|n| match n {
                n if !AGGREGATORS.contains(&n) => {
                    Err(format!("no aggregator {n:?}: {}", AGGREGATORS.join(" ")))
                }
                n if NUMERIC.contains(&n) && !dt.is_primitive_numeric() => {
                    Err(format!("{n}: {column} isn't numeric ({})", type_name(&dt)))
                }
                n => Ok(n.to_string()),
            })
            .collect()
    }

    /// `z+`: `column_agg=value` for each of `names` over the selected rows
    /// (all rows when none are), as VisiData's status line.
    pub fn memo_aggregate(&self, column: &str, names: &str) -> Result<String, String> {
        let names = self.aggregator_names(column, names)?;
        if names.is_empty() {
            return Err("no aggregator given".into());
        }
        let mut lf = self.view_plan_rid(true);
        if !self.view.selected.is_empty() {
            let sel: Vec<u64> = self.view.selected.iter().copied().collect();
            lf = lf.filter(rid_in(&sel));
        }
        let exprs: Vec<Expr> = names.iter().map(|n| agg_expr(column, n)).collect();
        let df = lf.select(exprs).collect().map_err(|e| e.to_string())?;
        let parts: Vec<String> = df
            .columns()
            .iter()
            .map(|c| {
                let v = c.get(0).map(|v| any_text(&v)).unwrap_or_default();
                format!("{}={v}", c.name())
            })
            .collect();
        Ok(parts.join("  "))
    }

    /// `ty` (a columns-sheet type: `int float str date currency json`, or
    /// empty for the loaded type) on `column`. Unchanged on error.
    pub fn cast_column(&mut self, column: &str, ty: &str) -> Result<(), String> {
        let dt = match ty.trim() {
            "" => {
                self.view.type_casts.retain(|(c, _)| c != column);
                return Ok(());
            }
            "int" => DataType::Int64,
            "float" => DataType::Float64,
            "str" | "string" => DataType::String,
            "date" => crate::sheet::date_cast_type(self, column),
            "currency" => {
                let step = PlanStep::Currency {
                    column: column.into(),
                };
                return self.apply_step(step).map_err(|e| format!("currency: {e}"));
            }
            "json" => return self.json_decode(column),
            other => {
                return Err(format!(
                    "type {other:?}: int, float, str, date, currency or json"
                ));
            }
        };
        self.set_type_cast(column.into(), dt);
        Ok(())
    }

    /// Every column, hidden ones too, in the order the sheet shows them:
    /// keys first, then the plan's order.
    pub fn all_columns(&self) -> Vec<(String, DataType)> {
        let Ok(schema) = self.sorted_plan(false).collect_schema() else {
            return Vec::new();
        };
        let keys = self
            .view
            .keys
            .iter()
            .filter_map(|k| schema.get(k).map(|d| (k, d)));
        let rest = schema
            .iter()
            .filter(|(n, _)| !self.view.keys.iter().any(|k| k == n.as_str()));
        keys.map(|(n, d)| (n.clone(), d.clone()))
            .chain(rest.map(|(n, d)| (n.to_string(), d.clone())))
            .collect()
    }

    /// `C`: a row per column of `of` — `name width type fmtstr value
    /// aggregators sortorder key` (VisiData's; `key` added; `fmtstr` is a
    /// float column's precision as `%.Nf`, S-32). `width` is
    /// empty for a hidden column; `value` is the cursor row's.
    pub fn columns_of(of: &Sheet, id: SheetId) -> Self {
        Self::columns_sheet(&[of], id, format!("{}_columns", of.name))
    }

    /// `gC`: the columns of every sheet in `sheets`, a `sheet` column first.
    pub fn columns_all(sheets: &[&Sheet], id: SheetId) -> Self {
        Self::columns_sheet(sheets, id, "all_columns".into())
    }

    #[expect(clippy::expect_used, reason = "columns built here have one length")]
    fn columns_sheet(sheets: &[&Sheet], id: SheetId, name: String) -> Self {
        let mut cols = Vec::new();
        let (mut sheet, mut names, mut widths, mut types, mut values) =
            (vec![], vec![], vec![], vec![], vec![]);
        let mut fmts: Vec<Option<String>> = vec![];
        let (mut aggs, mut order, mut keys) = (vec![], vec![], vec![]);
        for s in sheets {
            let shown: Vec<String> = s
                .window()
                .map(|df| {
                    df.get_column_names()
                        .iter()
                        .map(|n| n.to_string())
                        .collect()
                })
                .unwrap_or_default();
            let v = &s.view;
            for (n, dt) in s.all_columns() {
                let at = shown.iter().position(|c| *c == n);
                sheet.push(s.name.clone());
                widths.push(match v.hidden.contains(&n) {
                    true => None,
                    false => {
                        Some(at.and_then(|i| s.state.widths.get(i)).copied().unwrap_or(0) as i64)
                    }
                });
                types.push(type_name(&dt));
                fmts.push(
                    v.precision
                        .iter()
                        .rev()
                        .find(|(c, _)| *c == n)
                        .map(|(_, d)| format!("%.{d}f"))
                        .or_else(|| {
                            let f = v.date_fmts.iter().rev().find(|(c, _)| *c == n);
                            f.map(|(_, f)| f.clone())
                        }),
                );
                values.push(at.and_then(|i| {
                    let c = &s.window()?.columns()[i];
                    let av = c.get(v.row_cursor).ok()?;
                    (!av.is_null()).then(|| any_text(&av))
                }));
                let a: Vec<&str> = v
                    .aggregators
                    .iter()
                    .filter(|(c, _)| *c == n)
                    .map(|(_, a)| a.as_str())
                    .collect();
                aggs.push(a.join(" "));
                order.push(
                    v.sort
                        .iter()
                        .zip(1i64..)
                        .find(|(k, _)| k.column == n)
                        .map(|(k, i)| match k.order {
                            SortOrder::Desc => -i,
                            _ => i,
                        }),
                );
                keys.push(v.keys.contains(&n));
                names.push(n.clone());
                cols.push((s.id, n));
            }
        }
        let mut columns = vec![
            Column::new("name".into(), names),
            Column::new("width".into(), widths),
            Column::new("type".into(), types),
            Column::new("fmtstr".into(), fmts),
            Column::new("value".into(), values),
            Column::new("aggregators".into(), aggs),
            Column::new("sortorder".into(), order),
            Column::new("key".into(), keys),
        ];
        if sheets.len() > 1 {
            columns.insert(0, Column::new("sheet".into(), sheet));
        }
        let df = DataFrame::new_infer_height(columns).expect("columns dataframe");
        // on the row of the column the sheet's cursor is on
        let row_cursor = match sheets {
            [one] => one
                .window()
                .and_then(|df| {
                    df.get_column_names()
                        .get(one.view.col_cursor)
                        .map(|n| n.to_string())
                })
                .and_then(|n| cols.iter().position(|(_, c)| *c == n))
                .unwrap_or(0),
            _ => 0,
        };
        let mut sheet = Sheet::meta(id, name, df, Origin::Columns { cols: cols.into() });
        sheet.view.row_cursor = row_cursor;
        sheet
    }
}
