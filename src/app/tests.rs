use super::*;
use std::io::Write;

/// App over `name,n / alice,3 / bob,1 / carol,2` with the first window materialized.
fn test_app() -> (App, tempfile::NamedTempFile) {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(tmp, "name,n\nalice,3\nbob,1\ncarol,2").unwrap();
    tmp.flush().unwrap();
    let csv = Some(crate::reader::SourceKind::Csv);
    let loaded = crate::reader::open(tmp.path().as_os_str(), csv).unwrap();
    let mut app = App::new(Sheet::from_source(0, loaded), None, None);
    let s = app.sheets.active_mut();
    s.window = Some(s.window_plan().collect().unwrap());
    (app, tmp)
}

/// `test_app` with a 2-row window over the 3-row data, count already known.
fn paged_app() -> (App, tempfile::NamedTempFile) {
    let (mut app, tmp) = test_app();
    let s = app.sheets.active_mut();
    s.view.window_rows = 2;
    s.window = Some(s.window_plan().collect().unwrap());
    s.view.total_rows = Some(3);
    s.view.count_key = Some(s.count_key());
    (app, tmp)
}

/// Feed the result the materialize task would send for the active sheet.
fn land_window(app: &mut App) {
    let s = app.sheets.active();
    let msg = MaterializeResult::Ok {
        sheet_id: s.id,
        revision: s.revision,
        df: s.window_plan_with_rids().collect().unwrap(),
    };
    app.on_materialize(msg);
}

fn titlebar(app: &mut App) -> String {
    let mut term = Terminal::new(ratatui::backend::TestBackend::new(80, 6)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    (0..80).map(|x| buf[(x, 0)].symbol()).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn goto_bottom_slides_to_last_data_row() {
    let (mut app, _tmp) = paged_app();
    app.dispatch(Action::GotoBottom);
    assert_eq!(app.sheets.active().view.window_offset, 1);
    land_window(&mut app);
    let v = &app.sheets.active().view;
    assert_eq!((v.row_cursor, v.jump_bottom), (1, false));
    assert!(
        titlebar(&mut app).contains("row 3/3"),
        "{}",
        titlebar(&mut app)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn goto_bottom_before_count_settles_when_count_lands() {
    let (mut app, _tmp) = paged_app();
    let s = app.sheets.active_mut();
    s.view.total_rows = None;
    let key = s.count_key();
    app.dispatch(Action::GotoBottom);
    let v = &app.sheets.active().view;
    assert_eq!((v.window_offset, v.row_cursor, v.jump_bottom), (0, 1, true));
    assert!(
        titlebar(&mut app).contains("row 2/?"),
        "{}",
        titlebar(&mut app)
    );
    // stale count is ignored
    app.on_materialize(MaterializeResult::Count {
        sheet_id: 0,
        key: "stale".into(),
        total: Ok(99),
    });
    assert_eq!(app.sheets.active().view.total_rows, None);
    app.on_materialize(MaterializeResult::Count {
        sheet_id: 0,
        key,
        total: Ok(3),
    });
    assert_eq!(app.sheets.active().view.window_offset, 1);
    land_window(&mut app);
    let v = &app.sheets.active().view;
    assert_eq!((v.row_cursor, v.jump_bottom), (1, false));
}

#[tokio::test(flavor = "multi_thread")]
async fn cursor_move_cancels_pending_goto_bottom() {
    let (mut app, _tmp) = paged_app();
    app.sheets.active_mut().view.total_rows = None;
    app.dispatch(Action::GotoBottom);
    app.dispatch(Action::CursorUp { count: None });
    assert!(!app.sheets.active().view.jump_bottom);
}

#[tokio::test(flavor = "multi_thread")]
async fn materialize_keeps_toast() {
    let (mut app, _tmp) = test_app();
    app.toast = Some("saved 3 rows".into());
    land_window(&mut app);
    assert_eq!(app.toast.as_deref(), Some("saved 3 rows"));
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_keeps_filter_and_sort_on_column() {
    let (mut app, _tmp) = test_app();
    let v = &mut app.sheets.active_mut().view;
    v.filter = Some(FilterState {
        column: "name".into(),
        regex: "a".into(),
    });
    v.sort.push(SortKey {
        column: "name".into(),
        order: SortOrder::Asc,
    });
    app.dispatch(Action::RenameCol {
        new_name: "who".into(),
    });
    let s = app.sheets.active();
    assert_eq!(s.view.filter.as_ref().unwrap().column, "who");
    assert_eq!(s.view.sort[0].column, "who");
    assert!(s.window_plan().collect().is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn rename_prompt_is_rendered() {
    let (mut app, _tmp) = test_app();
    let mut line = LineEdit::default();
    line.set("foo");
    app.input = Some((Prompt::Rename, line));
    let mut term = Terminal::new(ratatui::backend::TestBackend::new(40, 6)).unwrap();
    term.draw(|f| app.render(f)).unwrap();
    let buf = term.backend().buffer();
    let last: String = (0..40).map(|x| buf[(x, 5)].symbol()).collect();
    assert!(last.starts_with("rename: foo"), "status bar: {last:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn page_down_on_last_window_stays_in_bounds() {
    let (mut app, _tmp) = test_app();
    // Window of 4 holding 3 rows = last (partial) window; nothing beyond it.
    app.sheets.active_mut().view.window_rows = 4;
    for _ in 0..3 {
        app.dispatch(Action::PageDown);
    }
    let v = &app.sheets.active().view;
    assert_eq!(v.window_offset, 0);
    assert_eq!(v.row_cursor, 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_filter_regex_is_rejected_with_toast() {
    let (mut app, _tmp) = test_app();
    app.dispatch(Action::SetFilter {
        column: "name".into(),
        regex: "(".into(),
    });
    assert!(app.sheets.active().view.filter.is_none());
    assert!(app.toast.as_deref().unwrap().starts_with("bad regex"));
}

#[tokio::test(flavor = "multi_thread")]
async fn selection_follows_its_row_across_sort_and_filter() {
    let (mut app, _tmp) = test_app(); // alice,3 bob,1 carol,2
    land_window(&mut app);
    app.dispatch(Action::ToggleSelectRow); // alice
    app.dispatch(Action::SetSort {
        column: "n".into(),
        descending: false,
    });
    land_window(&mut app); // bob, carol, alice
    let s = app.sheets.active();
    assert_eq!(s.state.window_rids, vec![1, 2, 0]);
    assert_eq!(s.view.selected.iter().copied().collect::<Vec<_>>(), vec![0]);
    app.dispatch(Action::SetFilter {
        column: "name".into(),
        regex: "a".into(),
    }); // alice + carol remain, selection kept
    app.dispatch(Action::DupSelected);
    while !app.busy.is_empty() {
        let res = app.materialize_rx.recv().await.unwrap();
        app.on_materialize(res);
    }
    assert_eq!(app.sheets.len(), 2, "dup sheet pushed");
    let dup = app.sheets.active().plan.clone().collect().unwrap();
    assert_eq!(
        dup.column("name").unwrap().get(0).unwrap().get_str(),
        Some("alice")
    );
}
