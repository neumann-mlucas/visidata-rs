//! Python (Polars) pieces of `:export`: the view chain and expression helpers.

use super::*;

impl Script {
    pub(super) fn py_view(&mut self, s: &Sheet, rid: bool, errs: bool) -> Result<String, String> {
        let base = self.base(s)?;
        let v = &s.view;
        let mut ops = Vec::new();
        let deleted = s.deleted_rids();
        let carried = carries_rid(s);
        if rid && !carried {
            ops.push(".with_row_index(\"__rid\")".to_string());
        }
        if !deleted.is_empty() {
            ops.push(format!(".filter(~pl.col(\"__rid\").is_in({deleted:?}))"));
        }
        if carried && !rid {
            ops.push(".drop(\"__rid\")".into());
        }
        for (c, dt) in &v.type_casts {
            let e = py_str(&err_col(c));
            if errs {
                ops.push(format!(
                    ".with_columns(pl.col({}).is_not_null().alias({e}))",
                    py_str(c)
                ));
            }
            let text = src_type(s, c) == Some(DataType::String);
            if *dt == DataType::Date && text {
                self.need(&py_lenient_date());
                ops.push(format!(".with_columns(lenient_date({}))", py_str(c)));
            } else if matches!(dt, DataType::Datetime(..)) && text {
                self.need(&py_lenient_date());
                self.need(&py_lenient_datetime());
                ops.push(format!(".with_columns(lenient_datetime({}))", py_str(c)));
            } else {
                ops.push(format!(
                    ".with_columns(pl.col({c}).cast({}, strict=False))",
                    py_dtype(dt),
                    c = py_str(c)
                ));
            }
            if errs {
                ops.push(format!(
                    ".with_columns((pl.col({e}) & pl.col({}).is_null()).alias({e}))",
                    py_str(c)
                ));
            }
        }
        if let Some(f) = &v.filter {
            ops.push(format!(
                ".filter(pl.col({}).cast(pl.Utf8).str.contains({}))",
                py_str(&f.column),
                py_str(&f.regex)
            ));
        }
        if let Some(p) = &v.filter_expr {
            ops.push(format!(
                ".filter(pl.sql_expr({}).fill_null(False))",
                py_str(p)
            ));
        }
        if !v.sort.is_empty() {
            let by: Vec<String> = v.sort.iter().map(|k| py_str(&k.column)).collect();
            let desc: Vec<&str> = v
                .sort
                .iter()
                .map(|k| {
                    if k.order == SortOrder::Desc {
                        "True"
                    } else {
                        "False"
                    }
                })
                .collect();
            ops.push(format!(
                ".sort([{}], descending=[{d}], nulls_last=[{d}], maintain_order=True)",
                by.join(", "),
                d = desc.join(", ")
            ));
        }
        if let Some(mut cols) = visible_columns(s)? {
            if rid {
                cols.push("__rid".into());
            }
            if errs {
                cols.extend(v.type_casts.iter().map(|(c, _)| err_col(c)));
            }
            let cols: Vec<String> = cols.iter().map(|c| py_str(c)).collect();
            ops.push(format!(".select([{}])", cols.join(", ")));
        }
        Ok(if ops.is_empty() {
            base
        } else {
            self.bind(format!("{base}{}", ops.join("")))
        })
    }
}

/// Polars 2 casts no text to bool/date/datetime: parse those.
pub(super) fn py_from_text(e: &str, dtype: &DataType) -> String {
    match dtype {
        DataType::Boolean => format!("({e} == \"true\")"),
        DataType::Date => format!("{e}.str.to_date()"),
        DataType::Datetime(..) => format!("{e}.str.to_datetime().cast({})", py_dtype(dtype)),
        _ => format!("{e}.cast({})", py_dtype(dtype)),
    }
}

