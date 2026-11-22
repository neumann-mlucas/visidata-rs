//! S-57 §2: every action `to_ex` writes parses back to itself, for any text.

use proptest::prelude::*;

use super::*;
use crate::exline;

/// Strings that break ex syntax: quotes, escapes, separators, edges, Unicode.
const HARD: &[&str] = &[
    "",
    " ",
    "a b",
    " lead",
    "trail ",
    "q\"t",
    "it's",
    "back\\slash",
    "\\",
    "#",
    "# x",
    "a,b",
    "x -- y",
    "k=v",
    "=",
    "-odd",
    "+odd",
    "|",
    "*",
    "$1",
    "%",
    "\t",
    "a\nb",
    "名前",
    "e\u{301}",
    "👍🏽",
    "\"\"",
    "''",
];

/// Any text: plain words, hard strings, any printable string.
fn text() -> BoxedStrategy<String> {
    prop_oneof![
        "[a-z]{1,6}",
        proptest::sample::select(HARD).prop_map(str::to_string),
        "\\PC{0,8}",
    ]
    .boxed()
}

/// Plain words only: every action must have an ex form for them.
fn word() -> BoxedStrategy<String> {
    "[a-z]{1,6}".boxed()
}

/// Every action that `to_ex` writes with arguments, its text fields from `t`.
fn action(t: BoxedStrategy<String>) -> impl Strategy<Value = Action> {
    let t = || t.clone();
    let maybe_text = || proptest::option::of(t());
    let texts = || proptest::collection::vec(t(), 0..3);
    let some_texts = || proptest::collection::vec(t(), 1..3);
    // Fixed vocabularies: the parser refuses other values, so no action holds one.
    let pick = |v: &'static [&'static str]| proptest::sample::select(v).prop_map(str::to_string);
    let keyed_how = || pick(&["inner", "left", "full", "merge", "extend"]);
    let color = || pick(&["red", "white", "#ff0000"]);
    let style = || pick(&["red", "bold", "bg=yellow,bold"]);
    // `-` is stdin, which `:diff` and `:tail` refuse.
    let path = || t().prop_filter("stdin", |p| p != "-");
    use Action::*;
    prop_oneof![
        (1..50usize).prop_map(|n| CursorDown { count: Some(n) }),
        (1..50usize).prop_map(|n| CursorRight { count: Some(n) }),
        (1..9i64, any::<bool>()).prop_map(|(n, l)| SlideCol {
            by: if l { -n } else { n }
        }),
        (1..9i64, any::<bool>()).prop_map(|(n, l)| TimeWalk {
            n: if l { -n } else { n }
        }),
        t().prop_map(|new_name| RenameCol { new_name }),
        (t(), t()).prop_map(|(column, regex)| SetFilter { column, regex }),
        (t(), any::<bool>()).prop_map(|(column, descending)| SetSort { column, descending }),
        (t(), t(), t(), t()).prop_map(|(index, on, values, agg)| Pivot {
            index,
            on,
            values,
            agg
        }),
        (some_texts(), texts()).prop_map(|(on, index)| Unpivot { on, index }),
        (path(), texts()).prop_map(|(path, keys)| Diff { path, keys }),
        (path(), any::<bool>()).prop_map(|(path, follow)| Tail { path, follow }),
        (t(), any::<bool>()).prop_map(|(expr, unselect)| SelectExpr { expr, unselect }),
        t().prop_map(|path| Save { path }),
        t().prop_map(|path| Export { path }),
        (t(), some_texts(), keyed_how()).prop_map(|(source, keys, how)| Join { source, keys, how }),
        (t(), pick(&["append", "concat"])).prop_map(|(source, how)| Join {
            source,
            keys: vec![],
            how
        }),
        prop_oneof![keyed_how(), pick(&["append", "concat"])].prop_map(|how| JoinAll { how }),
        (t(), t()).prop_map(|(name, expr)| Derive { name, expr }),
        (t(), t(), proptest::collection::vec((t(), t()), 1..3)).prop_map(|(key, their, cols)| {
            Lookup {
                source: "b.csv".into(),
                key,
                their_key: their,
                cols,
            }
        }),
        t().prop_map(|query| Sql { query }),
        t().prop_map(|sql| ExecSql { sql }),
        t().prop_map(|expr| SetExpr { expr }),
        maybe_text().prop_map(|expr| SetFilterExpr { expr }),
        (maybe_text(), t(), style()).prop_map(|(column, pred, style)| Color {
            column,
            pred,
            style
        }),
        (t(), proptest::option::of((color(), color())))
            .prop_map(|(column, colors)| Scale { column, colors }),
        t().prop_map(|column| DataBar { column }),
        maybe_text().prop_map(|column| NoColor { column }),
        proptest::option::of(pick(&["view", "off"])).prop_map(|arg| Trust { arg }),
        (t(), maybe_text()).prop_map(|(column, pred)| Validate { column, pred }),
        t().prop_map(|value| SetCol { value }),
        t().prop_map(|value| EditCell { value }),
        (t(), maybe_text()).prop_map(|(path, format)| Open { path, format }),
        (1..1000u64, maybe_text()).prop_map(|(row, column)| SetCursor { row, column }),
        t().prop_map(|path| Source { path }),
        proptest::collection::vec(0..1000u64, 1..4).prop_map(|ids| SelectIds { ids }),
        maybe_text().prop_map(|path| MkSession { path }),
        t().prop_map(|names| AggregateCol { names }),
        t().prop_map(|column| HideNamed { column }),
        (maybe_text(), t()).prop_map(|(column, path)| Patch { column, path }),
        (1..5usize).prop_map(|depth| ExpandCol { depth }),
        t().prop_map(|needle| Hits { needle }),
        proptest::option::of(1..100u32).prop_map(|bins| Hist { bins }),
        (t(), some_texts()).prop_map(|(x, ys)| Plot { x, ys }),
        t().prop_map(|text| SetNote { text }),
        t().prop_map(|name| AddCol { name }),
        t().prop_map(|pattern| Capture { pattern }),
        proptest::option::of(0..10u8).prop_map(|digits| SetPrecision { digits }),
        t().prop_map(|fmt| SetFmtstr { fmt }),
        ("[a-z_]{1,12}", maybe_text()).prop_map(|(name, value)| SetOption { name, value }),
        some_texts().prop_map(|columns| Order { columns }),
        (t(), t(), any::<bool>()).prop_map(|(pattern, replacement, all)| Substitute {
            pattern,
            replacement,
            all
        }),
    ]
}

proptest! {
    /// S-57 §2.2 (1): a written ex line reads back as the same action.
    #[test]
    fn to_ex_parses_back(a in action(text())) {
        if let Some(line) = a.to_ex() {
            prop_assert!(!line.contains('\n'), "{a:?} wrote a newline");
            let got = exline::parse(&line);
            prop_assert!(
                matches!(&got, Ok(v) if v.as_slice() == std::slice::from_ref(&a)),
                "{a:?}\n:{line}\n{got:?}"
            );
        }
    }

    /// S-57 §2.2 (2): plain words always have an ex form.
    #[test]
    fn plain_words_always_have_an_ex_form(a in action(word())) {
        prop_assert!(a.to_ex().is_some(), "{a:?}");
    }

    /// S-57 §2.2 (3): a quoted argument splits back to itself.
    #[test]
    fn quote_arg_splits_back(s in text()) {
        let got = exline::split_args(&quote_arg(&s), "");
        prop_assert!(matches!(&got, Ok(v) if v.as_slice() == std::slice::from_ref(&s)), "{s:?} -> {got:?}");
    }
}
