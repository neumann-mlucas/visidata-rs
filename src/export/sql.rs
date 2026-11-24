//! SQL (DuckDB) pieces of `:export`: the view query and expression helpers.

use super::*;

impl Script {
    pub(super) fn view_query(
        &mut self,
        s: &Sheet,
        rid: bool,
        errs: bool,
    ) -> Result<String, String> {
        let base = self.base(s)?;
        let v = &s.view;
        let deleted = s.deleted_rids();
        let carried = carries_rid(s);
        let mut q = if rid && !carried {
            format!("SELECT *, row_number() OVER () - 1 AS __rid FROM {base}")
        } else {
            format!("SELECT * FROM {base}")
        };
        let keep = if carried && !rid {
            " EXCLUDE (__rid)"
        } else {
            ""
        };
        if !deleted.is_empty() {
            let ids: Vec<String> = deleted.iter().map(u64::to_string).collect();
            q = format!(
                "SELECT *{keep} FROM ({q}) WHERE __rid NOT IN ({})",
                ids.join(", ")
            );
        } else if !keep.is_empty() {
            q = format!("SELECT *{keep} FROM ({q})");
        }
        if !v.type_casts.is_empty() {
            let mut flags = String::new();
            let casts: Vec<String> = v
                .type_casts
                .iter()
                .map(|(c, dt)| {
                    let i = q_ident(c);
                    let src = src_type(s, c);
                    let text = src == Some(DataType::String);
                    let cast = if *dt == crate::sheet::LEN_CAST {
                        match &src {
                            Some(DataType::List(_)) => format!("CAST(len({i}) AS UINTEGER)"),
                            Some(DataType::Struct(f)) => {
                                let set: Vec<String> = (f.iter())
                                    .map(|f| {
                                        format!("({i}.{} IS NOT NULL)::UINTEGER", q_ident(f.name()))
                                    })
                                    .collect();
                                format!("CASE WHEN {i} IS NOT NULL THEN {} END", set.join(" + "))
                            }
                            _ => format!("CAST(length(CAST({i} AS VARCHAR)) AS UINTEGER)"),
                        }
                    } else if *dt == DataType::Date && text {
                        self.need(&sql_lenient_date());
                        format!("lenient_date({i})")
                    } else if matches!(dt, DataType::Datetime(..)) && text {
                        self.need(&sql_lenient_date());
                        self.need(&sql_lenient_datetime());
                        format!("lenient_datetime({i})")
                    } else if dt.is_numeric() && src == Some(DataType::Date) {
                        // Polars casts a date to its days since 1970
                        format!("TRY_CAST(({i} - DATE '1970-01-01') AS {})", sql_dtype(dt))
                    } else if dt.is_integer() && src.as_ref().is_some_and(DataType::is_float) {
                        // Polars truncates a float to an int; DuckDB would round
                        format!("TRY_CAST(trunc({i}) AS {})", sql_dtype(dt))
                    } else {
                        format!("TRY_CAST({i} AS {})", sql_dtype(dt))
                    };
                    if errs {
                        let e = q_ident(&err_col(c));
                        flags += &format!(", ({i} IS NOT NULL AND {cast} IS NULL) AS {e}");
                    }
                    format!("{cast} AS {i}")
                })
                .collect();
            q = format!("SELECT * REPLACE ({}){flags} FROM ({q})", casts.join(", "));
        }
        let cols = match visible_columns(s)? {
            Some(mut cols) => {
                if rid {
                    cols.push("__rid".into());
                }
                if errs {
                    cols.extend(v.type_casts.iter().map(|(c, _)| err_col(c)));
                }
                cols.iter()
                    .map(|c| q_ident(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
            None => "*".into(),
        };
        let mut sql = format!("SELECT {cols}\nFROM ({q})");
        let mut wheres = Vec::new();
        if let Some(f) = &v.filter {
            wheres.push(format!(
                "regexp_matches(CAST({} AS VARCHAR), {})",
                q_ident(&f.column),
                q_str(&f.regex)
            ));
        }
        if let Some(p) = &v.filter_expr {
            wheres.push(format!("coalesce(({p}), false)"));
        }
        if !wheres.is_empty() {
            sql += &format!("\nWHERE {}", wheres.join(" AND "));
        }
        if !v.sort.is_empty() {
            let by: Vec<String> = v
                .sort
                .iter()
                .map(|k| {
                    // nulls are the smallest value, as in the app
                    let dir = if k.order == SortOrder::Desc {
                        "DESC NULLS LAST"
                    } else {
                        "ASC NULLS FIRST"
                    };
                    format!("{} {dir}", q_ident(&k.column))
                })
                .collect();
            sql += &format!("\nORDER BY {}", by.join(", "));
        }
        Ok(sql)
    }
}

/// Each plan step wraps the query so far (DuckDB `* RENAME` / `* REPLACE`).
pub(super) fn sql_step(q: String, step: &PlanStep) -> String {
    match step {
        PlanStep::Replace {
            column,
            pattern,
            replacement,
            all,
            rids,
        } => {
            let c = q_ident(column);
            let v = format!(
                "regexp_replace({c}, {}, {}{})",
                q_str(pattern),
                q_str(&re2_replacement(replacement)),
                if *all { ", 'g'" } else { "" }
            );
            let v = match rids {
                Some(r) => {
                    let ids: Vec<String> = r.iter().map(u64::to_string).collect();
                    format!(
                        "CASE WHEN __rid IN ({}) THEN {v} ELSE {c} END",
                        ids.join(", ")
                    )
                }
                None => v,
            };
            format!("SELECT * REPLACE ({v} AS {c}) FROM ({q})")
        }
        PlanStep::Capture {
            column,
            pattern,
            name,
        } => {
            let text = format!("CAST({} AS VARCHAR)", q_ident(column));
            let p = q_str(pattern);
            let names: Vec<String> = crate::sheet::capture_fields(pattern)
                .unwrap_or_default()
                .iter()
                .map(|f| q_str(f))
                .collect();
            format!(
                "SELECT *, CASE WHEN regexp_matches({text}, {p}) THEN regexp_extract({text}, {p}, [{}]) END AS {} FROM ({q})",
                names.join(", "),
                q_ident(name)
            )
        }
        PlanStep::GroupAgg {
            agg,
            by,
            over,
            casts,
            name,
        } => {
            let typed = |c: &str| match casts.iter().find(|(n, _)| n == c) {
                Some((_, dt)) => format!("TRY_CAST({} AS {})", q_ident(c), sql_dtype(dt)),
                None => q_ident(c),
            };
            let part: Vec<String> = over.iter().map(|c| q_ident(c)).collect();
            let part = match part.is_empty() {
                true => String::new(),
                false => format!("PARTITION BY {}", part.join(", ")),
            };
            let v = match (agg.as_str(), by.as_slice()) {
                ("rank", _) => {
                    let order: Vec<String> = (by.iter())
                        .map(|(c, d)| match d {
                            true => format!("{} DESC NULLS LAST", typed(c)),
                            false => format!("{} ASC NULLS FIRST", typed(c)),
                        })
                        .collect();
                    format!("dense_rank() OVER ({part} ORDER BY {})", order.join(", "))
                }
                (_, [(c, _)]) => sql_agg_of(&typed(c), agg, &format!(" OVER ({part})")),
                _ => unreachable!("one column per aggregator"),
            };
            format!("SELECT *, {v} AS {} FROM ({q})", q_ident(name))
        }
        PlanStep::DeleteRows { .. } | PlanStep::AddRows { .. } => q, // see `steps`
        PlanStep::Json { column, dtype } => format!(
            "SELECT * REPLACE (from_json({c}, {}) AS {c}) FROM ({q})",
            q_str(&json_structure(dtype)),
            c = q_ident(column)
        ),
        PlanStep::Expand {
            column,
            fields,
            after,
        } => {
            let kids = fields.iter().map(|(path, name)| {
                // DuckDB lists count from 1; past the end is NULL
                let get =
                    path.iter()
                        .fold(q_ident(column), |e, f| match crate::sheet::list_index(f) {
                            Some(i) => format!("({e})[{}]", i + 1),
                            None => format!("struct_extract({e}, {})", q_str(f)),
                        });
                format!("{get} AS {}", q_ident(name))
            });
            let after: Vec<String> = after.iter().map(|c| q_ident(c)).collect();
            let head = match after.is_empty() {
                true => "*".to_string(),
                false => format!("* EXCLUDE ({})", after.join(", ")),
            };
            let cols: Vec<String> = std::iter::once(head).chain(kids).chain(after).collect();
            format!("SELECT {} FROM ({q})", cols.join(", "))
        }
        PlanStep::Rename { old, new } => format!(
            "SELECT * RENAME ({} AS {}) FROM ({q})",
            q_ident(old),
            q_ident(new)
        ),
        PlanStep::Currency { column } => format!(
            "SELECT * REPLACE (TRY_CAST(regexp_replace(CAST({c} AS VARCHAR), '[^0-9.\\-]', '', 'g') AS DOUBLE) AS {c}) FROM ({q})",
            c = q_ident(column)
        ),
        PlanStep::Derive {
            name,
            expr,
            replaces: true,
        } => format!(
            "SELECT * REPLACE (({expr}) AS {}) FROM ({q})",
            q_ident(name)
        ),
        PlanStep::Derive { name, expr, .. } => {
            format!("SELECT *, ({expr}) AS {} FROM ({q})", q_ident(name))
        }
        PlanStep::SetCol {
            column,
            value,
            dtype,
            rids,
        } => {
            let ids: Vec<String> = rids.iter().map(u64::to_string).collect();
            format!(
                "SELECT * REPLACE (CASE WHEN __rid IN ({}) THEN TRY_CAST({} AS {}) ELSE {c} END AS {c}) FROM ({q})",
                ids.join(", "),
                value.as_deref().map_or("NULL".into(), q_str),
                sql_dtype(dtype),
                c = q_ident(column)
            )
        }
        PlanStep::SetExpr {
            column,
            expr,
            dtype,
            rids,
        } => {
            let ids: Vec<String> = rids.iter().map(u64::to_string).collect();
            format!(
                "SELECT * REPLACE (CASE WHEN __rid IN ({}) THEN TRY_CAST(({expr}) AS {}) ELSE {c} END AS {c}) FROM ({q})",
                ids.join(", "),
                sql_dtype(dtype),
                c = q_ident(column)
            )
        }
        PlanStep::Lookup {
            name, key, table, ..
        } => {
            let (keys, vals, dtype) = lookup_lists(table);
            let (n, t) = (q_ident(name), sql_dtype(&dtype));
            if keys.is_empty() {
                return format!("SELECT *, CAST(NULL AS {t}) AS {n} FROM ({q})");
            }
            let rows: Vec<String> = keys
                .iter()
                .zip(&vals)
                .map(|(k, v)| {
                    format!(
                        "({}, {})",
                        q_str(k),
                        v.as_deref().map_or("NULL".into(), q_str)
                    )
                })
                .collect();
            format!(
                // a join doesn't keep row order: number the rows, sort back
                "SELECT * EXCLUDE (__k, __v, __ord), TRY_CAST(__v AS {t}) AS {n} \
                 FROM (SELECT *, row_number() OVER () AS __ord FROM ({q})) \
                 LEFT JOIN (VALUES {}) AS __l(__k, __v) ON CAST({} AS VARCHAR) = __l.__k ORDER BY __ord",
                rows.join(", "),
                q_ident(key)
            )
        }
        PlanStep::SetCells { column, patch } => {
            let (rids, vals) = patch_lists(patch);
            let rows: Vec<String> = rids
                .iter()
                .zip(&vals)
                .map(|(r, v)| format!("({r}, {}, true)", v.as_deref().map_or("NULL".into(), q_str)))
                .collect();
            let dtype = patch
                .column("__vdrs_v")
                .map(|c| c.dtype().clone())
                .unwrap_or(DataType::String);
            format!(
                // a join doesn't keep row order: number the rows, sort back
                "SELECT * EXCLUDE (__v, __hit, __ord) REPLACE (CASE WHEN __hit THEN TRY_CAST(__v AS {}) ELSE {c} END AS {c}) \
                 FROM (SELECT *, row_number() OVER () AS __ord FROM ({q})) \
                 LEFT JOIN (VALUES {}) AS __p(__rid, __v, __hit) USING (__rid) ORDER BY __ord",
                sql_dtype(&dtype),
                rows.join(", "),
                c = q_ident(column)
            )
        }
    }
}

/// Pivot `p` on `on`, one cell per `keys` × value holding aggregate `agg`.
/// Grouped first so an absent cell is null, not `agg` over nothing.
pub(super) fn sql_pivot(p: &str, keys: &[String], on: &str, agg: &str) -> String {
    let k: Vec<String> = keys.iter().map(|k| q_ident(k)).collect();
    let on = q_ident(on);
    let select = k.iter().map(|c| format!("{c}, ")).collect::<String>();
    let by: Vec<String> = (1..=k.len() + 1).map(|i| i.to_string()).collect();
    let group = if k.is_empty() {
        String::new()
    } else {
        format!(" GROUP BY {}", k.join(", "))
    };
    // PIVOT sorts the new columns by name; the app keeps them as first seen.
    let text = format!("coalesce(CAST({on} AS VARCHAR), '')");
    format!(
        "SELECT * FROM (PIVOT (SELECT {select}{text} AS {on}, {agg} AS __v \
         FROM {p} GROUP BY {}) ON {on} IN (SELECT {on} FROM (SELECT {text} AS {on}, \
         row_number() OVER () AS __r FROM {p}) GROUP BY {on} ORDER BY min(__r)) USING first(__v){group})",
        by.join(", ")
    )
}

pub(super) fn sql_pivot_aggs(
    p: &str,
    keys: &[String],
    on: &str,
    aggs: &[(String, String)],
) -> String {
    let k: Vec<String> = keys.iter().map(|k| q_ident(k)).collect();
    let o = q_ident(on);
    let lead = k.iter().map(|c| format!("{c}, ")).collect::<String>();
    let text = format!("coalesce(CAST({o} AS VARCHAR), '')");
    let alias: Vec<String> = aggs.iter().map(|(c, a)| q_ident(&agg_name(c, a))).collect();
    // no CTEs: a PIVOT reading its columns from the data can't see them
    let g = format!(
        "(SELECT {lead}{text} AS {o}, {} FROM {p} GROUP BY ALL)",
        aggs.iter()
            .map(|(c, a)| sql_agg(c, a))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let combos = match keys {
        [] => format!("(SELECT DISTINCT {o} FROM {g})"),
        _ => format!(
            "(SELECT * FROM (SELECT DISTINCT {} FROM {g}) CROSS JOIN (SELECT DISTINCT {o} FROM {g}))",
            k.join(", ")
        ),
    };
    let filled: Vec<String> = aggs
        .iter()
        .zip(&alias)
        .map(
            |((_, a), q)| match crate::sheet::columns::zero_on_empty(a) {
                true => format!("coalesce(g.{q}, 0) AS {q}"),
                false => format!("g.{q}"),
            },
        )
        .collect();
    let using_join: Vec<String> = k.iter().cloned().chain([o.clone()]).collect();
    let prefixes = pivot_prefixes(aggs);
    let using: Vec<String> = alias
        .iter()
        .zip(&prefixes)
        .map(|(q, pre)| match pre.strip_suffix('_') {
            Some(name) => format!("first({q}) AS {}", q_ident(name)),
            None => format!("first({q})"),
        })
        .collect();
    let group = match keys {
        [] => String::new(),
        _ => format!(" GROUP BY {}", k.join(", ")),
    };
    // PIVOT names `<value>_<alias>`; VisiData's is `<alias>_<value>`. Keys
    // go by `__vdk<i>_` meanwhile: no alias ends in `_`, so the rename
    // can't match one.
    let (select, rename) = if prefixes[0].is_empty() {
        ("*".to_string(), String::new())
    } else {
        let alts: Vec<String> = prefixes
            .iter()
            .map(|s| regex::escape(s.trim_end_matches('_')).replace('\'', "''"))
            .collect();
        let tmp = |i: usize| q_ident(&format!("__vdk{i}_"));
        let back: String = k
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{} AS {c}, ", tmp(i)))
            .collect();
        let rename = match keys {
            [] => String::new(),
            _ => format!(
                " RENAME ({})",
                k.iter()
                    .enumerate()
                    .map(|(i, c)| format!("{c} AS {}", tmp(i)))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
        let cols = format!("COLUMNS('^(.*)_({})$') AS '\\2_\\1'", alts.join("|"));
        (format!("{back}{cols}"), rename)
    };
    format!(
        "SELECT {select} FROM (SELECT *{rename} FROM (PIVOT (SELECT c.*, {filled} FROM {combos} c LEFT JOIN {g} g \
         USING ({using_join})) ON {o} IN (SELECT {o} FROM (SELECT {text} AS {o}, \
         row_number() OVER () AS __r FROM {p}) GROUP BY {o} ORDER BY min(__r)) USING {using}{group}))",
        filled = filled.join(", "),
        using_join = using_join.join(", "),
        using = using.join(", "),
    )
}

/// [`crate::sheet::columns::agg_expr`] in DuckDB SQL.
pub(super) fn sql_agg(c: &str, agg: &str) -> String {
    let e = sql_agg_of(&q_ident(c), agg, "");
    format!("{e} AS {}", q_ident(&agg_name(c, agg)))
}

/// `agg` of SQL value `q`, unnamed; `over` follows the aggregate call (a
/// window, or empty).
fn sql_agg_of(q: &str, agg: &str, over: &str) -> String {
    match agg {
        "distinct" => format!("count(DISTINCT {q}){over}"),
        "sum" => format!("coalesce(sum({q}){over}, 0)"),
        "mean" | "avg" => format!("avg({q}){over}"),
        "stdev" => format!("stddev_samp({q}){over}"),
        a if let Some(n) = crate::sheet::columns::percentile(a) => {
            format!("quantile_cont({q}, {}){over}", n as f64 / 100.0)
        }
        _ => format!("{agg}({q}){over}"),
    }
}

/// `read_csv_auto` arguments matching how vdrs read this file: DuckDB must not
/// sniff `7/3/2018` as a date.
pub(super) fn sql_csv_args(s: &Sheet) -> Result<String, String> {
    let mut args = ", header = true, sample_size = -1, null_padding = true, \
                    auto_type_candidates = ['BOOLEAN', 'BIGINT', 'DOUBLE', 'VARCHAR']"
        .to_string();
    if s.kind == SourceKind::Tsv {
        args += ", delim = '\\t'";
    }
    let dates: Vec<String> = csv_dates(s)?
        .iter()
        .map(|(n, d)| format!("{}: '{}'", q_str(n), sql_dtype(d)))
        .collect();
    if !dates.is_empty() {
        args += &format!(", types = {{{}}}", dates.join(", "));
    }
    Ok(args)
}

/// `read_json_auto` arguments: the types vdrs read, or DuckDB turns ISO-looking
/// strings into timestamps that Polars keeps as text.
pub(super) fn sql_json_args(s: &Sheet) -> Result<String, String> {
    use DataType::*;
    let schema = csv_columns(s)?;
    let scalar = |d: &DataType| {
        matches!(
            d,
            Int8 | Int16
                | Int32
                | Int64
                | UInt8
                | UInt16
                | UInt32
                | UInt64
                | Float32
                | Float64
                | Boolean
                | String
                | Null
        )
    };
    if !schema.iter().all(|(_, d)| scalar(d)) {
        // ponytail: nested JSON keeps DuckDB's sniffing; spell out STRUCT/LIST types if it drifts
        return Ok(std::string::String::new());
    }
    let cols: Vec<std::string::String> = schema
        .iter()
        .map(|(n, d)| format!("{}: '{}'", q_str(n), sql_dtype(d)))
        .collect();
    Ok(format!(", columns = {{{}}}", cols.join(", ")))
}

/// SQL twin of `sheet::lenient_datetime`.
pub(super) fn sql_lenient_datetime() -> String {
    let re = q_str(TIME_OF_DAY);
    let num = |i: u8| {
        format!("coalesce(TRY_CAST(nullif(regexp_extract(t, {re}, {i}), '') AS BIGINT), 0)")
    };
    format!(
        "CREATE TEMP MACRO lenient_datetime(s) AS (\n  SELECT CAST(lenient_date(s) AS TIMESTAMP) + to_seconds(\
         (CASE WHEN ap = 'p' AND h < 12 THEN h + 12 WHEN ap = 'a' AND h = 12 THEN 0 ELSE h END) * 3600 + m * 60 + sec)\n  \
         FROM (SELECT {h} AS h, {m} AS m, {sec} AS sec, lower(regexp_extract(t, {re}, 4)) AS ap \
         FROM (SELECT trim(CAST(s AS VARCHAR)) AS t))\n);",
        h = num(1),
        m = num(2),
        sec = num(3),
    )
}

/// SQL twin of `sheet::lenient_date`.
pub(super) fn sql_lenient_date() -> String {
    let tries: Vec<String> = DATE_LAYOUTS
        .iter()
        .map(|(layout, shape)| {
            format!(
                "CASE WHEN regexp_matches(d, {}) THEN CAST(try_strptime(d, {}) AS DATE) END",
                q_str(shape),
                q_str(layout)
            )
        })
        .collect();
    format!(
        "CREATE TEMP MACRO lenient_date(s) AS (\n  SELECT coalesce({})\n  FROM (SELECT regexp_replace(trim(CAST(s AS VARCHAR)), {}, '') AS d)\n);",
        tries.join(",\n    "),
        q_str(DATE_TIME_PART)
    )
}

/// [`Binning::index`] of column `c` (an SQL identifier): null for null and
/// NaN (DuckDB's `least()` skips nulls and sorts NaN last).
pub(super) fn sql_bin_index(c: &str, min: f64, width: f64, n: usize) -> String {
    format!(
        "CASE WHEN {c} IS NULL OR isnan(CAST({c} AS DOUBLE)) THEN NULL \
         ELSE greatest(least(floor((CAST({c} AS DOUBLE) - {min:?}) / {width:?}), {last}), 0)::BIGINT END",
        last = n - 1
    )
}

pub(super) fn sql_dtype(dt: &DataType) -> &'static str {
    match dt {
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => "BIGINT",
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => "UBIGINT",
        DataType::Float32 | DataType::Float64 => "DOUBLE",
        DataType::Boolean => "BOOLEAN",
        DataType::Date => "DATE",
        DataType::Datetime(..) => "TIMESTAMP",
        _ => "VARCHAR",
    }
}