pub(super) fn py_step(t: &str, step: &PlanStep) -> String {
    match step {
        PlanStep::Replace {
            column,
            pattern,
            replacement,
            all,
            rids,
        } => {
            let f = if *all { "replace_all" } else { "replace" };
            let v = format!(
                "pl.col({c}).str.{f}({}, {})",
                py_str(pattern),
                py_str(replacement),
                c = py_str(column)
            );
            let v = match rids {
                Some(r) => format!(
                    "pl.when(pl.col(\"__rid\").is_in({:?})).then({v}).otherwise(pl.col({c}))",
                    &r[..],
                    c = py_str(column)
                ),
                None => v,
            };
            format!("{t} = {t}.with_columns({v}.alias({}))", py_str(column))
        }
        PlanStep::Capture {
            column,
            pattern,
            name,
        } => {
            let text = format!("pl.col({}).cast(pl.Utf8)", py_str(column));
            let p = py_str(pattern);
            let fields: Vec<String> = crate::sheet::capture_fields(pattern)
                .unwrap_or_default()
                .iter()
                .enumerate()
                .map(|(i, f)| format!("{text}.str.extract({p}, {}).alias({})", i + 1, py_str(f)))
                .collect();
            format!(
                "{t} = {t}.with_columns(pl.when({text}.str.contains({p})).then(pl.struct([{}])).otherwise(None).alias({}))",
                fields.join(", "),
                py_str(name)
            )
        }
        PlanStep::DeleteRows { .. } | PlanStep::AddRows { .. } => String::new(), // see `steps`
        PlanStep::Json { column, dtype } => format!(
            "{t} = {t}.with_columns(pl.col({c}).str.json_decode({}))",
            py_json_dtype(dtype),
            c = py_str(column)
        ),
        PlanStep::Expand {
            column,
            fields,
            after,
        } => {
            let kids = fields.iter().map(|(path, name)| {
                let get: String = path
                    .iter()
                    .map(|f| match crate::sheet::list_index(f) {
                        Some(i) => format!(".list.get({i}, null_on_oob=True)"),
                        None => format!(".struct.field({})", py_str(f)),
                    })
                    .collect();
                format!("pl.col({}){get}.alias({})", py_str(column), py_str(name))
            });
            let after_: Vec<String> = after.iter().map(|c| py_str(c)).collect();
            let cols: Vec<String> =
                std::iter::once(format!("pl.all().exclude([{}])", after_.join(", ")))
                    .chain(kids)
                    .chain(after_.iter().map(|c| format!("pl.col({c})")))
                    .collect();
            format!("{t} = {t}.select([{}])", cols.join(", "))
        }
        PlanStep::Rename { old, new } => {
            format!("{t} = {t}.rename({{{}: {}}})", py_str(old), py_str(new))
        }
        PlanStep::Currency { column } => format!(
            "{t} = {t}.with_columns(pl.col({c}).cast(pl.Utf8).str.replace_all(r\"[^0-9.\\-]\", \"\").cast(pl.Float64, strict=False))",
            c = py_str(column)
        ),
        PlanStep::Derive { name, expr, .. } => format!(
            "{t} = {t}.with_columns(pl.sql_expr({}).alias({}))",
            py_str(expr),
            py_str(name)
        ),
        PlanStep::SetCol {
            column,
            value,
            rids,
            dtype,
        } => format!(
            "{t} = {t}.with_columns(pl.when(pl.col(\"__rid\").is_in({rids:?}))\
             .then({v}).otherwise(pl.col({c})).alias({c}))",
            // Polars 2 casts no text to bool/date/datetime: parse those.
            v = match (value.as_deref(), dtype) {
                (Some("true"), DataType::Boolean) => "pl.lit(True)".into(),
                (Some(_), DataType::Boolean) => "pl.lit(False)".into(),
                (Some(v), DataType::Date) => format!("pl.lit({}).str.to_date()", py_str(v)),
                (Some(v), DataType::Datetime(..)) => format!(
                    "pl.lit({}).str.to_datetime().cast({t}.collect_schema()[{c}])",
                    py_str(v),
                    c = py_str(column)
                ),
                (v, _) => format!(
                    "pl.lit({}).cast({t}.collect_schema()[{c}])",
                    v.map_or("None".into(), py_str),
                    c = py_str(column)
                ),
            },
            c = py_str(column)
        ),
        PlanStep::SetExpr {
            column, expr, rids, ..
        } => format!(
            "{t} = {t}.with_columns(pl.when(pl.col(\"__rid\").is_in({rids:?}))\
             .then(pl.sql_expr({}).cast({t}.collect_schema()[{c}], strict=False)).otherwise(pl.col({c})).alias({c}))",
            py_str(expr),
            c = py_str(column)
        ),
        PlanStep::Lookup {
            name, key, table, ..
        } => {
            let (keys, vals, dtype) = lookup_lists(table);
            let keys: Vec<String> = keys.iter().map(|k| py_str(k)).collect();
            let vals: Vec<String> = vals
                .iter()
                .map(|v| v.as_deref().map_or("None".into(), py_str))
                .collect();
            let v = format!(
                "pl.col({}).cast(pl.Utf8).replace_strict([{}], [{}], default=None, return_dtype=pl.Utf8)",
                py_str(key),
                keys.join(", "),
                vals.join(", ")
            );
            format!(
                "{t} = {t}.with_columns({}.alias({}))",
                py_from_text(&v, &dtype),
                py_str(name)
            )
        }
        PlanStep::SetCells { column, patch } => {
            let (rids, vals) = patch_lists(patch);
            let vals: Vec<String> = vals
                .iter()
                .map(|v| v.as_deref().map_or("None".into(), py_str))
                .collect();
            let c = py_str(column);
            // Polars 2 casts no text to bool/date/datetime: parse those.
            let v = match patch.column("__vdrs_v").map(|c| c.dtype().clone()) {
                Ok(DataType::Boolean) => "(pl.col(\"__v\") == \"true\")".to_string(),
                Ok(DataType::Date) => "pl.col(\"__v\").str.to_date()".into(),
                Ok(DataType::Datetime(..)) => {
                    format!("pl.col(\"__v\").str.to_datetime().cast({t}.collect_schema()[{c}])")
                }
                _ => format!("pl.col(\"__v\").cast({t}.collect_schema()[{c}])"),
            };
            format!(
                "{t} = {t}.join(pl.DataFrame({{\"__rid\": {rids:?}, \"__v\": [{}]}}, schema={{\"__rid\": pl.UInt64, \"__v\": pl.Utf8}})\
                 .lazy().with_columns(pl.lit(True).alias(\"__hit\")), on=\"__rid\", how=\"left\", maintain_order=\"left\")\
                 .with_columns(pl.when(pl.col(\"__hit\").is_not_null()).then({v}).otherwise(pl.col({c})).alias({c}))\
                 .drop(\"__v\", \"__hit\")",
                vals.join(", ")
            )
        }
    }
}

