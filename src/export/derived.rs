//! Derived sheets (`zM` unfurl, joins, diff, describe) as Python or SQL.

use super::*;

/// `zM` of view `p`, as `Sheet::unfurl_from`.
pub(super) fn unfurl_code(
    py: bool,
    p: &str,
    parent: &Sheet,
    column: &str,
    empty: bool,
) -> Result<String, String> {
    // ponytail: refused, not translated; add when an export needs it
    if empty {
        return Err("export: zM under unfurl_empty isn't exported".into());
    }
    use crate::sheet::{UNFURL_ROW as U, Unfurl, unfurl_kind};
    let cols = columns(parent)?;
    let dt = &cols
        .iter()
        .find(|(n, _)| n == column)
        .ok_or_else(|| format!("export: no column {column}"))?
        .1;
    let kind = unfurl_kind(dt)?;
    let is_struct = matches!(kind, Unfurl::Struct { .. });
    let (key, val) = (format!("{column}_key"), format!("{column}_value"));
    let names = |k: String, v: String, q: &dyn Fn(&str) -> String| -> String {
        cols.iter()
            .map(|(n, _)| match n == column {
                true => format!("{k}, {v}"),
                false => q(n),
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    Ok(if py {
        let c = py_str(column);
        let items = match &kind {
            Unfurl::List => format!("pl.col({c})"),
            Unfurl::Struct { fields, text } => {
                let cast = if *text { ".cast(pl.Utf8)" } else { "" };
                let parts: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        format!(
                            "pl.struct(pl.lit({f}).alias(\"k\"), pl.col({c}).struct.field({f}){cast}.alias(\"v\"))",
                            f = py_str(f)
                        )
                    })
                    .collect();
                format!("pl.concat_list([{}])", parts.join(", "))
            }
            Unfurl::Scalar => format!("pl.concat_list([pl.col({c})])"),
        };
        let (k, v) = if is_struct {
            (
                format!("pl.col({c}).struct.field(\"k\")"),
                format!("pl.col({c}).struct.field(\"v\")"),
            )
        } else {
            (
                format!("pl.int_range(pl.len(), dtype=pl.Int64).over(\"{U}\")"),
                format!("pl.col({c})"),
            )
        };
        format!(
            "{p}.with_row_index(\"{U}\").with_columns({items}.alias({c})).explode({c}).select([{}])",
            names(
                format!("{k}.alias({})", py_str(&key)),
                format!("{v}.alias({})", py_str(&val)),
                &|n| format!("pl.col({})", py_str(n))
            )
        )
    } else {
        let c = q_ident(column);
        let items = match &kind {
            Unfurl::List => format!("CASE WHEN {c} IS NULL THEN [NULL] ELSE {c} END"),
            Unfurl::Struct { fields, text } => {
                let parts: Vec<String> = fields
                    .iter()
                    .map(|f| {
                        let v = format!("struct_extract({c}, {})", q_str(f));
                        let v = if *text {
                            format!("CAST({v} AS VARCHAR)")
                        } else {
                            v
                        };
                        format!("{{'k': {}, 'v': {v}}}", q_str(f))
                    })
                    .collect();
                format!("[{}]", parts.join(", "))
            }
            Unfurl::Scalar => format!("[{c}]"),
        };
        let (k, v) = if is_struct {
            ("struct_extract(__e, 'k')", "struct_extract(__e, 'v')")
        } else {
            ("__i", "__e")
        };
        format!(
            "SELECT {} FROM (SELECT *, generate_subscripts(__l, 1) - 1 AS __i, unnest(__l) AS __e \
             FROM (SELECT *, row_number() OVER () AS {U}, {items} AS __l FROM {p}) WHERE len(__l) > 0) \
             ORDER BY {U}, __i",
            names(
                format!("{k} AS {}", q_ident(&key)),
                format!("{v} AS {}", q_ident(&val)),
                &|n| q_ident(n)
            )
        )
    })
}

/// `:join` of view `p` (sheet `left`) with view `r` (sheet `right`).
/// `flag`: one more right column to carry through (row diff).
#[allow(clippy::too_many_arguments)]
pub(super) fn join_code(
    py: bool,
    p: &str,
    r: &str,
    left: &Sheet,
    right: &Sheet,
    keys: &[String],
    how: &str,
    flag: Option<&str>,
) -> Result<String, String> {
    let names = |s| -> Result<Vec<String>, String> {
        Ok(columns(s)?.into_iter().map(|(n, _)| n).collect())
    };
    let (ln, rn) = (names(left)?, names(right)?);
    // Right columns Polars suffixes `_right` (same name as a left one).
    let dups: Vec<&String> = rn
        .iter()
        .filter(|n| !keys.contains(n) && ln.contains(n))
        .collect();
    // inner/left/full: a column on both sides is `<sheet>_<column>` on each,
    // as `Sheet::join_from` (a diff keeps Polars' `_right`)
    let prefixed = flag.is_none() && matches!(how, "inner" | "left" | "full");
    let pre = |s: &Sheet, n: &str| format!("{}_{n}", crate::sheet::extend_prefix(s));
    Ok(match (how, py) {
        ("append", true) => format!(
            "pl.concat([{p}.with_columns(pl.lit({}).alias(\"origin_sheet\")), \
             {r}.with_columns(pl.lit({}).alias(\"origin_sheet\"))], how=\"diagonal_relaxed\")",
            py_str(&left.name),
            py_str(&right.name)
        ),
        ("append", false) => format!(
            "SELECT *, {} AS origin_sheet FROM {p}\nUNION ALL BY NAME\nSELECT *, {} AS origin_sheet FROM {r}",
            q_str(&left.name),
            q_str(&right.name)
        ),
        ("concat", true) => {
            let by_pos: Vec<String> = rn
                .iter()
                .zip(&ln)
                .map(|(r, l)| format!("pl.col({}).alias({})", py_str(r), py_str(l)))
                .collect();
            format!(
                "pl.concat([{p}.with_columns(pl.lit({}).alias(\"origin_sheet\")), \
                 {r}.select([{}]).with_columns(pl.lit({}).alias(\"origin_sheet\"))], how=\"vertical_relaxed\")",
                py_str(&left.name),
                by_pos.join(", "),
                py_str(&right.name)
            )
        }
        // SQL's UNION ALL already pairs columns by position, names from the left.
        ("concat", false) => format!(
            "SELECT *, {} AS origin_sheet FROM {p}\nUNION ALL\nSELECT *, {} AS origin_sheet FROM {r}",
            q_str(&left.name),
            q_str(&right.name)
        ),
        (_, true) => {
            let on: Vec<String> = keys.iter().map(|k| py_str(k)).collect();
            let on = on.join(", ");
            let h = match how {
                "merge" => "full",
                "extend" => "left",
                h => h,
            };
            let renamed = |t: &str, s: &Sheet| {
                let pairs: Vec<String> = dups
                    .iter()
                    .map(|n| format!("{}: {}", py_str(n), py_str(&pre(s, n))))
                    .collect();
                format!("{t}.rename({{{}}})", pairs.join(", "))
            };
            let (p, r) = match prefixed && !dups.is_empty() {
                true => (renamed(p, left), renamed(r, right)),
                false => (p.to_string(), r.to_string()),
            };
            let (p, r) = (p.as_str(), r.as_str());
            let r = &if how == "extend" {
                let pairs: Vec<String> = rn
                    .iter()
                    .filter(|n| !keys.contains(n))
                    .map(|n| {
                        let to = format!("{}_{n}", crate::sheet::extend_prefix(right));
                        format!("{}: {}", py_str(n), py_str(&to))
                    })
                    .collect();
                format!("{r}.rename({{{}}})", pairs.join(", "))
            } else {
                r.to_string()
            };
            // VisiData's order, as `Sheet::join_from` (a diff orders itself)
            let numbered = |t: &str, row: &str, first: &str| match flag {
                Some(_) => t.to_string(),
                None => format!(
                    "{t}.with_row_index(\"{row}\").with_columns(pl.col(\"{row}\").cast(pl.Int64).min().over([{on}]).alias(\"{first}\"))"
                ),
            };
            let mut e = format!(
                "{}.join({}, on=[{on}], how={}, coalesce=True, maintain_order=\"left_right\")",
                numbered(p, JL, JLK),
                numbered(r, JR, JRK),
                py_str(h)
            );
            if flag.is_none() {
                e += &format!(
                    ".sort([pl.col(\"{JLK}\").fill_null(pl.col(\"{JRK}\") + {JOIN_RIGHT_KEYS}), \"{JL}\", \"{JR}\"], \
                     nulls_last=True, maintain_order=True).drop([\"{JL}\", \"{JLK}\", \"{JR}\", \"{JRK}\"])"
                );
            }
            if how == "merge" && !dups.is_empty() {
                let fold: Vec<String> = dups
                    .iter()
                    .map(|b| {
                        format!(
                            "pl.col({}).fill_null(pl.col({b})).alias({b})",
                            py_str(&format!("{b}_right")),
                            b = py_str(b)
                        )
                    })
                    .collect();
                let drop: Vec<String> =
                    dups.iter().map(|b| py_str(&format!("{b}_right"))).collect();
                e += &format!(
                    ".with_columns([{}]).drop([{}])",
                    fold.join(", "),
                    drop.join(", ")
                );
            }
            e
        }
        (_, false) => {
            let jt = match how {
                "inner" => "INNER",
                "left" | "extend" => "LEFT",
                _ => "FULL OUTER",
            };
            let rcols: Vec<String> = rn
                .iter()
                .map(|n| {
                    if how == "extend" && !keys.contains(n) {
                        let to = format!("{}_{n}", crate::sheet::extend_prefix(right));
                        format!("{} AS {}", q_ident(n), q_ident(&to))
                    } else if dups.contains(&n) && prefixed {
                        format!("{} AS {}", q_ident(n), q_ident(&pre(right, n)))
                    } else if dups.contains(&n) {
                        format!("{} AS {}", q_ident(n), q_ident(&format!("{n}_right")))
                    } else {
                        q_ident(n)
                    }
                })
                .chain(flag.map(q_ident))
                .chain(
                    flag.is_none()
                        .then(|| [JR.to_string(), JRK.to_string()])
                        .into_iter()
                        .flatten(),
                )
                .collect();
            let using: Vec<String> = keys.iter().map(|k| q_ident(k)).collect();
            let part = using.join(", ");
            // VisiData's order, as `Sheet::join_from` (a diff orders itself)
            let numbered = |t: &str, row: &str, first: &str| match flag {
                Some(_) => t.to_string(),
                None => format!(
                    "(SELECT *, min({row}) OVER (PARTITION BY {part}) AS {first} \
                     FROM (SELECT *, row_number() OVER () AS {row} FROM {t}))"
                ),
            };
            let p = &match prefixed && !dups.is_empty() {
                true => {
                    let ren: Vec<String> = dups
                        .iter()
                        .map(|n| format!("{} AS {}", q_ident(n), q_ident(&pre(left, n))))
                        .collect();
                    format!("(SELECT * RENAME ({}) FROM {p})", ren.join(", "))
                }
                false => p.to_string(),
            };
            let mut q = format!(
                "SELECT * FROM {} {jt} JOIN (SELECT {} FROM {}) USING ({part})",
                numbered(p, JL, JLK),
                rcols.join(", "),
                numbered(r, JR, JRK),
            );
            if flag.is_none() {
                q = format!(
                    "SELECT * EXCLUDE ({JL}, {JLK}, {JR}, {JRK}) FROM ({q} \
                     ORDER BY coalesce({JLK}, {JRK} + {JOIN_RIGHT_KEYS}), {JL} NULLS LAST, {JR} NULLS LAST)"
                );
            }
            if how == "merge" && !dups.is_empty() {
                let fold: Vec<String> = dups
                    .iter()
                    .map(|b| {
                        format!(
                            "coalesce({}, {b}) AS {b}",
                            q_ident(&format!("{b}_right")),
                            b = q_ident(b)
                        )
                    })
                    .collect();
                let drop: Vec<String> = dups
                    .iter()
                    .map(|b| q_ident(&format!("{b}_right")))
                    .collect();
                q = format!(
                    "SELECT * EXCLUDE ({}) FROM (SELECT * REPLACE ({}) FROM ({q}))",
                    drop.join(", "),
                    fold.join(", ")
                );
            }
            q
        }
    })
}

/// Row diff over the full join `j`: tag `__diff`, keep rows only on one side
/// or with a changed shared column (mirrors `Sheet::row_diff_from`).
pub(super) fn diff_code(
    py: bool,
    j: &str,
    left: &Sheet,
    other: &Sheet,
    keys: &[String],
) -> Result<String, String> {
    let (a, b) = (columns(left)?, columns(other)?);
    let shared: Vec<&String> = a
        .iter()
        .map(|(n, _)| n)
        .filter(|n| !keys.contains(n) && b.iter().any(|(m, _)| m == *n))
        .collect();
    Ok(if py {
        let null = |c: &str| format!("pl.col({}).is_null()", py_str(c));
        let (only_a, only_b) = (null(DIFF_RIGHT), null(DIFF_LEFT));
        let mismatch: Vec<String> = shared
            .iter()
            .map(|c| {
                format!(
                    "pl.col({}).ne_missing(pl.col({}))",
                    py_str(c),
                    py_str(&format!("{c}_right"))
                )
            })
            .collect();
        let mismatch = if mismatch.is_empty() {
            "pl.lit(False)".into()
        } else {
            mismatch.join(" | ")
        };
        format!(
            "{j}.with_columns(pl.when({only_a}).then(pl.lit(\"only_a\")).when({only_b}).then(pl.lit(\"only_b\"))\
             .otherwise(pl.lit(\"changed\")).alias(\"__diff\")).filter({only_a} | {only_b} | {mismatch})\
             .drop([{}, {}])",
            py_str(DIFF_LEFT),
            py_str(DIFF_RIGHT)
        )
    } else {
        let (only_a, only_b) = (
            format!("{DIFF_RIGHT} IS NULL"),
            format!("{DIFF_LEFT} IS NULL"),
        );
        let mismatch: Vec<String> = shared
            .iter()
            .map(|c| {
                format!(
                    "{} IS DISTINCT FROM {}",
                    q_ident(c),
                    q_ident(&format!("{c}_right"))
                )
            })
            .collect();
        let mismatch = if mismatch.is_empty() {
            "FALSE".into()
        } else {
            mismatch.join(" OR ")
        };
        format!(
            "SELECT * EXCLUDE ({DIFF_LEFT}, {DIFF_RIGHT}), CASE WHEN {only_a} THEN 'only_a' WHEN {only_b} THEN 'only_b' ELSE 'changed' END AS __diff\n\
             FROM ({j})\nWHERE {only_a} OR {only_b} OR {mismatch}"
        )
    })
}

/// `I`: one row per column of view `p` (mirrors `Sheet::describe_from`);
/// `p` carries `err_col(c)` for each cast column `c`.
pub(super) fn describe_code(py: bool, p: &str, parent: &Sheet) -> Result<String, String> {
    let cols = columns(parent)?;
    if cols.is_empty() {
        return Err("export: nothing to describe".into());
    }
    let cast = |n: &str| parent.view.type_casts.iter().any(|(c, _)| c == n);
    let rows: Vec<String> = cols
        .iter()
        .enumerate()
        .map(|(i, (name, dt))| {
            let numeric = dt.is_numeric();
            let nested = dt.is_nested() || matches!(dt, DataType::Duration(_)); // no text cast
            let ordered = !nested && (numeric || dt.is_temporal());
            if py {
                let c = format!("pl.col({})", py_str(name));
                let e = cast(name).then(|| format!("pl.col({})", py_str(&err_col(name))));
                let text = |ok: bool, x: String| {
                    if ok { format!("{x}.cast(pl.Utf8)") } else { "pl.lit(None, dtype=pl.Utf8)".into() }
                };
                let float = |ok: bool, x: String| {
                    if ok { format!("{x}.cast(pl.Float64)") } else { "pl.lit(None, dtype=pl.Float64)".into() }
                };
                let errors = match &e {
                    Some(e) => format!("{e}.sum().cast(pl.UInt64)"),
                    None => "pl.lit(0, dtype=pl.UInt64)".into(),
                };
                let valid = match &e {
                    Some(e) => format!("{c}.filter(~{e})"),
                    None => c.clone(),
                };
                let cnt = format!("{c}.count().over({c})");
                // `when` evaluates every branch: `get` on no values must not fail
                let median = format!(
                    "pl.when({c}.count() == 0).then(pl.lit(None, dtype=pl.Utf8))\
                     .when({c}.count() % 2 == 1).then({c}.drop_nulls().sort().get({c}.count() // 2, null_on_oob=True).cast(pl.Utf8))\
                     .otherwise({})",
                    text(numeric, format!("{c}.median()"))
                );
                [
                    format!("pl.lit({}).alias(\"sheet\")", py_str(&parent.name)),
                    format!("pl.lit({}).alias(\"column\")", py_str(name)),
                    format!("pl.lit({}).alias(\"type\")", py_str(&dt.to_string())),
                    format!("{errors}.alias(\"errors\")"),
                    format!("({c}.null_count().cast(pl.UInt64) - {errors}).alias(\"nulls\")"),
                    format!("{valid}.n_unique().cast(pl.UInt64).alias(\"distinct\")"),
                    format!(
                        "{}.alias(\"mode\")",
                        text(!nested, format!("{c}.filter({c}.is_not_null() & ({cnt} == {cnt}.max())).first()"))
                    ),
                    format!("{}.alias(\"min\")", text(ordered, format!("{c}.min()"))),
                    format!("{}.alias(\"max\")", text(ordered, format!("{c}.max()"))),
                    format!("{}.alias(\"sum\")", text(numeric, format!("{c}.sum()"))),
                    format!(
                        "{}.alias(\"median\")",
                        if ordered { median } else { text(false, String::new()) }
                    ),
                    format!("{}.alias(\"mean\")", float(numeric, format!("{c}.mean()"))),
                    format!("{}.alias(\"stdev\")", float(numeric, format!("{c}.std(ddof=1)"))),
                ]
                .join(", ")
            } else {
                let c = q_ident(name);
                let e = cast(name).then(|| q_ident(&err_col(name)));
                let text = |ok: bool, x: String| {
                    if ok { format!("CAST({x} AS VARCHAR)") } else { "CAST(NULL AS VARCHAR)".into() }
                };
                let float = |ok: bool, x: String| {
                    if ok { format!("CAST({x} AS DOUBLE)") } else { "CAST(NULL AS DOUBLE)".into() }
                };
                let errors = match &e {
                    Some(e) => format!("count(*) FILTER (WHERE {e})"),
                    None => "0".into(),
                };
                let null_values = match &e {
                    Some(e) => format!("count(*) FILTER (WHERE {c} IS NULL AND NOT {e})"),
                    None => format!("(count(*) - count({c}))"),
                };
                let mode = format!(
                    "(SELECT v FROM (SELECT v, pos, count(*) OVER (PARTITION BY v) AS n \
                     FROM (SELECT {c} AS v, row_number() OVER () AS pos FROM {p})) \
                     WHERE v IS NOT NULL ORDER BY n DESC, pos LIMIT 1)"
                );
                let median = format!(
                    "CASE WHEN count({c}) = 0 THEN NULL WHEN count({c}) % 2 = 1 \
                     THEN CAST(quantile_disc({c}, 0.5) AS VARCHAR) ELSE {} END",
                    text(numeric, format!("median({c})"))
                );
                format!(
                    "SELECT {i} AS __i, {} AS sheet, {} AS \"column\", {} AS \"type\", \
                     CAST({errors} AS UBIGINT) AS errors, \
                     CAST(count(*) - count({c}) - {errors} AS UBIGINT) AS nulls, \
                     CAST(count(DISTINCT {c}) + ({null_values} > 0)::INT AS UBIGINT) AS \"distinct\", \
                     {} AS mode, {} AS \"min\", {} AS \"max\", {} AS \"sum\", {} AS median, \
                     {} AS mean, {} AS stdev FROM {p}",
                    q_str(&parent.name),
                    q_str(name),
                    q_str(&dt.to_string()),
                    text(!nested, mode),
                    text(ordered, format!("min({c})")),
                    text(ordered, format!("max({c})")),
                    // a sum of no values is 0, as in Polars and VisiData
                    text(numeric, format!("coalesce(sum({c}), 0)")),
                    if ordered { median } else { text(false, String::new()) },
                    float(numeric, format!("avg({c})")),
                    float(numeric, format!("stddev_samp({c})")),
                )
            }
        })
        .collect();
    Ok(if py {
        let sel: Vec<String> = rows.iter().map(|r| format!("{p}.select({r})")).collect();
        format!("pl.concat([\n    {},\n])", sel.join(",\n    "))
    } else {
        format!(
            "SELECT * EXCLUDE (__i) FROM (\n{}\n) ORDER BY __i",
            rows.join("\nUNION ALL\n")
        )
    })
}
