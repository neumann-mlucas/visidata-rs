//! Derived sheets: freq, joins, pivots, SQL, diff, refresh, notes, splits, plots, colors.

use super::*;

/// `F` ties keep first-seen order, as VisiData's stable sort by count.
#[tokio::test(flavor = "multi_thread")]
async fn freq_ties_keep_first_seen_order() {
    let mut h = csv(b"k\nz\nb\nz\nm\nb\na\n").await;
    h.keys("F").await;
    assert_eq!(
        view_csv(&h)
            .lines()
            .map(|l| l.split(',').next().unwrap())
            .collect::<Vec<_>>(),
        ["k", "z", "b", "m", "a"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn bad_user_input_is_rejected_without_breaking_the_sheet() {
    let mut h = Harness::new(30).await;
    h.keys(":").await;
    h.type_("sort nosuch").await;
    assert_eq!(status(&h), "no column: nosuch");
    h.keys(":").await;
    h.type_("filter nosuch x").await;
    assert_eq!(status(&h), "no column: nosuch");
    h.keys("^").await;
    h.type_("grp").await; // id → grp would duplicate
    assert_eq!(status(&h), "column already exists: grp");
    // sheet still healthy: derived sheets build
    h.keys("F").await;
    assert!(
        title(&h).contains("freq(id)") && title(&h).ends_with("/30"),
        "{}",
        title(&h)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn long_sheet_name_never_hides_position() {
    let mut h = Harness::new(3).await;
    h.app.sheets.active_mut().name = format!("/{}/data.csv", "very/long/dir".repeat(20));
    h.pump().await;
    let t = title(&h);
    assert!(
        t.contains("…") && t.contains("data.csv") && t.contains("row 1/3"),
        "{t}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_sheet_survives_every_basic_key() {
    let mut h = Harness::new(0).await;
    for k in [
        "G", "j", "k", "<C-d>", "<C-u>", "g g", "s", "t", "u", "g s", "n", "$", "F", "q",
    ] {
        h.keys(k).await;
        assert!(
            title(&h).contains("row 0/0") && !title(&h).contains("sel "),
            "{k}: {}",
            title(&h)
        );
    }
    h.keys("\"").await;
    assert_eq!(status(&h), "dup: no rows selected");
}

#[tokio::test(flavor = "multi_thread")]
async fn join_types_against_file_and_open_sheet() {
    let mut h = Harness::new(5).await; // id 1..5
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("o.csv");
    std::fs::write(&other, "id,name,extra\n2,TWO,x\n3,,y\n9,NINE,z\n").unwrap();
    let o = other.display();
    for (how, rows) in [("inner", 2), ("left", 5), ("full", 6), ("append", 8)] {
        h.keys(":").await;
        h.type_(&format!("join {o} id {how}")).await;
        assert!(
            title(&h).contains(&format!("row 1/{rows}")),
            "{how}: {}",
            h.screen()
        );
        h.keys("q").await;
    }
    // merge: right's non-null name wins, left kept where right is null
    h.keys(":").await;
    h.type_(&format!("join {o} id merge")).await;
    let s = h
        .app
        .sheets
        .active()
        .view_plan()
        .sort(["id"], Default::default());
    let df = s.collect().unwrap();
    let names: Vec<_> = df.column("name").unwrap().str().unwrap().iter().collect();
    assert_eq!(names[1..3], [Some("TWO"), Some("name_3")]);
    assert!(df.column("name_right").is_err());
    // an open sheet as the right side, by #id: freq(name) joined back on name
    h.keys("q l F").await; // freq sheet on `name`
    let freq_id = h.app.sheets.active().id;
    h.keys("S <CR>").await; // switcher row 1 = base sheet → back on top
    assert!(title(&h).contains("row 1/5"), "{}", title(&h));
    h.keys(":").await;
    h.type_(&format!("join #{freq_id} name")).await;
    let df = h.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(df.height(), 5);
    assert!(df.column("count").is_ok(), "{:?}", df.get_column_names());
}

#[tokio::test(flavor = "multi_thread")]
async fn freq_dive_opens_matching_rows() {
    let mut h = Harness::new(5000).await; // grp = id % 7: 1..=5 → 715 rows, 0 and 6 → 714
    h.keys("l l F").await;
    assert_eq!(first_cell(&h), "1"); // count tie broken by value
    h.keys("<CR>").await;
    assert!(
        title(&h).contains("grp=1") && title(&h).contains("row 1/715"),
        "{}",
        title(&h)
    );
    let df = h.app.sheets.active().view_plan().collect().unwrap();
    assert!(
        df.column("grp")
            .unwrap()
            .i64()
            .unwrap()
            .iter()
            .all(|v| v == Some(1))
    );
    h.keys("q s s g <CR>").await; // two freq rows selected → union
    assert!(title(&h).contains("row 1/1430"), "{}", title(&h));
    h.keys("q q <CR>").await; // <CR> on a data sheet explains itself
    assert_eq!(status(&h), "<CR>: nothing to open here");
}

/// S-35 C1: with `numeric_binning`, `F` on a number counts ranges in a job,
/// and `<CR>` on a range opens the rows in it.
#[tokio::test(flavor = "multi_thread")]
async fn binned_freq_counts_ranges_and_dives() {
    let mut h = Harness::new(5000).await;
    h.keys(":").await;
    h.type_("set numeric_binning true").await;
    assert_eq!(status(&h), "numeric_binning=true");
    h.keys(":").await;
    h.type_("set histogram_bins 4").await;
    h.keys("F").await;
    let df = h.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(df.height(), 4, "{}", h.screen());
    assert!(
        h.screen().contains("1.00 - 1250.75        1250"),
        "{}",
        h.screen()
    );
    h.keys("<CR>").await;
    assert!(title(&h).contains("row 1/1250"), "{}", title(&h));
    h.keys("q s s g <CR>").await; // two ranges → union
    assert!(title(&h).contains("row 1/2500"), "{}", title(&h));
    // a session writes the options its `F` ran under (S-38 A); they stay set
    h.keys("q").await;
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.vdrs");
    crate::session::save(h.app.sheets.iter(), &path).unwrap();
    let script = std::fs::read_to_string(&path).unwrap();
    assert!(
        script.contains("set numeric_binning true\nset histogram_bins 4\n"),
        "{script}"
    );
    let mut r = Harness::new(3).await;
    r.app.source(&path.display().to_string(), false);
    r.pump().await;
    let df = r.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(df.height(), 4, "{}", r.screen());
    assert!(r.app.theme.numeric_binning);
}

/// Narrow bins share a label: `<CR>` opens its own bin, and a session keeps
/// the dive (S-35 C1).
#[tokio::test(flavor = "multi_thread")]
async fn binned_dive_opens_its_own_bin() {
    let mut h = csv(b"x\n0\n0.0002\n0.0008\n0.0009\n0.001\n").await;
    h.keys(":").await;
    h.type_("set numeric_binning true").await;
    h.keys(":").await;
    h.type_("set histogram_bins 2").await;
    h.keys("F").await;
    h.keys("j <CR>").await; // both bins read `0.00 - 0.00`
    assert!(title(&h).contains("row 1/3"), "{}", title(&h));
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("s.vdrs");
    crate::session::save(h.app.sheets.iter(), &path).unwrap();
    let script = std::fs::read_to_string(&path).unwrap();
    assert!(!script.contains("# not saved"), "{script}");
    let mut r = Harness::new(3).await;
    r.app.source(&path.display().to_string(), false);
    r.pump().await;
    let df = r.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(df.height(), 3, "{}", r.screen());
}

#[tokio::test(flavor = "multi_thread")]
async fn freq_rows_picked_from_a_freq_sheet_still_dive() {
    let mut h = Harness::new(5000).await; // grp 1, 2: 715 rows; 0, 3..=6: 714
    h.keys("l l F").await;
    h.keys("l F").await; // freq of the freq sheet's count
    h.keys("<CR>").await; // count 715 comes first: grp 1 and 2
    assert!(title(&h).contains("row 1/2"), "{}", title(&h));
    h.keys("<CR>").await; // and the source rows of the first one
    assert!(
        title(&h).contains("grp=1") && title(&h).contains("row 1/715"),
        "{}",
        title(&h)
    );
    // with grp hidden on the freq sheet the dived rows lack it: no dive on
    h.keys("q q q").await;
    h.keys("g h -").await;
    h.keys("F").await;
    h.keys("<CR>").await;
    h.keys("<CR>").await;
    assert_eq!(status(&h), "<CR>: nothing to open here", "{}", h.screen());
}

/// A dive of a dive keys on what the first `F` grouped by (a date fmtstr, a
/// bin), though the sheet it dives from was made from a freq sheet.
#[tokio::test(flavor = "multi_thread")]
async fn dive_of_dive_keys_on_the_first_freq() {
    let mut h = csv(b"d,v\n2024-01-05,1\n2024-01-20,2\n2024-02-03,3\n").await;
    h.keys("@ :").await;
    h.type_("fmtstr %Y-%m").await;
    h.keys("F").await;
    h.keys("F").await;
    h.keys("<CR>").await;
    assert!(title(&h).contains("row 1/1"), "{}", h.screen());
    h.keys("<CR>").await;
    assert!(title(&h).contains("row 1/2"), "{}", h.screen());

    let mut h = csv(b"x\n1\n2\n3\n10\n").await;
    h.keys(":").await;
    h.type_("set numeric_binning true").await;
    h.keys(":").await;
    h.type_("set histogram_bins 2").await;
    h.keys("F").await;
    h.keys("F").await;
    h.keys("j <CR>").await;
    assert!(title(&h).contains("row 1/1"), "{}", title(&h));
    h.keys("<CR>").await; // the second bin: 10 alone
    assert_eq!(view_csv(&h), "x\n10\n", "{}", h.screen());
}

/// The null bin holds NaN too: its dive opens every row it counted, under a
/// name that says which bin.
#[tokio::test(flavor = "multi_thread")]
async fn null_bin_dive_opens_nan_rows() {
    let mut h = csv(b"x,k\n1,a\n2,b\nNaN,c\n10,d\n,e\n").await;
    h.keys(":").await;
    h.type_("set numeric_binning true").await;
    h.keys(":").await;
    h.type_("set histogram_bins 2").await;
    h.keys("F").await;
    assert!(
        view_csv(&h).ends_with("\n,2,40.0,■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■■\n"),
        "{}",
        view_csv(&h)
    );
    h.keys("G <CR>").await;
    assert!(title(&h).contains("row 1/2"), "{}", h.screen());
    assert!(
        h.app.sheets.active().name.starts_with("x in null bin: "),
        "{}",
        h.app.sheets.active().name
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn diff_prompt_uses_key_columns_and_export_writes_scripts() {
    let mut h = Harness::new(3).await;
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("o.csv");
    std::fs::write(&other, "id,name,grp,amt\n1,name_1,1,1.5\n2,X,2,3.0\n").unwrap();
    h.keys("! D").await; // key = id
    h.type_(&other.display().to_string()).await;
    assert!(title(&h).contains("row 1/2"), "{}", h.screen()); // id 2 changed, id 3 only_a
    h.keys("q").await;
    for ext in ["py", "sql"] {
        let out = dir.path().join(format!("view.{ext}"));
        h.keys(":").await;
        h.type_(&format!("export {}", out.display())).await;
        let script = std::fs::read_to_string(&out).unwrap();
        assert!(script.contains(".csv"), "{script}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn parts_list_opens_each_table() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("two.db");
    let c = rusqlite::Connection::open(&db).unwrap();
    c.execute_batch("create table a(x); insert into a values (1); create table b(y); insert into b values (8),(9);")
        .unwrap();
    drop(c);
    let loaded = crate::reader::open(db.as_os_str(), None).unwrap();
    let parts = crate::reader::list_parts(&db, loaded.kind).unwrap();
    let mut app = App::new(Sheet::parts_list(0, loaded, parts), None, None);
    app.spawn_materialize();
    let mut h = Harness::from_app(app).await;
    assert!(
        title(&h).contains("tables") && title(&h).contains("row 1/2"),
        "{}",
        title(&h)
    );
    h.keys("j <CR>").await;
    assert!(
        title(&h).contains(":b") && title(&h).contains("row 1/2"),
        "{}",
        title(&h)
    );
}

/// S-42 D: an HTML page lists its tables and links, a zip its members;
/// `<CR>` opens one.
#[tokio::test(flavor = "multi_thread")]
async fn html_and_zip_list_their_parts() {
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("p.html");
    std::fs::write(
        &page,
        "<h2>Prices</h2><table id=prices><tr><th>item</th><th>usd</th></tr>\
         <tr><td>tea</td><td>3</td></tr><tr><td>jam</td><td>5</td></tr></table>",
    )
    .unwrap();
    let loaded = crate::reader::open(page.as_os_str(), None).unwrap();
    let mut app = App::new(Sheet::opened(0, loaded), None, None);
    app.spawn_materialize();
    let mut h = Harness::from_app(app).await;
    assert!(title(&h).contains("row 1/2"), "{}", title(&h)); // prices, links
    assert!(h.screen().contains("prices"), "{}", h.screen());
    h.keys("<CR>").await;
    assert!(
        title(&h).contains(":prices") && title(&h).contains("row 1/2"),
        "{}",
        title(&h)
    );
    assert_eq!(first_cell(&h), "tea", "{}", h.screen());

    let zip = dir.path().join("a.zip");
    {
        use std::io::Write;
        let mut z = ::zip::ZipWriter::new(std::fs::File::create(&zip).unwrap());
        z.start_file("d/n.csv", ::zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(b"n\n1\n2\n3\n").unwrap();
        z.finish().unwrap();
    }
    let loaded = crate::reader::open(zip.as_os_str(), None).unwrap();
    let mut app = App::new(Sheet::opened(0, loaded), None, None);
    app.spawn_materialize();
    let mut h = Harness::from_app(app).await;
    assert!(
        title(&h).contains("members") && title(&h).contains("row 1/1"),
        "{}",
        title(&h)
    );
    h.keys("<CR>").await;
    assert!(
        title(&h).contains("d/n.csv") && title(&h).contains("row 1/3"),
        "{}",
        title(&h)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn duckdb_index_exec_sql_open_and_save() {
    let have = std::process::Command::new("duckdb")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !have {
        assert!(
            std::env::var_os("VDRS_REQUIRE_TOOLS").is_none(),
            "duckdb isn't usable"
        );
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("app.duckdb");
    let mut df = df!("id" => [1i64, 2], "note" => ["a", "b"]).unwrap();
    crate::duckdb::save(&mut df, &db, "t").unwrap();
    let loaded = crate::reader::open(db.as_os_str(), None).unwrap();
    let mut app = App::new(Sheet::parts_list(0, loaded, Vec::new()), None, None);
    app.spawn_materialize();
    let mut h = Harness::from_app(app).await;
    assert!(
        title(&h).contains("tables") && title(&h).contains("row 1/1"),
        "{}",
        title(&h)
    );
    // DDL: nothing to show, the index reloads in place.
    h.keys(":").await;
    h.type_("exec-sql CREATE SCHEMA o; CREATE TABLE o.t AS SELECT 42 AS x")
        .await;
    assert!(
        title(&h).contains("tables") && title(&h).contains("row 1/2"),
        "{}",
        title(&h)
    );
    // Same name, other schema: `<CR>` opens o.t, not main.t.
    h.keys("j <CR>").await;
    assert!(
        title(&h).contains(":o.t") && title(&h).contains("row 1/1"),
        "{}",
        title(&h)
    );
    // A query from a table sheet runs in its file and opens the rows.
    h.keys(":").await;
    h.type_("exec-sql SELECT count(*) AS n FROM main.t").await;
    let n = h
        .app
        .sheets
        .active()
        .window()
        .unwrap()
        .column("n")
        .unwrap()
        .get(0)
        .unwrap()
        .try_extract::<i64>()
        .unwrap();
    assert_eq!(n, 2);
    // `:save` into another .duckdb: a table named after the sheet.
    h.keys("q").await;
    let out = dir.path().join("out.duckdb");
    h.keys(":").await;
    h.type_(&format!("save {}", out.display())).await;
    let ix = crate::duckdb::index(&out).unwrap();
    assert_eq!(
        ix.column("name").unwrap().get(0).unwrap().get_str(),
        Some("t")
    );
    assert_eq!(
        ix.column("cols")
            .unwrap()
            .get(0)
            .unwrap()
            .try_extract::<i64>()
            .unwrap(),
        1
    ); // Back on the index, cursor on o.t's row: dropping it leaves one row.
    h.keys("q").await;
    h.keys(":").await;
    h.type_("exec-sql DROP TABLE o.t").await;
    assert!(
        title(&h).contains("tables") && title(&h).contains("row 1/1"),
        "{}",
        title(&h)
    );
    h.keys("<CR>").await;
    assert!(title(&h).contains(":main.t"), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn pivot_prompt_previews_live() {
    let mut h = Harness::new(20).await;
    h.keys(":").await;
    h.type_no_enter("pivot grp").await;
    assert!(h.screen().contains("next: <columns>"), "{}", h.screen());
    h.type_no_enter(" nope").await;
    assert!(
        h.screen().contains("unknown column: nope"),
        "{}",
        h.screen()
    );
    for _ in 0..4 {
        h.keys("<BS>").await;
    }
    h.type_no_enter("na").await; // half-typed: still a hint, not an error
    assert!(h.screen().contains("next: <columns>"), "{}", h.screen());
    h.type_no_enter("me id").await;
    let screen = h.screen();
    assert!(screen.contains("pivot preview"), "{screen}");
    eprintln!("{screen}");
    assert!(screen.contains("name_1"), "{screen}"); // pivoted column header
    h.type_no_enter(" bogus").await; // bad agg: error in the panel, no toast
    assert!(h.screen().contains("unknown agg: bogus"), "{}", h.screen());
    assert!(h.app.toast.is_none());
    h.keys("<Esc>").await;
    assert!(!h.screen().contains("pivot"), "{}", h.screen());
}

#[tokio::test(flavor = "multi_thread")]
async fn select_equal_and_freq_dive_match_datetimes() {
    let mut h = csv(b"t,y\n2024-01-02 03:04:05,a\n2024-01-03 03:04:05,b\n").await;
    h.keys(",").await;
    assert_eq!(h.app.sheets.active().view.selected.len(), 1);
    h.keys("F").await;
    h.keys("<CR>").await;
    assert!(title(&h).contains("row 1/1"), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn row_diff_tells_null_cells_from_missing_rows() {
    let dir = tempfile::tempdir().unwrap();
    let b = dir.path().join("b.csv");
    std::fs::write(&b, "id,v,w\n1,z,1\n2,,1\n3,y,1\n4,y,\n6,y,1\n").unwrap();
    let mut h = csv(b"id,v,w\n1,,1\n2,x,1\n3,y,1\n4,y,9\n5,y,1\n").await;
    h.keys(":").await;
    h.type_(&format!("diff {} id", b.display())).await;
    let mut df = h.app.sheets.active().view_plan().collect().unwrap();
    df = df.sort(["id"], Default::default()).unwrap();
    let tags: Vec<_> = df
        .column("__diff")
        .unwrap()
        .str()
        .unwrap()
        .iter()
        .flatten()
        .collect();
    assert_eq!(tags, ["changed", "changed", "changed", "only_a", "only_b"]);
    assert!(
        !df.get_column_names()
            .iter()
            .any(|n| n.starts_with("__vdrs"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn w_works_on_a_key_column_and_with_text_null() {
    let mut h = csv(b"a,b\nx,1\ny,2\n").await;
    h.keys("! W").await;
    assert_eq!(h.app.toast, None);
    assert!(title(&h).contains("pivot(a)"), "{}", title(&h));
    let mut h = csv(b"a,b\nnull,1\n,2\n").await;
    h.keys("W").await;
    assert_eq!(h.app.toast, None);
    assert!(h.screen().contains("null"), "{}", h.screen());
    assert!(
        h.screen().contains('2'),
        "two rows count as null: {}",
        h.screen()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn freq_dive_survives_renaming_the_value_column() {
    let mut h = csv(b"a\nx\ny\nx\n").await;
    h.keys("F ").await;
    h.keys("^").await;
    h.type_("val").await;
    h.keys("<CR>").await;
    assert_eq!(h.app.toast, None);
    assert!(title(&h).contains("row 1/2"), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_file_open_is_reported_and_keeps_the_sheet() {
    let mut h = csv(b"a\n1\n").await;
    for cmd in [
        "tail /no/such.csv",
        "diff /no/such.csv a",
        "join /no/such.csv a",
    ] {
        h.keys(":").await;
        h.type_(cmd).await;
        assert!(
            h.app
                .toast
                .as_deref()
                .is_some_and(|t| t.contains("/no/such")),
            "{cmd}"
        );
        assert_eq!(h.app.sheets.len(), 1);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sheet_changed_from_gc_redraws_when_raised_from_the_sheet_list() {
    let mut h = Harness::new(5).await;
    h.keys("F").await; // the base sheet is not under the columns sheet
    h.keys("g C").await;
    h.keys("d").await; // hide the base sheet's `id`
    h.keys("S").await;
    h.keys("<CR>").await; // switcher row 1: the base sheet
    // The crumbs end at the active sheet, raised to the top.
    let t = title(&h);
    let crumb = t.rsplit('›').next().unwrap_or_default();
    assert!(!crumb.contains("freq") && !crumb.contains("columns"), "{t}");
    let hd = h.screen().lines().nth(1).unwrap().to_string();
    assert!(!hd.contains("id") && hd.contains("name"), "{hd}");
}

#[tokio::test(flavor = "multi_thread")]
async fn hits_sheet_lists_hits_and_enter_jumps_back() {
    let mut h = Harness::new(30).await;
    h.keys("]").await; // sort by id descending: hit rows are view rows
    h.keys(":").await;
    h.type_("hits name_2").await; // name_2, name_20..29
    assert!(
        title(&h).contains("hits(name_2)") && title(&h).contains("row 1/11"),
        "{}",
        title(&h)
    );
    let df = h.app.sheets.active().window().unwrap().clone();
    assert_eq!(
        df.column("column").unwrap().str().unwrap().get(0),
        Some("name")
    );
    assert_eq!(
        df.column("value").unwrap().str().unwrap().get(0),
        Some("name_29")
    );
    h.keys("j <CR>").await;
    assert_eq!(h.current_column(), "name");
    assert!(title(&h).contains("row 3/30"), "{}", title(&h)); // name_28: 3rd by id desc
    assert_eq!(h.app.sheets.len(), 2, "the hit sheet stays");
}

#[test]
fn hits_cap_says_so() {
    let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    writeln!(tmp, "a").unwrap();
    for _ in 0..50 {
        writeln!(tmp, "x").unwrap();
    }
    tmp.flush().unwrap();
    let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
    let s = Sheet::from_source(0, loaded);
    let (hits, capped) = Sheet::hits_from(&s, 1, "x", 10).unwrap();
    assert!(capped);
    assert_eq!(hits.plan.collect().unwrap().height(), 10);
}

/// A chain refreshes parents first: the freq of a dive follows the base.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_walks_the_lineage_parents_first() {
    let mut h = csv(b"id,region\n1,a\n2,b\n3,a\n4,c\n").await;
    h.keys("g s \"").await; // a copy of all four rows (selected)
    h.keys("l F").await;
    assert!(title(&h).ends_with("/3"), "{}", title(&h));
    let base = h.app.sheets.iter().next().unwrap().name.clone();
    raise(&mut h, &base).await;
    h.keys("g z |").await; // filter the base
    h.type_("^a$").await;
    raise(&mut h, "freq(").await;
    assert!(title(&h).contains("freq("), "{}", title(&h));
    h.keys("<C-r>").await;
    assert!(title(&h).ends_with("/1"), "{}", title(&h));
}

/// Edits made on a derived sheet are replayed onto the refreshed rows.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_keeps_the_edits() {
    let mut h = csv(b"id,region\n1,a\n2,b\n3,a\n").await;
    h.keys("g s").await;
    h.keys("\"").await;
    h.keys(":").await;
    h.type_("derive dbl = id * 2").await;
    h.keys("<C-r>").await;
    assert!(title(&h).contains("selected"), "{}", title(&h));
    let v = view_csv(&h);
    assert!(v.starts_with("id,dbl,region") && v.contains("3,6,a"), "{v}");
}

/// `U` after a refresh puts the lineage back too, so `:export` agrees.
#[tokio::test(flavor = "multi_thread")]
async fn undo_of_refresh_restores_the_lineage() {
    let mut h = csv(b"id,region\n1,a\n2,b\n3,a\n").await;
    h.keys("g s").await;
    h.keys("\"").await;
    let key = |h: &Harness| {
        h.app
            .sheets
            .active()
            .state
            .lineage
            .as_ref()
            .unwrap()
            .parent
            .plan_key()
    };
    let before = key(&h);
    let base = h.app.sheets.iter().next().unwrap().name.clone();
    raise(&mut h, &base).await;
    h.keys("g z |").await;
    h.type_("^a$").await;
    raise(&mut h, "selected").await;
    h.keys("<C-r>").await;
    assert_ne!(key(&h), before);
    h.keys("U").await;
    assert_eq!(key(&h), before);
}

/// S-30 B: `zn` notes a cell (marker, sidebar), survives a restart, `zn`
/// again edits it, `:notes` lists them and `Enter` jumps; empty deletes.
#[tokio::test(flavor = "multi_thread")]
async fn cell_notes_mark_cells_persist_and_jump() {
    let mut h = csv(b"id,name\n1,alpha\n2,beta\n3,gamma\n").await;
    let path = h.app.sheets.active().source.path.clone().unwrap();
    let marked = |h: &Harness, row: usize| h.screen().lines().nth(row).unwrap().contains('•');
    h.keys("l j z n").await;
    h.type_("check me").await;
    assert_eq!(status(&h), "note saved");
    assert!(marked(&h, 3) && !marked(&h, 2), "{}", h.screen());
    h.keys(":").await;
    h.type_("sidebar cell").await;
    assert!(h.screen().contains("note: check me"), "{}", h.screen());

    // a new vdrs on the same file sees it
    let loaded = crate::reader::open(path.as_os_str(), None).unwrap();
    let mut again = Harness::with_source(loaded, tempfile::NamedTempFile::new().unwrap()).await;
    assert!(marked(&again, 3), "{}", again.screen());
    again.keys("l j z n").await;
    assert!(status(&again).ends_with("check me"), "{}", status(&again));
    again.keys("<Esc> g g :").await;
    again.type_("notes").await;
    assert!(title(&again).contains("› notes "), "{}", title(&again));
    let t = csv_text(again.app.sheets.active(), b'|');
    assert!(t.contains("2|id 1|name|check me|false|"), "{t}");
    again.keys("<CR>").await;
    assert!(
        title(&again).contains("name 2/2") && title(&again).contains("row 2/3"),
        "{}",
        title(&again)
    );
    again.keys(":").await;
    again.type_("note").await;
    assert_eq!(status(&again), "note deleted");
    assert!(!marked(&again, 3), "{}", again.screen());
}

/// S-23 B: `Z` splits (the sheet under this one in the other pane), `Tab`
/// moves focus, keys move only the focused pane, `gTab` swaps, `gZ` and a
/// pane's last `q` go back to one.
#[tokio::test(flavor = "multi_thread")]
async fn split_panes_focus_swap_and_close() {
    let mut h = Harness::new(30).await;
    let titles = |h: &Harness| {
        h.screen()
            .lines()
            .filter(|l| l.contains(" row "))
            .map(|l| l.replace(',', ""))
            .collect::<Vec<_>>()
    };
    h.keys("l l F").await; // freq(grp) on top of the base sheet
    h.keys("Z").await;
    let t = titles(&h);
    assert_eq!(t.len(), 2, "{}", h.screen());
    assert!(
        t[0].contains("freq(grp)") && t[1].contains("row 1/30"),
        "{t:?}"
    );
    h.keys("j").await;
    let t = titles(&h);
    assert!(
        t[0].contains("row 2/7") && t[1].contains("row 1/30"),
        "{t:?}"
    );
    let active = |h: &Harness| h.app.sheets.active().name.clone();
    h.keys("<Tab> j j").await; // focus the base sheet, below
    assert!(!active(&h).contains("freq"), "{}", active(&h));
    let t = titles(&h);
    assert!(
        t[0].contains("row 2/7") && t[1].contains("row 3/30"),
        "{t:?}"
    );
    h.keys("g z |").await;
    h.type_("^3$").await; // grp 3: 4 rows
    h.keys("<Tab>").await;
    assert!(active(&h).contains("freq(grp)"), "{}", active(&h));
    assert!(titles(&h)[1].contains("/4 "), "{}", h.screen());
    h.keys("g <Tab>").await; // freq goes to the bottom, still focused
    let t = titles(&h);
    assert!(t[0].contains("/4 ") && t[1].contains("freq(grp)"), "{t:?}");
    h.keys("g Z").await;
    assert_eq!(titles(&h).len(), 1, "{}", h.screen());
    h.keys("Z q").await; // freq popped: the base was the other pane
    assert_eq!(titles(&h).len(), 1, "{}", h.screen());
    assert!(!active(&h).contains("freq"), "{}", active(&h));
}

/// S-23 C1: `.` needs a numeric key column (VisiData's messages), then plots
/// the cursor column; `g.` every numeric one; `:plot` names them.
#[tokio::test(flavor = "multi_thread")]
async fn plots_draw_binned_series_against_the_key_column() {
    let mut h = csv(b"x,y,z,s\n1,1,10,a\n2,4,8,b\n3,9,6,c\n4,16,4,d\n5,25,2,e\n6,36,0,f\n7,49,1,g\n8,64,3,h\n9,81,5,i\n10,100,7,j\n").await;
    h.keys("l .").await;
    assert_eq!(
        status(&h),
        "no key columns; press `!` on a numeric column to set x-axis"
    );
    h.keys("l l l ! .").await;
    assert_eq!(
        status(&h),
        "no numeric key column; set type with `#` `%` or `@`"
    );
    h.keys("! 0 ! l .").await;
    assert!(title(&h).contains("plot(x→y)"), "{}", title(&h));
    // y = x²: braille cells, axes from the data, the legend top left, the
    // cursor box (an eighth of each range) in the middle
    let screen = h.screen();
    let plot: Vec<&str> = screen.lines().skip(1).take(10).collect();
    let want = r#" 100│ ■ y                                                                                          ⠈
    │                                                                                    ⡀
    │
    │                                         ⢀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⡀                   ⠐
50.5│                                         ⢸           ⡇         ⠄
    │                                         ⠈⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠉⠁
    │                                          ⡀
    │                               ⠠
   1│⡀         ⠠          ⠁
     1                                              x                                             10"#;
    assert_eq!(plot.join("\n"), want);
    h.keys("q g .").await;
    assert!(title(&h).contains("plot(x→yz)"), "{}", title(&h));
    assert!(h.screen().contains("z"), "{}", h.screen());
    h.keys("q :").await;
    h.type_("plot x s").await;
    assert_eq!(status(&h), "plot: plot: s is str; needs numbers");
}

/// A plot of 200k rows is a few hundred binned rows (O(screen)).
#[tokio::test(flavor = "multi_thread")]
async fn a_plot_of_many_rows_stays_screen_sized() {
    let mut h = Harness::new(200_000).await;
    h.keys(":").await;
    h.type_("plot id amt grp").await;
    let p = h.app.sheets.active();
    assert!(p.name.starts_with("plot(id→amt,grp)"), "{}", p.name);
    let rows = p.window().unwrap().height();
    assert!(
        (100..=200).contains(&rows),
        "{rows} bins for a 100-column screen"
    );
}

/// S-23 C2: `:hist` bars the cursor column; `:bar` draws a freq table.
#[tokio::test(flavor = "multi_thread")]
async fn hist_and_bar_sheets_draw_bars() {
    let mut h = csv(b"v,g\n1,a\n2,a\n2,b\n3,a\n9,c\n10,a\n").await;
    h.keys(":").await;
    h.type_("hist 3").await;
    assert!(title(&h).contains("hist(v)"), "{}", title(&h));
    let screen = h.screen();
    let lines: Vec<&str> = screen.lines().collect();
    // (1,4] holds 1 2 2 3, (4,7] nothing, (7,10] 9 10
    let labels: Vec<&str> = lines[10].split_whitespace().collect();
    assert_eq!(labels, ["1–4", "4–7", "7–10"], "{screen}");
    let values: String = lines[9].chars().filter(char::is_ascii_digit).collect();
    assert_eq!(values, "42", "{screen}");
    h.keys("q :").await;
    h.type_("bar").await;
    assert_eq!(status(&h), "bar: bar: needs a frequency table (F)");
    h.keys("l F :").await;
    h.type_("bar").await;
    let screen = h.screen();
    let rows: Vec<String> = screen
        .lines()
        .skip(1)
        .take(3)
        .map(|l| l.replace('█', ""))
        .collect();
    assert_eq!(rows, ["a 4", "b 1", "c 1"], "{screen}");
}

/// S-23 C3/C4: `+` zooms and bins again inside the view, `_` fits; `s` on
/// the plot selects the parent's rows in the cursor box, `Enter` opens them.
#[tokio::test(flavor = "multi_thread")]
async fn plot_zoom_bins_the_view_and_box_selects_parent_rows() {
    let mut rows = String::from("x,y\n");
    for i in 0..400 {
        rows += &format!("{i},{}\n", (i * 7) % 100);
    }
    let mut h = csv(rows.as_bytes()).await;
    h.keys("! l .").await;
    let view = |h: &Harness| h.app.sheets.active().state.plot.clone().unwrap();
    let full = view(&h).x;
    assert_eq!(full, [0.0, 399.0]);
    h.keys("+").await;
    let z = view(&h).x;
    assert!((z[1] - z[0] - 399.0 / 2.0).abs() < 1e-9, "{z:?}");
    // bins cover only what's in view
    let xs = h
        .app
        .sheets
        .active()
        .window()
        .unwrap()
        .column("x")
        .unwrap()
        .f64()
        .unwrap()
        .clone();
    assert!(
        xs.min().unwrap() >= z[0] && xs.max().unwrap() <= z[1],
        "{z:?}"
    );
    h.keys("_").await;
    assert_eq!(view(&h).x, full);
    h.keys("l L L k").await; // move right, wider, up
    let [x0, y0, w, hh] = view(&h).cursor;
    assert!(status(&h).starts_with(" box x "), "{}", status(&h));
    let want = (0..400)
        .filter(|i| {
            let (x, y) = (f64::from(*i), f64::from((i * 7) % 100));
            (x0..=x0 + w).contains(&x) && (y0..=y0 + hh).contains(&y)
        })
        .count();
    assert!(want > 0);
    h.keys("s").await;
    let parent = h.app.sheets.iter().next().unwrap();
    assert_eq!(parent.view.selected.len(), want);
    assert_eq!(
        status(&h),
        format!("selected {want} rows in {}", parent.name)
    );
    h.keys("<CR>").await;
    assert!(title(&h).ends_with(&format!("/{want}")), "{}", title(&h));
    h.keys("q u").await;
    assert!(h.app.sheets.iter().next().unwrap().view.selected.is_empty());
}

/// S-27 A: `:color` paints cells where the predicate holds; the colors
/// follow the rows through a sort, `U` drops the rule, a rename keeps it,
/// and the rule's column never reaches `:save`.
#[tokio::test(flavor = "multi_thread")]
async fn color_rules_paint_matching_cells_and_follow_the_rows() {
    use ratatui::style::Color;
    let mut h = csv(b"id,amount\n1,50\n2,150\n3,300\n").await;
    h.keys(":").await;
    h.type_("color amount id >= 2 red").await;
    assert_eq!(fg_of(&h, "150"), Color::Red, "{}", h.screen());
    assert_eq!(fg_of(&h, "300"), Color::Red);
    assert_ne!(fg_of(&h, "50"), Color::Red);
    assert_ne!(
        fg_of(&h, "2"),
        Color::Red,
        "cell rule: other columns untouched"
    );
    h.keys("l ] h").await; // amount desc: 300, 150, 50
    assert_eq!(first_cell(&h), "3", "{}", h.screen());
    assert_eq!(fg_of(&h, "150"), Color::Red);
    assert_ne!(fg_of(&h, "50"), Color::Red);
    // rename keeps the rule on its column
    h.keys("l :").await;
    h.type_("rename amt").await;
    h.keys("h").await;
    assert_eq!(
        h.app.sheets.active().view.formats[0].column.as_deref(),
        Some("amt")
    );
    assert_eq!(fg_of(&h, "150"), Color::Red);
    h.keys(":").await;
    h.type_("rename key").await;
    assert_eq!(
        status(&h),
        "a :color rule reads id; clear it (:nocolor) first"
    );
    let out = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    h.keys(":").await;
    h.type_(&format!("save {}", out.path().display())).await;
    let saved = std::fs::read_to_string(out.path()).unwrap();
    assert!(!saved.contains("__vdrs"), "{saved}");
    assert!(!csv_text(h.app.sheets.active(), b',').contains("__vdrs"));
    h.keys("U U U").await; // rename, sort, color
    assert!(h.app.sheets.active().view.formats.is_empty());
    assert_ne!(fg_of(&h, "150"), Color::Red);
}

/// S-27 A: `*` paints the whole row; a bad style or predicate is refused
/// with no undo step; `:nocolor` clears.
#[tokio::test(flavor = "multi_thread")]
async fn color_row_rules_bad_input_and_nocolor() {
    use ratatui::style::Color;
    let mut h = csv(b"id,amount\n1,50\n2,150\n").await;
    h.keys(":").await;
    h.type_("color * amount > 100 bg=yellow").await;
    let screen = h.screen();
    let y = screen.lines().position(|l| l.contains("150")).unwrap() as u16;
    assert_eq!(h.term.backend().buffer()[(1, y)].bg, Color::Yellow);
    h.keys(":").await;
    h.type_("color amount amount > 1 rouge").await;
    assert!(
        status(&h).contains("color: unknown color: rouge"),
        "{}",
        status(&h)
    );
    h.keys(":").await;
    h.type_("color amount nope > 1 red").await;
    assert!(status(&h).starts_with("color:"), "{}", status(&h));
    assert_eq!(h.app.sheets.active().view.formats.len(), 1);
    h.keys(":").await;
    h.type_("nocolor").await;
    assert_eq!(status(&h), "1 color rule cleared");
    h.keys("U").await; // the refused ones left no undo step: the rule is back
    assert_eq!(h.app.sheets.active().view.formats.len(), 1);
}

/// S-27 B: `:scale` runs low→high color over the column's range, `:databar`
/// fills a cell's width by its share; a text column is refused; `U` and
/// `:nocolor` remove them.
#[tokio::test(flavor = "multi_thread")]
async fn scale_and_databar_paint_by_the_columns_range() {
    use ratatui::style::Color;
    let mut h = csv(b"name,amount\na,10\nb,20\nc,30\nd,15\n").await;
    h.keys(":").await;
    h.type_("scale amount white #ff0000").await;
    assert_eq!(h.app.sheets.active().view.scales[0].range, [10.0, 30.0]);
    assert_eq!(
        cell_at(&h, "30").bg,
        Color::Rgb(255, 0, 0),
        "{}",
        h.screen()
    );
    assert_eq!(cell_at(&h, "20").bg, Color::Rgb(255, 128, 128));
    assert_eq!(
        cell_at(&h, "b").bg,
        cell_at(&h, "d").bg,
        "only the amount cells"
    );
    h.keys("U").await;
    assert!(h.app.sheets.active().view.scales.is_empty());
    assert_ne!(cell_at(&h, "30").bg, Color::Rgb(255, 0, 0));
    h.keys(":").await;
    h.type_("databar amount").await;
    let bar = h.app.theme.num_fg;
    assert_eq!(cell_at(&h, "30").bg, bar, "the max fills the cell");
    assert_ne!(cell_at(&h, "10").bg, bar, "the min has no bar");
    h.keys(":").await;
    h.type_("scale name").await;
    assert_eq!(status(&h), "scale: name is not numeric (str)");
    h.keys(":").await;
    h.type_("nocolor amount").await;
    assert_eq!(status(&h), "1 color rule cleared");
    assert_ne!(cell_at(&h, "30").bg, bar);
}

/// S-26 A5, A7: `z<C-h>` lists the bindings; `<C-^>` swaps to the sheet
/// under this one.
#[tokio::test(flavor = "multi_thread")]
async fn help_sheet_jump_prev_and_the_refresh_guard() {
    let mut h = Harness::new(5).await;
    h.keys("z <C-h>").await;
    let s = h.app.sheets.active();
    assert_eq!(s.name, "help");
    let t = csv_text(s, b'|');
    assert!(
        t.contains("z <C-h>|help_commands|list the key bindings"),
        "{t}"
    );
    assert!(t.contains("<C-^>|jump_prev|"), "{t}");
    assert!(
        t.contains("[|sort_asc|sort ascending by current column|Column > Sort by > current column only > ascending > sort-asc"),
        "{t}"
    );
    h.keys("q l l F").await; // freq(grp) on the base sheet
    let freq = h.app.sheets.active().name.clone();
    h.keys("<C-^>").await;
    assert_eq!(h.app.sheets.active().id, 0, "base sheet on top");
    h.keys("<C-^>").await;
    assert_eq!(h.app.sheets.active().name, freq);
    h.keys("q").await;
    h.keys("<C-^>").await;
    assert_eq!(status(&h), "no previous sheet");
}

/// S-26 A4: with `NO_COLOR` nothing on screen has a color; the cursor row
/// is reverse video. 256 colors: no RGB left.
#[tokio::test(flavor = "multi_thread")]
async fn no_color_and_256_color_frames() {
    use ratatui::style::{Color, Modifier};
    let mut h = Harness::new(5).await;
    h.keys(":").await;
    h.type_("color * id = 2 red").await;
    h.app.color = crate::theme::ColorDepth::Ansi256;
    h.pump().await;
    let rgb = |c: Color| matches!(c, Color::Rgb(..));
    let buf = h.term.backend().buffer().clone();
    assert!(buf.content.iter().all(|c| !rgb(c.fg) && !rgb(c.bg)));
    h.app.color = crate::theme::ColorDepth::None;
    h.pump().await;
    let buf = h.term.backend().buffer();
    assert!(
        buf.content
            .iter()
            .all(|c| c.fg == Color::Reset && c.bg == Color::Reset)
    );
    assert!(
        buf[(1, 2)].modifier.contains(Modifier::REVERSED),
        "cursor row"
    );
    let ruled = cell_at(&h, "name_2");
    assert!(
        ruled.modifier.contains(Modifier::REVERSED),
        ":color as reverse"
    );
}

/// S-38 B: `"` keeps the parent's column widths, as VisiData's copies do.
#[tokio::test(flavor = "multi_thread")]
async fn copies_keep_the_parents_widths() {
    let mut h = csv(b"rep,n\nRichardson,1\nAl,2\n").await;
    let widths = |h: &Harness| h.app.sheets.active().state.widths.clone();
    let parent = widths(&h);
    h.keys("j s \"").await;
    assert!(title(&h).contains("selected (1 rows)"), "{}", title(&h));
    assert_eq!(widths(&h), parent);
}

/// `:lookup` (S-36 A): their first match per key lands after the cursor;
/// the value follows a key edit and an edit of their sheet; a cycle is
/// refused; their sheet closed, the values stay.
#[tokio::test(flavor = "multi_thread")]
async fn lookup_follows_keys_and_their_sheet() {
    let mut h = Harness::new(4).await; // id 1..4
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("p.csv");
    std::fs::write(&path, "id,price\n1,10\n2,20\n2,99\n4,40\n").unwrap();
    h.keys(":").await;
    h.type_(&format!("open {}", path.display())).await;
    let theirs = h.app.sheets.active().id;
    let mine = h.app.sheets.iter().find(|s| s.id != theirs).unwrap().id;
    h.keys("S <CR>").await;
    assert_eq!(h.app.sheets.active().id, mine);
    h.keys(":").await;
    h.type_(&format!("lookup #{theirs} id price")).await;
    assert_eq!(status(&h), "lookup: 1 rows repeat a id; first match kept");
    assert_eq!(h.current_column(), "price");
    let prices = |h: &Harness| -> Vec<String> {
        view_csv(h)
            .lines()
            .skip(1)
            .map(|l| l.split(',').nth(1).unwrap().to_string())
            .collect()
    };
    assert_eq!(prices(&h), ["10", "20", "", "40"]);
    // key 3 → 4: the value follows
    h.keys("j j h e <C-u>").await;
    h.type_("4").await;
    assert_eq!(prices(&h), ["10", "20", "40", "40"]);
    // their price of 1 → 11, then back here: the lookup catches up
    h.app.sheets.switch_to(theirs);
    h.pump().await;
    h.keys("g g l e <C-u>").await;
    h.type_("11").await;
    h.keys("S <CR>").await;
    assert_eq!(h.app.sheets.active().id, mine);
    assert_eq!(prices(&h)[0], "11");
    // theirs looking up mine would be a cycle
    h.app.sheets.switch_to(theirs);
    h.pump().await;
    h.keys(":").await;
    h.type_(&format!("lookup #{mine} id name")).await;
    assert!(status(&h).contains("looks up this sheet"), "{}", status(&h));
    // their sheet closed: values kept, link frozen
    h.keys("q y").await; // discard their edit
    assert_eq!(h.app.sheets.active().id, mine);
    assert!(
        status(&h).contains("price keep their values"),
        "{}",
        status(&h)
    );
    assert_eq!(prices(&h), ["11", "20", "40", "40"]);
    // one undo takes the column away
    h.keys("U U").await;
    assert!(!view_csv(&h).starts_with("id,price"), "{}", view_csv(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn freq_keys_transpose_join_all_and_open_cell() {
    let mut h = csv(b"k,g,n\na,x,1\nb,x,2\na,x,3\n").await;
    h.keys("!").await;
    h.keys("T").await;
    assert!(
        view_csv(&h).starts_with("k,a,b,a_duplicated_3\ng,x,x,x\nn,1,2,3\n"),
        "{}",
        view_csv(&h)
    );
    h.keys("q g F").await;
    assert!(
        view_csv(&h).starts_with("k,count,percent,histogram\na,2,66.67,"),
        "{}",
        view_csv(&h)
    );
    h.keys("<CR>").await;
    assert_eq!(view_csv(&h), "k,g,n\na,x,1\na,x,3\n");
    h.keys("q g &").await;
    h.type_("inner").await;
    let s = h.app.sheets.active();
    assert!(s.name.ends_with("(inner)"), "{}", s.name);
    assert_eq!(s.window().map(|w| w.height()), Some(3), "{}", view_csv(&h));

    let mut h = csv(b"id,tags\n1,\"[{\"\"p\"\":1,\"\"q\"\":\"\"a\"\"},{\"\"p\"\":2}]\"\n").await;
    h.keys("l z <CR>").await;
    assert!(h.app.sheets.active().name.ends_with(".csv[0].tags"));
    assert_eq!(view_csv(&h), "p,q\n1,a\n2,\n");
    h.keys("<CR>").await;
    assert!(h.app.sheets.active().name.ends_with(".csv[0].tags[0]"));
    assert_eq!(view_csv(&h), "key,value\np,1\nq,a\n");
}

/// S-51: `g&` over three sheets. A cell goes back to the sheet it came from,
/// the inner joins' sheets too (S-52), never landing on another row.
#[tokio::test(flavor = "multi_thread")]
async fn join_all_edits_write_back_to_every_sheet() {
    let dir = tempfile::tempdir().unwrap();
    let path = |n: &str, text: &str| {
        let p = dir.path().join(n);
        std::fs::write(&p, text).unwrap();
        p.display().to_string()
    };
    // C&B has rows 3, 2, 1; the result's rows 2, 1 are its rows 1, 2
    let b = path("b.csv", "id,w\n1,x\n2,y\n3,t\n");
    let c = path("c.csv", "id,z\n3,s\n2,q\n1,r\n");
    let mut h = csv(b"id,v\n2,b\n1,a\n").await; // A
    for p in [&b, &c] {
        h.keys(":").await;
        h.type_(&format!("open {p}")).await;
    }
    h.keys("!").await;
    h.keys("g &").await;
    h.type_("inner").await;
    assert_eq!(view_csv(&h), "id,z,w,v\n2,q,y,b\n1,r,x,a\n");
    h.keys("l e <C-u>").await;
    h.type_("Q").await;
    assert_eq!(view_csv(&h), "id,z,w,v\n2,Q,y,b\n1,r,x,a\n");
    h.keys("l l e <C-u>").await;
    h.type_("B").await;
    assert_eq!(view_csv(&h), "id,z,w,v\n2,Q,y,B\n1,r,x,a\n");
    h.keys("h e <C-u>").await;
    h.type_("W").await;
    assert_eq!(view_csv(&h), "id,z,w,v\n2,Q,W,B\n1,r,x,a\n");
    let col = |i: usize, c: &str| -> Vec<Option<String>> {
        let s = h.app.sheets.iter().nth(i).unwrap();
        let df = s.view_plan().collect().unwrap();
        let c = df.column(c).unwrap().str().unwrap();
        c.iter().map(|v| v.map(str::to_string)).collect()
    };
    let s = |v: &str| Some(v.to_string());
    assert_eq!(col(0, "v"), [s("B"), s("a")]); // A
    assert_eq!(col(1, "w"), [s("x"), s("W"), s("t")]); // B
    assert_eq!(col(2, "z"), [s("s"), s("Q"), s("r")]); // C
}

/// S-52: a source row `g&` shows on two rows (the inner join doubled it):
/// an edit of its cell shows on both, and lands once in its sheet.
#[tokio::test(flavor = "multi_thread")]
async fn join_all_edit_fans_out_through_inner_joins() {
    let dir = tempfile::tempdir().unwrap();
    let path = |n: &str, text: &str| {
        let p = dir.path().join(n);
        std::fs::write(&p, text).unwrap();
        p.display().to_string()
    };
    let b = path("b.csv", "id,w\n1,x\n1,y\n");
    let c = path("c.csv", "id,z\n1,s\n");
    let mut h = csv(b"id,v\n1,a\n").await; // A
    for p in [&b, &c] {
        h.keys(":").await;
        h.type_(&format!("open {p}")).await;
    }
    h.keys("!").await;
    h.keys("g &").await;
    h.type_("inner").await;
    assert_eq!(view_csv(&h), "id,z,w,v\n1,s,x,a\n1,s,y,a\n");
    h.keys("l e <C-u>").await;
    h.type_("S").await;
    assert_eq!(view_csv(&h), "id,z,w,v\n1,S,x,a\n1,S,y,a\n");
    let c = h.app.sheets.iter().nth(2).unwrap();
    assert_eq!(csv_text(c, b','), "id,z\n1,S\n");
}

/// S-51: `gF` over keys with empty cells: `g<CR>` on every row brings back
/// every parent row (nulls match nulls); `g&` append needs no keys.
#[tokio::test(flavor = "multi_thread")]
async fn freq_keys_dive_matches_nulls_and_join_all_appends() {
    let mut h = csv(b"k,g\na,\n,x\na,\n").await;
    h.keys("! l ! g F").await;
    assert!(
        view_csv(&h).starts_with("k,g,count,percent,histogram\na,,2,"),
        "{}",
        view_csv(&h)
    );
    h.keys("g s").await;
    h.keys("g <CR>").await;
    assert_eq!(view_csv(&h), "k,g\na,\n,x\na,\n", "{}", status(&h));

    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("o.csv");
    std::fs::write(&other, "k,g\nz,y\n").unwrap();
    let mut h = csv(b"k,g\na,x\n").await;
    h.keys(":").await;
    h.type_(&format!("open {}", other.display())).await;
    h.keys("g &").await;
    h.type_("append").await;
    let s = h.app.sheets.active();
    assert!(s.name.ends_with("(append)"), "{}", s.name);
    assert_eq!(s.window().map(|w| w.height()), Some(2), "{}", view_csv(&h));
}

/// S-46 B: `zEnter` on a `gF` sheet opens the rows outside the cursor row's
/// keys; `gzEnter` the rows outside every selected row's keys.
#[tokio::test(flavor = "multi_thread")]
async fn dive_except_on_freq_keys() {
    let mut h = csv(b"k,g\na,x\nb,x\na,x\n,y\n").await;
    h.keys("! l ! g F").await;
    assert!(view_csv(&h).starts_with("k,g,count"), "{}", view_csv(&h));
    h.keys("z <CR>").await;
    assert_eq!(view_csv(&h), "k,g\nb,x\n,y\n", "{}", status(&h));
    h.keys("q s s g z <CR>").await;
    assert_eq!(view_csv(&h), "k,g\n,y\n", "{}", status(&h));
    h.keys("q q g z <CR>").await;
    assert_eq!(status(&h), "gz<CR>: only on a frequency sheet");
}

/// S-46 B5: `gS` lists the open sheets, then the data sheets quit since the
/// start; `<CR>` on a quit one opens it again, its view as it was left.
/// `S` lists only the open ones.
#[tokio::test(flavor = "multi_thread")]
async fn sheets_all_reopens_a_quit_sheet() {
    let mut h = csv(b"k,v\nx,1\ny,2\nx,3\n").await;
    h.keys("F").await;
    let freq = h.app.sheets.active().id;
    h.keys("j").await;
    h.keys("C").await; // a columns sheet quit isn't kept
    h.keys("q").await;
    h.keys("q").await;
    assert_eq!(h.app.sheets.len(), 1);
    h.keys("S").await;
    assert_eq!(h.app.sheets.active().window().unwrap().height(), 1);
    h.keys("q").await;
    h.keys("g S").await;
    assert_eq!(h.app.sheets.active().name, "<sheets_all>");
    assert_eq!(h.app.sheets.active().window().unwrap().height(), 2);
    h.keys("j").await;
    h.keys("<CR>").await;
    assert_eq!(h.app.sheets.active().id, freq);
    assert_eq!(h.app.sheets.len(), 2);
    assert_eq!(h.app.sheets.active().view.row_cursor, 1);
    h.keys("g S").await; // open again: listed once, on the stack
    assert_eq!(h.app.sheets.active().window().unwrap().height(), 2);
}

/// S-46 B6: `gP` asks for a filetype (default `tsv`) and opens the
/// clipboard's text as sheet `syspaste`: the last terminal paste, or the
/// paste command's output. CRLF line ends read as on disk.
#[tokio::test(flavor = "multi_thread")]
async fn open_syspaste_opens_the_clipboard() {
    let mut h = csv(b"a\n1\n").await;
    h.keys("g P").await;
    assert_eq!(status(&h), ":open-syspaste tsv");
    h.keys("<CR>").await;
    assert_eq!(status(&h), "nothing pasted: paste with the terminal first");
    h.paste("k\tv\r\nx\t1\r\n\n").await;
    h.keys("g P").await;
    h.keys("<CR>").await;
    h.pump().await;
    assert_eq!(h.app.sheets.active().name, "syspaste");
    assert_eq!(view_csv(&h), "k,v\nx,1\n");
    h.app.paste_cmd = Some(vec!["printf".into(), "p,q\\n3,4\\n".into()]);
    h.keys(":").await;
    h.type_("open-syspaste csv").await;
    h.pump().await;
    assert_eq!(view_csv(&h), "p,q\n3,4\n");
    h.keys(":").await;
    h.type_("open-syspaste nope").await;
    assert_eq!(status(&h), "open-syspaste: unknown filetype nope");
}

/// S-46 B7: `:save-all dir/` writes each stacked sheet as `<name>.tsv`;
/// `:save-all f.duckdb` writes one table each; a name met again gets `_2`;
/// a one-sheet format refuses more than one sheet.
#[tokio::test(flavor = "multi_thread")]
async fn save_all_writes_every_sheet() {
    let dir = tempfile::tempdir().unwrap();
    let mut h = csv(b"k,v\nx,1\ny,2\nx,3\n").await;
    h.keys("F").await;
    let base = h.app.sheets.active().table_name(); // the freq sheet's is its parent's
    let names = [base.clone(), format!("{base}_2")];
    h.keys(":").await;
    h.type_(&format!("save-all {}/out/", dir.path().display()))
        .await;
    h.pump().await;
    assert_eq!(
        status(&h),
        format!("saved 2 sheets → {}/out/", dir.path().display())
    );
    let freq = std::fs::read_to_string(dir.path().join("out").join(format!("{}.tsv", names[1])));
    assert_eq!(
        freq.unwrap().lines().next(),
        Some("k\tcount\tpercent\thistogram")
    );
    assert!(
        dir.path()
            .join("out")
            .join(format!("{}.tsv", names[0]))
            .exists()
    );
    let db = dir.path().join("all.duckdb");
    h.keys(":").await;
    h.type_(&format!("save-all {}", db.display())).await;
    h.pump().await;
    assert_eq!(status(&h), format!("saved 2 sheets → {}", db.display()));
    let tables = crate::duckdb::index(&db).unwrap();
    assert_eq!(tables.height(), 2, "{tables}");
    h.keys(":").await;
    h.type_(&format!("save-all {}/one.csv", dir.path().display()))
        .await;
    assert_eq!(
        status(&h),
        "save-all: 2 sheets can't go in one file; end the path with / or use .duckdb"
    );
}