pub(super) fn py_pivot_aggs(
    p: &str,
    keys: &[String],
    on: &str,
    aggs: &[(String, String)],
) -> String {
    let list = |v: Vec<String>| format!("[{}]", v.join(", "));
    let o = py_str(on);
    let (idx, add_k) = match keys {
        [] => (
            vec![py_str("__k")],
            ".with_columns(pl.lit(0).alias(\"__k\"))",
        ),
        k => (k.iter().map(|k| py_str(k)).collect(), ""),
    };
    let by = list(idx.iter().cloned().chain([o.clone()]).collect());
    let ks = list(keys.iter().map(|k| py_str(k)).collect());
    let idx = list(idx);
    let agg_exprs = list(aggs.iter().map(|(c, a)| py_agg(c, a)).collect());
    let alias = list(aggs.iter().map(|(c, a)| py_str(&agg_name(c, a))).collect());
    let pre = list(pivot_prefixes(aggs).iter().map(|s| py_str(s)).collect());
    let zero = list(
        aggs.iter()
            .map(|(_, a)| {
                if crate::sheet::columns::zero_on_empty(a) {
                    "True"
                } else {
                    "False"
                }
                .to_string()
            })
            .collect(),
    );
    format!(
        "{p}.with_columns(pl.col({o}).cast(pl.Utf8).fill_null(\"\")){add_k}\
         .group_by({by}, maintain_order=True).agg({agg_exprs}).collect()\
         .pipe(lambda g: (lambda ps: pl.DataFrame([*[ps[0][k] for k in {ks}], \
         *[(ps[i][v].fill_null(0) if {zero}[i] else ps[i][v]).alias({pre}[i] + v) \
         for v in ps[0].columns if v not in {idx} for i in range(len(ps))]]))\
         ([g.pivot(on={o}, index={idx}, values=a, aggregate_function=\"first\") for a in {alias}])).lazy()"
    )
}

/// Base column `c` holds text, so `@` parses it leniently (see `Sheet::view_plan`).
/// [`crate::sheet::columns::agg_expr`] in Polars Python.
pub(super) fn py_agg(c: &str, agg: &str) -> String {
    let m = match agg {
        "count" => "count().cast(pl.Int64)",
        "distinct" => "drop_nulls().n_unique().cast(pl.Int64)",
        "mean" | "avg" => "mean()",
        "mode" => "drop_nulls().mode(maintain_order=True).first()",
        "stdev" => "std()",
        // ponytail: float even on an int column (vdrs truncates to the column's type)
        a if let Some(n) = crate::sheet::columns::percentile(a) => {
            &format!("quantile({}, interpolation=\"linear\")", n as f64 / 100.0)
        }
        _ => &format!("{agg}()"),
    };
    format!(
        "pl.col({}).{m}.alias({})",
        py_str(c),
        py_str(&format!("{c}_{agg}"))
    )
}

/// What the read changed (`reader::CsvRead`): blank lines' all-empty rows
/// left out (S-49), text cells without leading blanks (S-48).
pub(super) fn py_strip(s: &Sheet) -> String {
    let mut out = String::new();
    if s.source.csv.blank_lines {
        out += ".filter(pl.any_horizontal(pl.all().is_not_null()))";
    } else if s.source.csv.trailing_blanks > 0 {
        let k = s.source.csv.trailing_blanks;
        out += &format!(".filter(pl.int_range(pl.len()) < pl.len() - {k})");
    }
    if s.source.csv.stripped {
        out += ".with_columns(pl.col(pl.String).str.strip_chars_start(\" \"))";
    }
    out
}

