//! `:export path.py|path.sql` — the current view as a script that rebuilds it
//! from the source files: Python (Polars) or SQL (DuckDB dialect).
//!
//! Per sheet, in `Sheet::view_plan` order: source (a file, or a derivation of
//! the parent snapshot in `state.lineage`) → plan steps (rename, currency,
//! derive, setcol) → type casts → filter → sort → visible columns. Each sheet
//! in the lineage becomes one Python variable / SQL CTE (`t1`, `t2`, …).
//! Not exportable: schema diff, Lua-aggregator pivots, stdin.

use std::path::Path;

use polars::prelude::DataType;

use crate::reader::SourceKind;
use crate::sheet::{
    DATE_LAYOUTS, DATE_TIME_PART, DIFF_LEFT, DIFF_RIGHT, Derivation, HISTOGRAM_WIDTH, Lineage,
    Origin, PlanStep, Sheet, SortOrder, err_col, freq_keeps_order, freq_stat_names,
};

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
    sc.need("import polars as pl");
    let v = sc.view(sheet, false)?;
    let mut out = sc.head;
    out.push(String::new());
    out.extend(sc.defs);
    out.push(String::new());
    out.push(format!("print({v}.collect())"));
    Ok(out.join("\n") + "\n")
}

pub fn to_sql(sheet: &Sheet) -> Result<String, String> {
    let mut sc = Script::new(Lang::Sql);
    let q = sc.view_query(sheet, false, false)?;
    let head: String = sc.head.iter().map(|l| l.clone() + "\n").collect();
    // DuckDB 2.0: a PIVOT that reads its columns from the data can't see CTEs
    // and can't live in a view, so those scripts materialize each step.
    const PIVOT: &str = "(PIVOT (";
    let body = if q.contains(PIVOT) || sc.defs.iter().any(|d| d.contains(PIVOT)) {
        sc.defs
            .iter()
            .map(|d| format!("CREATE OR REPLACE TEMP TABLE {d};\n"))
            .collect()
    } else {
        format!("WITH\n{}\n", sc.defs.join(",\n"))
    };
    Ok(format!("{head}{body}{q};\n"))
}

#[derive(Clone, Copy, PartialEq)]
enum Lang {
    Py,
    Sql,
}

/// Python: `defs` are statements; SQL: `defs` are CTEs. `head`: imports /
/// extension loads the sources need.
struct Script {
    lang: Lang,
    head: Vec<String>,
    defs: Vec<String>,
    n: usize,
}

