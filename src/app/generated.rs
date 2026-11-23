//! S-55: generated tables and scripts, run through the real `Harness`, checked
//! against oracles that need no expected file: the exported Polars and DuckDB
//! scripts (§4.1), a session save and replay (§4.2), and the app's own
//! invariants after each step (§4.4). S-56's relations live in `relations`.
//!
//! A failure prints the shrunk case as a golden case (`input.csv`, `script`),
//! ready to commit under `tests/golden/`.

use std::io::Write;
use std::sync::LazyLock;

use proptest::prelude::*;

use super::golden::{Restore, restore, roundtrip};
use super::scenario::{Harness, csv_text};
use super::*;

mod relations;
mod undo;
mod walk;

/// One runtime per test process: proptest bodies are sync.
pub(super) static RT: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
});

/// `n` cases unless `PROPTEST_CASES` says otherwise; shrinking stops after
/// a minute, as each step reruns the app.
pub(super) fn config(n: u32) -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|c| c.parse().ok())
        .unwrap_or(n);
    ProptestConfig {
        cases,
        max_shrink_time: 60_000,
        ..ProptestConfig::default()
    }
}

// ---------------------------------------------------------------- tables

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Kind {
    Int,
    Float,
    Text,
    Bool,
    Date,
}

/// Values per kind: edges of the type, and text that looks like other types.
fn pool(kind: Kind) -> &'static [&'static str] {
    match kind {
        // Under 230: an expression multiplies up to 8 values, and on i64
        // overflow DuckDB raises while Polars wraps (S-55 §7).
        Kind::Int => &["0", "-1", "7", "42", "-99", "200"],
        // Not 1e300: DuckDB's stddev overflows on it, Polars gives a value (S-55 §7).
        Kind::Float => &[
            "0.0",
            "-0.0",
            "2.5",
            "-1.25",
            "1e-300",
            "1e15",
            "3.141592653589793",
        ],
        Kind::Text => &[
            "x",
            "y",
            "a b",
            " pad ",
            "q\"t",
            "a,b",
            "tab\there",
            "名前",
            "e\u{301}",
            "007",
            "2018-07-03",
            "NULL",
            "true",
        ],
        Kind::Bool => &["true", "false", "True"],
        Kind::Date => &["2018-07-03", "2020-02-29", "1999-12-31", "7/3/2018"],
    }
}

/// Column names: plain, spaced, quoted, Unicode, SQL keywords, a leading
/// digit, and near-duplicates.
const NAMES: &[&str] = &[
    "a",
    "b",
    "amt",
    "first name",
    "q\"t",
    "名前",
    "select",
    "order",
    "1st",
    "A",
    "x-y",
    "grp",
];

/// A table: a unique integer `id` column first, then 1 to 5 generated ones.
#[derive(Clone, Debug)]
pub(super) struct Table {
    pub names: Vec<String>,
    /// Cells of the generated columns; `None` is an empty (null) cell.
    pub rows: Vec<Vec<Option<String>>>,
}

pub(super) fn table() -> impl Strategy<Value = Table> {
    let kind = prop_oneof![
        Just(Kind::Int),
        Just(Kind::Float),
        Just(Kind::Text),
        Just(Kind::Bool),
        Just(Kind::Date)
    ];
    let column = (kind, prop_oneof![Just(0u32), Just(10), Just(50)]);
    (
        proptest::sample::subsequence(NAMES, 1..=5).prop_shuffle(),
        proptest::collection::vec(column, 5),
        0..30usize,
    )
        .prop_flat_map(|(names, cols, n)| {
            let cols: Vec<(Kind, u32)> = cols[..names.len()].to_vec();
            let cells: Vec<_> = cols
                .iter()
                .map(|&(k, nulls)| {
                    let v = proptest::sample::select(pool(k)).prop_map(str::to_string);
                    let cell = (0..100u32, v).prop_map(move |(p, v)| (p >= nulls).then_some(v));
                    proptest::collection::vec(cell, n)
                })
                .collect();
            (Just(names), cells)
        })
        .prop_map(|(names, cells)| {
            let n = cells.first().map_or(0, Vec::len);
            Table {
                names: names.iter().map(|s| s.to_string()).collect(),
                rows: (0..n)
                    .map(|r| cells.iter().map(|c| c[r].clone()).collect())
                    .collect(),
            }
        })
}