/// `pl.scan_csv` arguments matching how vdrs read this file.
pub(super) fn py_csv_args(s: &Sheet) -> Result<String, String> {
    let mut args =
        ", infer_schema_length=None, truncate_ragged_lines=True, encoding=\"utf8-lossy\""
            .to_string();
    if s.source.csv.stripped {
        // the names without their blanks
        let names: Vec<String> = csv_columns(s)?.iter_names().map(|n| py_str(n)).collect();
        args += &format!(", new_columns=[{}]", names.join(", "));
    }
    if s.kind == SourceKind::Tsv {
        args += ", separator=\"\\t\"";
    }
    let dates: Vec<String> = csv_dates(s)?
        .iter()
        .map(|(n, d)| match d {
            DataType::String => format!("{}: pl.Utf8", py_str(n)),
            d => format!("{}: pl.{}", py_str(n), py_temporal(d)),
        })
        .collect();
    if !dates.is_empty() {
        args += &format!(", schema_overrides={{{}}}", dates.join(", "));
    }
    Ok(args)
}

pub(super) fn py_temporal(d: &DataType) -> &'static str {
    if *d == DataType::Date {
        "Date"
    } else {
        "Datetime"
    }
}

/// Python twin of `sheet::lenient_date`.
pub(super) fn py_lenient_date() -> String {
    let tries: Vec<String> = DATE_LAYOUTS
        .iter()
        .map(|(layout, shape)| {
            format!(
                "        pl.when(d.str.contains({})).then(d.str.strptime(pl.Date, {}, strict=False)),",
                py_str(shape),
                py_str(layout)
            )
        })
        .collect();
    format!(
        "\ndef lenient_date(c):\n    d = pl.col(c).str.strip_chars().str.replace({}, \"\")\n    return pl.coalesce([\n{}\n    ]).alias(c)",
        py_str(DATE_TIME_PART),
        tries.join("\n")
    )
}

/// Python twin of `sheet::lenient_datetime` (S-38 C).
pub(super) fn py_lenient_datetime() -> String {
    format!(
        "\ndef lenient_datetime(c):\n    t = pl.col(c).str.strip_chars()\n    \
         p = lambda i: t.str.extract({re}, i)\n    \
         n = lambda i: p(i).cast(pl.Int64).fill_null(0)\n    \
         h, ap = n(1), p(4).str.to_lowercase()\n    \
         h = pl.when((ap == \"p\") & (h < 12)).then(h + 12).when((ap == \"a\") & (h == 12)).then(0).otherwise(h)\n    \
         return (lenient_date(c).cast(pl.Datetime(\"us\")) + pl.duration(seconds=h * 3600 + n(2) * 60 + n(3))).alias(c)",
        re = py_str(TIME_OF_DAY)
    )
}

/// Python string literal.
/// A type `:type json` inferred, as Polars Python writes it.
pub(super) fn py_json_dtype(dt: &DataType) -> String {
    match dt {
        DataType::Struct(fs) => {
            let fs: Vec<String> = fs
                .iter()
                .map(|f| format!("{}: {}", py_str(&f.name), py_json_dtype(f.dtype())))
                .collect();
            format!("pl.Struct({{{}}})", fs.join(", "))
        }
        DataType::List(inner) => format!("pl.List({})", py_json_dtype(inner)),
        DataType::Null => "pl.Null".into(),
        DataType::String => "pl.String".into(),
        d => py_dtype(d).into(),
    }
}

pub(super) fn py_str(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out += &format!("\\x{:02x}", c as u32),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// [`Binning::index`] of column `c` (a Python string): null for null and NaN.
pub(super) fn py_bin_index(c: &str, min: f64, width: f64, n: usize) -> String {
    format!(
        "((pl.col({c}).cast(pl.Float64) - {min:?}) / {width:?}).floor().clip(0, {last}).cast(pl.Int64, strict=False)",
        last = n - 1
    )
}

pub(super) fn py_dtype(dt: &DataType) -> &'static str {
    match dt {
        DataType::Int64 => "pl.Int64",
        DataType::Float64 => "pl.Float64",
        DataType::Date => "pl.Date",
        DataType::Boolean => "pl.Boolean",
        DataType::Datetime(polars::prelude::TimeUnit::Milliseconds, _) => "pl.Datetime(\"ms\")",
        DataType::Datetime(polars::prelude::TimeUnit::Microseconds, _) => "pl.Datetime(\"us\")",
        DataType::Datetime(polars::prelude::TimeUnit::Nanoseconds, _) => "pl.Datetime(\"ns\")",
        _ => "pl.Utf8",
    }
}
