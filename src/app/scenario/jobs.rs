//! S-33: nothing in `dispatch` waits for a scan of the view.

use super::*;
use crate::reader::SourceKind;

/// Each collect of the slow sheet's plan sleeps this long.
const NAP: Duration = Duration::from_millis(1500);
/// `dispatch` must return within this; a blocking collect takes `NAP`.
const QUICK: Duration = Duration::from_millis(500);

/// 3000 rows (more than a window, so edits can't scan inline): `id`,
/// `t` (text, null but every third row), `j` (JSON text); every collect naps.
async fn slow() -> Harness {
    let n = 3000;
    let df = df!(
        "id" => (1..=n as i64).collect::<Vec<_>>(),
        "t" => (0..n).map(|i| (i % 3 == 0).then(|| format!("t{i}"))).collect::<Vec<_>>(),
        "j" => (0..n).map(|i| format!(r#"{{"a":{i}}}"#)).collect::<Vec<_>>(),
    )
    .unwrap();
    let mut src = crate::reader::LoadedSource::from_df(SourceKind::Csv, "slow".into(), None, df);
    src.lazy = src.lazy.map(
        |df| {
            std::thread::sleep(NAP);
            Ok(df)
        },
        AllowedOptimizations::default(),
        None,
        Some("nap"),
    );
    let mut app = App::new(Sheet::from_source(0, src), None, None);
    app.spawn_materialize();
    Harness::from_app(app).await
}

/// `keys` return quickly with a job running.
fn press_quick(h: &mut Harness, keys: &str) {
    let t = Instant::now();
    for k in keys.split(' ') {
        h.app.on_key(k.parse().unwrap());
    }
    assert!(t.elapsed() < QUICK, "{keys} blocked for {:?}", t.elapsed());
    assert!(!h.app.busy.is_empty(), "{keys}: no job");
}

fn columns(h: &Harness) -> Vec<String> {
    let s = h.app.sheets.active();
    s.plan
        .clone()
        .collect_schema()
        .unwrap()
        .iter_names()
        .map(|c| c.to_string())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn edits_scan_in_the_background() {
    let mut h = slow().await;
    // i: a numbered column, one undo step
    press_quick(&mut h, "i");
    h.pump().await;
    assert!(
        columns(&h).contains(&"incr".to_string()),
        "{:?}",
        columns(&h)
    );
    h.keys("U").await;
    assert!(
        !columns(&h).contains(&"incr".to_string()),
        "{:?}",
        columns(&h)
    );
    h.keys("R").await;
    assert!(
        columns(&h).contains(&"incr".to_string()),
        "{:?}",
        columns(&h)
    );

    // gi on the selected rows; f fills t; gzp pastes
    h.keys("g g s j s").await;
    press_quick(&mut h, "g i");
    h.pump().await;
    assert!(
        h.app.toast.as_deref().unwrap_or("").starts_with("set "),
        "{:?}",
        h.app.toast
    );
    h.app.goto_column("t");
    h.keys("g u").await;
    press_quick(&mut h, "f");
    h.pump().await;
    assert_eq!(h.app.toast.as_deref(), Some("filled 2000 values"));
    h.keys("z y s").await;
    press_quick(&mut h, "g z p");
    h.pump().await;
    assert!(
        h.app
            .toast
            .as_deref()
            .unwrap_or("")
            .starts_with("pasted into"),
        "{:?}",
        h.app.toast
    );

    // :type json
    h.app.goto_column("j");
    h.keys(":").await;
    h.type_no_enter("type json").await;
    press_quick(&mut h, "<CR>");
    h.pump().await;
    let s = h.app.sheets.active();
    let dt = s.plan.clone().collect_schema().unwrap().get("j").cloned();
    assert!(matches!(dt, Some(DataType::Struct(_))), "{dt:?}");

    // :s under a filter reads the view's rows
    h.keys("g u :").await;
    h.type_("filter t ^t").await; // every row: more than a window
    h.app.goto_column("t");
    h.keys(":").await;
    h.type_no_enter("s/t/T/").await;
    press_quick(&mut h, "<CR>");
    h.pump().await;
    let toast = h.app.toast.clone().unwrap_or_default();
    assert!(toast.starts_with("replaced /t/ in t on "), "{toast}");
    assert!(toast.ends_with(" rows in view"), "{toast}");
}

/// D1: an edit made while the scan runs refuses its result; D3: `<C-c>`
/// drops it and nothing changes.
#[tokio::test(flavor = "multi_thread")]
async fn stale_or_cancelled_scans_change_nothing() {
    let mut h = slow().await;
    press_quick(&mut h, "i");
    h.keys("[").await; // a sort: the rows' order changed
    assert!(!columns(&h).contains(&"incr".to_string()));
    assert_eq!(
        h.app.toast.as_deref(),
        Some("addcol-incr: the sheet changed while it ran; not applied")
    );

    press_quick(&mut h, "i");
    h.keys("<C-c>").await;
    assert!(!columns(&h).contains(&"incr".to_string()));
}

/// D6, as a guard for every action: `dispatch` returns at once on a sheet
/// whose every collect naps. A new action that scans on the UI thread fails here.
#[tokio::test(flavor = "multi_thread")]
async fn no_action_blocks_the_ui() {
    // Instant by nature, or not about the view: kept short and explained.
    const EXEMPT: &[&str] = &[];
    let mut h = slow().await;
    let base = h.app.sheets.active().clone();
    let t = Instant::now();
    base.view_order_rids().unwrap();
    assert!(
        t.elapsed() >= NAP,
        "the sheet doesn't nap: the guard checks nothing"
    );
    let mut slow_ones = Vec::new();
    let ex = crate::exline::COMMANDS.iter().filter_map(|c| {
        let acts = crate::exline::parse(c).or_else(|_| crate::exline::parse(&format!("{c} id")));
        acts.ok().map(|a| (c.to_string(), a))
    });
    let named = Action::NAMED
        .iter()
        .map(|(n, a)| (n.to_string(), vec![a.clone()]));
    for (name, actions) in named.chain(ex) {
        if EXEMPT.contains(&name.as_str()) {
            continue;
        }
        h.app.cancel_jobs(true);
        h.app.sheets = Sheets::new(base.clone());
        h.app.input = None;
        h.app.quit = false;
        let t = Instant::now();
        for a in actions {
            h.app.dispatch(a);
        }
        if t.elapsed() > QUICK {
            slow_ones.push(format!("{name} {:?}", t.elapsed()));
        }
    }
    assert!(slow_ones.is_empty(), "block the UI: {slow_ones:#?}");
}