impl Table {
    /// The CSV, quoted by this module (not by vdrs or Polars).
    pub fn csv(&self) -> String {
        let q = |s: &str| {
            if s.contains([',', '"', '\n', '\r']) || s.trim() != s {
                format!("\"{}\"", s.replace('"', "\"\""))
            } else {
                s.to_string()
            }
        };
        let mut out = String::from("id");
        for n in &self.names {
            out.push(',');
            out.push_str(&q(n));
        }
        out.push('\n');
        for (i, row) in self.rows.iter().enumerate() {
            out.push_str(&(i + 1).to_string());
            for c in row {
                out.push(',');
                out.push_str(&c.as_deref().map(q).unwrap_or_default());
            }
            out.push('\n');
        }
        out
    }

    /// A `Harness` over this table, under default options.
    pub async fn open(&self) -> Harness {
        keymap_and_theme(None); // reader options are global
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        tmp.write_all(self.csv().as_bytes()).unwrap();
        tmp.flush().unwrap();
        let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
        Harness::with_source(loaded, tmp).await
    }
}

// ---------------------------------------------------------- expressions

/// A numeric SQL expression. Column leaves pick among the numeric columns of
/// the sheet when the step runs, so most expressions are well typed.
#[derive(Clone, Debug)]
pub(super) enum Num {
    Col(usize),
    Int(i64),
    Float(&'static str),
    Add(Box<Num>, Box<Num>),
    Sub(Box<Num>, Box<Num>),
    Mul(Box<Num>, Box<Num>),
    Length(Txt),
}

#[derive(Clone, Debug)]
pub(super) enum Txt {
    Col(usize),
    Lit(&'static str),
    Upper(Box<Txt>),
    Lower(Box<Txt>),
}

#[derive(Clone, Debug)]
pub(super) enum Pred {
    Lt(Num, Num),
    Eq(Num, Num),
    TxtEq(Txt, Txt),
    IsNull(usize),
    And(Box<Pred>, Box<Pred>),
    Or(Box<Pred>, Box<Pred>),
    Not(Box<Pred>),
    /// Ill typed on purpose: text plus a number (the error path).
    Bad(usize),
}

fn txt() -> impl Strategy<Value = Txt> {
    let leaf = prop_oneof![
        (0..8usize).prop_map(Txt::Col),
        proptest::sample::select(&["x", "", "a''b", "名前", "NULL"][..]).prop_map(Txt::Lit),
    ];
    leaf.prop_recursive(2, 4, 1, |t| {
        prop_oneof![
            t.clone().prop_map(|t| Txt::Upper(Box::new(t))),
            t.prop_map(|t| Txt::Lower(Box::new(t))),
        ]
    })
}

fn num() -> impl Strategy<Value = Num> {
    let leaf = prop_oneof![
        3 => (0..8usize).prop_map(Num::Col),
        1 => (-3..10i64).prop_map(Num::Int),
        1 => proptest::sample::select(&["0.5", "-2.25", "1e10"][..]).prop_map(Num::Float),
        1 => txt().prop_map(Num::Length),
    ];
    leaf.prop_recursive(3, 8, 2, |n| {
        let b =
            |n: BoxedStrategy<Num>| (n.clone(), n).prop_map(|(a, b)| (Box::new(a), Box::new(b)));
        prop_oneof![
            b(n.clone().boxed()).prop_map(|(a, b)| Num::Add(a, b)),
            b(n.clone().boxed()).prop_map(|(a, b)| Num::Sub(a, b)),
            b(n.boxed()).prop_map(|(a, b)| Num::Mul(a, b)),
        ]
    })
}

pub(super) fn pred() -> impl Strategy<Value = Pred> {
    let leaf = prop_oneof![
        3 => (num(), num()).prop_map(|(a, b)| Pred::Lt(a, b)),
        2 => (num(), num()).prop_map(|(a, b)| Pred::Eq(a, b)),
        2 => (txt(), txt()).prop_map(|(a, b)| Pred::TxtEq(a, b)),
        2 => (0..8usize).prop_map(Pred::IsNull),
        1 => (0..8usize).prop_map(Pred::Bad),
    ];
    leaf.prop_recursive(2, 6, 2, |p| {
        prop_oneof![
            (p.clone(), p.clone()).prop_map(|(a, b)| Pred::And(Box::new(a), Box::new(b))),
            (p.clone(), p.clone()).prop_map(|(a, b)| Pred::Or(Box::new(a), Box::new(b))),
            p.prop_map(|p| Pred::Not(Box::new(p))),
        ]
    })
}

/// The visible columns of the active sheet, with their types.
pub(super) struct Schema(Vec<(String, DataType)>);

impl Schema {
    pub fn of(h: &Harness) -> Self {
        let s = h.app.sheets.active();
        let schema = s
            .view_plan()
            .collect_schema()
            .map(|sc| sc.iter().map(|(n, t)| (n.to_string(), t.clone())).collect())
            .unwrap_or_default();
        Schema(schema)
    }