impl Script {
    fn new(lang: Lang) -> Self {
        Script {
            lang,
            head: Vec::new(),
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

    fn need(&mut self, line: &str) {
        if !self.head.iter().any(|l| l == line) {
            self.head.push(line.to_string());
        }
    }

    /// Python: append `name = <stmt>` lines that rework `name` in place.
    fn py(&mut self, line: String) {
        self.defs.push(line);
    }

    /// The sheet's view as a named table. `rid` adds `__rid`: row ids of the
    /// base plan (what selections hold).
    fn view(&mut self, s: &Sheet, rid: bool) -> Result<String, String> {
        self.view_with(s, rid, false)
    }

    /// `errs`: cast columns also carry `err_col(c)`, true where the cast failed.
    fn view_with(&mut self, s: &Sheet, rid: bool, errs: bool) -> Result<String, String> {
        match self.lang {
            Lang::Py => self.py_view(s, rid, errs),
            Lang::Sql => {
                let q = self.view_query(s, rid, errs)?;
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
    fn source(&mut self, s: &Sheet) -> Result<String, String> {
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
        let table = match (kind, part) {
            (SourceKind::Sqlite, None) => crate::reader::sqlite_tables(path)
                .map_err(|e| e.to_string())?
                .into_iter()
                .next()
                .ok_or("export: no tables in the sqlite file")?,
            _ => part.unwrap_or_default().to_string(),
        };
        match self.lang {
            Lang::Py => {
                let p = py_str(&shown);
                let gz = path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("gz"));
                Ok(match (kind, gz) {
                    (SourceKind::Csv | SourceKind::Tsv, false) => {
                        format!("pl.scan_csv({p}{})", py_csv_args(s)?)
                    }
                    (SourceKind::Csv | SourceKind::Tsv, true) => {
                        format!("pl.read_csv({p}{}).lazy()", py_csv_args(s)?)
                    }
                    (SourceKind::Parquet, _) => format!("pl.scan_parquet({p})"),
                    (SourceKind::Arrow, _) => format!("pl.scan_ipc({p})"),
                    (SourceKind::Jsonl, _) => format!("pl.scan_ndjson({p})"),
                    (SourceKind::Json, _) => format!("pl.read_json({p}).lazy()"),
                    (SourceKind::Xlsx, _) => match part {
                        Some(sh) => format!("pl.read_excel({p}, sheet_name={}).lazy()", py_str(sh)),
                        None => format!("pl.read_excel({p}).lazy()"),
                    },
                    (SourceKind::Sqlite, _) => {
                        // vdrs reads sqlite as text, null as ""
                        self.need("import sqlite3");
                        format!(
                            "pl.read_database({}, sqlite3.connect({p})).lazy()\
                             .select(pl.all().cast(pl.Utf8).fill_null(\"\"))",
                            py_str(&format!("SELECT * FROM {}", q_ident(&table)))
                        )
                    }
                    (SourceKind::Duckdb, _) => {
                        self.need("import duckdb");
                        format!(
                            "duckdb.connect({p}, read_only=True).sql({}).pl().lazy()",
                            py_str(&format!("FROM {}", duckdb_table(part)?))
                        )
                    }
                    (SourceKind::Stdin, _) => {
                        return Err("export: stdin can't be re-read".into());
                    }
                })
            }
            Lang::Sql => {
                let p = q_str(&shown);
                Ok(match kind {
                    SourceKind::Csv | SourceKind::Tsv => {
                        // Polars' names, and only the header's columns:
                        // null_padding can add more.
                        let schema = csv_columns(s)?;
                        // SQL has no blank identifiers; such a column becomes `column<i>`.
                        let names: Vec<String> = schema
                            .iter_names()
                            .enumerate()
                            .map(|(i, n)| match n.as_str() {
                                "" => format!("column{i}"),
                                n => n.to_string(),
                            })
                            .collect();
                        let mut folded: Vec<String> =
                            names.iter().map(|n| n.to_lowercase()).collect();
                        folded.sort();
                        if let Some(d) = folded.windows(2).find(|w| w[0] == w[1]) {
                            return Err(format!(
                                "export: DuckDB can't hold columns differing only by case: {:?}",
                                d[0]
                            ));
                        }
                        let quoted: Vec<String> = names.iter().map(|n| q_str(n)).collect();
                        let cols: Vec<String> = names.iter().map(|n| q_ident(n)).collect();
                        format!(
                            "SELECT {} FROM read_csv_auto({p}, names = [{}]{})",
                            cols.join(", "),
                            quoted.join(", "),
                            sql_csv_args(s)?
                        )
                    }
                    SourceKind::Parquet => format!("SELECT * FROM read_parquet({p})"),
                    SourceKind::Jsonl | SourceKind::Json => {
                        format!("SELECT * FROM read_json_auto({p}{})", sql_json_args(s)?)
                    }
                    SourceKind::Sqlite => {
                        format!("SELECT * FROM sqlite_scan({p}, {})", q_str(&table))
                    }
                    SourceKind::Xlsx => {
                        self.need("INSTALL excel;\nLOAD excel;");
                        match part {
                            Some(sh) => {
                                format!("SELECT * FROM read_xlsx({p}, sheet = {})", q_str(sh))
                            }
                            None => format!("SELECT * FROM read_xlsx({p})"),
                        }
                    }
                    SourceKind::Arrow => {
                        self.need("INSTALL nanoarrow FROM community;\nLOAD nanoarrow;");
                        format!("SELECT * FROM read_arrow({p})")
                    }
                    SourceKind::Duckdb => {
                        let db = q_ident(&shown);
                        self.need(&format!("ATTACH IF NOT EXISTS {p} AS {db} (READ_ONLY);"));
                        format!("SELECT * FROM {db}.{}", duckdb_table(part)?)
                    }
                    SourceKind::Stdin => return Err("export: stdin can't be re-read".into()),
                })
            }
        }
    }

    /// The derived table, built from the parent snapshot.
    fn derive(&mut self, l: &Lineage) -> Result<String, String> {
        let rid = matches!(l.how, Derivation::Rids(_));
        let errs = matches!(l.how, Derivation::Describe);
        let p = self.view_with(&l.parent, rid, errs)?;
        let py = self.lang == Lang::Py;
        Ok(match &l.how {
            Derivation::Copy if py => p,
            Derivation::Copy => format!("SELECT * FROM {p}"),
            Derivation::Freq { column } if py => {
                let (n, pct, hist) = freq_stat_names(column);
                let (stable, order) = if freq_keeps_order(&l.parent, column) {
                    (", maintain_order=True", String::new())
                } else {
                    (
                        "",
                        format!(
                            ".sort([{n}, {c}], descending=[True, False], nulls_last=True)",
                            c = py_str(column),
                            n = py_str(n)
                        ),
                    )
                };
                format!(
                    "{p}.group_by({c}{stable}).agg(pl.len().alias({n}))\
                     .with_columns((pl.col({n}).cast(pl.Float64) * 100 / pl.col({n}).cast(pl.Float64).sum()).round(2).alias({pct}))\
                     .with_columns(pl.lit({bar}).str.head(pl.col({n}) * {w} // pl.col({n}).max()).alias({hist})){order}",
                    c = py_str(column),
                    n = py_str(n),
                    pct = py_str(pct),
                    hist = py_str(hist),
                    bar = py_str(&"■".repeat(HISTOGRAM_WIDTH)),
                    w = HISTOGRAM_WIDTH,
                )
            }
            Derivation::Freq { column } => {
                let (n, pct, hist) = freq_stat_names(column);
                let (from, order) = if freq_keeps_order(&l.parent, column) {
                    // DuckDB keeps the scan's order (preserve_insertion_order).
                    (
                        format!("(SELECT *, row_number() OVER () AS __pos FROM {p})"),
                        "min(__pos)".to_string(),
                    )
                } else {
                    (
                        p,
                        format!(
                            "{n} DESC, {c} ASC NULLS LAST",
                            c = q_ident(column),
                            n = q_ident(n)
                        ),
                    )
                };
                format!(
                    "SELECT {c}, count(*) AS {n}, round(count(*) * 100.0 / sum(count(*)) OVER (), 2) AS {pct},\n\
                     repeat('■', (count(*) * {w} // max(count(*)) OVER ())::INTEGER) AS {hist}\n\
                     FROM {from} GROUP BY {c} ORDER BY {order}",
                    c = q_ident(column),
                    n = q_ident(n),
                    pct = q_ident(pct),
                    hist = q_ident(hist),
                    w = HISTOGRAM_WIDTH,
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
                    let query = query.trim().trim_end_matches(';');
                    // Only one WITH per query: the user's joins ours.
                    let word = query.split_whitespace().next().unwrap_or_default();
                    if word.eq_ignore_ascii_case("with") {
                        format!("WITH {},\n{}", ctes.join(", "), &query[4..])
                    } else {
                        format!("WITH {}\n{query}", ctes.join(", "))
                    }
                }
            }
            Derivation::Join { right, keys, how } => {
                let r = self.view(right, false)?;
                join_code(py, &p, &r, &l.parent, right, keys, how, None)?
            }
            Derivation::Diff { other, keys } => {
                let r = self.view(other, false)?;
                let flag = |t: &str, f: &str| {
                    if py {
                        format!("{t}.with_columns(pl.lit(True).alias({}))", py_str(f))
                    } else {
                        format!("(SELECT *, TRUE AS {f} FROM {t})")
                    }
                };
                let (fp, fr) = (flag(&p, DIFF_LEFT), flag(&r, DIFF_RIGHT));
                let j = join_code(
                    py,
                    &fp,
                    &fr,
                    &l.parent,
                    other,
                    keys,
                    "full",
                    Some(DIFF_RIGHT),
                )?;
                diff_code(py, &j, &l.parent, other, keys)?
            }
            Derivation::Describe => describe_code(py, &p, &l.parent)?,
            Derivation::PivotCount { keys, on } => {
                let on_py = py_str(on);
                match (py, keys.is_empty()) {
                    (true, false) => format!(
                        "{p}.with_columns(pl.col({on_py}).cast(pl.Utf8).fill_null(\"null\"))\
                         .group_by([{k}, {on_py}], maintain_order=True).len().collect()\
                         .pivot(on={on_py}, index=[{k}], values=\"len\", aggregate_function=\"first\").lazy()",
                        k = keys
                            .iter()
                            .map(|k| py_str(k))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    (true, true) => format!(
                        "{p}.with_columns(pl.col({on_py}).cast(pl.Utf8).fill_null(\"null\"))\
                         .group_by({on_py}, maintain_order=True).len().with_columns(pl.lit(0).alias(\"__k\")).collect()\
                         .pivot(on={on_py}, index=\"__k\", values=\"len\").drop(\"__k\").lazy()\
                         .select(pl.all().cast(pl.Int64))"
                    ),
                    (false, _) => sql_pivot(&p, keys, on, "count(*)"),
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
                // Aggregate first, as the app's numbers come out: an absent
                // cell stays null; a sum over only nulls is 0.
                let v = q_ident(values);
                let (py_agg, sql_agg) = match agg.as_str() {
                    "sum" => ("sum", format!("coalesce(sum({v}), 0)")),
                    "mean" | "avg" | "average" => ("mean", format!("avg({v})")),
                    "count" | "len" => ("len", "count(*)".to_string()),
                    "max" => ("max", format!("max({v})")),
                    "min" => ("min", format!("min({v})")),
                    "first" => ("first", format!("first({v})")),
                    "last" => ("last", format!("last({v})")),
                    other => {
                        return Err(format!(
                            "export: Lua aggregator {other:?} can't be exported"
                        ));
                    }
                };
                if py {
                    format!(
                        "{p}.group_by([{i}, {o}], maintain_order=True).agg(pl.col({vs}).{py_agg}()).collect()\
                         .pivot(on={o}, index={i}, values={vs}, aggregate_function=\"first\").lazy()",
                        o = py_str(on),
                        i = py_str(index),
                        vs = py_str(values),
                    )
                } else {
                    sql_pivot(&p, std::slice::from_ref(index), on, &sql_agg)
                }
            }
        })
    }

    fn py_view(&mut self, s: &Sheet, rid: bool, errs: bool) -> Result<String, String> {
        let base = self.base(s)?;
        let v = &s.view;
        let mut ops = Vec::new();
        if rid {
            ops.push(".with_row_index(\"__rid\")".to_string());
        }
        for (c, dt) in &v.type_casts {
            let e = py_str(&err_col(c));
            if errs {
                ops.push(format!(
                    ".with_columns(pl.col({}).is_not_null().alias({e}))",
                    py_str(c)
                ));
            }
            if *dt == DataType::Date && is_text(s, c) {
                self.need(&py_lenient_date());
                ops.push(format!(".with_columns(lenient_date({}))", py_str(c)));
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
                ".sort([{}], descending=[{}], maintain_order=True)",
                by.join(", "),
                desc.join(", ")
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

    fn view_query(&mut self, s: &Sheet, rid: bool, errs: bool) -> Result<String, String> {
        let base = self.base(s)?;
        let v = &s.view;
        let mut q = if rid {
            format!("SELECT *, row_number() OVER () - 1 AS __rid FROM {base}")
        } else {
            format!("SELECT * FROM {base}")
        };
        if !v.type_casts.is_empty() {
            let mut flags = String::new();
            let casts: Vec<String> = v
                .type_casts
                .iter()
                .map(|(c, dt)| {
                    let i = q_ident(c);
                    let cast = if *dt == DataType::Date && is_text(s, c) {
                        self.need(&sql_lenient_date());
                        format!("lenient_date({i})")
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

fn columns(s: &Sheet) -> Result<Vec<(String, DataType)>, String> {
    let schema = s.view_plan().collect_schema().map_err(|e| e.to_string())?;
    Ok(schema
        .iter()
        .map(|(n, d)| (n.to_string(), d.clone()))
        .collect())
}

/// `:join` of view `p` (sheet `left`) with view `r` (sheet `right`).
/// `flag`: one more right column to carry through (row diff).
#[allow(clippy::too_many_arguments)]
fn join_code(
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
            let h = if how == "merge" { "full" } else { how };
            let mut e = format!(
                "{p}.join({r}, on=[{}], how={}, coalesce=True, maintain_order=\"left_right\")",
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
            let jt = match how {
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
                .chain(flag.map(q_ident))
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
    })
}

/// Row diff over the full join `j`: tag `__diff`, keep rows only on one side
/// or with a changed shared column (mirrors `Sheet::row_diff_from`).
fn diff_code(
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
fn describe_code(py: bool, p: &str, parent: &Sheet) -> Result<String, String> {
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
                let median = format!(
                    "pl.when({c}.count() == 0).then(pl.lit(None, dtype=pl.Utf8))\
                     .when({c}.count() % 2 == 1).then({c}.drop_nulls().sort().get({c}.count() // 2).cast(pl.Utf8))\
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
                    text(numeric, format!("sum({c})")),
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

fn py_step(t: &str, step: &PlanStep) -> String {
    match step {
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
            "{t} = {t}.with_row_index(\"__rid\").with_columns(pl.when(pl.col(\"__rid\").is_in({rids:?}))\
             .then({v}).otherwise(pl.col({c})).alias({c})).drop(\"__rid\")",
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
            "{t} = {t}.with_row_index(\"__rid\").with_columns(pl.when(pl.col(\"__rid\").is_in({rids:?}))\
             .then(pl.sql_expr({}).cast({t}.collect_schema()[{c}], strict=False)).otherwise(pl.col({c})).alias({c})).drop(\"__rid\")",
            py_str(expr),
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
                "SELECT * EXCLUDE (__rid) REPLACE (CASE WHEN __rid IN ({}) THEN TRY_CAST({} AS {}) ELSE {c} END AS {c}) \
                 FROM (SELECT *, row_number() OVER () - 1 AS __rid FROM ({q}))",
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
                "SELECT * EXCLUDE (__rid) REPLACE (CASE WHEN __rid IN ({}) THEN TRY_CAST(({expr}) AS {}) ELSE {c} END AS {c}) \
                 FROM (SELECT *, row_number() OVER () - 1 AS __rid FROM ({q}))",
                ids.join(", "),
                sql_dtype(dtype),
                c = q_ident(column)
            )
        }
    }
}

/// Pivot `p` on `on`, one cell per `keys` × value holding aggregate `agg`.
/// Grouped first so an absent cell is null, not `agg` over nothing.
fn sql_pivot(p: &str, keys: &[String], on: &str, agg: &str) -> String {
    let k: Vec<String> = keys.iter().map(|k| q_ident(k)).collect();
    let on = q_ident(on);
    let select = k.iter().map(|c| format!("{c}, ")).collect::<String>();
    let by: Vec<String> = (1..=k.len() + 1).map(|i| i.to_string()).collect();
    let group = if k.is_empty() {
        String::new()
    } else {
        format!(" GROUP BY {}", k.join(", "))
    };
    format!(
        "SELECT * FROM (PIVOT (SELECT {select}coalesce(CAST({on} AS VARCHAR), 'null') AS {on}, {agg} AS __v \
         FROM {p} GROUP BY {}) ON {on} USING first(__v){group})",
        by.join(", ")
    )
}

/// Base column `c` holds text, so `@` parses it leniently (see `Sheet::view_plan`).
fn is_text(s: &Sheet, c: &str) -> bool {
    let schema = s.plan.clone().collect_schema();
    schema.is_ok_and(|sc| sc.get(c) == Some(&DataType::String))
}

/// A CSV's columns as vdrs read them. A tail's can change after opening; its
/// rows know them.
fn csv_columns(s: &Sheet) -> Result<std::sync::Arc<polars::prelude::Schema>, String> {
    match &s.state.tail {
        Some(cache) => Ok(cache.df.schema().clone()),
        None => s
            .source
            .lazy
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string()),
    }
}

/// CSV columns that loaded as dates (only ISO does; `reader::scan_csv`).
fn csv_dates(s: &Sheet) -> Result<Vec<(String, DataType)>, String> {
    Ok(csv_columns(s)?
        .iter()
        .filter(|(_, d)| crate::reader::is_date(d))
        .map(|(n, d)| (n.to_string(), d.clone()))
        .collect())
}

/// `pl.scan_csv` arguments matching how vdrs read this file.
fn py_csv_args(s: &Sheet) -> Result<String, String> {
    let mut args =
        ", infer_schema_length=None, truncate_ragged_lines=True, encoding=\"utf8-lossy\""
            .to_string();
    if s.kind == SourceKind::Tsv {
        args += ", separator=\"\\t\"";
    }
    let dates: Vec<String> = csv_dates(s)?
        .iter()
        .map(|(n, d)| format!("{}: pl.{}", py_str(n), py_temporal(d)))
        .collect();
    if !dates.is_empty() {
        args += &format!(", schema_overrides={{{}}}", dates.join(", "));
    }
    Ok(args)
}

fn py_temporal(d: &DataType) -> &'static str {
    if *d == DataType::Date {
        "Date"
    } else {
        "Datetime"
    }
}

/// `read_csv_auto` arguments matching how vdrs read this file: DuckDB must not
/// sniff `7/3/2018` as a date.
fn sql_csv_args(s: &Sheet) -> Result<String, String> {
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
fn sql_json_args(s: &Sheet) -> Result<String, String> {
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

/// Python twin of `sheet::lenient_date`.
fn py_lenient_date() -> String {
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

/// SQL twin of `sheet::lenient_date`.
fn sql_lenient_date() -> String {
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

/// Python string literal.
fn py_str(s: &str) -> String {
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

/// `schema.table` of a duckdb part as SQL; an `:exec-sql` result has no table.
fn duckdb_table(part: Option<&str>) -> Result<String, String> {
    let part = part.ok_or("export: an :exec-sql result can't be re-read")?;
    let (schema, table) = part.split_once('.').unwrap_or(("main", part));
    Ok(format!("{}.{}", q_ident(schema), q_ident(table)))
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
        let (s, t) = sheet();
        let other = crate::reader::open(t.path().as_os_str(), None).unwrap();
        let schema_diff = Sheet::schema_diff_from(&s, 1, &other).unwrap();
        assert!(to_python(&schema_diff).is_err());
        assert!(script_for(&s, Path::new("x.txt")).is_err());
    }

    #[test]
    fn derived_sheets_export_their_lineage() {
        // String checks only; `app::golden` runs scripts through DuckDB and Polars.
        let (s, _t) = sheet();
        let mut r = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        writeln!(r, "name,extra\nalice,x").unwrap();
        let right = Sheet::from_source(5, crate::reader::open(r.path().as_os_str(), None).unwrap());
        let mut sel = s.clone();
        sel.view.selected = std::sync::Arc::new([0u64].into());
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
            (
                Sheet::describe_from(&s, 8).unwrap(),
                "pl.col(\"qty\").n_unique()",
                "ORDER BY __i",
            ),
            (
                Sheet::pivot_current_from(&s, 9, &["name".into()], "qty").unwrap(),
                "index=[\"name\"], values=\"len\"",
                "USING first(__v) GROUP BY \"name\"",
            ),
            (
                Sheet::row_diff_from(&s, 10, &right, &["name".into()]).unwrap(),
                "pl.col(\"__vdrs_right\").is_null()",
                "WHEN __vdrs_right IS NULL THEN 'only_a'",
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

    #[test]
    fn python_strings_escape_control_characters() {
        assert_eq!(
            py_str("a\u{1f}\"b\\\n\u{301}"),
            "\"a\\x1f\\\"b\\\\\\n\u{301}\""
        );
    }

    #[test]
    fn sql_refuses_columns_differing_only_by_case() {
        let t = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        std::fs::write(t.path(), "a,A\n1,2\n").unwrap();
        let loaded = crate::reader::open(t.path().as_os_str(), None).unwrap();
        let s = Sheet::from_source(0, loaded);
        assert!(to_sql(&s).unwrap_err().contains("only by case"));
        assert!(to_python(&s).is_ok());
    }
}
