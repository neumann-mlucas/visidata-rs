//! Rendering, moving, searching, prompts, jobs.

use super::*;

#[tokio::test(flavor = "multi_thread")]
async fn title_shows_what_shapes_the_view_and_always_the_position() {
    let mut h = Harness::new(5000).await;
    assert!(!title(&h).contains("sel "), "{}", title(&h));
    h.keys("l [ s -").await; // s moves the cursor down
    let line = h.screen().lines().next().unwrap().to_string();
    assert!(line.trim_end().ends_with("row 2/5,000"), "{line}");
    for chip in ["sel 1", "hidden 1"] {
        assert!(line.contains(chip), "{chip}: {line}");
    }
    // One sort key shows as the header's arrow, not a chip; the kind not at all.
    assert!(!line.contains("sort") && !line.contains(" csv "), "{line}");
    assert_eq!(line.width(), 99, "the row block ends the bar: {line:?}");

    h.app.sheets.active_mut().name = "x".repeat(300);
    h.pump().await;
    let line = h.screen().lines().next().unwrap().to_string();
    assert!(line.contains("row 2/5,000") && line.contains('…'), "{line}");
    assert_eq!(line.width(), 99, "{line:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn slow_work_spins_in_the_title_and_dims_stale_rows() {
    let mut h = Harness::new(3).await;
    h.app.busy.push(Job {
        id: 0,
        sheet_id: h.app.sheets.active().id,
        label: "x",
        abort: tokio::spawn(async {}).abort_handle(),
    });
    h.app.working_since = Some(Instant::now() - Duration::from_secs(1));
    h.app.sheets.active_mut().begin_window();
    h.app.draw(&mut h.term).unwrap();
    assert!(
        title(&h).contains(|c| SPINNER.contains(&c)),
        "{}",
        title(&h)
    );
    let buf = h.term.backend().buffer();
    assert!(buf[(1, 3)].modifier.contains(Modifier::DIM));

    h.app.busy.clear();
    let s = h.app.sheets.active_mut();
    s.land_error(s.revision());
    h.app.draw(&mut h.term).unwrap();
    assert!(h.app.working_since.is_none());
    assert!(!title(&h).contains(|c| SPINNER.contains(&c)));
}

/// Nulls sort as VisiData's smallest value: first with `[`, last with `]`.
#[tokio::test(flavor = "multi_thread")]
async fn nulls_lead_ascending_and_trail_descending_sorts() {
    let mut h = csv(b"n,k\n2,a\n,b\n1,c\n3,d\n").await;
    h.keys("[").await;
    assert_eq!(view_csv(&h), "n,k\n,b\n1,c\n2,a\n3,d\n");
    h.keys("]").await;
    assert_eq!(view_csv(&h), "n,k\n3,d\n2,a\n1,c\n,b\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn csv_moves_into_memory_after_opening() {
    let mut h = Harness::new(5000).await;
    let path = h.app.sheets.active().source.path.clone().unwrap();
    std::fs::remove_file(path).unwrap();
    h.keys("G").await;
    assert!(title(&h).contains("row 5000/5000"), "{}", title(&h));
    assert!(h.app.toast.is_none(), "{:?}", h.app.toast);
    h.keys("]").await;
    assert_eq!(first_cell(&h), "5000", "{}", h.screen());
}

#[tokio::test(flavor = "multi_thread")]
async fn columns_keep_their_width_while_scrolling() {
    let mut csv = b"id,note\n".to_vec();
    for i in 0..5000 {
        let note = if i >= 4900 {
            "x".repeat(30)
        } else {
            "y".into()
        };
        csv.extend(format!("{i},{note}\n").bytes());
    }
    let mut h = super::scenario::csv(&csv).await;
    let width = |h: &Harness| h.app.sheets.active().state.widths[1];
    h.keys("G").await;
    let wide = width(&h);
    assert!(wide >= 30, "{wide}");
    h.keys("g g").await;
    assert_eq!(width(&h), wide);
}

#[test]
fn grouped_and_fit() {
    assert_eq!(grouped(0), "0");
    assert_eq!(grouped(999), "999");
    assert_eq!(grouped(1000), "1,000");
    assert_eq!(grouped(1234567), "1,234,567");
    assert_eq!(fit("abc", 5), "abc  ");
    assert_eq!(fit("abcdef", 3), "abc");
    assert_eq!(fit("日本", 3), "日 ");
}

#[tokio::test(flavor = "multi_thread")]
async fn navigate_search_filter_save_across_windows() {
    let mut h = Harness::new(5000).await; // 12 rows tall → 9 data rows per screen
    assert!(title(&h).contains("row 1/5000"), "{}", title(&h));

    h.keys("G").await;
    assert!(title(&h).contains("row 5000/5000"), "{}", title(&h));
    assert!(h.screen().contains("5000 name_5000"), "{}", h.screen());

    // :save writes every row of the view, not just the loaded window
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("all.csv");
    h.keys(":").await;
    h.type_(&format!("w {}", out.display())).await;
    let saved = crate::reader::open(out.as_os_str(), None).unwrap();
    assert_eq!(saved.lazy.collect().unwrap().height(), 5000);

    h.keys("g g").await;
    assert!(title(&h).contains("row 1/5000"), "{}", title(&h));
    assert_eq!(first_cell(&h), "1");

    h.keys(&"<C-d> ".repeat(12)).await;
    assert!(title(&h).contains("row 109/5000"), "{}", title(&h));

    h.keys("]").await; // sort desc on id
    assert_eq!(first_cell(&h), "5000");
    h.keys(":").await;
    h.type_("unsort").await;
    h.keys("g g g /").await;
    h.type_("name_4999").await;
    assert!(title(&h).contains("row 4999/5000"), "{}", title(&h));

    h.keys("0 l l g z |").await; // filter grp
    h.type_("^3$").await;
    let want = (1..=5000).filter(|i| i % 7 == 3).count();
    assert!(
        title(&h).contains(&format!("row 1/{want}")),
        "{}",
        title(&h)
    );
    h.keys("F").await;
    assert!(
        title(&h).contains("freq(grp)") && title(&h).ends_with("/1"),
        "{}",
        title(&h)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn selection_covers_whole_view() {
    let mut h = Harness::new(5000).await;
    h.keys("g s").await;
    assert!(title(&h).contains("sel 5000"), "{}", title(&h));
    h.keys("g u l |").await;
    h.type_("^name_49").await; // 49, 490-499, 4900-4999
    assert!(title(&h).contains("sel 111"), "{}", title(&h));
    h.keys("\"").await;
    assert!(title(&h).contains("row 1/111"), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn key_col_keeps_cursor_on_column() {
    let mut h = Harness::new(10).await;
    h.keys("l l !").await; // key grp → moves to the front
    assert!(title(&h).contains("grp 1/4"), "{}", title(&h));
    h.keys("!").await; // unkey → back to its place
    assert!(title(&h).contains("grp 3/4"), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn multiline_errors_show_first_line_only() {
    let mut h = Harness::new(3).await;
    h.app.toast = Some("materialize: boom\n\nResolved plan: …".into());
    h.pump().await;
    assert_eq!(status(&h), "materialize: boom");
}

#[tokio::test(flavor = "multi_thread")]
async fn wide_and_control_chars_render_on_one_line() {
    let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    tmp.write_all("k,v,z\n名前,\"a\nb\",end\nx\ty,q,end\n".as_bytes())
        .unwrap();
    tmp.flush().unwrap();
    let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
    let mut h = Harness::with_source(loaded, tmp).await;
    let screen = h.screen();
    let rows: Vec<&str> = screen.lines().collect();
    assert!(
        rows[2].starts_with("名前") && rows[2].contains("a↵b"),
        "{screen}"
    );
    assert!(rows[3].contains("x→y"), "{screen}");
    // `z` column starts at the same terminal column on both rows
    let col = |r: &str| r[..r.find("end").unwrap()].width();
    assert_eq!(col(rows[2]), col(rows[3]), "{screen}");
    h.keys("j").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn heavy_commands_run_in_background_and_report() {
    let mut h = Harness::new(3000).await;
    h.keys(":").await;
    h.type_("pivot grp grp id count").await; // job → pushes pivot sheet
    assert!(
        title(&h).contains("pivot(") && title(&h).contains("row 1/7"),
        "{}",
        title(&h)
    );
    h.keys("q").await;
    // while a job runs the status bar says so
    h.app.on_key("g".parse().unwrap());
    h.app.on_key("s".parse().unwrap());
    h.app.draw(&mut h.term).unwrap();
    assert!(
        status(&h).starts_with("working: select all"),
        "{}",
        status(&h)
    );
    h.pump().await;
    assert!(title(&h).contains("sel 3000"), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn long_values_further_down_widen_their_column() {
    let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    let mut body = "k,v\n".to_string();
    for i in 0..200 {
        body += &format!(
            "{i},{}\n",
            if i == 150 { "a-much-longer-value" } else { "x" }
        );
    }
    tmp.write_all(body.as_bytes()).unwrap();
    let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
    let mut h = Harness::with_source(loaded, tmp).await;
    h.keys("1 5 0 j").await; // row 151 holds the long value; far past the first-64 sample
    assert!(h.screen().contains("a-much-longer-value"), "{}", h.screen());
}

#[tokio::test(flavor = "multi_thread")]
async fn cursor_column_scrolls_into_view() {
    let mut h = Harness::new(10).await;
    h.term = Terminal::new(ratatui::backend::TestBackend::new(16, 5)).unwrap();
    h.keys("g l").await;
    assert!(
        h.screen().lines().nth(1).unwrap().contains("amt"),
        "{}",
        h.screen()
    );
    h.keys("0").await;
    assert!(
        h.screen()
            .lines()
            .nth(1)
            .unwrap()
            .trim_start()
            .starts_with("id"),
        "{}",
        h.screen()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn floats_show_exactly_and_select_equal_compares_them_exactly() {
    let mut h = csv(b"x,y\n1.00000001,a\n1.0,b\n123456789.125,c\n").await;
    assert!(h.screen().contains("1.00000001"), "{}", h.screen());
    assert!(h.screen().contains("123456789.125"), "{}", h.screen());
    h.keys(",").await;
    let rid = |h: &Harness| {
        h.app
            .sheets
            .active()
            .view
            .selected
            .iter()
            .copied()
            .collect::<Vec<_>>()
    };
    assert_eq!(rid(&h), vec![0], "cursor row only, not the row holding 1.0");
    h.keys("g u j j ,").await;
    assert_eq!(rid(&h), vec![2]);
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_during_a_pending_window_act_on_the_new_columns() {
    let mut h = csv(b"a,b,c\n1,2,3\n").await;
    h.keys("- -").await; // no pump between: both must land
    assert_eq!(h.app.sheets.active().view.hidden.len(), 2);
    h.keys("g v").await;
    h.keys("- F").await; // freq on the column now under the cursor
    assert!(title(&h).contains("freq(b)"), "{}", title(&h));
    assert_eq!(h.app.toast, None);
}

#[tokio::test(flavor = "multi_thread")]
async fn repeat_does_not_reopen_a_prompt() {
    let mut h = csv(b"a\nx\ny\n").await;
    h.keys("|").await;
    h.type_("x").await;
    assert_eq!(
        h.app.last_action,
        Some(vec![Action::SelectByRegex {
            regex: "x".into(),
            unselect: false,
            all: false
        }])
    );
    h.keys(":").await; // `.` plots (VisiData); repeat is `:repeat-last`
    h.type_("repeat-last").await;
    assert!(h.app.input.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_half_written_last_row_does_not_set_column_types() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.csv");
    std::fs::write(&f, "d,n\n2024-01-02,5\n2024-0").unwrap();
    let mut h = csv(b"x\n1\n").await;
    tail_sheet(&mut h, &f).await;
    std::fs::write(&f, "d,n\n2024-01-02,5\n2024-01-03,6\n").unwrap();
    h.app.on_tail_tick();
    h.pump().await;
    let df = h.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(df.height(), 2);
    assert_eq!(df.column("d").unwrap().dtype(), &DataType::Date);
}

/// S-26 phase 0: keys VisiData binds mean what they mean there.
#[tokio::test(flavor = "multi_thread")]
async fn visidata_keys_currency_columns_uniform() {
    let mut h = csv(b"a,price,k\n1,$1.50,x\n2,$2.00,x\n").await;
    h.keys("l $").await; // type-currency
    let s = h.app.sheets.active();
    let df = s.view_plan().collect().unwrap();
    assert_eq!(df.column("price").unwrap().dtype(), &DataType::Float64);
    h.keys("g l").await;
    assert_eq!(h.current_column(), "k");
    h.keys("g h").await;
    assert_eq!(h.current_column(), "a");
    h.keys("g -").await; // hide-uniform-cols
    assert!(h.app.sheets.active().view.hidden.contains("k"));
    h.keys("g v").await; // unhide-cols
    assert!(h.app.sheets.active().view.hidden.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn slash_searches_the_current_column_g_slash_all() {
    // `x1` sits in column b on row 1 and in column a on row 3
    let mut h = csv(b"a,b\np,x1\nq,y\nx1,z\n").await;
    h.keys("/").await; // cursor on a
    h.type_("^X").await; // regex, case-insensitive
    assert_eq!(h.app.sheets.active().view.row_cursor, 2);
    h.keys("g g g /").await;
    h.type_("x1").await; // starts after the cursor row, as VisiData
    assert_eq!(h.app.sheets.active().view.row_cursor, 2);
    assert_eq!(h.current_column(), "a");
    h.keys("n").await; // wraps
    assert_eq!(h.app.sheets.active().view.row_cursor, 0);
    assert_eq!(h.current_column(), "b");
    h.keys("g g g h ?").await; // backward from the top wraps
    h.type_("q").await;
    assert_eq!(h.app.sheets.active().view.row_cursor, 1);
    h.keys("?").await;
    h.type_("(").await;
    assert!(
        h.app
            .toast
            .as_deref()
            .unwrap_or("")
            .starts_with("bad regex")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_c_cancels_jobs_and_gq_quits() {
    let mut h = Harness::new(3).await;
    h.app.spawn_job("slow", || {
        std::thread::sleep(Duration::from_millis(300));
        Ok(JobDone::Toast("done".into()))
    });
    h.keys("<C-c>").await;
    assert_eq!(h.app.toast.as_deref(), Some("cancelled 1 job"));
    assert!(!h.app.quit);
    tokio::time::sleep(Duration::from_millis(500)).await;
    h.pump().await;
    assert_eq!(h.app.toast.as_deref(), Some("cancelled 1 job"));
    h.keys("<Space>").await;
    assert!(matches!(h.app.input, Some((Prompt::Ex, _))));
    h.keys("<Esc> g q").await;
    assert!(h.app.quit);
}

/// S-23 A: `b` opens the sidebar on the cursor column's stats, `Ctrl+G`
/// cycles its pages, `:plan` keeps the plan → optimized → off cycle.
#[tokio::test(flavor = "multi_thread")]
async fn sidebar_pages() {
    let mut h = csv(b"id,qty,note\n1,2,a\n2,,b\n3,4,a long note that wraps\n").await;
    assert!(!h.screen().contains("column: "), "starts closed");
    h.keys("l b").await;
    let scr = h.screen();
    assert!(scr.contains("column: qty"), "{scr}");
    for line in ["type     i64", "nulls    1", "distinct 3", "sum      6"] {
        assert!(scr.contains(line), "{line}\n{scr}");
    }
    h.keys("l").await; // follows the cursor
    assert!(h.screen().contains("mode     a"), "{}", h.screen());

    h.keys("j j <C-g>").await;
    assert!(h.screen().contains("cell: note"), "{}", h.screen());
    assert!(h.screen().contains("a long note"), "{}", h.screen());
    h.keys("<C-g>").await;
    assert!(h.screen().contains(" help "), "{}", h.screen());
    assert!(h.screen().contains("     j  down"), "{}", h.screen());
    h.keys("<C-g>").await;
    assert!(h.screen().contains(" plan "), "{}", h.screen());
    h.keys("b").await;
    assert_eq!(h.app.sidebar, None);
    h.keys("b").await; // reopens the last page
    assert_eq!(h.app.sidebar, Some(SidebarPage::Plan));

    h.keys(":").await;
    h.type_("plan").await;
    assert!(h.screen().contains("plan (opt)"), "{}", h.screen());
    h.keys(":").await;
    h.type_("plan").await;
    assert_eq!(h.app.sidebar, None);
    h.keys(":").await;
    h.type_("sidebar cell").await;
    assert_eq!(h.app.sidebar, Some(SidebarPage::Cell));
    h.keys(":").await;
    h.type_("sidebar nope").await;
    assert!(
        status(&h).contains("no sidebar page nope (column, cell"),
        "{}",
        status(&h)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_job_result_does_not_take_a_newer_jobs_place() {
    let mut h = Harness::new(3).await;
    let id = h.app.sheets.active().id;
    h.app.busy.push(Job {
        id: 2,
        sheet_id: id,
        label: "search",
        abort: tokio::spawn(async {}).abort_handle(),
    });
    // job 1 (same sheet, same label) was cancelled after its result was sent
    h.app
        .on_job_done(1, id, "search", Ok(JobDone::Toast("stale".into())));
    assert!(h.app.toast.is_none());
    assert_eq!(h.app.busy.len(), 1);
    h.app
        .on_job_done(2, id, "search", Ok(JobDone::Toast("fresh".into())));
    assert_eq!(h.app.toast.as_deref(), Some("fresh"));
    assert!(h.app.busy.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn selection_stats_follow_selection_and_column() {
    let mut h = csv(b"id,name,amt\n1,a,1.5\n2,b,\n3,a,-4.25\n4,c,1000\n").await;
    assert_eq!(status(&h), "", "no hint line");
    h.keys("l l s s s").await;
    assert_eq!(
        status(&h),
        "sel 3 · sum -2.75 · avg -1.38 · min -4.25 · max 1.5"
    );
    h.keys("h").await; // text column: count, distinct
    assert_eq!(status(&h), "sel 3 · count 3 · distinct 2");
    h.keys("j s").await;
    assert_eq!(status(&h), "sel 4 · count 4 · distinct 3");
    h.keys("g u").await;
    assert_eq!(status(&h), "", "no hint line");
}

/// S-26 A9: a click puts the cursor on the cell under it (the header: just
/// the column); the wheel moves it 3 rows.
#[tokio::test(flavor = "multi_thread")]
async fn mouse_clicks_and_wheel_move_the_cursor() {
    use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
    let mut h = Harness::new(30).await;
    let at = |kind, column, row| MouseEvent {
        kind,
        column,
        row,
        modifiers: KeyModifiers::NONE,
    };
    let screen = h.screen();
    let header = screen.lines().nth(1).unwrap();
    let x = header.find("grp").unwrap() as u16;
    let y = screen.lines().position(|l| l.contains("name_3")).unwrap() as u16;
    h.app
        .on_mouse(at(MouseEventKind::Down(MouseButton::Left), x, y));
    h.pump().await;
    assert_eq!(h.current_column(), "grp");
    assert!(title(&h).contains("row 3/30"), "{}", title(&h));
    h.app
        .on_mouse(at(MouseEventKind::Down(MouseButton::Left), 1, 1));
    h.pump().await;
    assert_eq!(h.current_column(), "id");
    assert!(title(&h).contains("row 3/30"), "header click keeps the row");
    h.app.on_mouse(at(MouseEventKind::ScrollDown, 1, 5));
    h.pump().await;
    assert!(title(&h).contains("row 6/30"), "{}", title(&h));
    let p = crate::plugins::Plugins::load_from_source(r#"vd.option("mouse", "true")"#).unwrap();
    assert!(p.options().mouse);
    assert!(crate::plugins::Plugins::load_from_source(r#"vd.option("mouse", "yes")"#).is_err());
}

/// `H` `L` `gH` `gL` `zH` slide columns within their section (S-34); `U`
/// undoes a slide; `:order` and a session keep the order.
#[tokio::test(flavor = "multi_thread")]
async fn slides_reorder_columns_within_their_section() {
    let header = |h: &Harness| view_csv(h).lines().next().unwrap().to_string();
    let mut h = csv(b"a,b,c,d\n1,2,3,4\n").await;
    h.keys("l H").await;
    assert_eq!(header(&h), "b,a,c,d");
    assert_eq!(h.current_column(), "b");
    h.keys("g L").await;
    assert_eq!(header(&h), "a,c,d,b");
    assert_eq!(h.current_column(), "b");
    h.keys("L").await; // at the end: no move, no undo step
    h.keys("U").await;
    assert_eq!(header(&h), "b,a,c,d");
    // a key column stays among the keys; others can't pass them
    h.keys("g h l l !").await; // key `c`
    assert_eq!(header(&h), "c,b,a,d");
    h.keys("g l g H").await;
    assert_eq!(header(&h), "c,d,b,a");
    assert_eq!(h.current_column(), "d");
    h.keys("g l z H").await;
    h.type_("2").await;
    assert_eq!(header(&h), "c,a,d,b");
    h.keys(":").await;
    h.type_("order d a").await;
    assert_eq!(header(&h), "c,d,a,b");
    let s = h.app.sheets.active();
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.vdrs");
    crate::session::save(h.app.sheets.iter(), &path).unwrap();
    let script = std::fs::read_to_string(&path).unwrap();
    assert!(script.contains("\norder d a b\n"), "{script}");
    assert_eq!(s.view.order, ["d", "a", "b"]);
}

/// A session puts new columns back where they went, wherever its replay's
/// cursor is; `@` restores the type it picked.
#[tokio::test(flavor = "multi_thread")]
async fn session_keeps_new_columns_and_date_types() {
    let header = |h: &Harness| view_csv(h).lines().next().unwrap().to_string();
    let mut h = csv(b"a,b,c,t\n1,2,3,2018-07-03 10:05\n").await;
    h.keys("l l :").await;
    h.type_("derive m = a + 1").await;
    h.keys("g h :").await;
    h.type_("derive n = a + 2").await;
    assert_eq!(header(&h), "a,n,b,c,m,t");
    h.keys("g l @").await;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.vdrs");
    crate::session::save(h.app.sheets.iter(), &path).unwrap();
    let script = std::fs::read_to_string(&path).unwrap();
    assert!(script.contains("\ntype datetime\n"), "{script}");
    let mut r = Harness::new(3).await;
    r.app.source(&path.display().to_string(), false);
    r.pump().await;
    assert_eq!(header(&r), "a,n,b,c,m,t");
    let dt = r.app.sheets.active().view_plan().collect_schema().unwrap();
    assert!(
        matches!(dt.get("t"), Some(DataType::Datetime(..))),
        "{dt:?}"
    );
}

/// `@` picks date or datetime from the loaded rows; past them it says so.
#[tokio::test(flavor = "multi_thread")]
async fn date_cast_warns_when_rows_are_not_all_loaded() {
    let mut h = csv(b"t\n2018-07-03\n").await;
    h.keys("@").await;
    assert_eq!(h.app.toast, None);
    let mut big = String::from("t\n");
    for _ in 0..crate::sheet::DEFAULT_WINDOW_ROWS + 1 {
        big.push_str("2018-07-03\n");
    }
    big.push_str("2018-07-03 10:05\n");
    let mut h = csv(big.as_bytes()).await;
    h.keys("@").await;
    let toast = h.app.toast.clone().unwrap_or_default();
    assert!(toast.contains("no time in the loaded rows"), "{toast:?}");
}

/// `[` on a columns sheet reorders its sheet's columns, as VisiData; `U` on
/// the sheet undoes it (S-34 A4).
#[tokio::test(flavor = "multi_thread")]
async fn sorting_the_columns_sheet_reorders_the_sheet() {
    let header = |h: &Harness| view_csv(h).lines().next().unwrap().to_string();
    let mut h = csv(b"zeta,alpha,mid\n1,2,3\n").await;
    h.keys("C [").await;
    h.keys("q").await;
    assert_eq!(header(&h), "alpha,mid,zeta");
    h.keys("U").await;
    assert_eq!(header(&h), "zeta,alpha,mid");
}
/// A count past `isize` moves as far as it can, the way it was going.
#[tokio::test(flavor = "multi_thread")]
async fn huge_counts_saturate_instead_of_reversing() {
    let mut h = Harness::new(50).await;
    let huge = vec!["9"; 20].join(" ");
    h.keys(&format!("{huge} j")).await;
    assert_eq!(
        {
            let v = &h.app.sheets.active().view;
            v.window_offset + v.row_cursor as u64
        },
        49
    );
    h.keys(&format!("{huge} k")).await;
    assert_eq!(
        {
            let v = &h.app.sheets.active().view;
            v.window_offset + v.row_cursor as u64
        },
        0
    );
    h.keys(&format!("{huge} l")).await;
    assert!(h.app.sheets.active().view.col_cursor > 0);
    h.keys(&format!("{huge} h")).await;
    assert_eq!(h.app.sheets.active().view.col_cursor, 0);
}

/// VisiData's `zs` `zc` `zo` `_` `^V` on a sheet; the vdrs actions they
/// replaced (freeze, unfreeze, clear sort, visual block) live in the
/// vim profile.
#[tokio::test(flavor = "multi_thread")]
async fn visidata_z_keys_select_before_go_col_open_cell_resize() {
    let mut other = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    std::io::Write::write_all(&mut other, b"k\nopened\n").unwrap();
    let long = "x".repeat(60);
    let path = other.path().to_string_lossy().into_owned();
    let mut h = csv(format!("a,b,p\n1,{long},{path}\n2,y,\n3,z,\n4,w,\n").as_bytes()).await;

    h.keys("j j z s").await; // rows above the cursor, not the cursor's
    let sel = h.app.sheets.active().view.selected.len();
    assert_eq!(sel, 2, "{}", status(&h));

    h.keys("z c").await;
    h.type_("-1").await;
    assert_eq!(h.app.sheets.active().view.col_cursor, 2);
    h.keys("z c").await;
    h.type_("9").await;
    assert_eq!(h.app.sheets.active().view.col_cursor, 2);
    assert!(status(&h).contains("out of range"), "{}", status(&h));
    h.keys("z c").await;
    h.type_("1").await;
    assert_eq!(h.app.sheets.active().view.col_cursor, 1);

    let width = |h: &Harness| h.app.sheets.active().state.widths[1];
    let default = width(&h);
    assert!(default < 60, "{default}");
    h.keys("_").await;
    assert!(width(&h) >= 60, "{}", width(&h));
    h.keys("_").await;
    assert_eq!(width(&h), default);

    h.keys("<C-v>").await;
    assert!(
        status(&h).contains(env!("CARGO_PKG_VERSION")),
        "{}",
        status(&h)
    );

    h.keys("z c").await;
    h.type_("2").await;
    h.keys("j z o").await; // empty cell
    assert_eq!(status(&h), "no path given");
    h.keys("g g z o").await;
    h.pump().await;
    assert_eq!(first_cell(&h), "opened");
}

#[tokio::test(flavor = "multi_thread")]
async fn vim_profile_keeps_freeze_unfreeze_unsort() {
    let mut h = csv(b"a,b\n2,x\n1,y\n").await;
    h.app.plugins =
        Some(crate::plugins::Plugins::load_from_source(r#"vd.option("keymap", "vim")"#).unwrap());
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    h.keys("l z s").await;
    assert_eq!(h.app.sheets.active().view.frozen_left, 2);
    h.keys("z c").await;
    assert_eq!(h.app.sheets.active().view.frozen_left, 0);
    h.keys("0 [").await;
    assert_eq!(first_cell(&h), "1");
    h.keys("z o").await;
    assert_eq!(first_cell(&h), "2");
}

/// VisiData's `s` `t` `u` select / toggle / unselect then move down; `zu`
/// unselects the rows above the cursor; `g%` off the columns sheet asks for
/// float precision.
#[tokio::test(flavor = "multi_thread")]
async fn visidata_s_t_u_move_down_zu_unselects_above_g_percent_precision() {
    let mut h = csv(b"a,x\n1,1.5\n2,2.5\n3,3.5\n4,4.5\n").await;
    let sel = |h: &Harness| h.app.sheets.active().view.selected.len();
    let row = |h: &Harness| h.app.sheets.active().view.row_cursor;
    h.keys("s").await;
    assert_eq!((sel(&h), row(&h)), (1, 1));
    h.keys("k s").await; // s selects, never toggles off
    assert_eq!((sel(&h), row(&h)), (1, 1));
    h.keys("t t").await; // rows 2, 3 on
    assert_eq!((sel(&h), row(&h)), (3, 3));
    h.keys("k t").await; // row 3 off
    assert_eq!((sel(&h), row(&h)), (2, 3));
    h.keys("g g u").await;
    assert_eq!((sel(&h), row(&h)), (1, 1));
    h.keys("g s G z u").await; // the last row stays
    assert_eq!(sel(&h), 1, "{}", status(&h));
    h.keys("l g %").await; // on the float column
    assert_eq!(h.app.input.as_ref().unwrap().1.text(), "precision ");
    h.type_("3").await;
    assert!(h.screen().contains("1.500"), "{}", h.screen());
}

/// `zs` / `zu` are one `U` step each: the next `U` undoes what came before.
#[tokio::test(flavor = "multi_thread")]
async fn select_unselect_before_is_one_undo_step() {
    let mut h = csv(b"a\n1\n2\n3\n4\n").await;
    let sel = |h: &Harness| h.app.sheets.active().view.selected.len();
    h.keys("g s").await; // the selection lands before `zu` starts
    h.keys("G z u").await;
    assert_eq!(sel(&h), 1, "{}", status(&h));
    h.keys("U").await;
    assert_eq!(sel(&h), 4, "{}", status(&h));
    h.keys("U").await; // undoes `gs`
    assert_eq!(sel(&h), 0, "{}", status(&h));
    h.keys("g u").await;
    h.keys("G z s").await;
    assert_eq!(sel(&h), 3, "{}", status(&h));
    h.keys("U").await;
    assert_eq!(sel(&h), 0, "{}", status(&h));
}

/// `zc` before the window loads counts the plan's columns.
#[tokio::test(flavor = "multi_thread")]
async fn go_col_while_loading_uses_plan_columns() {
    let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    std::io::Write::write_all(&mut tmp, b"a,b,c\n1,2,3\n").unwrap();
    let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
    let mut h = Harness::from_app(App::new(Sheet::from_source(0, loaded), None, None)).await;
    assert!(h.app.sheets.active().window().is_none());
    h.keys("z c").await;
    h.type_("2").await;
    assert_eq!(h.app.sheets.active().view.col_cursor, 2, "{}", status(&h));
    assert!(!status(&h).contains("out of range"), "{}", status(&h));
}

/// `_` on a copy maxes a column its parent drew clamped (the inherited
/// floor is not "already maxed"), and toggles back.
#[tokio::test(flavor = "multi_thread")]
async fn resize_col_max_on_a_copy_maxes_first() {
    let long = "x".repeat(60);
    let mut h = csv(format!("a,b\n1,{long}\n").as_bytes()).await;
    h.keys("g \"").await; // a job pushes the copy
    h.keys("l").await;
    let width = |h: &Harness| h.app.sheets.active().state.widths[1];
    let default = width(&h);
    assert!(default < 60, "{default}");
    h.keys("_").await;
    assert!(width(&h) >= 60, "{}", width(&h));
    h.keys("_").await;
    assert_eq!(width(&h), default);
}

/// `_` before the window loads leaves the width alone.
#[tokio::test(flavor = "multi_thread")]
async fn resize_col_max_while_loading_toasts() {
    let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    std::io::Write::write_all(&mut tmp, b"a\n1\n").unwrap();
    let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
    let mut h = Harness::from_app(App::new(Sheet::from_source(0, loaded), None, None)).await;
    h.keys("_").await;
    assert_eq!(status(&h), "_: still loading");
    assert!(h.app.sheets.active().state.width_floor.is_empty());
}

/// `_` measures cells as drawn: a long date `fmtstr` counts, not the raw date.
#[tokio::test(flavor = "multi_thread")]
async fn resize_col_max_measures_display_format() {
    let mut h = csv(b"d\n2024-01-02\n").await;
    h.keys("@").await;
    h.keys(":").await;
    h.type_("fmtstr %Y-%m-%d %A %B %Y-%m-%d %A %B %Y").await;
    let width = |h: &Harness| h.app.sheets.active().state.widths[0];
    assert_eq!(width(&h), 40, "{}", h.screen());
    h.keys("_").await;
    assert!(width(&h) > 40, "{}", width(&h));
}

/// `g%` is VisiData's setcol-precision-input; `.` doesn't reopen the prompt.
/// On the columns sheet it is type-float-selected, and `.` repeats the cast.
#[tokio::test(flavor = "multi_thread")]
async fn g_percent_precision_prompt_and_columns_sheet_float() {
    let mut h = csv(b"a,b\n1,2\n").await;
    h.keys("g %").await;
    assert!(h.app.input.is_some());
    assert_eq!(h.app.last_action, None);
    h.keys("<Esc>").await;
    h.keys("C s g % q").await;
    let dtype = |h: &Harness, c: &str| {
        h.app
            .sheets
            .active()
            .window()
            .unwrap()
            .column(c)
            .unwrap()
            .dtype()
            .clone()
    };
    assert_eq!(dtype(&h, "a"), DataType::Float64, "{}", status(&h));
    assert_eq!(dtype(&h, "b"), DataType::Int64);
    assert_eq!(
        h.app.last_action,
        Some(vec![Action::TypeSelected { name: "float" }])
    );
}

/// `zo` opens the cell as displayed; a missing file gets VisiData's message.
#[tokio::test(flavor = "multi_thread")]
async fn open_cell_file_missing_says_so() {
    let mut h = csv(b"p\n/no/such/file.csv\n").await;
    h.keys("z o").await;
    assert_eq!(status(&h), "file /no/such/file.csv does not exist");
    assert_eq!(h.app.sheets.len(), 1);
}

/// S-36 C: `:footer` shows each `+` aggregator over the filtered view under
/// its column, in its number format; a filter or an edit updates it.
#[tokio::test(flavor = "multi_thread")]
async fn footer_totals_follow_filter_and_edits() {
    let mut h = csv(b"k,amt\na,1000\nb,2500.5\nc,3\n").await;
    let footer = |h: &Harness| h.screen().lines().nth(10).unwrap_or_default().to_string();
    h.keys("l +").await;
    h.type_("sum max").await;
    h.keys(":").await;
    h.type_("footer").await;
    assert_eq!(status(&h), "disp_footer=true");
    assert!(footer(&h).contains("Σ 3 "), "{}", h.screen());
    assert!(
        footer(&h).contains("sum 3,503.5 · max 2,500.5"),
        "{}",
        h.screen()
    );
    h.keys(":").await;
    h.type_("fmtstr {:,.0f}").await;
    assert!(
        footer(&h).contains("sum 3,504 · max 2,500"),
        "{}",
        h.screen()
    );
    h.keys("h :").await;
    h.type_("filter k [ab]").await;
    assert!(
        footer(&h).contains("Σ 2 ") && footer(&h).contains("sum 3,500"),
        "{}",
        h.screen()
    );
    h.keys("l e <C-u>").await;
    h.type_("10").await;
    assert!(footer(&h).contains("sum 2,510"), "{}", h.screen());
    // the cursor still reaches the last row above it
    h.keys("G").await;
    assert!(title(&h).contains("row 2/2"), "{}", title(&h));
    h.keys(":").await;
    h.type_("footer").await;
    assert!(!h.screen().contains("Σ"), "{}", h.screen());
}

/// S-37 P0c: `O` lists every option; `e` on `value` checks and sets it now;
/// `z<C-s>` writes `options.lua`, which loads back to the same value.
#[tokio::test(flavor = "multi_thread")]
async fn options_sheet_sets_now_and_commits() {
    let path = crate::plugins::options_path().unwrap();
    let _ = std::fs::remove_file(&path);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "-- by hand\n").unwrap();
    let mut h = Harness::new(3).await;
    h.keys("O").await;
    assert!(title(&h).contains("options"), "{}", title(&h));
    let df = h.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(df.height(), crate::options::OPTIONS.len());
    let row = crate::options::OPTIONS
        .iter()
        .position(|o| o.name == "numeric_binning")
        .unwrap();
    h.keys(&"j ".repeat(row)).await;
    h.keys("e").await;
    assert_eq!(status(&h), "options: e edits value");
    h.keys("l e <C-u>").await;
    h.type_("yes").await;
    assert!(
        status(&h).contains("\"true\" or \"false\""),
        "{}",
        status(&h)
    );
    h.keys("e <C-u>").await;
    h.type_("true").await;
    assert_eq!(status(&h), "numeric_binning=true");
    assert!(h.app.theme.numeric_binning);
    assert!(
        h.screen()
            .lines()
            .any(|l| l.starts_with("numeric_binning") && l.contains("true ")),
        "{}",
        h.screen()
    );
    h.keys("z <C-s>").await;
    assert!(status(&h).starts_with("options: 1 saved"), "{}", status(&h));
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        text,
        "-- by hand\nvd.option(\"numeric_binning\", \"true\")\n"
    );
    let p = crate::plugins::Plugins::load_from_source(&text).unwrap();
    assert!(p.options().numeric_binning);
    // `:set` goes through the same table
    h.keys("q :").await;
    h.type_("set keymap vim").await;
    assert_eq!(status(&h), "keymap=vim (from the next start)");
    h.keys(":").await;
    h.type_("set nope 1").await;
    assert!(
        status(&h).starts_with(":set: unknown option nope ("),
        "{}",
        status(&h)
    );
    h.keys(":").await;
    h.type_("set histogram_bins").await;
    assert_eq!(status(&h), "histogram_bins=0");
}

/// S-41 A: `\\` unselects by regex on the cursor column, `g|` / `g\\` match
/// any visible column.
#[tokio::test(flavor = "multi_thread")]
async fn regex_select_any_column() {
    let mut h = csv(b"a,b\nX1,y\nz,x2\nq,q\n").await;
    let sel = |h: &Harness| h.app.sheets.active().view.selected.len();
    h.keys("g |").await;
    h.type_("x").await;
    assert_eq!(sel(&h), 2, "{}", status(&h));
    h.keys("\\").await;
    h.type_("1").await;
    assert_eq!(sel(&h), 1, "{}", status(&h));
    h.keys("g |").await;
    h.type_("q").await;
    h.keys("g \\").await;
    h.type_("2|q").await;
    assert_eq!(sel(&h), 0, "{}", status(&h));
    h.keys("-").await; // hidden columns don't match
    h.keys("g |").await;
    h.type_("x1").await; // matches the hidden X1 only, ignoring case
    assert_eq!(sel(&h), 0, "{}", status(&h));
}

/// S-41 B: `z}` selects down to the next selected row, `z{` up to the
/// previous one; with none, a toast.
#[tokio::test(flavor = "multi_thread")]
async fn select_to_next_and_prev_selected() {
    let mut h = csv(b"n\nr0\nr1\nr2\nr3\nr4\nr5\nr6\n").await;
    let sel = |h: &Harness| {
        h.app
            .sheets
            .active()
            .view
            .selected
            .iter()
            .copied()
            .collect::<Vec<_>>()
    };
    h.keys("5 j s").await; // r5 selected, cursor on r6
    h.keys("g g j z }").await;
    assert_eq!(sel(&h), [1, 2, 3, 4, 5], "{}", status(&h));
    h.keys("g u G z {").await;
    assert!(status(&h).contains("no selected row"), "{}", status(&h));
    h.keys("g g s G z {").await;
    assert_eq!(sel(&h).len(), 7, "{}", status(&h));
}

/// S-41 C: `r` searches the key columns (the first visible column without
/// keys) and keeps the cursor column; `n` repeats it.
#[tokio::test(flavor = "multi_thread")]
async fn search_keys_keeps_the_column() {
    let mut h = csv(b"id,k,v\n1,a,b\n2,b,a\n3,b,c\n").await;
    let pos = |h: &Harness| {
        let v = &h.app.sheets.active().view;
        (v.window_offset + v.row_cursor as u64, v.col_cursor)
    };
    h.keys("l l r").await; // no keys: the first column
    h.type_("3").await;
    assert_eq!(pos(&h), (2, 2), "{}", status(&h));
    h.keys("h !").await; // key k: it moves to the front (k, id, v)
    h.keys("g g l r").await;
    h.type_("B").await;
    assert_eq!(pos(&h), (1, 1), "{}", status(&h));
    h.keys("n").await;
    assert_eq!(pos(&h), (2, 1), "{}", status(&h));
}

/// S-41 F: `zs` on the describe sheet selects the rows a stat describes
/// on the sheet below; an even count's median takes both middle rows.
#[tokio::test(flavor = "multi_thread")]
async fn describe_select_cell() {
    let mut h = csv(b"n,k\n1,a\n2,b\n3,c\n4,d\n,e\n").await;
    h.keys("I").await;
    assert!(h.app.sheets.active().name.starts_with("describe"));
    // column errors nulls distinct mode min max sum median
    h.keys("8 l z s").await;
    h.keys("2 h z s").await; // max
    h.keys("4 h z s").await; // nulls
    h.keys("q").await;
    let sel: Vec<u64> = h
        .app
        .sheets
        .active()
        .view
        .selected
        .iter()
        .copied()
        .collect();
    assert_eq!(sel, [1, 2, 3, 4], "{}", status(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn visidata_motion_sort_and_prompt_keys() {
    let mut h = csv(b"n,k\n1,a\n1,b\n,c\n2,d\n").await;
    let row = |h: &Harness| h.app.sheets.active().view.row_cursor;
    h.keys(">").await; // 1 → the null differs
    assert_eq!(row(&h), 2);
    h.keys(">").await;
    assert_eq!(row(&h), 3);
    h.keys("z <").await;
    assert_eq!(row(&h), 2);
    h.keys("<").await;
    assert_eq!(row(&h), 1);
    h.keys("g g z >").await;
    assert_eq!(row(&h), 2);

    h.keys("g g s j s").await; // rows 0 and 2 selected, cursor on 3
    h.keys("{").await;
    assert_eq!(row(&h), 2, "{}", status(&h));
    h.keys("{").await;
    assert_eq!(row(&h), 0, "{}", status(&h));
    h.keys("}").await;
    assert_eq!(row(&h), 2, "{}", status(&h));

    h.keys("c").await;
    h.type_("^k$").await;
    assert_eq!(h.app.sheets.active().view.col_cursor, 1);
    h.keys("! g ]").await;
    assert!(
        view_csv(&h).starts_with("k,n\nd,2\nc,\n"),
        "{}",
        view_csv(&h)
    );
    h.keys("M").await;
    assert!(
        view_csv(&h).starts_with("k,Variable,Value\n"),
        "{}",
        view_csv(&h)
    );
    let below = h.app.sheets.iter().rev().nth(1).unwrap().name.clone();

    let line = |h: &Harness| h.app.input.as_ref().map(|(_, l)| l.text().to_string());
    h.keys("o").await;
    assert_eq!(line(&h).as_deref(), Some("open "));
    h.keys("<Esc> <C-s>").await;
    assert_eq!(line(&h).as_deref(), Some("save "));
    h.keys("<Esc> &").await;
    assert_eq!(line(&h), Some(format!("join {below} ")));
}

#[tokio::test(flavor = "multi_thread")]
async fn title_crumbs_and_status_names_the_last_command() {
    let mut h = Harness::new(5).await;
    let base = h.app.sheets.active().name.clone();
    assert!(h.screen().lines().last().unwrap().trim().is_empty());
    h.keys("l l F").await;
    assert!(
        title(&h).starts_with(&format!(" {base} › freq(grp) ")),
        "{}",
        title(&h)
    );
    let last = h.screen().lines().last().unwrap().to_string();
    assert!(last.trim_end().ends_with("F  open_freq_table"), "{last:?}");
    h.keys("<CR>").await;
    assert!(title(&h).contains(" › freq(grp) › grp="), "{}", title(&h));
    h.keys(":").await;
    h.type_("derive d = id * 2").await;
    assert_eq!(h.screen().lines().last().unwrap().trim(), "derive");
}

#[tokio::test(flavor = "multi_thread")]
async fn title_crumbs_cut_only_the_parents_name() {
    let mut h = Harness::new(5).await;
    let base = h.app.sheets.active().name.clone();
    h.keys("l l F").await;
    h.keys("<CR>").await;
    h.keys("F").await;
    // `freq(id): grp=1: <base>`: its parent is `grp=1: <base>`, not the base,
    // whose name it also ends in.
    let want = format!(" {base} › freq(grp) › grp=1 › freq(id) ");
    assert!(title(&h).starts_with(&want), "{}", title(&h));
}
