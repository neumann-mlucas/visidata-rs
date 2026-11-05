//! `:export path.py|path.sql` — the current view as a script that rebuilds it
//! from the source files: Python (Polars) or SQL (DuckDB dialect).
//!
//! Per sheet, in `Sheet::view_plan` order: source (a file, or a derivation of
//! the parent snapshot in `state.lineage`) → plan steps (rename, currency,
//! derive, setcol) → type casts → filter → sort → visible columns. Each sheet
//! in the lineage becomes one Python variable / SQL CTE (`t1`, `t2`, …).
//! Not exportable: describe, diff, `W` pivots, Lua-aggregator pivots, stdin.

use std::path::Path;

use polars::prelude::DataType;

use crate::reader::SourceKind;
use crate::sheet::{Derivation, Lineage, Origin, PlanStep, Sheet, SortOrder, freq_stat_names};

/// Script for `sheet`, language picked by `path`'s extension.
pub fn script_for(sheet: &Sheet, path: &Path) -> Result<String, String> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext.to_ascii_lowercase().as_str() {
        "py" => to_python(sheet),
        "sql" => to_sql(sheet),
        _ => Err(format!(
            "export: use a .py or .sql path, not {:?}",
            path.display().to_string()
        )),
    }
}

pub fn to_python(sheet: &Sheet) -> Result<String, String> {
    let mut sc = Script::new(Lang::Py);
    let v = sc.view(sheet, false)?;
    let mut out = vec!["import polars as pl".to_string(), String::new()];
    out.extend(sc.defs);
    out.push(String::new());
    out.push(format!("print({v}.collect())"));
    Ok(out.join("\n") + "\n")
}

pub fn to_sql(sheet: &Sheet) -> Result<String, String> {
    let mut sc = Script::new(Lang::Sql);
    let q = sc.view_query(sheet, false)?;
    Ok(format!("WITH\n{}\n{q};\n", sc.defs.join(",\n")))
}

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    Py,
    Sql,
}

/// Python: `defs` are statements; SQL: `defs` are CTEs.
struct Script {
    lang: Lang,
    defs: Vec<String>,
    n: usize,
}

impl Script {
    fn new(lang: Lang) -> Self {
        Script {
            lang,
            defs: Vec::new(),
            n: 0,
        }
    }

    /// Name `expr` (a Polars expression / a SQL query) and return the name.
    fn bind(&mut self, expr: String) -> String {
        self.n += 1;
        let name = format!("t{}", self.n);
        self.defs.push(match self.lang {
            Lang::Py => format!("{name} = {expr}"),
            Lang::Sql => format!("{name} AS (\n  {}\n)", expr.replace('\n', "\n  ")),
        });
        name
    }

    /// Python: append `name = <stmt>` lines that rework `name` in place.
    fn py(&mut self, line: String) {
        self.defs.push(line);
    }

    /// The sheet's view as a named table. `rid` adds `__rid`: row ids of the
    /// base plan (what selections hold).
    fn view(&mut self, s: &Sheet, rid: bool) -> Result<String, String> {
        match self.lang {
            Lang::Py => self.py_view(s, rid),
            Lang::Sql => {
                let q = self.view_query(s, rid)?;
                Ok(self.bind(q))
            }
        }
    }

    /// Source + plan steps, named.
    fn base(&mut self, s: &Sheet) -> Result<String, String> {
        let src = match &s.state.lineage {
            Some(l) => self.derive(l)?,
            None => self.source(s)?,
        };
        match self.lang {
            Lang::Py => {
                let t = self.bind(src);
                for step in &s.state.steps {
                    let line = py_step(&t, step);
                    self.py(line);
                }
                Ok(t)
            }
            Lang::Sql => {
                let mut q = src;
                for step in &s.state.steps {
                    q = sql_step(q, step);
                }
                Ok(self.bind(q))
            }
        }
    }

