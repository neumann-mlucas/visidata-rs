//! `:export path.py|path.sql` — the current view as a script that rebuilds it
//! from the source files: Python (Polars) or SQL (DuckDB dialect).
//!
//! Per sheet, in `Sheet::view_plan` order: source (a file, or a derivation of
//! the parent snapshot in `state.lineage`) → plan steps (rename, currency,
//! derive, setcol) → type casts → filter → sort → visible columns. Each sheet
//! in the lineage becomes one Python variable / SQL CTE (`t1`, `t2`, …).
//! Not exportable: schema diff, Lua-aggregator pivots, stdin.

use std::path::Path;

use polars::prelude::{DataFrame, DataType};

use crate::reader::SourceKind;
use crate::sheet::{
    Binning, DATE_LAYOUTS, DATE_TIME_PART, DIFF_LEFT, DIFF_RIGHT, Derivation, HISTOGRAM_WIDTH,
    Lineage, Origin, PlanStep, Sheet, SortOrder, err_col, freq_keeps_order, freq_stat_names,
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
                if carries_rid(s) {
                    self.py(format!(
                        "{t} = {t}.with_row_index(\"__rid\").with_columns(pl.col(\"__rid\").cast(pl.UInt64))"
                    ));
                }
                let added = added_rows(s);
                if !added.is_empty() {
                    let frames: Vec<String> = added
                        .iter()
                        .map(|(ids, pos, cols)| {
                            let mut data = vec![
                                format!("\"__rid\": pl.Series({ids:?}, dtype=pl.UInt64)"),
                                format!("\"__pos\": pl.Series({pos:?}, dtype=pl.Float64)"),
                            ];
                            let mut conv = Vec::new();
                            for (name, dtype, texts) in cols {
                                let vals: Vec<String> = texts
                                    .iter()
                                    .map(|v| v.as_deref().map_or("None".into(), py_str))
                                    .collect();
                                data.push(format!(
                                    "{}: pl.Series([{}], dtype=pl.Utf8)",
                                    py_str(name),
                                    vals.join(", ")
                                ));
                                let c = format!("pl.col({})", py_str(name));
                                conv.push(format!(
                                    "{}.alias({})",
                                    py_from_text(&c, dtype),
                                    py_str(name)
                                ));
                            }
                            let conv = if conv.is_empty() {
                                String::new()
                            } else {
                                format!(".with_columns({})", conv.join(", "))
                            };
                            format!("pl.DataFrame({{{}}}).lazy(){conv}", data.join(", "))
                        })
                        .collect();
                    self.py(format!(
                        "{t} = pl.concat([{t}.with_columns(pl.col(\"__rid\").cast(pl.Float64).alias(\"__pos\")), {}], how=\"diagonal_relaxed\")\
                         .sort(\"__pos\", maintain_order=True).drop(\"__pos\")",
                        frames.join(", ")
                    ));
                }
                for step in steps(s) {
                    let line = py_step(&t, step);
                    self.py(line);
                }
                Ok(t)
            }
            Lang::Sql => {
                let mut q = src;
                if carries_rid(s) {
                    q = format!(
                        "SELECT *, CAST(row_number() OVER () - 1 AS UBIGINT) AS __rid FROM ({q})"
                    );
                }
                let mut unions = Vec::new();
                for (ids, pos, cols) in added_rows(s) {
                    let names: Vec<String> = ["__rid".to_string(), "__pos".into()]
                        .into_iter()
                        .chain(cols.iter().map(|(n, _, _)| q_ident(n)))
                        .collect();
                    let rows: Vec<String> = (0..ids.len())
                        .map(|i| {
                            let vals: Vec<String> = [
                                format!("CAST({} AS UBIGINT)", ids[i]),
                                format!("CAST({:?} AS DOUBLE)", pos[i]),
                            ]
                            .into_iter()
                            .chain(cols.iter().map(|(_, dt, texts)| {
                                let v = texts[i].as_deref().map_or("NULL".into(), q_str);
                                format!("TRY_CAST({v} AS {})", sql_dtype(dt))
                            }))
                            .collect();
                            format!("({})", vals.join(", "))
                        })
                        .collect();
                    unions.push(format!(
                        " UNION ALL BY NAME SELECT * FROM (VALUES {}) AS __a({})",
                        rows.join(", "),
                        names.join(", ")
                    ));
                }
                if !unions.is_empty() {
                    q = format!(
                        "SELECT * EXCLUDE (__pos) FROM (SELECT *, CAST(__rid AS DOUBLE) AS __pos FROM ({q}){} ORDER BY __pos)",
                        unions.concat()
                    );
                }
                for step in steps(s) {
                    q = sql_step(q, step);
                }
                Ok(self.bind(q))
            }
        }
    }

    /// An empty source (`A`'s new sheet) with its columns.
    fn empty_source(&mut self, s: &Sheet) -> Result<String, String> {
        let schema = s
            .source
            .lazy
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        Ok(match self.lang {
            Lang::Py => {
                let cols: Vec<String> = schema
                    .iter()
                    .map(|(n, d)| format!("{}: {}", py_str(n), py_dtype(d)))
                    .collect();
                format!("pl.DataFrame(schema={{{}}}).lazy()", cols.join(", "))
            }
            Lang::Sql => {
                let cols: Vec<String> = schema
                    .iter()
                    .map(|(n, d)| format!("CAST(NULL AS {}) AS {}", sql_dtype(d), q_ident(n)))
                    .collect();
                format!("SELECT {} WHERE false", cols.join(", "))
            }
        })
    }

    /// File sheets: the read expression / query.
    fn source(&mut self, s: &Sheet) -> Result<String, String> {
        if !matches!(s.origin, Origin::File | Origin::Tail { .. }) {
            return Err(format!("export: can't rebuild {:?}", s.name));
        }
        let empty = || {
            s.source
                .lazy
                .clone()
                .limit(1)
                .collect()
                .is_ok_and(|d| d.height() == 0)
        };
        if s.source.path.is_none() && empty() {
            return self.empty_source(s); // `A`: rows come from its steps
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
                    (SourceKind::Xlsx, _) => {
                        let read = match part {
                            Some(sh) => format!("pl.read_excel({p}, sheet_name={})", py_str(sh)),
                            None => format!("pl.read_excel({p})"),
                        };
                        format!("{read}.lazy().select([{}])", source_casts(s, true)?)
                    }
                    (SourceKind::Sqlite, _) => {
                        self.need("import sqlite3");
                        format!(
                            "pl.read_database({}, sqlite3.connect({p})).lazy().select([{}])",
                            py_str(&format!("SELECT * FROM {}", q_ident(&table))),
                            source_casts(s, true)?
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
                    SourceKind::Sqlite => format!(
                        "SELECT {} FROM sqlite_scan({p}, {})",
                        source_casts(s, false)?,
                        q_str(&table)
                    ),
                    SourceKind::Xlsx => {
                        self.need("INSTALL excel;\nLOAD excel;");
                        let from = match part {
                            Some(sh) => format!("read_xlsx({p}, sheet = {})", q_str(sh)),
                            None => format!("read_xlsx({p})"),
                        };
                        format!("SELECT {} FROM {from}", source_casts(s, false)?)
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
        // a date `fmtstr` groups freq by the text it shows (S-35 C2)
        let fmt = match &l.how {
            Derivation::Freq { column, .. } => l.parent.date_fmt(column).map(|f| (column, f)),
            _ => None,
        };
        let p = match fmt {
            Some((c, f)) if py => format!(
                "{p}.with_columns(pl.col({c}).dt.strftime({f}))",
                c = py_str(c),
                f = py_str(f)
            ),
            Some((c, f)) => format!(
                "(SELECT * REPLACE (strftime({c}, {f}) AS {c}) FROM {p})",
                c = q_ident(c),
                f = q_str(f)
            ),
            None => p,
        };
        Ok(match &l.how {
            Derivation::Copy if py => p,
            Derivation::Copy => format!("SELECT * FROM {p}"),
            Derivation::Freq { column, bins } if py => {
                let (n, pct, hist) = freq_stat_names(column);
                let order = if freq_keeps_order(&l.parent, column) {
                    String::new()
                } else {
                    format!(
                        ".sort({n}, descending=True, maintain_order=True)",
                        n = py_str(n)
                    )
                };
                let pairs = crate::sheet::columns::freq_aggregators(&l.parent);
                let aggs: String = pairs
                    .iter()
                    .map(|(c, a)| format!(", {}", py_agg(c, a)))
                    .collect();
                let c = py_str(column);
                let grouped = match bins {
                    None => format!(
                        "{p}.group_by({c}, maintain_order=True).agg(pl.len().alias({n}){aggs})",
                        n = py_str(n)
                    ),
                    Some(Binning::Values) => format!(
                        "{p}.group_by({c}, maintain_order=True).agg(pl.len().alias({n}){aggs})\
                         .sort({c}, nulls_last=True, maintain_order=True)",
                        n = py_str(n)
                    ),
                    Some(Binning::Ranges {
                        min,
                        width,
                        n: bins,
                    }) => {
                        let labels: Vec<String> = Binning::labels(*min, *width, *bins)
                            .iter()
                            .map(|l| py_str(l))
                            .collect();
                        let zero: String = pairs
                            .iter()
                            .filter(|(_, a)| crate::sheet::columns::zero_on_empty(a))
                            .map(|(c, a)| format!(", {}", py_str(&format!("{c}_{a}"))))
                            .collect();
                        let names: String = pairs
                            .iter()
                            .map(|(c, a)| format!(", {}", py_str(&format!("{c}_{a}"))))
                            .collect();
                        format!(
                            "{p}.with_columns(((pl.col({c}).cast(pl.Float64) - {min:?}) / {width:?}).floor()\
                             .clip(upper_bound={last}).cast(pl.Int64).alias(\"__vdrs_bin\"))\
                             .group_by(\"__vdrs_bin\", maintain_order=True).agg(pl.len().alias({n}){aggs})\
                             .pipe(lambda g: pl.concat([pl.LazyFrame({{\"__vdrs_bin\": list(range({bins})), {c}: [{labels}]}}, \
                             schema={{\"__vdrs_bin\": pl.Int64, {c}: pl.Utf8}})\
                             .join(g, on=\"__vdrs_bin\", how=\"left\", maintain_order=\"left\")\
                             .with_columns(pl.col([{n}{zero}]).fill_null(0)), \
                             g.filter(pl.col(\"__vdrs_bin\").is_null()).with_columns(pl.lit(None, pl.Utf8).alias({c}))], \
                             how=\"diagonal\")).select([{c}, {n}{names}])",
                            last = bins - 1,
                            labels = labels.join(", "),
                            n = py_str(n),
                        )
                    }
                };
                format!(
                    "{grouped}\
                     .with_columns((pl.col({n}).cast(pl.Float64) * 100 / pl.col({n}).cast(pl.Float64).sum()).round(2).alias({pct}))\
                     .with_columns(pl.lit({bar}).str.head(pl.col({n}) * {w} // pl.col({n}).max()).alias({hist})){order}",
                    n = py_str(n),
                    pct = py_str(pct),
                    hist = py_str(hist),
                    bar = py_str(&"■".repeat(HISTOGRAM_WIDTH)),
                    w = HISTOGRAM_WIDTH,
                )
            }
            Derivation::Freq {
                column,
                bins:
                    Some(Binning::Ranges {
                        min,
                        width,
                        n: bins,
                    }),
            } => {
                let (n, pct, hist) = freq_stat_names(column);
                let pairs = crate::sheet::columns::freq_aggregators(&l.parent);
                let (c, nq) = (q_ident(column), q_ident(n));
                let aggs: String = pairs
                    .iter()
                    .map(|(c, a)| format!(", {}", sql_agg(c, a)))
                    .collect();
                let names: Vec<String> = pairs
                    .iter()
                    .map(|(c, a)| q_ident(&format!("{c}_{a}")))
                    .collect();
                let filled: String = pairs
                    .iter()
                    .zip(&names)
                    .map(
                        |((_, a), q)| match crate::sheet::columns::zero_on_empty(a) {
                            true => format!(", coalesce(g.{q}, 0) AS {q}"),
                            false => format!(", g.{q}"),
                        },
                    )
                    .collect();
                let names: String = names.iter().map(|q| format!(", {q}")).collect();
                // DuckDB's least() skips nulls: a null value has no bin
                let g = format!(
                    "(SELECT CASE WHEN {c} IS NULL THEN NULL ELSE least(floor((CAST({c} AS DOUBLE) - {min:?}) / {width:?}), {last})::BIGINT END AS __vdrs_bin, \
                     count(*) AS {nq}{aggs} FROM {p} GROUP BY 1)",
                    last = bins - 1,
                );
                let labels: Vec<String> = Binning::labels(*min, *width, *bins)
                    .iter()
                    .map(|l| q_str(l))
                    .collect();
                let order = if freq_keeps_order(&l.parent, column) {
                    "__vdrs_bin NULLS LAST".to_string()
                } else {
                    format!("{nq} DESC, __vdrs_bin NULLS LAST")
                };
                format!(
                    "SELECT {c}, {nq}{names}, round({nq} * 100.0 / sum({nq}) OVER (), 2) AS {pct},\n\
                     repeat('■', ({nq} * {w} // max({nq}) OVER ())::INTEGER) AS {hist}\n\
                     FROM (SELECT b.{c}, coalesce(g.{nq}, 0) AS {nq}{filled}, b.__vdrs_bin \
                     FROM (SELECT unnest(range({bins})) AS __vdrs_bin, unnest([{labels}]) AS {c}) b \
                     LEFT JOIN {g} g ON b.__vdrs_bin = g.__vdrs_bin \
                     UNION ALL BY NAME SELECT NULL::VARCHAR AS {c}, * FROM {g} WHERE __vdrs_bin IS NULL) \
                     ORDER BY {order}",
                    labels = labels.join(", "),
                    pct = q_ident(pct),
                    hist = q_ident(hist),
                    w = HISTOGRAM_WIDTH,
                )
            }
            Derivation::Freq { column, bins } => {
                let (n, pct, hist) = freq_stat_names(column);
                // first-seen order: DuckDB keeps the scan's order
                // (preserve_insertion_order)
                let from = format!("(SELECT *, row_number() OVER () AS __pos FROM {p})");
                // `numeric_binning` on few ints: one bin per value, ascending
                let first = match bins {
                    Some(_) => format!("{} NULLS LAST", q_ident(column)),
                    None => "min(__pos)".to_string(),
                };
                let order = if freq_keeps_order(&l.parent, column) {
                    first
                } else {
                    format!("{n} DESC, {first}", n = q_ident(n))
                };
                let aggs: String = crate::sheet::columns::freq_aggregators(&l.parent)
                    .iter()
                    .map(|(c, a)| format!(", {}", sql_agg(c, a)))
                    .collect();
                format!(
                    "SELECT {c}, count(*) AS {n}{aggs}, round(count(*) * 100.0 / sum(count(*)) OVER (), 2) AS {pct},\n\
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
            Derivation::Unfurl { column } => unfurl_code(py, &p, &l.parent, column)?,
            Derivation::PivotCount { keys, on, aggs } if !aggs.is_empty() => {
                if py {
                    py_pivot_aggs(&p, keys, on, aggs)
                } else {
                    sql_pivot_aggs(&p, keys, on, aggs)
                }
            }
            Derivation::PivotCount { keys, on, .. } => {
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

    fn view_query(&mut self, s: &Sheet, rid: bool, errs: bool) -> Result<String, String> {
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
                    let cast = if *dt == DataType::Date && is_text(s, c) {
                        self.need(&sql_lenient_date());
                        format!("lenient_date({i})")
                    } else if dt.is_numeric() && is_date(s, c) {
                        // Polars casts a date to its days since 1970
                        format!("TRY_CAST(({i} - DATE '1970-01-01') AS {})", sql_dtype(dt))
                    } else if dt.is_integer() && is_float(s, c) {
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

fn columns(s: &Sheet) -> Result<Vec<(String, DataType)>, String> {
    let schema = s.view_plan().collect_schema().map_err(|e| e.to_string())?;
    Ok(schema
        .iter()
        .map(|(n, d)| (n.to_string(), d.clone()))
        .collect())
}

use crate::sheet::{JL, JLK, JOIN_RIGHT_KEYS, JR, JRK};

/// `zM` of view `p`, as `Sheet::unfurl_from`.
fn unfurl_code(py: bool, p: &str, parent: &Sheet, column: &str) -> Result<String, String> {
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
            let on = on.join(", ");
            let h = match how {
                "merge" => "full",
                "extend" => "left",
                h => h,
            };
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

/// The steps that rebuild the base; added rows join the source first (as in
/// `Sheet::rebuild_plan`), deleted rows are masked by the view (`py_view` /
/// `view_query`, as in `Sheet::filtered_plan`).
fn steps(s: &Sheet) -> impl Iterator<Item = &PlanStep> {
    s.state
        .steps
        .iter()
        .filter(|s| !matches!(s, PlanStep::DeleteRows { .. } | PlanStep::AddRows { .. }))
}

/// Edits address rows by id, so their scripts carry `__rid` from the source
/// on (`Sheet` row ids: source positions, then added rows').
fn carries_rid(s: &Sheet) -> bool {
    s.state.steps.iter().any(PlanStep::is_edit)
}

/// One `AddRows` step: ids, positions, (base column, type, value texts).
type Added = (
    Vec<u64>,
    Vec<f64>,
    Vec<(String, DataType, Vec<Option<String>>)>,
);

/// `AddRows` rows: (id, position, column → value texts), base column names.
fn added_rows(s: &Sheet) -> Vec<Added> {
    let mut out = Vec::new();
    for step in &s.state.steps {
        let PlanStep::AddRows { rows } = step else {
            continue;
        };
        let ids = crate::sheet::rid_values(rows).unwrap_or_default();
        let pos: Vec<f64> = rows
            .column(crate::sheet::POS)
            .ok()
            .and_then(|c| c.f64().ok().map(|a| a.into_no_null_iter().collect()))
            .unwrap_or_default();
        let cols = rows
            .columns()
            .iter()
            .filter(|c| c.name() != crate::sheet::ROW_ID && c.name() != crate::sheet::POS)
            .map(|c| {
                let texts = crate::sheet::column_texts(c).unwrap_or_default();
                (c.name().to_string(), c.dtype().clone(), texts)
            })
            .collect();
        out.push((ids, pos, cols));
    }
    out
}

/// Polars 2 casts no text to bool/date/datetime: parse those.
fn py_from_text(e: &str, dtype: &DataType) -> String {
    match dtype {
        DataType::Boolean => format!("({e} == \"true\")"),
        DataType::Date => format!("{e}.str.to_date()"),
        DataType::Datetime(..) => format!("{e}.str.to_datetime().cast({})", py_dtype(dtype)),
        _ => format!("{e}.cast({})", py_dtype(dtype)),
    }
}

/// A Rust-regex replacement (`$1`, `${1}`, `$$`) in RE2 syntax (`\\1`).
fn re2_replacement(r: &str) -> String {
    let mut out = String::new();
    let mut it = r.chars().peekable();
    while let Some(c) = it.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '$' if it.peek() == Some(&'$') => {
                it.next();
                out.push('$');
            }
            '$' => {
                let braced = it.peek() == Some(&'{');
                if braced {
                    it.next();
                }
                let mut n = String::new();
                while let Some(d) = it.peek().filter(|d| d.is_ascii_digit()) {
                    n.push(*d);
                    it.next();
                }
                if braced && it.peek() == Some(&'}') {
                    it.next();
                }
                out.push('\\');
                out.push_str(&n);
            }
            c => out.push(c),
        }
    }
    out
}

fn py_step(t: &str, step: &PlanStep) -> String {
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
                    .map(|f| format!(".struct.field({})", py_str(f)))
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

/// A `SetCells` patch as row ids and value texts (null = None).
fn patch_lists(patch: &DataFrame) -> (Vec<u64>, Vec<Option<String>>) {
    let rids = crate::sheet::rid_values(patch).unwrap_or_default();
    let vals = patch.column("__vdrs_v");
    let vals = vals
        .and_then(crate::sheet::column_texts)
        .unwrap_or_default();
    (rids, vals)
}

/// Each plan step wraps the query so far (DuckDB `* RENAME` / `* REPLACE`).
fn sql_step(q: String, step: &PlanStep) -> String {
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
                let get = path.iter().fold(q_ident(column), |e, f| {
                    format!("struct_extract({e}, {})", q_str(f))
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
    // PIVOT sorts the new columns by name; the app keeps them as first seen.
    let text = format!("coalesce(CAST({on} AS VARCHAR), 'null')");
    format!(
        "SELECT * FROM (PIVOT (SELECT {select}{text} AS {on}, {agg} AS __v \
         FROM {p} GROUP BY {}) ON {on} IN (SELECT {on} FROM (SELECT {text} AS {on}, \
         row_number() OVER () AS __r FROM {p}) GROUP BY {on} ORDER BY min(__r)) USING first(__v){group})",
        by.join(", ")
    )
}

/// `W` with aggregators (S-35 B): what [`crate::sheet`]'s `pivot_aggs`
/// does, VisiData's column names (`prefix` + value).
fn pivot_prefixes(aggs: &[(String, String)]) -> Vec<String> {
    let several_cols = aggs.iter().any(|(c, _)| *c != aggs[0].0);
    aggs.iter()
        .map(|(c, a)| match (several_cols, aggs.len()) {
            (true, _) => format!("{c}_{a}_"),
            (false, 1) => String::new(),
            _ => format!("{a}_"),
        })
        .collect()
}

fn py_pivot_aggs(p: &str, keys: &[String], on: &str, aggs: &[(String, String)]) -> String {
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
    let alias = list(
        aggs.iter()
            .map(|(c, a)| py_str(&format!("{c}_{a}")))
            .collect(),
    );
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
        "{p}.with_columns(pl.col({o}).cast(pl.Utf8).fill_null(\"null\")){add_k}\
         .group_by({by}, maintain_order=True).agg({agg_exprs}).collect()\
         .pipe(lambda g: (lambda ps: pl.DataFrame([*[ps[0][k] for k in {ks}], \
         *[(ps[i][v].fill_null(0) if {zero}[i] else ps[i][v]).alias({pre}[i] + v) \
         for v in ps[0].columns if v not in {idx} for i in range(len(ps))]]))\
         ([g.pivot(on={o}, index={idx}, values=a, aggregate_function=\"first\") for a in {alias}])).lazy()"
    )
}

fn sql_pivot_aggs(p: &str, keys: &[String], on: &str, aggs: &[(String, String)]) -> String {
    let k: Vec<String> = keys.iter().map(|k| q_ident(k)).collect();
    let o = q_ident(on);
    let lead = k.iter().map(|c| format!("{c}, ")).collect::<String>();
    let text = format!("coalesce(CAST({o} AS VARCHAR), 'null')");
    let alias: Vec<String> = aggs
        .iter()
        .map(|(c, a)| q_ident(&format!("{c}_{a}")))
        .collect();
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
    // PIVOT names `<value>_<alias>`; VisiData's is `<alias>_<value>`.
    // ponytail: a key column whose name ends in `_<alias>` matches the
    // rename too; name the value columns one by one if that bites.
    let select = if prefixes[0].is_empty() {
        "*".to_string()
    } else {
        let alts: Vec<String> = prefixes
            .iter()
            .map(|s| regex::escape(s.trim_end_matches('_')).replace('\'', "''"))
            .collect();
        format!("{lead}COLUMNS('^(.*)_({})$') AS '\\2_\\1'", alts.join("|"))
    };
    format!(
        "SELECT {select} FROM (PIVOT (SELECT c.*, {filled} FROM {combos} c LEFT JOIN {g} g \
         USING ({using_join})) ON {o} IN (SELECT {o} FROM (SELECT {text} AS {o}, \
         row_number() OVER () AS __r FROM {p}) GROUP BY {o} ORDER BY min(__r)) USING {using}{group})",
        filled = filled.join(", "),
        using_join = using_join.join(", "),
        using = using.join(", "),
    )
}

/// Base column `c` holds text, so `@` parses it leniently (see `Sheet::view_plan`).
/// [`crate::sheet::columns::agg_expr`] in Polars Python.
fn py_agg(c: &str, agg: &str) -> String {
    let m = match agg {
        "count" => "count().cast(pl.Int64)",
        "distinct" => "drop_nulls().n_unique().cast(pl.Int64)",
        "mean" | "avg" => "mean()",
        "mode" => "drop_nulls().mode(maintain_order=True).first()",
        "stdev" => "std()",
        _ => &format!("{agg}()"),
    };
    format!(
        "pl.col({}).{m}.alias({})",
        py_str(c),
        py_str(&format!("{c}_{agg}"))
    )
}

/// [`crate::sheet::columns::agg_expr`] in DuckDB SQL.
fn sql_agg(c: &str, agg: &str) -> String {
    let q = q_ident(c);
    let e = match agg {
        "distinct" => format!("count(DISTINCT {q})"),
        "sum" => format!("coalesce(sum({q}), 0)"),
        "mean" | "avg" => format!("avg({q})"),
        "stdev" => format!("stddev_samp({q})"),
        _ => format!("{agg}({q})"),
    };
    format!("{e} AS {}", q_ident(&format!("{c}_{agg}")))
}

fn is_float(s: &Sheet, c: &str) -> bool {
    let schema = s.plan.clone().collect_schema();
    schema.is_ok_and(|sc| sc.get(c).is_some_and(DataType::is_float))
}

fn is_date(s: &Sheet, c: &str) -> bool {
    let schema = s.plan.clone().collect_schema();
    schema.is_ok_and(|sc| sc.get(c) == Some(&DataType::Date))
}

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

/// xlsx / sqlite columns as vdrs typed them (S-24 R3): numbers, booleans and
/// dates when every cell agreed, else text with empty for null.
fn source_casts(s: &Sheet, py: bool) -> Result<String, String> {
    let schema = s
        .source
        .lazy
        .clone()
        .collect_schema()
        .map_err(|e| e.to_string())?;
    let casts: Vec<String> = schema
        .iter()
        .map(|(n, d)| match (py, d) {
            (true, DataType::String) => {
                format!(
                    "pl.col({c}).cast(pl.Utf8).fill_null(\"\").alias({c})",
                    c = py_str(n)
                )
            }
            (true, d) => format!(
                "pl.col({c}).cast({}).alias({c})",
                py_dtype(d),
                c = py_str(n)
            ),
            (false, DataType::String) => {
                format!("coalesce(CAST({c} AS VARCHAR), '') AS {c}", c = q_ident(n))
            }
            (false, d) => format!("CAST({c} AS {}) AS {c}", sql_dtype(d), c = q_ident(n)),
        })
        .collect();
    Ok(casts.join(", "))
}

/// CSV columns whose type a script must name: dates (only ISO loads as one)
/// and text (`082053` stays text; `reader::csv_schema`).
fn csv_dates(s: &Sheet) -> Result<Vec<(String, DataType)>, String> {
    Ok(csv_columns(s)?
        .iter()
        .filter(|(_, d)| crate::reader::is_date(d) || **d == DataType::String)
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
/// A type `:type json` inferred, as Polars Python writes it.
fn py_json_dtype(dt: &DataType) -> String {
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

/// The same type as DuckDB `from_json`'s structure (JSON text).
fn json_structure(dt: &DataType) -> String {
    let text = |s: &str| {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' | '\\' => out.extend(['\\', c]),
                c if c.is_control() => out += &format!("\\u{:04x}", c as u32),
                c => out.push(c),
            }
        }
        out + "\""
    };
    match dt {
        DataType::Struct(fs) => {
            let fs: Vec<String> = fs
                .iter()
                .map(|f| format!("{}: {}", text(&f.name), json_structure(f.dtype())))
                .collect();
            format!("{{{}}}", fs.join(", "))
        }
        DataType::List(inner) => format!("[{}]", json_structure(inner)),
        DataType::Null => text("VARCHAR"),
        d => text(sql_dtype(d)),
    }
}

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
        DataType::Boolean => "pl.Boolean",
        DataType::Datetime(polars::prelude::TimeUnit::Milliseconds, _) => "pl.Datetime(\"ms\")",
        DataType::Datetime(polars::prelude::TimeUnit::Microseconds, _) => "pl.Datetime(\"us\")",
        DataType::Datetime(polars::prelude::TimeUnit::Nanoseconds, _) => "pl.Datetime(\"ns\")",
        _ => "pl.Utf8",
    }
}

/// The projection `view_plan` applies, or None when it's "all columns, as is".
fn visible_columns(sheet: &Sheet) -> Result<Option<Vec<String>>, String> {
    let v = &sheet.view;
    if v.hidden.is_empty() && v.keys.is_empty() && v.order.is_empty() {
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
            (
                freq_of_copy,
                "group_by(\"name\", maintain_order=True)",
                "GROUP BY \"name\"",
            ),
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
                Sheet::pivot_current_from(&s, 9, &["name".into()], "qty", &[]).unwrap(),
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
