//! S-56: metamorphic relations. Each runs a short generated script, then
//! takes two vdrs code paths that must give the same answer. Numbers in the
//! test names are the relation numbers in S-56 §3.

use std::collections::BTreeMap;

use super::super::golden::table as normalized;
use super::*;
use crate::sheet::PlanStep;

/// A row as column → normalized text; nulls are absent.
type Record = BTreeMap<String, String>;

/// Cell text that compares across types and engines: numbers in one float
/// spelling, so `7`, `7.0` and `"7"` agree.
fn norm(v: AnyValue) -> Option<String> {
    let s = match v {
        AnyValue::Null => return None,
        AnyValue::String(s) => s.to_string(),
        AnyValue::StringOwned(s) => s.to_string(),
        // not `to_string`: it shows 6 digits
        v if v.dtype().is_primitive_numeric() => v.extract::<f64>()?.to_string(),
        v => v.to_string().trim_matches('"').to_string(),
    };
    Some(match s.parse::<f64>() {
        Ok(f) => format!("{:.9e}", if f.abs() < 1e-12 { 0.0 } else { f }),
        Err(_) => s,
    })
}

fn frame(s: &Sheet) -> DataFrame {
    s.view_plan().collect().unwrap()
}

/// The view's rows in view order.
fn records(s: &Sheet) -> Vec<Record> {
    let df = frame(s);
    (0..df.height())
        .map(|r| {
            df.columns()
                .iter()
                .filter_map(|c| Some((c.name().to_string(), norm(c.get(r).ok()?)?)))
                .collect()
        })
        .collect()
}

/// The view's rows as a multiset.
fn bag(s: &Sheet) -> Vec<Record> {
    let mut r = records(s);
    r.sort();
    r
}

fn names(s: &Sheet) -> Vec<String> {
    frame(s)
        .get_column_names()
        .iter()
        .map(|n| n.to_string())
        .collect()
}

fn numbers(df: &DataFrame, c: &str) -> Vec<Option<f64>> {
    let col = df.column(c).unwrap().cast(&DataType::Float64).unwrap();
    col.f64().unwrap().iter().collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * a.abs().max(b.abs()).max(1.0)
}

async fn ex(h: &mut Harness, line: impl Into<String>) {
    Input::Ex(line.into()).run(h).await;
}

async fn keys(h: &mut Harness, k: &str) {
    h.keys(k).await;
}

async fn act(h: &mut Harness, a: Action) {
    ex(h, a.to_ex().expect("relation actions have an ex form")).await;
}

/// The prefix: a table and up to 3 generated steps.
fn prefix() -> impl Strategy<Value = (Table, Vec<Step>)> {
    (table(), proptest::collection::vec(step(), 0..=3))
}

/// Two apps that ran the same prefix, and the case text.
async fn twins(t: &Table, steps: &[Step]) -> (Harness, Harness, String) {
    let a = run(t, steps).await;
    let case = a.case(t);
    (a.h, run(t, steps).await.h, case)
}

/// The app after the prefix, with no regex filter, no expression filter
/// and nothing selected.
async fn plain(t: &Table, steps: &[Step]) -> (Harness, String) {
    let r = run(t, steps).await;
    let case = r.case(t);
    let mut h = r.h;
    ex(&mut h, "unfilter").await;
    keys(&mut h, "g u").await;
    (h, case)
}

fn column(h: &Harness, i: usize) -> Option<String> {
    Schema::of(h).any(i).map(str::to_string)
}

fn numeric(h: &Harness, i: usize) -> Option<String> {
    let s = h.app.sheets.active();
    let cols: Vec<String> = s
        .view_plan()
        .collect_schema()
        .ok()?
        .iter()
        .filter(|(_, t)| t.is_primitive_numeric())
        .map(|(n, _)| n.to_string())
        .collect();
    (!cols.is_empty()).then(|| cols[i % cols.len()].clone())
}

