//! S-58: undo as a state machine. A model keeps, for each undo state the
//! sheet made (`Sheet::undo_mark`), what the sheet showed there and its tree
//! edges; generated commands, `U` and `R` must move between those states.

use std::collections::BTreeMap;

use super::*;

/// What `U` must give back: the rows and every view setting a command
/// changes. Not the cursor or the window: moving them makes no undo state.
#[derive(Debug, PartialEq)]
struct Shown {
    rows: String,
    selected: Vec<u64>,
    view: String,
}

fn shown(h: &Harness) -> Shown {
    let s = h.app.sheets.active();
    let v = &s.view;
    Shown {
        rows: csv_text(s, b'\t'),
        selected: v.selected.iter().copied().collect(),
        view: format!(
            "{:?}",
            (
                (
                    &v.filter,
                    &v.filter_expr,
                    &v.sort,
                    &v.hidden,
                    &v.keys,
                    &v.type_casts
                ),
                (
                    &v.aggregators,
                    &v.precision,
                    &v.fmtstrs,
                    &v.order,
                    v.frozen_left
                ),
                (&v.formats, &v.scales, &v.validations),
            )
        ),
    }
}

/// A command: a generated step on this sheet, a row or cell edit, `U`, `R`.
#[derive(Clone, Debug)]
enum Op {
    Step(Step),
    Edit(usize, usize),
    Undo,
    Redo,
}

fn op() -> impl Strategy<Value = Op> {
    // steps that open or close a sheet end the walk: undo is per sheet
    let same_sheet = step().prop_filter("stays on the sheet", |s| {
        !matches!(
            s,
            Step::Freq { .. } | Step::Describe | Step::DupSel | Step::Pop | Step::Undo | Step::Redo
        )
    });
    prop_oneof![
        4 => same_sheet.prop_map(Op::Step),
        2 => (0..5usize, 0..VALUES.len()).prop_map(|(e, v)| Op::Edit(e, v)),
        3 => Just(Op::Undo),
        2 => Just(Op::Redo),
    ]
}

async fn apply(h: &mut Harness, op: &Op) -> String {
    let ex = |a: Action| Input::Ex(a.to_ex().expect("edits have an ex form"));
    let inputs = match op {
        Op::Step(s) => s.inputs(h),
        Op::Edit(0, v) => vec![ex(Action::SetCol {
            value: VALUES[*v].into(),
        })],
        Op::Edit(1, v) => vec![ex(Action::EditCell {
            value: VALUES[*v].into(),
        })],
        Op::Edit(2, _) => vec![Input::Keys("d".into())],
        Op::Edit(3, _) => vec![Input::Keys("y".into()), Input::Keys("p".into())],
        Op::Edit(_, _) => vec![ex(Action::AddCol {
            name: "zz_new".into(),
        })],
        Op::Undo => vec![Input::Keys("U".into())],
        Op::Redo => vec![Input::Keys("R".into())],
    };
    let mut typed = Vec::new();
    for i in inputs {
        i.run(h).await;
        typed.push(i.script());
    }
    typed.join("\n")
}

/// Run `ops`, checking each against the model; `Err` names the first
/// breach and the script so far.
async fn walk(t: &Table, ops: &[Op]) -> Result<(), String> {
    let mut h = t.open().await;
    let mark = |h: &Harness| h.app.sheets.active().undo_mark();
    let mut seen = BTreeMap::from([(mark(&h), shown(&h))]);
    let (mut parent, mut redo) = (BTreeMap::new(), BTreeMap::new());
    let mut script = Vec::new();
    for op in ops {
        let (from, sheets) = (mark(&h), h.app.sheets.len());
        script.push(apply(&mut h, op).await);
        if h.app.sheets.len() != sheets {
            return Ok(()); // a command opened a sheet
        }
        let (to, now) = (mark(&h), shown(&h));
        let fail = |why: String| {
            Err(format!(
                "{why}\n--- input.csv\n{}--- script\n{}",
                t.csv(),
                script.join("\n")
            ))
        };
        let want = match op {
            Op::Undo => parent.get(&from).copied(),
            Op::Redo => redo.get(&from).copied(),
            _ if to != from => {
                if seen.contains_key(&to) {
                    return fail(format!("a new state reused id {to}"));
                }
                parent.insert(to, from);
                redo.insert(from, to);
                seen.insert(to, now);
                continue;
            }
            // no new state: the sheet must show what it did
            _ => None,
        };
        let want = want.unwrap_or(from);
        if to != want {
            return fail(format!("{op:?} went to state {to}, the model says {want}"));
        }
        if seen[&want] != now {
            return fail(format!(
                "{op:?} to state {want}:\nwant {:?}\ngot  {now:?}",
                seen[&want]
            ));
        }
        if matches!(op, Op::Undo) && want != from {
            redo.insert(want, from); // `R` goes back where `U` came from
        }
    }
    Ok(())
}

proptest! {
    #![proptest_config(config(24))]

    /// S-58: `U` and `R` move along the undo tree, and each state shows what
    /// it showed when it was made; a command that changes the sheet makes a
    /// state.
    #[test]
    fn undo_and_redo_follow_the_model(t in table(), ops in proptest::collection::vec(op(), 1..=12)) {
        let r = RT.block_on(walk(&t, &ops));
        prop_assert!(r.is_ok(), "{}", r.unwrap_err());
    }
}