    /// The `i`th column (mod the count), if any.
    pub fn any(&self, i: usize) -> Option<&str> {
        (!self.0.is_empty()).then(|| self.0[i % self.0.len()].0.as_str())
    }

    fn of_type(&self, i: usize, keep: impl Fn(&DataType) -> bool) -> Option<&str> {
        let cols: Vec<&str> = self
            .0
            .iter()
            .filter(|(_, t)| keep(t))
            .map(|(n, _)| n.as_str())
            .collect();
        (!cols.is_empty()).then(|| cols[i % cols.len()])
    }
}

/// A SQL identifier: double quotes, `"` doubled.
fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

impl Num {
    pub fn sql(&self, s: &Schema) -> String {
        match self {
            // Not unsigned (freq and describe counts): `0 - 2` wraps in Polars
            // and raises in DuckDB (S-55 §7).
            Num::Col(i) => s
                .of_type(*i, |t| t.is_float() || t.is_signed_integer())
                .map_or("1".into(), ident),
            Num::Int(n) => n.to_string(),
            Num::Float(f) => f.to_string(),
            Num::Add(a, b) => format!("({} + {})", a.sql(s), b.sql(s)),
            Num::Sub(a, b) => format!("({} - {})", a.sql(s), b.sql(s)),
            Num::Mul(a, b) => format!("({} * {})", a.sql(s), b.sql(s)),
            // Polars' `length` is u32, so `length(x) - 12` wraps (S-55 §7).
            Num::Length(t) => format!("CAST(length({}) AS BIGINT)", t.sql(s)),
        }
    }
}

impl Txt {
    pub fn sql(&self, s: &Schema) -> String {
        match self {
            Txt::Col(i) => s
                .of_type(*i, |t| *t == DataType::String)
                .map_or("'x'".into(), ident),
            Txt::Lit(l) => format!("'{l}'"),
            Txt::Upper(t) => format!("upper({})", t.sql(s)),
            Txt::Lower(t) => format!("lower({})", t.sql(s)),
        }
    }
}

impl Pred {
    pub fn sql(&self, s: &Schema) -> String {
        match self {
            Pred::Lt(a, b) => format!("{} < {}", a.sql(s), b.sql(s)),
            Pred::Eq(a, b) => format!("{} = {}", a.sql(s), b.sql(s)),
            Pred::TxtEq(a, b) => format!("{} = {}", a.sql(s), b.sql(s)),
            Pred::IsNull(i) => s
                .any(*i)
                .map_or("1 IS NULL".into(), |c| format!("{} IS NULL", ident(c))),
            Pred::And(a, b) => format!("({}) AND ({})", a.sql(s), b.sql(s)),
            Pred::Or(a, b) => format!("({}) OR ({})", a.sql(s), b.sql(s)),
            Pred::Not(p) => format!("NOT ({})", p.sql(s)),
            Pred::Bad(i) => s
                .any(*i)
                .map_or("'x' + 1 > 0".into(), |c| format!("{} + 'x' > 0", ident(c))),
        }
    }
}

// ---------------------------------------------------------------- scripts

const REGEXES: &[&str] = &["a", "^$", "1", ".", "[0-9]+", "é", "x|y", "^-", "\\d"];
const VALUES: &[&str] = &["0", "x", "1.5", "2018-07-03", "-1", "名"];
const NEW_NAMES: &[&str] = &["n", "d 2", "q\"n", "名", "id", "a"];
const TYPES: &[&str] = &["int", "float", "string", "date"];

/// One script step. Indexes pick a column of the sheet when the step runs.
#[derive(Clone, Debug)]
pub(super) enum Step {
    Filter { col: usize, re: usize },
    FilterExpr(Pred),
    Unfilter,
    Sort { col: usize, desc: bool },
    Unsort,
    Derive { name: usize, expr: Num },
    Select(Pred),
    SelectAll,
    UnselectAll,
    SetCol { col: usize, value: usize },
    Rename { col: usize, name: usize },
    Type { col: usize, kind: usize },
    Hide { col: usize },
    Unhide,
    Freq { col: usize },
    Describe,
    DupSel,
    Undo,
    Redo,
    Pop,
}

pub(super) fn step() -> impl Strategy<Value = Step> {
    let c = || 0..8usize;
    prop_oneof![
        3 => (c(), 0..REGEXES.len()).prop_map(|(col, re)| Step::Filter { col, re }),
        3 => pred().prop_map(Step::FilterExpr),
        1 => Just(Step::Unfilter),
        3 => (c(), any::<bool>()).prop_map(|(col, desc)| Step::Sort { col, desc }),
        1 => Just(Step::Unsort),
        3 => (0..NEW_NAMES.len() + 4, num()).prop_map(|(name, expr)| Step::Derive { name, expr }),
        2 => pred().prop_map(Step::Select),
        1 => Just(Step::SelectAll),
        1 => Just(Step::UnselectAll),
        2 => (c(), 0..VALUES.len()).prop_map(|(col, value)| Step::SetCol { col, value }),
        2 => (c(), 0..NEW_NAMES.len()).prop_map(|(col, name)| Step::Rename { col, name }),
        2 => (c(), 0..TYPES.len()).prop_map(|(col, kind)| Step::Type { col, kind }),
        1 => c().prop_map(|col| Step::Hide { col }),
        1 => Just(Step::Unhide),
        2 => c().prop_map(|col| Step::Freq { col }),
        1 => Just(Step::Describe),
        1 => Just(Step::DupSel),
        2 => Just(Step::Undo),
        1 => Just(Step::Redo),
        1 => Just(Step::Pop),
    ]
}

pub(super) fn script() -> impl Strategy<Value = Vec<Step>> {
    proptest::collection::vec(step(), 1..=6)
}

/// What a step types: an ex command or keys.
#[derive(Clone, Debug)]
pub(super) enum Input {
    Ex(String),
    Keys(String),
}

impl Input {
    /// The line(s) of a golden `script`.
    pub fn script(&self) -> String {
        match self {
            Input::Ex(e) => format!(":\n> {e}"),
            Input::Keys(k) => k.clone(),
        }
    }