/// `id` is shown, unique and never null in the view.
fn unique_id(s: &Sheet) -> bool {
    let df = frame(s);
    df.column("id").is_ok_and(|c| {
        c.null_count() == 0 && c.as_materialized_series().n_unique().ok() == Some(df.height())
    })
}

fn pushed(h: &Harness, before: usize) -> bool {
    h.app.sheets.len() > before
}

proptest! {
    #![proptest_config(config(12))]

    /// 1: `|` on a column, then `"`, gives the rows `:filter` keeps.
    #[test]
    fn rel_01_select_regex_then_copy_equals_filter((t, steps) in prefix(), c in 0..8usize, re in 0..REGEXES.len()) {
        let ok = RT.block_on(async {
            let (mut a, case) = plain(&t, &steps).await;
            let (mut b, _) = plain(&t, &steps).await;
            let Some(col) = column(&a, c) else { return Ok(()) };
            let Some(filter) = (Action::SetFilter { column: col.clone(), regex: REGEXES[re].into() }).to_ex() else {
                return Ok(());
            };
            on(&col).run(&mut a).await;
            keys(&mut a, "|").await;
            a.type_(REGEXES[re]).await;
            let n = a.app.sheets.len();
            keys(&mut a, "\"").await;
            ex(&mut b, filter).await;
            let want = bag(b.app.sheets.active());
            let got = if pushed(&a, n) { bag(a.app.sheets.active()) } else { vec![] };
            if got == want { Ok(()) } else { Err(format!("{col} ~ {}\n{case}", REGEXES[re])) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 2: `:select p`, then `"`, gives the rows `:filter-expr p` keeps.
    #[test]
    fn rel_02_select_expr_then_copy_equals_filter_expr((t, steps) in prefix(), p in pred()) {
        let ok = RT.block_on(async {
            let (mut a, case) = plain(&t, &steps).await;
            let (mut b, _) = plain(&t, &steps).await;
            let p = p.sql(&Schema::of(&a));
            act(&mut b, Action::SetFilterExpr { expr: Some(p.clone()) }).await;
            if b.app.sheets.active().view.filter_expr.is_none() {
                return Ok(()); // refused: an ill-typed predicate
            }
            act(&mut a, Action::SelectExpr { expr: p.clone(), unselect: false }).await;
            let n = a.app.sheets.len();
            keys(&mut a, "\"").await;
            let want = bag(b.app.sheets.active());
            let got = if pushed(&a, n) { bag(a.app.sheets.active()) } else { vec![] };
            if got == want { Ok(()) } else { Err(format!("{p}\n{case}")) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 3: `gs` then `"` equals `g"`.
    #[test]
    fn rel_03_select_all_then_copy_equals_copy((t, steps) in prefix()) {
        let ok = RT.block_on(async {
            let (mut a, mut b, case) = twins(&t, &steps).await;
            let n = a.app.sheets.len();
            keys(&mut a, "g s").await;
            keys(&mut a, "\"").await;
            keys(&mut b, "g \"").await;
            let want = bag(b.app.sheets.active());
            let got = if pushed(&a, n) { bag(a.app.sheets.active()) } else { vec![] };
            if got == want { Ok(()) } else { Err(case) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 4: `:sort k` then `:unsort` gives back the order before the sort.
    #[test]
    fn rel_04_sort_then_unsort_restores_order((t, steps) in prefix(), c in 0..8usize, desc in any::<bool>()) {
        let ok = RT.block_on(async {
            let mut r = run(&t, &steps).await;
            let case = r.case(&t);
            let h = &mut r.h;
            ex(h, "unsort").await;
            let before = records(h.app.sheets.active());
            let Some(column) = column(h, c) else { return Ok(()) };
            act(h, Action::SetSort { column, descending: desc }).await;
            ex(h, "unsort").await;
            if records(h.app.sheets.active()) == before { Ok(()) } else { Err(case) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 5: a sort keeps every row, past the first 1024-row window too.
    #[test]
    fn rel_05_sort_keeps_every_row(t in table(), c in 0..8usize, desc in any::<bool>()) {
        let mut t = t;
        if !t.rows.is_empty() {
            let rows = t.rows.clone();
            t.rows = rows.iter().cycle().take(1500).cloned().collect();
        }
        let ok = RT.block_on(async {
            let mut h = t.open().await;
            let before = bag(h.app.sheets.active());
            let Some(column) = column(&h, c) else { return Ok(()) };
            act(&mut h, Action::SetSort { column: column.clone(), descending: desc }).await;
            h.keys("G").await; // the last window
            if bag(h.app.sheets.active()) == before { Ok(()) } else { Err(format!("sort {column}")) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 6 and 7: freq counts sum to the rows below, and Enter on a freq row
    /// opens exactly that many rows, all with one value.
    #[test]
    fn rel_06_07_freq_counts_and_dives((t, steps) in prefix(), c in 0..8usize) {
        let ok = RT.block_on(async {
            let mut r = run(&t, &steps).await;
            let case = r.case(&t);
            let h = &mut r.h;
            let Some(col) = column(h, c) else { return Ok(()) };
            let below = frame(h.app.sheets.active()).height();
            on(&col).run(h).await;
            let n = h.app.sheets.len();
            keys(h, "F").await;
            if !pushed(h, n) { return Ok(()) }
            let freq = frame(h.app.sheets.active());
            // the column after the value: `count`, or `freq_count` when the
            // sheet below has a `count` (a freq of a freq)
            let count = freq.get_column_names()[1].to_string();
            let counts = numbers(&freq, &count);
            let sum: f64 = counts.iter().flatten().sum();
            if sum as usize != below {
                return Err(format!("freq of {col} counts {sum}, the sheet has {below} rows\n{case}"));
            }
            for (i, count) in counts.iter().enumerate().take(6) {
                act(h, Action::SetCursor { row: i as u64 + 1, column: None }).await;
                let n = h.app.sheets.len();
                keys(h, "<CR>").await;
                if !pushed(h, n) { return Err(format!("no dive on freq row {i}\n{case}")) }
                let dive = frame(h.app.sheets.active());
                let values = dive.column(&col).map(|c| c.as_materialized_series().n_unique().unwrap_or(0));
                if Some(dive.height() as f64) != *count || values.is_ok_and(|v| v > 1) {
                    return Err(format!(
                        "freq row {i} of {col} counts {count:?}, its dive has {} rows\n{case}",
                        dive.height()
                    ));
                }
                keys(h, "q").await;
            }
            Ok(())
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 8: freq on a column equals SQL's group-by count of it.
    #[test]
    fn rel_08_freq_equals_sql_group_by((t, steps) in prefix(), c in 0..8usize) {
        let ok = RT.block_on(async {
            let (mut a, mut b, case) = twins(&t, &steps).await;
            let Some(col) = column(&a, c) else { return Ok(()) };
            on(&col).run(&mut a).await;
            let n = a.app.sheets.len();
            keys(&mut a, "F").await;
            act(&mut b, Action::Sql {
                query: format!("SELECT {0}, count(*) AS count FROM self GROUP BY {0}", ident(&col)),
            })
            .await;
            if !pushed(&a, n) || !pushed(&b, n) { return Ok(()) }
            // the value and its count (a freq of a freq names it `freq_count`)
            let pick = |s: &Sheet, count: &str| {
                let mut r: Vec<Record> = records(s)
                    .into_iter()
                    .map(|r| {
                        r.into_iter()
                            .filter(|(k, _)| *k == col || k == count)
                            .map(|(k, v)| (if k == count { "count".into() } else { k }, v))
                            .collect()
                    })
                    .collect();
                r.sort();
                r
            };
            let count = names(a.app.sheets.active())[1].clone();
            let (want, got) = (pick(b.app.sheets.active(), "count"), pick(a.app.sheets.active(), &count));
            if got == want { Ok(()) } else { Err(format!("freq {col}\n{got:?}\nsql\n{want:?}\n{case}")) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 9: `describe` min, max and mean of a numeric column equal its footer
    /// aggregators.
    #[test]
    fn rel_09_describe_equals_the_footer((t, steps) in prefix(), c in 0..8usize) {
        let ok = RT.block_on(async {
            let (mut a, mut b, case) = twins(&t, &steps).await;
            let Some(col) = numeric(&a, c) else { return Ok(()) };
            on(&col).run(&mut a).await;
            act(&mut a, Action::AggregateCol { names: "min max mean".into() }).await;
            let Ok((_, totals)) = a.app.sheets.active().footer_totals() else { return Ok(()) };
            let n = b.app.sheets.len();
            keys(&mut b, "I").await;
            if !pushed(&b, n) { return Ok(()) }
            let d = frame(b.app.sheets.active());
            let Some(row) = (0..d.height()).find(|&r| {
                d.column("column").ok().and_then(|c| c.get(r).ok()).and_then(norm) == Some(col.clone())
            }) else {
                return Err(format!("describe has no row for {col}\n{case}"));
            };
            let footer: BTreeMap<String, Option<f64>> = totals
                .iter()
                .filter(|(c, _)| *c == col)
                .flat_map(|(_, t)| t.split(" · "))
                .filter_map(|item| item.split_once(' '))
                .map(|(agg, v)| (agg.to_string(), v.replace(',', "").parse().ok()))
                .collect();
            for agg in ["min", "max", "mean"] {
                let described = numbers(&d, agg)[row];
                let shown = footer.get(agg).copied().flatten();
                let same = match (described, shown) {
                    // the footer shows a few digits
                    (Some(x), Some(y)) => (x - y).abs() <= 0.01 * x.abs().max(1.0),
                    (None, None) => true,
                    _ => false,
                };
                if !same {
                    return Err(format!("{agg} of {col}: describe {described:?}, footer {shown:?}\n{case}"));
                }
            }
            Ok(())
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 10: the cells of a `sum` pivot add up to the column's sum.
    #[test]
    fn rel_10_pivot_cells_sum_to_the_column((t, steps) in prefix(), on_i in 0..8usize, v in 0..8usize) {
        let ok = RT.block_on(async {
            let (mut a, case) = plain(&t, &steps).await;
            let (Some(on_col), Some(values)) = (column(&a, on_i), numeric(&a, v)) else { return Ok(()) };
            if !unique_id(a.app.sheets.active()) || on_col == "id" || values == "id" || on_col == values {
                return Ok(());
            }
            let want: f64 = numbers(&frame(a.app.sheets.active()), &values).iter().flatten().sum();
            let n = a.app.sheets.len();
            act(&mut a, Action::Pivot { index: "id".into(), on: on_col.clone(), values: values.clone(), agg: "sum".into() }).await;
            if !pushed(&a, n) { return Ok(()) }
            let p = frame(a.app.sheets.active());
            let got: f64 = p
                .get_column_names()
                .iter()
                .filter(|c| c.as_str() != "id")
                .filter(|c| p.column(c.as_str()).is_ok_and(|c| c.dtype().is_primitive_numeric()))
                .map(|c| numbers(&p, c.as_str()).iter().flatten().sum::<f64>())
                .sum();
            if close(got, want) { Ok(()) } else { Err(format!("pivot on {on_col} of {values}: {got} != {want}\n{case}")) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 11: unpivot every column but `id`, then pivot back, gives each cell
    /// back (melt drops nulls, so compare the non-null cells).
    #[test]
    fn rel_11_unpivot_then_pivot_restores(t in table()) {
        let ok = RT.block_on(async {
            let mut h = t.open().await;
            let cols: Vec<String> = names(h.app.sheets.active()).into_iter().filter(|c| c != "id").collect();
            let Some(line) = (Action::Unpivot { on: cols.clone(), index: vec!["id".into()] }).to_ex() else {
                return Ok(());
            };
            let cells = |s: &Sheet| -> BTreeMap<(String, String), String> {
                let rows = records(s);
                rows.iter()
                    .flat_map(|r| {
                        let id = r.get("id").cloned().unwrap_or_default();
                        r.iter()
                            .filter(|(k, _)| *k != "id")
                            .map(move |(k, v)| ((id.clone(), k.clone()), v.clone()))
                    })
                    .collect()
            };
            let want = cells(h.app.sheets.active());
            let n = h.app.sheets.len();
            ex(&mut h, line).await;
            if !pushed(&h, n) { return Err(format!("unpivot refused\n{}", t.csv())) }
            let melted = names(h.app.sheets.active());
            let [_, var, val] = melted.as_slice() else {
                return Err(format!("unpivot columns {melted:?}"));
            };
            act(&mut h, Action::Pivot { index: "id".into(), on: var.clone(), values: val.clone(), agg: "first".into() }).await;
            let got = cells(h.app.sheets.active());
            let diff: Vec<_> = want
                .iter()
                .filter(|(k, v)| got.get(*k) != Some(*v))
                .map(|(k, v)| (k, v, got.get(k)))
                .chain(got.iter().filter(|(k, _)| !want.contains_key(*k)).map(|(k, v)| (k, v, None)))
                .collect();
            if diff.is_empty() { Ok(()) } else { Err(format!("(id, column), want, got: {diff:?}\n{}", t.csv())) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 14: hide, freeze and column order change no row values.
    #[test]
    fn rel_14_view_changes_keep_values((t, steps) in prefix(), c in 0..8usize) {
        let ok = RT.block_on(async {
            let mut r = run(&t, &steps).await;
            let case = r.case(&t);
            let h = &mut r.h;
            ex(h, "unhide").await; // the last `unhide` shows every column
            let before = bag(h.app.sheets.active());
            let Some(col) = column(h, c) else { return Ok(()) };
            let mut order = names(h.app.sheets.active());
            order.reverse();
            act(h, Action::Order { columns: order }).await;
            on(&col).run(h).await;
            ex(h, "freeze").await;
            act(h, Action::HideNamed { column: col.clone() }).await;
            let mut hidden: Vec<Record> = before
                .iter()
                .map(|r| r.iter().filter(|(k, _)| **k != col).map(|(k, v)| (k.clone(), v.clone())).collect())
                .collect();
            hidden.sort();
            if bag(h.app.sheets.active()) != hidden {
                return Err(format!("after hiding {col}\n{case}"));
            }
            ex(h, "unhide").await;
            if bag(h.app.sheets.active()) == before { Ok(()) } else { Err(format!("after unhide\n{case}")) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 15: rename a column away and back: same rows, order and aggregators.
    #[test]
    fn rel_15_rename_round_trip((t, steps) in prefix(), c in 0..8usize) {
        let ok = RT.block_on(async {
            let mut r = run(&t, &steps).await;
            let case = r.case(&t);
            let h = &mut r.h;
            let Some(col) = column(h, c) else { return Ok(()) };
            let (rows, cols) = (records(h.app.sheets.active()), names(h.app.sheets.active()));
            let aggs = h.app.sheets.active().view.aggregators.clone();
            on(&col).run(h).await;
            act(h, Action::RenameCol { new_name: "zz_tmp".into() }).await;
            if !names(h.app.sheets.active()).contains(&"zz_tmp".to_string()) { return Ok(()) } // refused
            on("zz_tmp").run(h).await;
            act(h, Action::RenameCol { new_name: col.clone() }).await;
            let s = h.app.sheets.active();
            if records(s) == rows && names(s) == cols && s.view.aggregators == aggs {
                Ok(())
            } else {
                Err(format!("rename {col} and back\n{case}"))
            }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 16: `:derive d = "c"` copies `c` into `d`, for every type.
    #[test]
    fn rel_16_derive_copies_a_column((t, steps) in prefix(), c in 0..8usize) {
        let ok = RT.block_on(async {
            let mut r = run(&t, &steps).await;
            let case = r.case(&t);
            let h = &mut r.h;
            let Some(col) = column(h, c) else { return Ok(()) };
            // :derive reads the base column, not the view's cast (S-21)
            if h.app.sheets.active().view.type_casts.iter().any(|(n, _)| *n == col) { return Ok(()) }
            act(h, Action::Derive { name: "zz_copy".into(), expr: ident(&col) }).await;
            let rows = records(h.app.sheets.active());
            if !names(h.app.sheets.active()).contains(&"zz_copy".to_string()) {
                return Err(format!("derive refused a copy of {col}\n{case}"));
            }
            match rows.iter().find(|r| r.get("zz_copy") != r.get(&col)) {
                None => Ok(()),
                Some(r) => Err(format!("copy of {col} differs: {r:?}\n{case}")),
            }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 17: an edit, then `U`, gives back the rows, columns and cursor.
    #[test]
    fn rel_17_edit_then_undo_restores((t, steps) in prefix(), edit in 0..5usize, v in 0..VALUES.len()) {
        let ok = RT.block_on(async {
            let mut r = run(&t, &steps).await;
            let case = r.case(&t);
            let h = &mut r.h;
            if edit == 0 { keys(h, "g s").await }
            let snap = |h: &Harness| {
                let s = h.app.sheets.active();
                (h.app.sheets.len(), records(s), names(s), s.view.row_cursor, s.view.col_cursor)
            };
            let before = snap(h);
            let what = match edit {
                0 => { act(h, Action::SetCol { value: VALUES[v].into() }).await; "setcol" }
                1 => { act(h, Action::EditCell { value: VALUES[v].into() }).await; "edit" }
                2 => { keys(h, "d").await; "d" }
                3 => { keys(h, "y").await; keys(h, "p").await; "y p" }
                _ => { act(h, Action::AddCol { name: "zz_new".into() }).await; "addcol" }
            };
            if snap(h) == before { return Ok(()) } // refused: nothing to undo
            keys(h, "U").await;
            let after = snap(h);
            if after == before { Ok(()) } else { Err(format!("{what} then U\n{before:?}\n{after:?}\n{case}")) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 18: `gd` on `n` selected rows deletes exactly those `n`.
    #[test]
    fn rel_18_delete_selected_count((t, steps) in prefix(), p in pred()) {
        let ok = RT.block_on(async {
            let (mut h, case) = plain(&t, &steps).await;
            let p = p.sql(&Schema::of(&h));
            act(&mut h, Action::SelectExpr { expr: p.clone(), unselect: false }).await;
            let s = h.app.sheets.active();
            let Ok(selected) = s.selected_in_view_order() else { return Ok(()) };
            let before = bag(s);
            keys(&mut h, "g d").await;
            let after = bag(h.app.sheets.active());
            if after.len() == before.len() && !selected.is_empty() { return Ok(()) } // refused
            let mut rest = before.clone();
            let kept = after.iter().all(|r| match rest.iter().position(|x| x == r) {
                Some(i) => { rest.remove(i); true }
                None => false,
            });
            if kept && after.len() + selected.len() == before.len() {
                Ok(())
            } else {
                Err(format!("{} selected by {p}: {} rows -> {}\n{case}", selected.len(), before.len(), after.len()))
            }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 19: `y` then `p` adds one row equal to the yanked one.
    #[test]
    fn rel_19_yank_then_paste_duplicates((t, steps) in prefix()) {
        let ok = RT.block_on(async {
            let mut r = run(&t, &steps).await;
            let case = r.case(&t);
            let h = &mut r.h;
            let s = h.app.sheets.active();
            // open (S-56 §6): `y` copies a column a `:derive` replaced as
            // shown, and the paste goes under that derive again
            if s.state.steps.iter().any(|st| matches!(st, PlanStep::Derive { replaces: true, .. })) {
                return Ok(());
            }
            let rows = records(s);
            let Some(row) = rows.get(s.view.row_cursor).cloned() else { return Ok(()) };
            let mut want = rows.clone();
            want.push(row.clone());
            want.sort();
            keys(h, "y").await;
            keys(h, "p").await;
            let got = bag(h.app.sheets.active());
            if got.len() == rows.len() { return Ok(()) } // refused (a derived sheet)
            let mut extra = got.clone();
            for r in &rows {
                if let Some(i) = extra.iter().position(|x| x == r) { extra.remove(i); }
            }
            if got == want { Ok(()) } else { Err(format!("yanked {row:?}\npasted {extra:?}\n{case}")) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 20: `:save` in each format, then open the file: the same rows.
    #[test]
    fn rel_20_save_then_open((t, steps) in prefix(), ext in proptest::sample::select(&["csv", "tsv", "jsonl", "parquet"][..])) {
        let ok = RT.block_on(async {
            let mut r = run(&t, &steps).await;
            let case = r.case(&t);
            let h = &mut r.h;
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join(format!("v.{ext}"));
            act(h, Action::Save { path: path.display().to_string() }).await;
            if !path.exists() { return Err(format!("no {ext} written\n{case}")) }
            let loaded = crate::reader::open(path.as_os_str(), None).map_err(|e| e.to_string())?;
            let back = Harness::with_source(loaded, tempfile::NamedTempFile::new().unwrap()).await;
            // JSON lines of no rows hold no column names either
            if ext == "jsonl" && frame(h.app.sheets.active()).height() == 0 {
                let n = frame(back.app.sheets.active()).height();
                return if n == 0 { Ok(()) } else { Err(format!("{n} rows back\n{case}")) };
            }
            let (want, got) = (
                normalized(&csv_text(h.app.sheets.active(), b','), false),
                normalized(&csv_text(back.app.sheets.active(), b','), false),
            );
            if got == want { Ok(()) } else { Err(format!("{ext}\n{}\n---\n{}\n{case}", want.join("\n"), got.join("\n"))) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 21: `:sql SELECT * FROM self` equals the view.
    #[test]
    fn rel_21_sql_identity((t, steps) in prefix()) {
        let ok = RT.block_on(async {
            let mut r = run(&t, &steps).await;
            let case = r.case(&t);
            let h = &mut r.h;
            let want = bag(h.app.sheets.active());
            let n = h.app.sheets.len();
            act(h, Action::Sql { query: "SELECT * FROM self".into() }).await;
            if !pushed(h, n) { return Err(format!("sql refused\n{case}")) }
            if bag(h.app.sheets.active()) == want { Ok(()) } else { Err(case) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }

    /// 22 and 23: the view saved as parquet diffs empty against the view on
    /// `id`, and an inner join with it on `id` keeps every row.
    #[test]
    fn rel_22_23_self_diff_and_self_join((t, steps) in prefix(), join in any::<bool>()) {
        let ok = RT.block_on(async {
            let mut r = run(&t, &steps).await;
            let case = r.case(&t);
            let h = &mut r.h;
            if !unique_id(h.app.sheets.active()) { return Ok(()) }
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("self.parquet").display().to_string();
            act(h, Action::Save { path: path.clone() }).await;
            let rows = frame(h.app.sheets.active()).height();
            let n = h.app.sheets.len();
            if join {
                act(h, Action::Join { source: path, keys: vec!["id".into()], how: "inner".into() }).await;
            } else {
                act(h, Action::Diff { path, keys: vec!["id".into()] }).await;
            }
            if !pushed(h, n) { return Err(format!("join {join} refused\n{case}")) }
            let got = frame(h.app.sheets.active()).height();
            let want = if join { rows } else { 0 };
            if got == want { Ok(()) } else { Err(format!("join {join}: {got} rows, want {want}\n{case}")) }
        });
        prop_assert!(ok.is_ok(), "{}", ok.unwrap_err());
    }
}
