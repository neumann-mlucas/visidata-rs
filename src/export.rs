//! `:export path.py|path.sql` — the current view as a script that rebuilds it
//! from the source files: Python (Polars) or SQL (DuckDB dialect).
//!
//! Per sheet, in `Sheet::view_plan` order: source (a file, or a derivation of
//! the parent snapshot in `state.lineage`) → plan steps (rename, currency,
//! derive, setcol) → type casts → filter → sort → visible columns. Each sheet
//! in the lineage becomes one Python variable / SQL CTE (`t1`, `t2`, …).
//! Not exportable: schema diff, Lua-aggregator pivots, stdin.

mod derived;
mod python;
mod sql;

use derived::*;
use python::*;
use sql::*;
use std::path::Path;

use polars::prelude::{DataFrame, DataType};

use crate::duckdb::{ident as q_ident, lit as q_str};
use crate::reader::SourceKind;
use crate::sheet::columns::agg_name;
use crate::sheet::{
    Binning, DATE_LAYOUTS, DATE_TIME_PART, DIFF_LEFT, DIFF_RIGHT, Derivation, DiveKey,
    HISTOGRAM_WIDTH, Lineage, Origin, PlanStep, Sheet, SortOrder, TIME_OF_DAY, err_col,
    freq_keeps_order, freq_stat_names, pivot_prefixes,
};
use crate::sheet::{JL, JLK, JOIN_RIGHT_KEYS, JR, JRK, MELT_ROW, MELT_VAL, MELT_VAR};

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
    // DuckDB has no SELECT of no columns
    if sheet
        .view_plan()
        .collect_schema()
        .is_ok_and(|s| s.is_empty())
    {
        return Err("export: every column is hidden; nothing left to export".into());
    }
    let mut sc = Script::new(Lang::Sql);
    let q = sc.view_query(sheet, false, false, &[])?;
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
        self.view_with(s, rid, false, &[])
    }

    /// `errs`: cast columns also carry `err_col(c)`, true where the cast failed.
    /// `keep`: hidden columns to keep ([`Sheet::view_plan_keeping`]).
    fn view_with(
        &mut self,
        s: &Sheet,
        rid: bool,
        errs: bool,
        keep: &[String],
    ) -> Result<String, String> {
        match self.lang {
            Lang::Py => self.py_view(s, rid, errs, keep),
            Lang::Sql => {
                let q = self.view_query(s, rid, errs, keep)?;
                Ok(self.bind(q))
            }
        }
    }

    /// Source + plan steps, named.
    fn base(&mut self, s: &Sheet) -> Result<String, String> {
        let src = match &s.state.lineage {
            // its first edit copied its rows into memory (S-45 A)
            Some(_) if s.state.memory_base.is_some() => {
                return Err(format!(
                    "export: {:?} was edited; :save it and export the file",
                    s.name
                ));
            }
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
                let schemas = s.step_schemas().map_err(|e| e.to_string())?;
                for (step, schema) in s.state.steps.iter().zip(&schemas) {
                    if !matches!(step, PlanStep::DeleteRows { .. } | PlanStep::AddRows { .. }) {
                        q = sql_step(q, step, schema);
                    }
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
        let path = s.source.path.as_deref().ok_or(match s.source.via_loader {
            true => "export: rows came from a Lua loader; :save them and export the file",
            false => "export: sheet has no source file (stdin?)",
        })?;
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
        if s.source.xlsx_meta {
            return Err(
                "export: read with xlsx_meta_columns; exports can't read cell colours".into(),
            );
        }
        if s.source.header != 1 {
            return Err(format!(
                "export: read with header {}; exports read the first row as names",
                s.source.header
            ));
        }
        match self.lang {
            Lang::Py => {
                let p = py_str(&shown);
                let gz = path
                    .extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("gz"));
                Ok(match (kind, gz) {
                    (SourceKind::Csv | SourceKind::Tsv, false) => {
                        format!("pl.scan_csv({p}{}){}", py_csv_args(s)?, py_strip(s))
                    }
                    (SourceKind::Csv | SourceKind::Tsv, true) => {
                        format!("pl.read_csv({p}{}).lazy(){}", py_csv_args(s)?, py_strip(s))
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
                    (
                        k @ (SourceKind::Fixed
                        | SourceKind::Usv
                        | SourceKind::Lsv
                        | SourceKind::Jsonla
                        | SourceKind::Yaml
                        | SourceKind::Toml
                        | SourceKind::Html
                        | SourceKind::Zip
                        | SourceKind::Xml
                        | SourceKind::Msgpack),
                        _,
                    ) => return Err(format!("export: Polars has no {k:?} reader")),
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
                        // csv_skipinitialspace (S-48): text cells without leading blanks
                        let cols: Vec<String> = names
                            .iter()
                            .zip(schema.iter_values())
                            .map(
                                |(n, d)| match s.source.csv.stripped && *d == DataType::String {
                                    true => format!("ltrim({c}, ' ') AS {c}", c = q_ident(n)),
                                    false => q_ident(n),
                                },
                            )
                            .collect();
                        // blank lines' all-empty rows left out (S-49)
                        let blank = match s.source.csv.blank_lines {
                            true => format!(
                                " WHERE NOT ({})",
                                names
                                    .iter()
                                    .map(|n| format!("{} IS NULL", q_ident(n)))
                                    .collect::<Vec<_>>()
                                    .join(" AND ")
                            ),
                            false => String::new(),
                        };
                        format!(
                            "SELECT {} FROM read_csv_auto({p}, names = [{}]{}){blank}",
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
                    k @ (SourceKind::Fixed
                    | SourceKind::Usv
                    | SourceKind::Lsv
                    | SourceKind::Jsonla
                    | SourceKind::Yaml
                    | SourceKind::Toml
                    | SourceKind::Html
                    | SourceKind::Zip
                    | SourceKind::Xml
                    | SourceKind::Msgpack) => {
                        return Err(format!("export: DuckDB has no {k:?} reader"));
                    }
                })
            }
        }
    }

    /// The derived table, built from the parent snapshot.
    fn derive(&mut self, l: &Lineage) -> Result<String, String> {
        let rid = matches!(l.how, Derivation::Rids(_));
        let errs = matches!(l.how, Derivation::Describe);
        let p = self.view_with(&l.parent, rid, errs, &l.how.reads())?;
        let py = self.lang == Lang::Py;
        // a date `fmtstr` or a set number format groups freq by the text it
        // shows (S-35 C2, S-36 B)
        let shown: &[String] = match &l.how {
            Derivation::Freq {
                columns,
                bins: None,
            } => columns,
            _ => &[],
        };
        let p = (shown.iter()).fold(p, |p, c| {
            match (l.parent.date_fmt(c), l.parent.num_fmt(c)) {
                (Some(f), _) if py => format!(
                    "{p}.with_columns(pl.col({c}).dt.strftime({f}))",
                    c = py_str(c),
                    f = py_str(f)
                ),
                (Some(f), _) => format!(
                    "(SELECT * REPLACE (strftime({c}, {f}) AS {c}) FROM {p})",
                    c = q_ident(c),
                    f = q_str(f)
                ),
                (None, Some(n)) if py => {
                    let c = py_str(c);
                    format!("{p}.with_columns({}.alias({c}))", n.py(&c))
                }
                (None, Some(n)) => {
                    let c = q_ident(c);
                    format!("(SELECT * REPLACE ({} AS {c}) FROM {p})", n.sql(&c))
                }
                (None, None) => p,
            }
        });
        Ok(match &l.how {
            Derivation::Copy if py => p,
            Derivation::Copy => format!("SELECT * FROM {p}"),
            Derivation::Freq { columns, bins } if py => {
                let (n, pct, hist) = freq_stat_names(columns);
                let order = if freq_keeps_order(&l.parent, columns) {
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
                let c = py_str(columns.first().map_or("", String::as_str));
                let by: Vec<String> = columns.iter().map(|c| py_str(c)).collect();
                let by_value = format!(
                    "{p}.group_by({}, maintain_order=True).agg(pl.len().cast(pl.Int64).alias({n}){aggs})",
                    by.join(", "),
                    n = py_str(n)
                );
                let grouped = match bins {
                    None => by_value,
                    Some(Binning::Values { .. }) => {
                        format!("{by_value}.sort({c}, nulls_last=True, maintain_order=True)")
                    }
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
                            .map(|(c, a)| format!(", {}", py_str(&agg_name(c, a))))
                            .collect();
                        let names: String = pairs
                            .iter()
                            .map(|(c, a)| format!(", {}", py_str(&agg_name(c, a))))
                            .collect();
                        format!(
                            "{p}.with_columns({k}.alias(\"__vdrs_bin\"))\
                             .group_by(\"__vdrs_bin\", maintain_order=True).agg(pl.len().cast(pl.Int64).alias({n}){aggs})\
                             .pipe(lambda g: pl.concat([pl.LazyFrame({{\"__vdrs_bin\": list(range({bins})), {c}: [{labels}]}}, \
                             schema={{\"__vdrs_bin\": pl.Int64, {c}: pl.Utf8}})\
                             .join(g, on=\"__vdrs_bin\", how=\"left\", maintain_order=\"left\")\
                             .with_columns(pl.col([{n}{zero}]).fill_null(0)), \
                             g.filter(pl.col(\"__vdrs_bin\").is_null()).with_columns(pl.lit(None, pl.Utf8).alias({c}))], \
                             how=\"diagonal\")).select([{c}, {n}{names}])",
                            k = py_bin_index(&c, *min, *width, *bins),
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
                columns,
                bins:
                    Some(Binning::Ranges {
                        min,
                        width,
                        n: bins,
                    }),
            } => {
                let column = columns.first().map_or("", String::as_str);
                let (n, pct, hist) = freq_stat_names(columns);
                let pairs = crate::sheet::columns::freq_aggregators(&l.parent);
                let (c, nq) = (q_ident(column), q_ident(n));
                let aggs: String = pairs
                    .iter()
                    .map(|(c, a)| format!(", {}", sql_agg(c, a)))
                    .collect();
                let names: Vec<String> = pairs
                    .iter()
                    .map(|(c, a)| q_ident(&agg_name(c, a)))
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
                let g = format!(
                    "(SELECT {k} AS __vdrs_bin, count(*) AS {nq}{aggs} FROM {p} GROUP BY 1)",
                    k = sql_bin_index(&c, *min, *width, *bins),
                );
                let labels: Vec<String> = Binning::labels(*min, *width, *bins)
                    .iter()
                    .map(|l| q_str(l))
                    .collect();
                let order = if freq_keeps_order(&l.parent, columns) {
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
            Derivation::Freq { columns, bins } => {
                let (n, pct, hist) = freq_stat_names(columns);
                // first-seen order: DuckDB keeps the scan's order
                // (preserve_insertion_order)
                let from = format!("(SELECT *, row_number() OVER () AS __pos FROM {p})");
                // `numeric_binning` on few ints: one bin per value, ascending
                let first = match bins {
                    Some(_) => format!("{} NULLS LAST", q_ident(&columns.concat())),
                    None => "min(__pos)".to_string(),
                };
                let order = if freq_keeps_order(&l.parent, columns) {
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
                    c = (columns.iter().map(|c| q_ident(c)))
                        .collect::<Vec<_>>()
                        .join(", "),
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
            Derivation::Values {
                column,
                key,
                values,
                except,
            } => {
                // as `Sheet::rows_with_values`: a float `-0.0` matches as `0.0`
                let float = matches!(key, DiveKey::Value) && l.parent.is_float_column(column);
                let text: Vec<&String> = values.iter().flatten().collect();
                let null = values.iter().any(Option::is_none);
                if py {
                    let list: Vec<String> = text.iter().map(|t| py_str(t)).collect();
                    let c = py_str(column);
                    let k = match key {
                        DiveKey::Value if float => {
                            format!("pl.when(pl.col({c}) == 0).then(0.0).otherwise(pl.col({c}))")
                        }
                        DiveKey::Value => format!("pl.col({c})"),
                        DiveKey::Fmt(f) => format!("pl.col({c}).dt.strftime({})", py_str(f)),
                        DiveKey::Num(f) => num_fmt(f)?.py(&c),
                        DiveKey::Bin { min, width, n } => py_bin_index(&c, *min, *width, *n),
                    };
                    let or_null = if null {
                        format!(" | {k}.is_null()")
                    } else {
                        String::new()
                    };
                    let pred = format!("{k}.cast(pl.Utf8).is_in([{}]){or_null}", list.join(", "));
                    match except {
                        true => format!("{p}.filter(~({pred}).fill_null(False))"),
                        false => format!("{p}.filter({pred})"),
                    }
                } else {
                    let list: Vec<String> = text.iter().map(|t| q_str(t)).collect();
                    let c = q_ident(column);
                    let k = match key {
                        DiveKey::Value if float => {
                            format!("(CASE WHEN {c} = 0 THEN 0.0 ELSE {c} END)")
                        }
                        DiveKey::Value => c.clone(),
                        DiveKey::Fmt(f) => format!("strftime({c}, {})", q_str(f)),
                        DiveKey::Num(f) => num_fmt(f)?.sql(&c),
                        DiveKey::Bin { min, width, n } => sql_bin_index(&c, *min, *width, *n),
                    };
                    let mut w = if list.is_empty() {
                        "FALSE".to_string()
                    } else {
                        format!("CAST({k} AS VARCHAR) IN ({})", list.join(", "))
                    };
                    if null {
                        w += &format!(" OR {k} IS NULL");
                    }
                    match except {
                        true => format!("SELECT * FROM {p} WHERE NOT COALESCE({w}, FALSE)"),
                        false => format!("SELECT * FROM {p} WHERE {w}"),
                    }
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
            // ponytail: no script; Polars' transpose if exports of it matter
            Derivation::Transpose => return Err("export: a transposed sheet has no script".into()),
            Derivation::Unfurl { column, empty } => unfurl_code(py, &p, &l.parent, column, *empty)?,
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
                        "{p}.with_columns(pl.col({on_py}).cast(pl.Utf8).fill_null(\"\"))\
                         .group_by([{k}, {on_py}], maintain_order=True).len().with_columns(pl.col(\"len\").cast(pl.Int64)).collect()\
                         .pivot(on={on_py}, index=[{k}], values=\"len\", aggregate_function=\"first\").lazy()\
                         .with_columns(pl.exclude([{k}]).fill_null(0))",
                        k = keys
                            .iter()
                            .map(|k| py_str(k))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                    (true, true) => format!(
                        "{p}.with_columns(pl.col({on_py}).cast(pl.Utf8).fill_null(\"\"))\
                         .group_by({on_py}, maintain_order=True).len().with_columns(pl.lit(0).alias(\"__k\")).collect()\
                         .pivot(on={on_py}, index=\"__k\", values=\"len\").drop(\"__k\").lazy()\
                         .select(pl.all().cast(pl.Int64))"
                    ),
                    (false, _) => sql_pivot(&p, keys, on, "count(*)")
                        .replace("USING first(__v)", "USING coalesce(first(__v), 0)"),
                }
            }
            Derivation::Unpivot { on, index } if py => format!(
                "{p}{text}.with_row_index(\"{m}\").unpivot(on=[{}], index=[{}], variable_name=\"{var}\", value_name=\"{val}\")\
                 .filter(pl.col(\"{val}\").is_not_null()).sort(\"{m}\", maintain_order=True).drop(\"{m}\")\
                 .rename({{\"{var}\": \"Variable\", \"{val}\": \"Value\"}})",
                on.iter().map(|c| py_str(c)).collect::<Vec<_>>().join(", "),
                index
                    .iter()
                    .map(String::as_str)
                    .chain([MELT_ROW])
                    .map(py_str)
                    .collect::<Vec<_>>()
                    .join(", "),
                m = MELT_ROW,
                var = MELT_VAR,
                val = MELT_VAL,
                // as `Sheet::unpivot_from`: mixed types melt as text
                text = match l.parent.melt_as_text(on, index).as_slice() {
                    [] => String::new(),
                    t => format!(
                        ".with_columns(pl.col({}).cast(pl.Utf8))",
                        t.iter().map(|c| py_str(c)).collect::<Vec<_>>().join(", ")
                    ),
                },
            ),
            Derivation::Unpivot { on, index } => {
                let keep: Vec<String> = index
                    .iter()
                    .map(|c| q_ident(c))
                    .chain([
                        format!("{MELT_VAR} AS \"Variable\""),
                        format!("{MELT_VAL} AS \"Value\""),
                    ])
                    .collect();
                let text: Vec<String> = (l.parent.melt_as_text(on, index).iter())
                    .map(|c| format!("CAST({0} AS VARCHAR) AS {0}", q_ident(c)))
                    .collect();
                let p = match text.is_empty() {
                    true => p.to_string(),
                    false => format!("(SELECT * REPLACE ({}) FROM {p})", text.join(", ")),
                };
                format!(
                    "SELECT {} FROM {p} UNPIVOT ({MELT_VAL} FOR {MELT_VAR} IN ({}))",
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
                    "sum" => ("sum()", format!("coalesce(sum({v}), 0)")),
                    "mean" | "avg" | "average" => ("mean()", format!("avg({v})")),
                    "count" | "len" => ("len().cast(pl.Int64)", "count(*)".to_string()),
                    "max" => ("max()", format!("max({v})")),
                    "min" => ("min()", format!("min({v})")),
                    "first" => ("first()", format!("first({v})")),
                    "last" => ("last()", format!("last({v})")),
                    other => {
                        return Err(format!(
                            "export: Lua aggregator {other:?} can't be exported"
                        ));
                    }
                };
                if py {
                    format!(
                        "{p}.group_by([{i}, {o}], maintain_order=True).agg(pl.col({vs}).{py_agg}).collect()\
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
}

fn columns(s: &Sheet) -> Result<Vec<(String, DataType)>, String> {
    let schema = s.view_plan().collect_schema().map_err(|e| e.to_string())?;
    Ok(schema
        .iter()
        .map(|(n, d)| (n.to_string(), d.clone()))
        .collect())
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

/// A `Lookup` table as key texts and value texts (null = None).
fn lookup_lists(table: &DataFrame) -> (Vec<String>, Vec<Option<String>>, DataType) {
    let texts = |c: &str| {
        table
            .column(c)
            .and_then(crate::sheet::column_texts)
            .unwrap_or_default()
    };
    let keys = texts("__k")
        .into_iter()
        .map(Option::unwrap_or_default)
        .collect();
    let dtype = table
        .column("__v")
        .map_or(DataType::String, |c| c.dtype().clone());
    (keys, texts("__v"), dtype)
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

/// `c`'s type in `s`'s base plan (before the view's casts).
fn src_type(s: &Sheet, c: &str) -> Option<DataType> {
    s.plan.clone().collect_schema().ok()?.get(c).cloned()
}

/// A CSV's columns as vdrs read them. A tail's can change after opening; its
/// rows know them.
fn csv_columns(s: &Sheet) -> Result<std::sync::Arc<polars::prelude::Schema>, String> {
    match &s.state.tail {
        Some(live) => Ok(live
            .lock()
            .rows()
            .map_err(|e| e.to_string())?
            .schema()
            .clone()),
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

/// The projection `view_plan_keeping` applies, or None when it's "all
/// columns, as is".
fn visible_columns(sheet: &Sheet, keep: &[String]) -> Result<Option<Vec<String>>, String> {
    let v = &sheet.view;
    if v.hidden.is_empty() && v.keys.is_empty() && v.order.is_empty() {
        return Ok(None);
    }
    let names = |mut p: polars::prelude::LazyFrame| -> Result<Vec<String>, String> {
        let schema = p.collect_schema().map_err(|e| e.to_string())?;
        Ok(schema.iter_names().map(|n| n.to_string()).collect())
    };
    let shown = names(sheet.view_plan_keeping(false, keep))?;
    // keys already first, nothing hidden: no select
    Ok((shown != names(sheet.plan.clone())?).then_some(shown))
}

/// `schema.table` of a duckdb part as SQL; an `:exec-sql` result has no table.
fn duckdb_table(part: Option<&str>) -> Result<String, String> {
    let part = part.ok_or("export: an :exec-sql result can't be re-read")?;
    let (schema, table) = part.split_once('.').unwrap_or(("main", part));
    Ok(format!("{}.{}", q_ident(schema), q_ident(table)))
}

/// A number fmtstr a sheet kept; it parsed when set.
fn num_fmt(f: &str) -> Result<crate::numfmt::NumFmt, String> {
    crate::numfmt::NumFmt::parse(f)
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
                "USING coalesce(first(__v), 0) GROUP BY \"name\"",
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

    /// S-49: blank lines between rows drop all-empty rows in both scripts;
    /// ones only at the end drop just those rows (DuckDB skips them itself).
    #[test]
    fn csv_blank_lines_export_as_read() {
        let open = |text: &str| {
            let mut t = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
            t.write_all(text.as_bytes()).unwrap();
            let s = Sheet::from_source(0, crate::reader::open(t.path().as_os_str(), None).unwrap());
            (to_python(&s).unwrap(), to_sql(&s).unwrap(), t)
        };
        let (py, sql, _t) = open("a,b\n1,2\n,\n3,4\n\n\n");
        assert!(
            py.contains(".filter(pl.int_range(pl.len()) < pl.len() - 2)"),
            "{py}"
        );
        assert!(!py.contains("any_horizontal"), "{py}");
        assert!(!sql.contains("IS NULL"), "{sql}");
        let (py, sql, _t) = open("a,b\n1,2\n\n3,4\n");
        assert!(
            py.contains("any_horizontal(pl.all().is_not_null())"),
            "{py}"
        );
        assert!(!py.contains("int_range"), "{py}");
        assert!(
            sql.contains("WHERE NOT (\"a\" IS NULL AND \"b\" IS NULL)"),
            "{sql}"
        );
    }

    #[test]
    fn python_strings_escape_control_characters() {
        assert_eq!(
            py_str("a\u{1f}\"b\\\n\u{301}"),
            "\"a\\x1f\\\"b\\\\\\n\u{301}\""
        );
    }

    /// S-55: a derive on a derived sheet reads that sheet's columns, not its
    /// source's: the SQL export types the step from the derived rows.
    #[test]
    fn derive_on_a_derived_sheet_exports() {
        let (s, _t) = sheet();
        let mut d = Sheet::describe_from(&s, 1).unwrap();
        d.derive("w", "length(mode) - 9").unwrap();
        d.derive("v", "w * 2").unwrap();
        let sql = to_sql(&d).unwrap();
        assert!(sql.contains("length(mode) - 9"), "{sql}");
    }

    #[test]
    fn sql_refuses_columns_differing_only_by_case() {
        let t = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        std::fs::write(t.path(), "a,A\n1,2\n").unwrap();
        let loaded = crate::reader::open(t.path().as_os_str(), None).unwrap();
        let s = Sheet::from_source(0, loaded);
        assert!(to_sql(&s).unwrap_err().contains("only by case"));
        assert!(to_python(&s).is_ok());
        // S-55: a derive makes the clash
        std::fs::write(t.path(), "A\n1\n").unwrap();
        let loaded = crate::reader::open(t.path().as_os_str(), None).unwrap();
        let mut s = Sheet::from_source(0, loaded);
        s.derive("a", "\"A\" * 2").unwrap();
        assert!(to_sql(&s).unwrap_err().contains("only by case"));
        // S-55: no column shown
        s.view.hidden.extend(["A".to_string(), "a".to_string()]);
        assert!(to_sql(&s).unwrap_err().contains("every column is hidden"));
    }
}