    pub async fn run(&self, h: &mut Harness) {
        match self {
            Input::Ex(e) => {
                h.keys(":").await;
                h.type_(e).await;
            }
            Input::Keys(k) => h.keys(k).await,
        }
    }
}

/// `:cursor 1 <col>`: put the cursor on a column first.
fn on(col: &str) -> Input {
    let a = Action::SetCursor {
        row: 1,
        column: Some(col.to_string()),
    };
    Input::Ex(a.to_ex().expect("cursor has an ex form"))
}

fn ex(a: Action) -> Option<Input> {
    a.to_ex().map(Input::Ex)
}

impl Step {
    /// The inputs for this step on sheet `h`; empty when the step can't run
    /// there (no column, or no ex spelling for its arguments).
    pub fn inputs(&self, h: &Harness) -> Vec<Input> {
        let s = Schema::of(h);
        let col = |i: &usize| s.any(*i).map(str::to_string);
        let keys = |k: &str| vec![Input::Keys(k.into())];
        let some = |i: Option<Input>| i.into_iter().collect::<Vec<_>>();
        let at = |c: Option<String>, then: Input| match c {
            Some(c) => vec![on(&c), then],
            None => vec![],
        };
        match self {
            Step::Filter { col: c, re } => col(c).map_or(vec![], |column| {
                some(ex(Action::SetFilter {
                    column,
                    regex: REGEXES[*re].into(),
                }))
            }),
            Step::FilterExpr(p) => some(ex(Action::SetFilterExpr {
                expr: Some(p.sql(&s)),
            })),
            Step::Unfilter => vec![Input::Ex("unfilter".into())],
            Step::Sort { col: c, desc } => col(c).map_or(vec![], |column| {
                some(ex(Action::SetSort {
                    column,
                    descending: *desc,
                }))
            }),
            Step::Unsort => vec![Input::Ex("unsort".into())],
            Step::Derive { name, expr } => {
                let name = match NEW_NAMES.get(*name) {
                    Some(n) => n.to_string(),
                    None => s.any(*name).unwrap_or("d").to_string(), // overwrite
                };
                some(ex(Action::Derive {
                    name,
                    expr: expr.sql(&s),
                }))
            }
            Step::Select(p) => some(ex(Action::SelectExpr {
                expr: p.sql(&s),
                unselect: false,
            })),
            Step::SelectAll => keys("g s"),
            Step::UnselectAll => keys("g u"),
            Step::SetCol { col: c, value } => match ex(Action::SetCol {
                value: VALUES[*value].into(),
            }) {
                Some(set) => at(col(c), set),
                None => vec![],
            },
            Step::Rename { col: c, name } => {
                match ex(Action::RenameCol {
                    new_name: NEW_NAMES[*name].into(),
                }) {
                    Some(r) => at(col(c), r),
                    None => vec![],
                }
            }
            Step::Type { col: c, kind } => at(col(c), Input::Ex(format!("type {}", TYPES[*kind]))),
            Step::Hide { col: c } => {
                col(c).map_or(vec![], |column| some(ex(Action::HideNamed { column })))
            }
            Step::Unhide => vec![Input::Ex("unhide".into())],
            Step::Freq { col: c } => at(col(c), Input::Keys("F".into())),
            Step::Describe => keys("I"),
            Step::DupSel => keys("\""),
            Step::Undo => keys("U"),
            Step::Redo => keys("R"),
            // `q` on the last sheet quits.
            Step::Pop if h.app.sheets.len() > 1 => keys("q"),
            Step::Pop => vec![],
        }
    }
}

// ---------------------------------------------------------------- running

/// A finished run: the app, and the inputs it typed.
pub(super) struct Run {
    pub h: Harness,
    pub typed: Vec<Input>,
    /// §4.4 breaches, one line each.
    pub unhealthy: Vec<String>,
}

impl Run {
    /// The case as a golden case directory.
    pub fn case(&self, t: &Table) -> String {
        let script: Vec<String> = self.typed.iter().map(Input::script).collect();
        format!(
            "--- input.csv\n{}--- script\n{}\n",
            t.csv(),
            script.join("\n")
        )
    }
}

/// §4.4: the view collects, and the cursor is inside it.
fn health(h: &Harness) -> Option<String> {
    let s = h.app.sheets.active();
    let df = match s.view_plan().collect() {
        Ok(df) => df,
        Err(e) => return Some(format!("view doesn't collect: {e}")),
    };
    let v = &s.view;
    if df.height() > 0 && v.row_cursor >= df.height() {
        return Some(format!(
            "row cursor {} of {} rows",
            v.row_cursor,
            df.height()
        ));
    }
    let cols = h.app.column_names().len();
    if cols > 0 && v.col_cursor >= cols {
        return Some(format!("column cursor {} of {cols} columns", v.col_cursor));
    }
    None
}

/// Run `steps` on `t`, checking §4.4 after each input.
pub(super) async fn run(t: &Table, steps: &[Step]) -> Run {
    let mut h = t.open().await;
    let (mut typed, mut unhealthy) = (Vec::new(), Vec::new());
    for step in steps {
        for input in step.inputs(&h) {
            input.run(&mut h).await;
            if let Some(why) = health(&h) {
                unhealthy.push(format!("after {input:?}: {why}"));
            }
            typed.push(input);
        }
        if h.app.quit {
            break;
        }
    }
    Run {
        h,
        typed,
        unhealthy,
    }
}

proptest! {
    #![proptest_config(config(32))]

    /// §4.4: after each step the view collects and the cursor stays in it.
    #[test]
    fn generated_scripts_keep_the_app_healthy(t in table(), steps in script()) {
        let r = RT.block_on(run(&t, &steps));
        prop_assert!(r.unhealthy.is_empty(), "{}\n{}", r.unhealthy.join("\n"), r.case(&t));
    }

    /// §4.2: a session save and replay rebuilds the final view.
    #[test]
    fn generated_scripts_restore_from_sessions(t in table(), steps in script()) {
        let r = RT.block_on(run(&t, &steps));
        if let Restore::Differs(how) = RT.block_on(restore(&r.h, None)) {
            prop_assert!(false, "{how}\n{}", r.case(&t));
        }
    }
}

proptest! {
    #![proptest_config(config(24))]

    /// §4.1: the exported Polars and DuckDB scripts rebuild the final view,
    /// or refuse it with an `export:` reason.
    #[test]
    fn generated_scripts_export_their_view(t in table(), steps in script()) {
        let r = RT.block_on(run(&t, &steps));
        let want = csv_text(r.h.app.sheets.active(), b',');
        let mut fails = Vec::new();
        roundtrip(&r.h, "generated", (false, true), &want, &mut fails);
        fails.retain(|f| !f.contains(": export: "));
        prop_assert!(fails.is_empty(), "{}\n{}", fails.join("\n"), r.case(&t));
    }
}