    /// File sheets: the read expression / query.
    fn source(&self, s: &Sheet) -> Result<String, String> {
        if !matches!(s.origin, Origin::File | Origin::Tail { .. }) {
            return Err(format!("export: can't rebuild {:?}", s.name));
        }
        let path = s
            .source
            .path
            .as_deref()
            .ok_or("export: sheet has no source file (stdin?)")?;
        let shown = path.display().to_string();
        // `book.xlsx:Sheet2` / `app.db:users` opened one part of the file
        let part = s.source.display_name.strip_prefix(&format!("{shown}:"));
        let kind = s.kind;
        match self.lang {
            Lang::Py => {
                let p = py_str(&shown);
                let gz = path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("gz"));
                Ok(match (kind, gz) {
                    (SourceKind::Csv, false) => format!("pl.scan_csv({p})"),
                    (SourceKind::Csv, true) => format!("pl.read_csv({p}).lazy()"),
                    (SourceKind::Tsv, false) => format!("pl.scan_csv({p}, separator=\"\\t\")"),
                    (SourceKind::Tsv, true) => {
                        format!("pl.read_csv({p}, separator=\"\\t\").lazy()")
                    }
                    (SourceKind::Parquet, _) => format!("pl.scan_parquet({p})"),
                    (SourceKind::Arrow, _) => format!("pl.scan_ipc({p})"),
                    (SourceKind::Jsonl, _) => format!("pl.scan_ndjson({p})"),
                    (SourceKind::Json, _) => format!("pl.read_json({p}).lazy()"),
                    (SourceKind::Xlsx, _) => match part {
                        Some(sh) => format!("pl.read_excel({p}, sheet_name={}).lazy()", py_str(sh)),
                        None => format!("pl.read_excel({p}).lazy()"),
                    },
                    (SourceKind::Sqlite, _) | (SourceKind::Stdin, _) => {
                        return Err(format!("export: {kind:?} sources aren't supported in .py"));
                    }
                })
            }
            Lang::Sql => {
                let p = q_str(&shown);
                Ok(match kind {
                    SourceKind::Csv => format!("SELECT * FROM read_csv_auto({p})"),
                    SourceKind::Tsv => format!("SELECT * FROM read_csv_auto({p}, delim = '\\t')"),
                    SourceKind::Parquet => format!("SELECT * FROM read_parquet({p})"),
                    SourceKind::Jsonl | SourceKind::Json => {
                        format!("SELECT * FROM read_json_auto({p})")
                    }
                    SourceKind::Sqlite => match part {
                        Some(t) => format!("SELECT * FROM sqlite_scan({p}, {})", q_str(t)),
                        None => format!(
                            "SELECT * FROM sqlite_scan({p}, (SELECT name FROM sqlite_master LIMIT 1))"
                        ),
                    },
                    SourceKind::Arrow | SourceKind::Xlsx | SourceKind::Stdin => {
                        return Err(format!("export: {kind:?} sources aren't supported in .sql"));
                    }
                })
            }
        }
    }

    /// The derived table, built from the parent snapshot.
    fn derive(&mut self, l: &Lineage) -> Result<String, String> {
        let rid = matches!(l.how, Derivation::Rids(_));
        let p = self.view(&l.parent, rid)?;
        let py = self.lang == Lang::Py;
        Ok(match &l.how {
            Derivation::Copy if py => p,
            Derivation::Copy => format!("SELECT * FROM {p}"),
            Derivation::Freq { column } if py => {
                let (n, pct) = freq_stat_names(column);
                format!(
                    "{p}.group_by({c}).agg(pl.len().alias({n}))\
                     .with_columns((pl.col({n}).cast(pl.Float64) * 100 / pl.col({n}).cast(pl.Float64).sum()).round(2).alias({pct}))\
                     .sort([{n}, {c}], descending=[True, False], nulls_last=True)",
                    c = py_str(column),
                    n = py_str(n),
                    pct = py_str(pct)
                )
            }
            Derivation::Freq { column } => {
                let (n, pct) = freq_stat_names(column);
                format!(
                    "SELECT {c}, count(*) AS {n}, round(count(*) * 100.0 / sum(count(*)) OVER (), 2) AS {pct}\n\
                     FROM {p} GROUP BY {c} ORDER BY {n} DESC, {c} ASC NULLS LAST",
                    c = q_ident(column),
                    n = q_ident(n),
                    pct = q_ident(pct)
                )
            }
            Derivation::Rids(rids) => {
                let ids: Vec<String> = rids.iter().map(u64::to_string).collect();
                if py {
                    format!(
                        "{p}.filter(pl.col(\"__rid\").is_in([{}])).drop(\"__rid\")",
                        ids.join(", ")
                    )
                } else {
                    format!(
                        "SELECT * EXCLUDE (__rid) FROM {p} WHERE __rid IN ({})",
                        if ids.is_empty() {
                            "NULL".into()
                        } else {
                            ids.join(", ")
                        }
                    )
                }
            }
            Derivation::Values { column, values } => {
                let text: Vec<&String> = values.iter().flatten().collect();
                let null = values.iter().any(Option::is_none);
                if py {
                    let list: Vec<String> = text.iter().map(|t| py_str(t)).collect();
                    let c = py_str(column);
                    let or_null = if null {
                        format!(" | pl.col({c}).is_null()")
                    } else {
                        String::new()
                    };
                    format!(
                        "{p}.filter(pl.col({c}).cast(pl.Utf8).is_in([{}]){or_null})",
                        list.join(", ")
                    )
                } else {
                    let list: Vec<String> = text.iter().map(|t| q_str(t)).collect();
                    let c = q_ident(column);
                    let mut w = if list.is_empty() {
                        "FALSE".to_string()
                    } else {
                        format!("CAST({c} AS VARCHAR) IN ({})", list.join(", "))
                    };
                    if null {
                        w += &format!(" OR {c} IS NULL");
                    }
                    format!("SELECT * FROM {p} WHERE {w}")
                }
            }
            Derivation::Sql { query, others } => {
                let mut tables = vec![("self".to_string(), p)];
                for o in others {
                    tables.push((format!("s{}", o.id), self.view(o, false)?));
                }
                if py {
                    let frames: Vec<String> = tables
                        .iter()
                        .map(|(n, t)| format!("{}: {t}", py_str(n)))
                        .collect();
                    format!(
                        "pl.SQLContext(frames={{{}}}).execute({})",
                        frames.join(", "),
                        py_str(query)
                    )
                } else {
                    let ctes: Vec<String> = tables
                        .iter()
                        .map(|(n, t)| format!("{n} AS (SELECT * FROM {t})"))
                        .collect();
                    format!(
                        "WITH {}\n{}",
                        ctes.join(", "),
                        query.trim().trim_end_matches(';')
                    )
                }
            }
            Derivation::Join { right, keys, how } => {
                let r = self.view(right, false)?;
                let names = |s: &Sheet| -> Result<Vec<String>, String> {
                    let schema = s.view_plan().collect_schema().map_err(|e| e.to_string())?;
                    Ok(schema.iter_names().map(|n| n.to_string()).collect())
                };
                let (ln, rn) = (names(&l.parent)?, names(right)?);
                // Right columns Polars suffixes `_right` (same name as a left one).
                let dups: Vec<&String> = rn
                    .iter()
                    .filter(|n| !keys.contains(n) && ln.contains(n))
                    .collect();
                match (how.as_str(), py) {
                    ("append", true) => format!(
                        "pl.concat([{p}.with_columns(pl.lit({}).alias(\"origin_sheet\")), \
                         {r}.with_columns(pl.lit({}).alias(\"origin_sheet\"))], how=\"diagonal_relaxed\")",
                        py_str(&l.parent.name),
                        py_str(&right.name)
                    ),
                    ("append", false) => format!(
                        "SELECT *, {} AS origin_sheet FROM {p}\nUNION ALL BY NAME\nSELECT *, {} AS origin_sheet FROM {r}",
                        q_str(&l.parent.name),
                        q_str(&right.name)
                    ),
                    (_, true) => {
                        let on: Vec<String> = keys.iter().map(|k| py_str(k)).collect();
                        let h = if how == "merge" { "full" } else { how.as_str() };
                        let mut e = format!(
                            "{p}.join({r}, on=[{}], how={}, coalesce=True)",
                            on.join(", "),
                            py_str(h)
                        );
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
                        let jt = match how.as_str() {
                            "inner" => "INNER",
                            "left" => "LEFT",
                            _ => "FULL OUTER",
                        };
                        let rcols: Vec<String> = rn
                            .iter()
                            .map(|n| {
                                if dups.contains(&n) {
                                    format!("{} AS {}", q_ident(n), q_ident(&format!("{n}_right")))
                                } else {
                                    q_ident(n)
                                }
                            })
                            .collect();
                        let using: Vec<String> = keys.iter().map(|k| q_ident(k)).collect();
                        let mut q = format!(
                            "SELECT * FROM {p} {jt} JOIN (SELECT {} FROM {r}) USING ({})",
                            rcols.join(", "),
                            using.join(", ")
                        );
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
                }
            }
            Derivation::Unpivot { on, index } if py => format!(
                "{p}.unpivot(on=[{}], index=[{}])",
                on.iter().map(|c| py_str(c)).collect::<Vec<_>>().join(", "),
                index
                    .iter()
                    .map(|c| py_str(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Derivation::Unpivot { on, index } => {
                let keep: Vec<String> = index
                    .iter()
                    .map(|c| q_ident(c))
                    .chain(["\"variable\"".into(), "\"value\"".into()])
                    .collect();
                format!(
                    "SELECT {} FROM {p} UNPIVOT INCLUDE NULLS (\"value\" FOR \"variable\" IN ({}))",
                    keep.join(", "),
                    on.iter().map(|c| q_ident(c)).collect::<Vec<_>>().join(", ")
                )
            }
            Derivation::Pivot {
                index,
                on,
                values,
                agg,
            } => {
                let (py_agg, sql_agg) = match agg.as_str() {
                    "sum" => ("sum", "sum"),
                    "mean" | "avg" | "average" => ("mean", "avg"),
                    "count" | "len" => ("len", "count"),
                    "max" => ("max", "max"),
                    "min" => ("min", "min"),
                    "first" => ("first", "first"),
                    "last" => ("last", "last"),
                    other => {
                        return Err(format!(
                            "export: Lua aggregator {other:?} can't be exported"
                        ));
                    }
                };
                if py {
                    format!(
                        "{p}.collect().pivot(on={}, index={}, values={}, aggregate_function={}).lazy()",
                        py_str(on),
                        py_str(index),
                        py_str(values),
                        py_str(py_agg)
                    )
                } else {
                    format!(
                        "SELECT * FROM (PIVOT {p} ON coalesce(CAST({} AS VARCHAR), 'null') USING {sql_agg}({}) GROUP BY {})",
                        q_ident(on),
                        q_ident(values),
                        q_ident(index)
                    )
                }
            }
        })
    }

    fn py_view(&mut self, s: &Sheet, rid: bool) -> Result<String, String> {
        let base = self.base(s)?;
        let v = &s.view;
        let mut ops = Vec::new();
        if rid {
            ops.push(".with_row_index(\"__rid\")".to_string());
        }
        for (c, dt) in &v.type_casts {
            ops.push(format!(
                ".with_columns(pl.col({c}).cast({}, strict=False))",
                py_dtype(dt),
                c = py_str(c)
            ));
        }
        if let Some(f) = &v.filter {
            ops.push(format!(
                ".filter(pl.col({}).cast(pl.Utf8).str.contains({}))",
                py_str(&f.column),
                py_str(&f.regex)
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
                ".sort([{}], descending=[{}], maintain_order=True)",
                by.join(", "),
                desc.join(", ")
            ));
        }
        if let Some(mut cols) = visible_columns(s)? {
            if rid {
                cols.push("__rid".into());
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

    fn view_query(&mut self, s: &Sheet, rid: bool) -> Result<String, String> {
        let base = self.base(s)?;
        let v = &s.view;
        let mut q = if rid {
            format!("SELECT *, row_number() OVER () - 1 AS __rid FROM {base}")
        } else {
            format!("SELECT * FROM {base}")
        };
        if !v.type_casts.is_empty() {
            let casts: Vec<String> = v
                .type_casts
                .iter()
                .map(|(c, dt)| format!("TRY_CAST({c} AS {}) AS {c}", sql_dtype(dt), c = q_ident(c)))
                .collect();
            q = format!("SELECT * REPLACE ({}) FROM ({q})", casts.join(", "));
        }
        let cols = match visible_columns(s)? {
            Some(mut cols) => {
                if rid {
                    cols.push("__rid".into());
                }
                cols.iter()
                    .map(|c| q_ident(c))
                    .collect::<Vec<_>>()
                    .join(", ")
            }
            None => "*".into(),
        };
        let mut sql = format!("SELECT {cols}\nFROM ({q})");
        if let Some(f) = &v.filter {
            sql += &format!(
                "\nWHERE regexp_matches(CAST({} AS VARCHAR), {})",
                q_ident(&f.column),
                q_str(&f.regex)
            );
        }
        if !v.sort.is_empty() {
            let by: Vec<String> = v
                .sort
                .iter()
                .map(|k| {
                    let dir = if k.order == SortOrder::Desc {
                        "DESC"
                    } else {
                        "ASC"
                    };
                    format!("{} {dir} NULLS FIRST", q_ident(&k.column)) // Polars default
                })
                .collect();
            sql += &format!("\nORDER BY {}", by.join(", "));
        }
        Ok(sql)
    }
}

fn py_step(t: &str, step: &PlanStep) -> String {
    match step {
        PlanStep::Rename { old, new } => {
            format!("{t} = {t}.rename({{{}: {}}})", py_str(old), py_str(new))
        }
        PlanStep::Currency { column } => format!(
            "{t} = {t}.with_columns(pl.col({c}).cast(pl.Utf8).str.replace_all(r\"[^0-9.\\-]\", \"\").cast(pl.Float64))",
            c = py_str(column)
        ),
        PlanStep::Derive { name, expr } => format!(
            "{t} = {t}.with_columns(pl.sql_expr({}).alias({}))",
            py_str(expr),
            py_str(name)
        ),
        PlanStep::SetCol {
            column,
            value,
            rids,
            ..
        } => format!(
            "{t} = {t}.with_row_index(\"__rid\").with_columns(pl.when(pl.col(\"__rid\").is_in({rids:?}))\
             .then(pl.lit({v}).cast({t}.collect_schema()[{c}])).otherwise(pl.col({c})).alias({c})).drop(\"__rid\")",
            v = py_str(value),
            c = py_str(column)
        ),
    }
}

/// Each plan step wraps the query so far (DuckDB `* RENAME` / `* REPLACE`).
fn sql_step(q: String, step: &PlanStep) -> String {
    match step {
        PlanStep::Rename { old, new } => format!(
            "SELECT * RENAME ({} AS {}) FROM ({q})",
            q_ident(old),
            q_ident(new)
        ),
        PlanStep::Currency { column } => format!(
            "SELECT * REPLACE (TRY_CAST(regexp_replace(CAST({c} AS VARCHAR), '[^0-9.\\-]', '', 'g') AS DOUBLE) AS {c}) FROM ({q})",
            c = q_ident(column)
        ),
        PlanStep::Derive { name, expr } => {
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
                "SELECT * EXCLUDE (__rid) REPLACE (CASE WHEN __rid IN ({}) THEN TRY_CAST({} AS {}) ELSE {c} END AS {c}) \
                 FROM (SELECT *, row_number() OVER () - 1 AS __rid FROM ({q}))",
                ids.join(", "),
                q_str(value),
                sql_dtype(dtype),
                c = q_ident(column)
            )
        }
    }
}

/// Python string literal.
fn py_str(s: &str) -> String {
    format!("{s:?}") // Rust's escaping is valid Python for these strings
}

fn py_dtype(dt: &DataType) -> &'static str {
    match dt {
        DataType::Int64 => "pl.Int64",
        DataType::Float64 => "pl.Float64",
        DataType::Date => "pl.Date",
        _ => "pl.Utf8",
    }
}

/// The projection `view_plan` applies, or None when it's "all columns, as is".
fn visible_columns(sheet: &Sheet) -> Result<Option<Vec<String>>, String> {
    let v = &sheet.view;
    if v.hidden.is_empty() && v.keys.is_empty() {
        return Ok(None);
    }
    let schema = sheet
        .view_plan()
        .collect_schema()
        .map_err(|e| e.to_string())?;
    Ok(Some(schema.iter_names().map(|n| n.to_string()).collect()))
}

/// SQL identifier.
fn q_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

/// SQL string literal.
fn q_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

fn sql_dtype(dt: &DataType) -> &'static str {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sheet::{FilterState, SortKey};
    use std::io::Write;

    fn sheet() -> (Sheet, tempfile::NamedTempFile) {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        writeln!(tmp, "name,n,price\nalice,3,$1.50\nbob,1,$2").unwrap();
        tmp.flush().unwrap();
        let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
        let mut s = Sheet::from_source(0, loaded);
        s.plan = s.plan.clone().rename(["n"], ["qty"], true);
        s.state.steps.push(PlanStep::Rename {
            old: "n".into(),
            new: "qty".into(),
        });
        s.derive("double", "qty * 2").unwrap();
        s.view.filter = Some(FilterState {
            column: "name".into(),
            regex: "^a".into(),
        });
        s.view.sort.push(SortKey {
            column: "qty".into(),
            order: SortOrder::Desc,
        });
        s.view.hidden.insert("price".into());
        (s, tmp)
    }

    #[test]
    fn python_script_replays_view() {
        let (s, _t) = sheet();
        let py = to_python(&s).unwrap();
        for want in [
            "pl.scan_csv(",
            "t1 = t1.rename({\"n\": \"qty\"})",
            "pl.sql_expr(\"qty * 2\").alias(\"double\")",
            ".str.contains(\"^a\")",
            "descending=[True]",
            ".select([\"name\", \"qty\", \"double\"])",
        ] {
            assert!(py.contains(want), "missing {want:?} in\n{py}");
        }
    }

    #[test]
    fn sql_script_replays_view() {
        let (s, _t) = sheet();
        let sql = to_sql(&s).unwrap();
        for want in [
            "read_csv_auto(",
            "RENAME (\"n\" AS \"qty\")",
            "(qty * 2) AS \"double\"",
            "SELECT \"name\", \"qty\", \"double\"",
            "regexp_matches(CAST(\"name\" AS VARCHAR), '^a')",
            "ORDER BY \"qty\" DESC",
        ] {
            assert!(sql.contains(want), "missing {want:?} in\n{sql}");
        }
    }

    #[test]
    fn unexportable_sheets_and_bad_extension_refuse() {
        let (s, _t) = sheet();
        let described = Sheet::describe_from(&s, 1).unwrap();
        assert!(to_python(&described).is_err());
        assert!(script_for(&s, Path::new("x.txt")).is_err());
    }

    #[test]
    fn derived_sheets_export_their_lineage() {
        // Verified by hand 2026-11-05: DuckDB and Polars 1.x run these scripts
        // and reproduce the vdrs rows (join row order aside).
        let (s, _t) = sheet();
        let mut r = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        writeln!(r, "name,extra\nalice,x").unwrap();
        let right = Sheet::from_source(5, crate::reader::open(r.path().as_os_str(), None).unwrap());
        let mut sel = s.clone();
        sel.view.selected = [0u64].into_iter().collect();
        let freq_of_copy = Sheet::freq_from(&Sheet::copy_from(&s, 1), 2, "name");
        let cases = [
            (freq_of_copy, "group_by(\"name\")", "GROUP BY \"name\""),
            (
                Sheet::dup_selected_from(&sel, 3).unwrap(),
                "is_in([0])",
                "__rid IN (0)",
            ),
            (
                Sheet::join_from(&s, 4, &right, &["name".into()], "left").unwrap(),
                "how=\"left\"",
                "LEFT JOIN",
            ),
            (
                Sheet::sql_from(&s, 6, "SELECT * FROM s5", vec![right.clone()]).unwrap(),
                "\"s5\": t",
                "s5 AS (SELECT * FROM t",
            ),
        ];
        for (sheet, py, sql) in cases {
            let (p, q) = (to_python(&sheet).unwrap(), to_sql(&sheet).unwrap());
            assert!(
                p.contains(py) && p.contains("t1 = pl.scan_csv("),
                "{py:?} in\n{p}"
            );
            assert!(
                q.contains(sql) && q.contains("read_csv_auto("),
                "{sql:?} in\n{q}"
            );
        }
        // lineage is a snapshot: later edits to the parent don't leak in
        let mut parent = s.clone();
        let copy = Sheet::copy_from(&parent, 7);
        parent.view.filter = None;
        assert!(to_sql(&copy).unwrap().contains("regexp_matches"));
    }
}
