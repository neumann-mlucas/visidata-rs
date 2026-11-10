//! End-to-end: real keymap + dispatch + background Polars tasks + render.
use super::*;
use std::io::Write;
use unicode_width::UnicodeWidthStr;

pub struct Harness {
    pub app: App,
    pub term: Terminal<ratatui::backend::TestBackend>,
    _tmp: Option<tempfile::NamedTempFile>,
}

impl Harness {
    /// `rows`-row CSV: id,name,grp,amt with id = 1..=rows.
    pub async fn new(rows: usize) -> Self {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        writeln!(tmp, "id,name,grp,amt").unwrap();
        for i in 1..=rows {
            writeln!(tmp, "{i},name_{i},{},{}", i % 7, i as f64 * 1.5).unwrap();
        }
        tmp.flush().unwrap();
        let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
        Self::with_source(loaded, tmp).await
    }

    pub async fn with_source(
        loaded: crate::reader::LoadedSource,
        tmp: tempfile::NamedTempFile,
    ) -> Self {
        let mut app = App::new(Sheet::from_source(0, loaded), None, None);
        app.spawn_materialize();
        let mut h = Self::from_app(app).await;
        h._tmp = Some(tmp);
        h
    }

    pub async fn from_app(app: App) -> Self {
        let term = Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
        let mut h = Self {
            app,
            term,
            _tmp: None,
        };
        h.pump().await;
        h
    }

    /// Apply background results until the app is idle, then draw.
    pub async fn pump(&mut self) {
        // Generous timeout: parallel tests share CPUs; settled() ends it early.
        loop {
            self.app.fire_push_hooks();
            self.app.ask_sel_stats();
            self.app.follow_link();
            self.app.feed_other_pane();
            self.app.pump_macro();
            if self.app.settled() {
                break;
            }
            let next = self.app.materialize_rx.recv();
            match tokio::time::timeout(Duration::from_secs(20), next).await {
                Ok(Some(res)) => self.app.on_materialize(res),
                _ => break,
            }
        }
        // stale results (superseded counts) may still be queued; apply them too
        while let Ok(res) = self.app.materialize_rx.try_recv() {
            self.app.on_materialize(res);
        }
        self.app.draw(&mut self.term).unwrap();
    }

    /// The terminal pastes `text` (bracketed paste).
    pub async fn paste(&mut self, text: &str) {
        self.app.on_paste(text.into());
        self.pump().await;
    }

    /// Space-separated key tokens, e.g. "g g", "<C-d>", or literal text via `type_`.
    pub async fn keys(&mut self, ks: &str) {
        for k in ks.split(' ').filter(|k| !k.is_empty()) {
            self.app.on_key(k.parse().unwrap());
            self.app.draw(&mut self.term).unwrap();
        }
        self.pump().await;
    }

    pub async fn type_(&mut self, text: &str) {
        self.type_no_enter(text).await;
        self.keys("<CR>").await;
    }

    /// Type into the open prompt without submitting; waits for jobs.
    pub async fn type_no_enter(&mut self, text: &str) {
        for c in text.chars() {
            self.app
                .on_key(Key::from(crossterm::event::KeyEvent::from(KeyCode::Char(
                    c,
                ))));
        }
        self.pump().await;
        self.app.draw(&mut self.term).unwrap();
    }

    /// Name of the column under the cursor.
    pub fn current_column(&self) -> String {
        self.app.current_column_name().unwrap_or_default()
    }

    pub fn screen(&self) -> String {
        let buf = self.term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                // Like a terminal: a wide symbol covers the next cell too.
                let mut line = String::new();
                let mut x = 0;
                while x < buf.area.width {
                    let sym = buf[(x, y)].symbol();
                    line.push_str(sym);
                    x += sym.width().max(1) as u16;
                }
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Harness over a CSV holding exactly `bytes`.
async fn csv(bytes: &[u8]) -> Harness {
    let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    tmp.write_all(bytes).unwrap();
    tmp.flush().unwrap();
    let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
    Harness::with_source(loaded, tmp).await
}

/// Every row of `sheet`'s view as delimited text.
pub fn csv_text(sheet: &Sheet, sep: u8) -> String {
    let mut df = sheet.view_plan().collect().unwrap();
    let mut out = Vec::new();
    CsvWriter::new(&mut out)
        .with_separator(sep)
        .finish(&mut df)
        .unwrap();
    String::from_utf8(out).unwrap()
}

fn title(h: &Harness) -> String {
    // Thousands separators aside, tests read plain numbers.
    h.screen()
        .lines()
        .next()
        .unwrap_or_default()
        .replace(',', "")
}

#[tokio::test(flavor = "multi_thread")]
async fn title_shows_what_shapes_the_view_and_always_the_position() {
    let mut h = Harness::new(5000).await;
    assert!(!title(&h).contains("sel "), "{}", title(&h));
    h.keys("l [ s -").await;
    let line = h.screen().lines().next().unwrap().to_string();
    assert!(line.contains("row 1/5,000 · 0%"), "{line}");
    for chip in ["csv", "sort name▲", "sel 1", "hidden 1"] {
        assert!(line.contains(chip), "{chip}: {line}");
    }
    assert_eq!(line.width(), 99, "the row block ends the bar: {line:?}");

    h.app.sheets.active_mut().name = "x".repeat(300);
    h.pump().await;
    let line = h.screen().lines().next().unwrap().to_string();
    assert!(line.contains("row 1/5,000") && line.contains('…'), "{line}");
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

/// First data row (below the header), first cell.
fn first_cell(h: &Harness) -> String {
    let screen = h.screen();
    let line = screen.lines().nth(2).unwrap_or_default();
    line.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
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
    h.keys("z o g g g /").await;
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
        title(&h).contains("freq(grp)") && title(&h).contains("/1 "),
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

fn status(h: &Harness) -> String {
    h.screen().lines().last().unwrap_or_default().to_string()
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
        title(&h).contains("freq(id)") && title(&h).contains("/30 "),
        "{}",
        title(&h)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn multiline_errors_show_first_line_only() {
    let mut h = Harness::new(3).await;
    h.app.toast = Some("materialize: boom\n\nResolved plan: …".into());
    h.pump().await;
    assert_eq!(status(&h), "materialize: boom");
}

#[tokio::test(flavor = "multi_thread")]
async fn tail_follow_and_row_diff() {
    use std::io::Write;
    let mut h = Harness::new(3).await;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.jsonl");
    std::fs::write(&p, "{\"a\":1}\n{\"a\":2}\n").unwrap();
    h.keys(":").await;
    h.type_(&format!("tail {} -f", p.display())).await;
    h.keys("G").await;
    let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
    writeln!(f, "{{\"a\":3}}\n{{\"a\":4}}").unwrap();
    drop(f);
    h.app.on_tail_tick();
    h.pump().await;
    assert!(title(&h).contains("row 4/4"), "{}", title(&h)); // followed to the new bottom

    h.keys("q").await;
    let other = dir.path().join("o.csv");
    std::fs::write(
        &other,
        "id,name,grp,amt\n1,name_1,1,1.5\n2,CHANGED,2,3.0\n99,x,0,0\n",
    )
    .unwrap();
    h.keys(":").await;
    h.type_(&format!("diff {} id", other.display())).await;
    // id 2 changed, id 3 only_a, id 99 only_b; id 1 identical → hidden
    assert!(
        title(&h).contains("o.csv") && title(&h).contains("row 1/3"),
        "{}",
        h.screen()
    );
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
async fn diff_uses_plugin_loader_and_tail_refuses_it() {
    let dir = tempfile::tempdir().unwrap();
    let ml = dir.path().join("other.ml");
    std::fs::write(&ml, "opaque").unwrap();
    std::fs::write(
        dir.path().join("other.csv"),
        "id,name,grp,amt\n1,name_1,1,1.5\n",
    )
    .unwrap();
    // loader "converts" x.ml by pointing at its sibling x.csv
    let lua = r#"vd.register_loader { extensions = "ml",
        open = function(p) return (p:gsub("%.ml$", ".csv")) end }"#;
    let mut h = Harness::new(2).await;
    h.app.plugins = Some(crate::plugins::Plugins::load_from_source(lua).unwrap());
    h.keys(":").await;
    h.type_(&format!("diff {} id", ml.display())).await;
    assert!(
        title(&h).contains("other.ml") && title(&h).contains("sheets 2"),
        "{}",
        h.screen()
    );
    h.keys("q :").await;
    h.type_(&format!("tail {}", ml.display())).await;
    assert!(status(&h).contains("can't follow"), "{}", status(&h));
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
async fn prompt_line_editing_and_quoted_args() {
    let mut h = Harness::new(5).await;
    // type "ort -id", jump home, fix the typo, submit
    h.keys(":").await;
    for c in "ort -id".chars() {
        h.app
            .on_key(Key::from(crossterm::event::KeyEvent::from(KeyCode::Char(
                c,
            ))));
    }
    h.keys("<Home>").await;
    h.type_("s").await;
    assert_eq!(first_cell(&h), "5");
    h.keys(":").await;
    h.type_(r#"filter "name" ^name_[12]$"#).await;
    assert!(title(&h).contains("row 1/2"), "{}", title(&h));
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
    h.keys("q s j s g <CR>").await; // two freq rows selected → union
    assert!(title(&h).contains("row 1/1430"), "{}", title(&h));
    h.keys("q q <CR>").await; // <CR> on a data sheet explains itself
    assert_eq!(status(&h), "<CR>: nothing to open here");
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

#[tokio::test(flavor = "multi_thread")]
async fn derive_setcol_and_undo_redo() {
    let mut h = Harness::new(5).await;
    h.keys(":").await;
    h.type_("derive dbl = amt * 2").await;
    let names = |h: &Harness| h.screen().lines().nth(1).unwrap().to_string();
    assert!(names(&h).contains("dbl"), "{}", h.screen());
    h.keys("U").await;
    assert!(!names(&h).contains("dbl"), "{}", h.screen());
    h.keys("R").await;
    assert!(names(&h).contains("dbl"), "{}", h.screen());
    h.keys(":").await;
    h.type_("derive bad = nosuch + 1").await;
    assert!(status(&h).starts_with("derive:"), "{}", status(&h));
    // setcol on selected rows, type-checked
    h.keys("s j s 0 :").await;
    h.type_("setcol 42").await;
    assert_eq!(status(&h), "set id on 2 rows");
    let ids: Vec<_> = h
        .app
        .sheets
        .active()
        .view_plan()
        .collect()
        .unwrap()
        .column("id")
        .unwrap()
        .i64()
        .unwrap()
        .iter()
        .collect();
    assert_eq!(ids, [Some(42), Some(42), Some(3), Some(4), Some(5)]);
    h.keys(":").await;
    h.type_("setcol abc").await;
    assert!(status(&h).contains("doesn't fit id"), "{}", status(&h));
    h.keys("U U").await; // undo setcol, then the second selection
    assert!(title(&h).contains("sel 1"), "{}", title(&h));
    assert_eq!(first_cell(&h), "1");
}

#[tokio::test(flavor = "multi_thread")]
async fn sql_describe_pivot_uniform_copy() {
    let mut h = Harness::new(5000).await;
    h.keys(":").await;
    h.type_("sql SELECT grp, count(*) AS n FROM self GROUP BY grp ORDER BY grp")
        .await;
    assert!(
        title(&h).contains("sql:") || title(&h).contains("row 1/7"),
        "{}",
        title(&h)
    );
    assert!(title(&h).contains("row 1/7"), "{}", title(&h));
    // GROUP BY order isn't fixed by Polars: collected once so row ids hold
    let plan = h.app.sheets.active().plan.describe_plan().unwrap();
    assert!(plan.starts_with("DF "), "{plan}");
    h.keys("q I").await;
    assert!(
        title(&h).contains("describe") && title(&h).contains("row 1/4"),
        "{}",
        title(&h)
    );
    let d = h.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(
        d.column("distinct")
            .unwrap()
            .get(2)
            .unwrap()
            .extract::<u64>(),
        Some(7)
    ); // grp
    assert_eq!(
        d.column("max").unwrap().get(0).unwrap().get_str(),
        Some("5000")
    ); // id
    h.keys("q l l W").await; // no key cols → one row, a column per grp value
    let p = h.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(p.shape(), (1, 7));
    h.keys("q h !").await; // key on name… then W on grp → 5000 rows
    h.keys("l W").await;
    assert!(title(&h).contains("row 1/5000"), "{}", title(&h));
    h.keys("q :").await;
    h.type_("derive k = 1").await;
    h.keys(":").await;
    h.type_("uniform").await;
    assert_eq!(status(&h), "hid 1 uniform columns");
    h.keys("g \"").await;
    assert!(title(&h).contains("copy of"), "{}", title(&h));
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

#[tokio::test(flavor = "multi_thread")]
async fn duckdb_index_exec_sql_open_and_save() {
    let have = std::process::Command::new("duckdb")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !have {
        assert!(
            std::env::var_os("VDRS_REQUIRE_ROUNDTRIP").is_none(),
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
async fn tail_on_parquet_directory_picks_up_new_files() {
    let dir = tempfile::tempdir().unwrap();
    let write = |i: i64| {
        let mut df = df!("v" => [i, i]).unwrap();
        let f = std::fs::File::create(dir.path().join(format!("{i}.parquet"))).unwrap();
        ParquetWriter::new(f).finish(&mut df).unwrap();
    };
    write(1);
    let mut h = Harness::new(1).await;
    h.keys(":").await;
    h.type_(&format!("tail {}", dir.path().display())).await;
    assert!(title(&h).contains("row 1/2"), "{}", title(&h));
    write(2);
    h.app.on_tail_tick();
    h.pump().await;
    assert!(title(&h).contains("/4 "), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn lua_aggregator_renderer_and_select_equal() {
    let mut h = Harness::new(20).await;
    h.app.plugins = Some(
        crate::plugins::Plugins::load_from_source(
            r#"vd.register_aggregator("n", function(vs) return #vs end)
               vd.register_renderer { column = "name", render = function(v) return string.upper(v) end }"#,
        )
        .unwrap(),
    );
    h.pump().await;
    assert!(h.screen().contains("NAME_1 "), "{}", h.screen()); // renderer applied
    h.keys(":").await;
    h.type_("pivot grp name id n").await; // custom aggregator: rows per cell
    let df = h.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(df.shape(), (7, 21)); // grp + one column per name
    let total: f64 = df.columns()[1..]
        .iter()
        .map(|c| c.f64().unwrap().sum().unwrap_or(0.0))
        .sum();
    assert_eq!(total, 20.0);
    h.keys("q l l ,").await; // grp of row 1 is 1 → ids 1, 8, 15
    assert!(title(&h).contains("sel 3"), "{}", title(&h));
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
async fn lua_function_binding_sees_the_cursor_and_toasts() {
    let mut h = Harness::new(5).await;
    h.app.plugins = Some(
        crate::plugins::Plugins::load_from_source(
            r#"vd.bind("sheet", "X", function()
                 local s = vd.sheet()
                 vd.toast(s.column .. "=" .. s.value .. " @" .. s.cursor.row .. "/" .. #s.column_names)
               end, "show cell")
               vd.bind("sheet", "E", function() error("boom") end)"#,
        )
        .unwrap(),
    );
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    h.keys("j l X").await;
    assert!(status(&h).contains("name=name_2 @2/"), "{}", status(&h));
    h.keys("E").await;
    assert!(status(&h).contains("boom"), "{}", status(&h));
    // vd.toast outside a binding is a load error, not a silent queue
    let err = crate::plugins::Plugins::load_from_source(r#"vd.toast("hi")"#);
    assert!(err.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn every_named_action_is_an_ex_command() {
    let mut h = Harness::new(30).await;
    h.keys(":").await;
    h.type_("select-all").await;
    assert_eq!(h.app.sheets.active().view.selected.len(), 30);
    h.keys(":").await;
    h.type_("unselect_all").await; // snake case, as in vd.bind
    assert!(h.app.sheets.active().view.selected.is_empty());
    // :N → row N
    h.keys(":").await;
    h.type_("25").await;
    assert!(title(&h).contains("row 25/30"), "{}", title(&h));
    // a named action takes no arguments
    h.keys(":").await;
    h.type_("select-all now").await;
    assert!(
        h.screen().contains(":select-all takes no arguments"),
        "{}",
        h.screen()
    );
    assert!(h.app.sheets.active().view.selected.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn tab_completes_ex_command_names() {
    let mut h = Harness::new(5).await;
    h.keys(":").await;
    h.type_no_enter("dedu").await;
    h.keys("<Tab>").await;
    assert_eq!(h.app.input.as_ref().unwrap().1.text(), "dedupe-rows ");
    h.keys("<C-u>").await;
    h.type_no_enter("unselect-").await;
    h.keys("<Tab>").await; // unselect-all, unselect-row → common prefix
    assert_eq!(h.app.input.as_ref().unwrap().1.text(), "unselect-");
    assert!(
        h.screen().contains("unselect-all (g u) unselect-row (u)"),
        "{}",
        h.screen()
    );
    // S-26 A8: no prefix match → letters in order
    h.keys("<C-u>").await;
    h.type_no_enter("dedrw").await;
    h.keys("<Tab>").await;
    assert_eq!(h.app.input.as_ref().unwrap().1.text(), "dedupe-rows ");
    h.keys("<C-u>").await;
    h.type_no_enter("qzx").await;
    h.keys("<Tab>").await;
    assert!(
        h.screen().contains("no command matches \"qzx\""),
        "{}",
        h.screen()
    );
    h.keys("<Esc>").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn lua_command_runs_with_its_arguments() {
    let mut h = Harness::new(5).await;
    h.app.plugins = Some(
        crate::plugins::Plugins::load_from_source(
            r#"vd.command("hello", function(args) vd.toast("hi " .. args .. " on " .. vd.sheet().column) end)
               vd.bind("sheet", "X", ":hello key")"#,
        )
        .unwrap(),
    );
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    h.keys(":").await;
    h.type_no_enter("hell").await;
    h.keys("<Tab>").await; // Lua commands complete too
    h.type_("you").await;
    assert!(h.screen().contains("hi you on id"), "{}", h.screen());
    h.keys("X").await;
    assert!(h.screen().contains("hi key on id"), "{}", h.screen());
    // a Lua command can't shadow a built-in
    let err =
        crate::plugins::Plugins::load_from_source(r#"vd.command("select-all", function() end)"#);
    assert!(err.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn lua_push_sheet_opens_a_sheet() {
    let mut h = Harness::new(5).await;
    h.app.plugins = Some(
        crate::plugins::Plugins::load_from_source(
            r#"vd.bind("sheet", "X", function()
                 local rows = {}
                 for i, c in ipairs(vd.sheet().column_names) do rows[#rows + 1] = { i, c } end
                 vd.push_sheet { name = "cols", columns = { "n", "name" }, rows = rows }
               end)"#,
        )
        .unwrap(),
    );
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    h.keys("X").await;
    assert!(title(&h).contains("cols"), "{}", title(&h));
    assert_eq!(first_cell(&h), "1");
    h.keys("q").await;
    assert!(!title(&h).contains("cols"));
}

#[tokio::test(flavor = "multi_thread")]
async fn renderer_output_sets_width_and_runs_once_per_value() {
    let mut h = Harness::new(3).await;
    // `calls` grows on every Lua call: memoized output never changes
    h.app.plugins = Some(
        crate::plugins::Plugins::load_from_source(
            r#"calls = 0
               vd.register_renderer { column = "id", render = function(v)
                 calls = calls + 1
                 return "id-" .. v .. "-rendered-wide#" .. calls end }"#,
        )
        .unwrap(),
    );
    h.pump().await;
    let first = h.screen();
    assert!(first.contains("id-1-rendered-wide#1 "), "{first}"); // not cut to the i64 width
    h.keys("j k").await;
    assert_eq!(h.screen(), first);
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

async fn tail_sheet(h: &mut Harness, path: &std::path::Path) {
    h.keys(":").await;
    h.type_(&format!("tail {}", path.display())).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tail_keeps_edits_when_rows_arrive_and_undo_keeps_rows() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.csv");
    std::fs::write(&f, "a,b\n1,2\n").unwrap();
    let mut h = csv(b"x\n1\n").await;
    tail_sheet(&mut h, &f).await;
    h.keys("^").await;
    h.type_("renamed").await;
    h.keys(":").await;
    h.type_("derive c = renamed + 1").await;
    std::fs::write(&f, "a,b\n1,2\n3,4\n").unwrap();
    h.app.on_tail_tick();
    h.pump().await;
    assert_eq!(h.app.toast, None);
    assert!(title(&h).contains("row 1/2"), "{}", title(&h));
    assert!(h.screen().contains("renamed"), "{}", h.screen());
    h.keys("U").await; // undo derive: the new row stays
    assert!(title(&h).contains("row 1/2"), "{}", title(&h));
    assert!(!h.screen().contains(" c "), "{}", h.screen());
}

#[tokio::test(flavor = "multi_thread")]
async fn tail_of_gz_survives_a_refresh() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.csv.gz");
    let gz = |rows: &str| {
        let file = std::fs::File::create(&f).unwrap();
        let mut e = flate2::write::GzEncoder::new(file, flate2::Compression::default());
        e.write_all(rows.as_bytes()).unwrap();
        e.finish().unwrap();
    };
    gz("a\n1\n");
    let mut h = csv(b"x\n1\n").await;
    tail_sheet(&mut h, &f).await;
    gz("a\n1\n2\n");
    h.app.on_tail_tick();
    h.pump().await;
    assert_eq!(h.app.toast, None);
    assert!(title(&h).contains("row 1/2"), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn tail_shows_an_unterminated_last_line() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.csv");
    std::fs::write(&f, "a\n1\n2").unwrap();
    let mut h = csv(b"x\n1\n").await;
    tail_sheet(&mut h, &f).await;
    assert!(title(&h).contains("row 1/2"), "{}", title(&h));
    std::fs::write(&f, "a\n1\n23\n4").unwrap(); // the partial row grew, then a new one
    h.app.on_tail_tick();
    h.pump().await;
    assert!(title(&h).contains("row 1/3"), "{}", title(&h));
    assert!(h.screen().contains("23"), "{}", h.screen());

    let g = dir.path().join("u.csv");
    std::fs::write(&g, "a,b").unwrap(); // header only, no newline
    tail_sheet(&mut h, &g).await;
    assert_eq!(h.app.toast, None);
    std::fs::write(&g, "a,b\n1,2\n").unwrap();
    h.app.on_tail_tick();
    h.pump().await;
    assert!(title(&h).contains("row 1/1"), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn derive_over_the_filtered_column_recounts() {
    let mut h = csv(b"n\n1\n1\n2\n").await;
    h.keys("g z |").await;
    h.type_("^1$").await;
    assert!(title(&h).contains("row 1/2"), "{}", title(&h));
    h.keys(":").await;
    h.type_("derive n = n + 1").await;
    assert!(title(&h).contains("row 0/0"), "{}", title(&h));
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
async fn lua_bound_digit_and_input_function_fire() {
    let mut h = csv(b"a\n1\n2\n3\n").await;
    h.app.plugins = Some(
        crate::plugins::Plugins::load_from_source(
            r#"vd.bind("sheet", "1", function() vd.toast("one") end)
               vd.bind("input", "<C-t>", function() vd.toast("hi") end)"#,
        )
        .unwrap(),
    );
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    h.keys("1").await;
    assert_eq!(h.app.toast.as_deref(), Some("one"));
    h.keys("2 j").await; // unbound digits still count
    assert!(title(&h).contains("row 3/3"), "{}", title(&h));
    h.keys(": <C-t>").await;
    assert_eq!(h.app.toast.as_deref(), Some("hi"));
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
async fn repeat_does_not_reopen_a_prompt() {
    let mut h = csv(b"a\nx\ny\n").await;
    h.keys("|").await;
    h.type_("x").await;
    assert_eq!(
        h.app.last_action,
        Some(vec![Action::SelectByRegex { regex: "x".into() }])
    );
    h.keys(":").await; // `.` plots (VisiData); repeat is `:repeat-last`
    h.type_("repeat-last").await;
    assert!(h.app.input.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn tail_of_a_lone_single_column_header_gains_no_phantom_row() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.csv");
    std::fs::write(&f, "a").unwrap();
    let mut h = csv(b"x\n1\n").await;
    tail_sheet(&mut h, &f).await;
    std::fs::write(&f, "a\n1\n").unwrap();
    h.app.on_tail_tick();
    h.pump().await;
    assert!(title(&h).contains("row 1/1"), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn tail_reads_iso_dates_like_a_plain_open() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.csv");
    std::fs::write(&f, "d,u\n2024-01-02,7/3/2018\n").unwrap();
    let mut h = csv(b"x\n1\n").await;
    tail_sheet(&mut h, &f).await;
    std::fs::write(&f, "d,u\n2024-01-02,7/3/2018\n2024-01-03,7/4/2018\n").unwrap();
    h.app.on_tail_tick();
    h.pump().await;
    let df = h.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(df.height(), 2);
    assert_eq!(df.column("d").unwrap().dtype(), &DataType::Date);
    assert_eq!(df.column("u").unwrap().dtype(), &DataType::String);
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
async fn tail_turns_a_date_column_to_text_when_an_ambiguous_date_arrives() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.csv");
    std::fs::write(&f, "d\n2024-01-02\n").unwrap();
    let mut h = csv(b"x\n1\n").await;
    tail_sheet(&mut h, &f).await;
    std::fs::write(&f, "d\n2024-01-02\n7/3/2018\n").unwrap();
    h.app.on_tail_tick();
    h.pump().await;
    let df = h.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(df.column("d").unwrap().dtype(), &DataType::String);
    assert_eq!(
        df.column("d").unwrap().str().unwrap().get(1),
        Some("7/3/2018")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn tail_learns_types_from_the_first_rows_after_a_bare_header() {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("t.csv");
    std::fs::write(&f, "d,n\n").unwrap();
    let mut h = csv(b"x\n1\n").await;
    tail_sheet(&mut h, &f).await;
    std::fs::write(&f, "d,n\n2024-01-02,5\n").unwrap();
    h.app.on_tail_tick();
    h.pump().await;
    let df = h.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(df.column("d").unwrap().dtype(), &DataType::Date);
    assert_eq!(df.column("n").unwrap().dtype(), &DataType::Int64);
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

/// S-21 A: `z/`, `z|`, `z\`, `:select`, `=`.
#[tokio::test(flavor = "multi_thread")]
async fn expression_keys_search_select_and_derive() {
    let mut h = csv(b"qty,price\n1,10\n5,2\n3,4\n").await;
    h.keys("z /").await;
    h.type_("qty * price >= 12").await;
    assert_eq!(h.app.sheets.active().view.row_cursor, 2);
    h.keys("n").await; // the only match: wraps back to it
    assert_eq!(h.app.sheets.active().view.row_cursor, 2);

    h.keys("z |").await;
    h.type_("qty > 1").await;
    assert_eq!(h.app.sheets.active().view.selected.len(), 2);
    h.keys("z \\").await;
    h.type_("price = 2").await;
    assert_eq!(h.app.sheets.active().view.selected.len(), 1);
    let undo_depth = h.app.sheets.active().state.undo.depth();
    h.keys(":").await;
    h.type_("select qty + 1").await;
    assert_eq!(h.app.sheets.active().state.undo.depth(), undo_depth); // no empty undo step
    assert!(
        h.app
            .toast
            .as_deref()
            .unwrap_or("")
            .contains("not a boolean")
    );
    assert_eq!(h.app.sheets.active().view.selected.len(), 1);

    h.keys("=").await;
    h.type_("qty * price").await;
    assert!(h.app.sheets.active().has_column("qty * price"));
    h.keys("=").await;
    h.type_("total = qty * price").await;
    assert!(h.app.sheets.active().has_column("total"));
    h.keys("=").await;
    h.type_("qty = 5").await; // qty exists: the whole text is the expression
    let df = h.app.sheets.active().view_plan().collect().unwrap();
    assert_eq!(df.column("qty = 5").unwrap().dtype(), &DataType::Boolean);
    assert_eq!(df.column("qty").unwrap().dtype(), &DataType::Int64);
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

/// S-18 A: `y` `gy` `zy` `gzy` fill the register, typed, in view order.
#[tokio::test(flavor = "multi_thread")]
async fn copy_to_register_rows_and_cells() {
    let mut h = csv(b"id,x,note\n1,1.5,a\n2,,b\n3,0.5,c\n").await;
    h.keys("g y").await; // nothing selected
    assert_eq!(h.app.toast.as_deref(), Some("no rows selected"));
    assert!(h.app.register.is_none());

    h.keys("j y").await; // row 2: x is null
    let r = h.app.register.clone().unwrap();
    assert_eq!(r.kind, RegisterKind::Rows);
    assert_eq!(r.df.height(), 1);
    assert_eq!(r.df.column("x").unwrap().dtype(), &DataType::Float64);
    assert_eq!(r.df.column("x").unwrap().null_count(), 1);
    assert_eq!(h.app.toast.as_deref(), Some("copied 1 row"));

    // hidden columns stay in the register; selection in view order after a sort
    h.keys("g l -").await; // hide note
    h.keys("g h s j j s").await; // select rows 2 and 3
    h.keys("]").await; // sort id desc: 3, 2, 1
    h.keys("g y").await;
    let r = h.app.register.clone().unwrap();
    assert_eq!(r.df.get_column_names(), ["id", "x", "note"]);
    let ids: Vec<i64> =
        r.df.column("id")
            .unwrap()
            .i64()
            .unwrap()
            .into_no_null_iter()
            .collect();
    assert_eq!(ids, vec![3, 2]);
    assert_eq!(h.app.toast.as_deref(), Some("copied 2 rows"));

    h.keys("l g z y").await; // current column (x) of the selected rows
    let r = h.app.register.clone().unwrap();
    assert_eq!(r.kind, RegisterKind::Cells);
    assert_eq!(r.df.get_column_names(), ["x"]);
    assert_eq!(r.df.height(), 2);

    h.keys("g g z y").await; // cursor cell: x of id 3
    let r = h.app.register.clone().unwrap();
    assert_eq!(r.df.shape(), (1, 1));
    assert_eq!(r.df.column("x").unwrap().f64().unwrap().get(0), Some(0.5));
    assert_eq!(h.app.toast.as_deref(), Some("copied 1 cell"));

    // the register is per app: it survives opening and closing sheets
    h.keys("F q").await;
    assert_eq!(h.app.register.as_ref().unwrap().df.shape(), (1, 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn syscopy_sends_tsv_and_lines_to_the_clipboard() {
    use crate::clipboard::Clipboard;
    let mut h = csv(b"id,x,note\n1,1.5,a\tb\n2,,c\n3,0.5,d\n").await;
    let Clipboard::Memory(sink) = h.app.clipboard.clone() else {
        panic!("tests must use the memory sink");
    };
    let sent = |n: usize| sink.lock().unwrap().get(n).cloned();
    h.keys("g Y").await;
    assert_eq!(h.app.toast.as_deref(), Some("no rows selected"));

    h.keys("Y").await; // header + row; tab inside a value as U+001F
    assert_eq!(sent(0).as_deref(), Some("id\tx\tnote\n1\t1.5\ta\u{1f}b\n"));
    assert_eq!(h.app.toast.as_deref(), Some("copied 1 row to clipboard"));
    assert!(h.app.register.is_none(), "Y leaves the register alone");

    h.keys("g l - g h s j s").await; // hide note; select rows 1, 2
    h.keys("g Y").await; // visible columns only, null empty
    assert_eq!(sent(1).as_deref(), Some("id\tx\n1\t1.5\n2\t\n"));
    h.keys("l g z Y").await;
    assert_eq!(sent(2).as_deref(), Some("1.5\n\n"));
    assert_eq!(h.app.toast.as_deref(), Some("copied 2 cells to clipboard"));
    h.keys("g g z Y").await;
    assert_eq!(sent(3).as_deref(), Some("1.5"));

    h.app.clipboard = Clipboard::Command(vec!["vdrs-no-such-clip-cmd".into()]);
    h.keys("Y").await;
    let toast = h.app.toast.clone().unwrap();
    assert!(toast.contains("vdrs-no-such-clip-cmd not found"), "{toast}");
}

fn view_csv(h: &Harness) -> String {
    csv_text(h.app.sheets.active(), b',')
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_cell_prompt_undo_redo_and_types() {
    let mut h = csv(b"id,qty,price\n1,2,1.5\n2,3,2.0\n3,,4.0\n").await;
    h.keys("l e").await; // prefilled with the cell
    assert_eq!(h.app.input.as_ref().unwrap().1.text(), "2");
    h.keys("<C-u>").await;
    h.type_("5").await;
    assert_eq!(view_csv(&h), "id,qty,price\n1,5,1.5\n2,3,2.0\n3,,4.0\n");
    assert_eq!(
        (h.app.sheets.active().view.row_cursor, h.current_column()),
        (0, "qty".into())
    );

    h.keys("e <C-u> 9 <Esc>").await; // Esc changes nothing
    assert!(view_csv(&h).contains("1,5,1.5"));
    h.keys("U").await;
    assert!(view_csv(&h).contains("1,2,1.5"));
    h.keys("R").await;
    assert!(view_csv(&h).contains("1,5,1.5"));

    h.keys("e <C-u>").await;
    h.type_("abc").await;
    assert_eq!(status(&h), r#""abc" doesn't fit qty (i64)"#);
    h.keys("j e <C-u> <CR>").await; // empty = null
    assert_eq!(view_csv(&h), "id,qty,price\n1,5,1.5\n2,,2.0\n3,,4.0\n");

    // the value must fit the column as displayed too: `#` on price
    h.keys("l #").await;
    h.keys(":").await;
    h.type_("edit 2.5").await;
    assert!(status(&h).contains("doesn't fit price"), "{}", status(&h));
    h.keys(":").await;
    h.type_("edit 7").await;
    assert!(view_csv(&h).contains("2,,7"), "{}", view_csv(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_goes_before_derives_that_read_it_and_follows_its_row() {
    let mut h = csv(b"id,qty,price\n1,2,1.5\n2,3,2.0\n3,1,4.0\n").await;
    h.keys(":").await;
    h.type_("derive total = qty * price").await;
    h.keys("l e <C-u>").await; // qty of id 1
    h.type_("10").await;
    let v = view_csv(&h);
    assert!(v.contains("1,10,1.5,15.0"), "{v}");
    // the derived column itself: the cell is pinned
    h.keys("g l e <C-u>").await;
    h.type_("0").await;
    assert!(view_csv(&h).contains("1,10,1.5,0.0"), "{}", view_csv(&h));
    // sort/filter later: the edit stays on its row
    h.keys("g h l ]").await; // qty desc
    assert!(view_csv(&h).starts_with("id,qty,price,total\n1,10,1.5,0.0\n"));
    h.keys("j :").await; // id 2 is second
    h.type_("edit 4").await;
    h.keys("[").await;
    assert_eq!(
        view_csv(&h),
        "id,qty,price,total\n3,1,4.0,4.0\n2,4,2.0,8.0\n1,10,1.5,0.0\n"
    );
    // ge = setcol prompt on the selected rows
    h.keys("g h s j s g e").await;
    h.type_("7").await;
    assert!(view_csv(&h).starts_with("id,qty,price,total\n7,1,4.0,4.0\n7,4,2.0,8.0\n"));
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
    assert!(status(&h).contains("usage: sidebar"), "{}", status(&h));
}

/// S-18 F: `a` `ga` add blank rows after the cursor and move there; `p` `P`
/// paste copied rows; inserted rows keep their place under a sort.
#[tokio::test(flavor = "multi_thread")]
async fn add_and_paste_rows() {
    let mut h = csv(b"id,v\n1,a\n2,b\n3,c\n").await;
    h.keys("a").await;
    assert_eq!(view_csv(&h), "id,v\n1,a\n,\n2,b\n3,c\n");
    assert_eq!(h.app.sheets.active().view.row_cursor, 1);
    h.keys("e").await;
    h.type_("7").await;
    h.keys("g a").await;
    h.type_("2").await;
    assert_eq!(view_csv(&h), "id,v\n1,a\n7,\n,\n,\n2,b\n3,c\n");
    assert_eq!(h.app.sheets.active().view.row_cursor, 2);
    h.keys("U").await;
    assert_eq!(view_csv(&h), "id,v\n1,a\n7,\n2,b\n3,c\n");

    // sort, then clear it: the added row is back where it was put
    h.keys("]").await;
    assert!(
        view_csv(&h).starts_with("id,v\n7,\n3,c\n"),
        "{}",
        view_csv(&h)
    );
    h.keys("z o").await;
    assert_eq!(view_csv(&h), "id,v\n1,a\n7,\n2,b\n3,c\n");

    // p / P paste copied rows by column name
    h.keys("g g y").await;
    h.keys("G p").await;
    assert_eq!(status(&h), "pasted 1 row");
    assert_eq!(view_csv(&h), "id,v\n1,a\n7,\n2,b\n3,c\n1,a\n");
    h.keys("g g P").await;
    assert_eq!(view_csv(&h), "id,v\n1,a\n1,a\n7,\n2,b\n3,c\n1,a\n");
    h.keys("z y").await;
    h.keys("p").await;
    assert_eq!(status(&h), "p pastes rows: copy with y or gy");
}

/// `A`: a new empty sheet; add a row, type in it, save, reopen.
#[tokio::test(flavor = "multi_thread")]
async fn open_new_add_edit_save() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("x.csv");
    let mut h = csv(b"id\n1\n").await;
    h.keys("A").await;
    h.keys("a").await;
    h.keys("e").await;
    h.type_("hello").await;
    h.keys("a").await;
    h.keys("e").await;
    h.type_("world").await;
    assert_eq!(view_csv(&h), "A\nhello\nworld\n");
    h.keys(":").await;
    h.type_(&format!("save {}", out.display())).await;
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "A\nhello\nworld\n");
}

/// S-18 D: `zp` `gzp` paste copied cells, `zx` `gzx` cut them.
#[tokio::test(flavor = "multi_thread")]
async fn paste_and_cut_cells() {
    let mut h = csv(b"id,qty,note\n1,2,a\n2,3,b\n3,,c\n4,5,d\n").await;
    h.keys("l z p").await;
    assert_eq!(status(&h), "nothing copied");
    h.keys("y").await;
    h.keys("z p").await;
    assert_eq!(status(&h), "zp pastes cells: copy with zy or gzy");

    h.keys("z y").await;
    h.keys("j z p").await; // qty 2 into id 2
    assert_eq!(view_csv(&h), "id,qty,note\n1,2,a\n2,2,b\n3,,c\n4,5,d\n");
    h.keys("U").await;
    assert!(view_csv(&h).contains("2,3,b"));

    // gzp cycles the copied cells over the selected rows in view order
    h.keys("g g s j s g z y").await; // copy qty of ids 1, 2: [2, 3]
    h.keys("g u g g j s j s j s h ]").await; // select ids 2..4; sort id desc
    h.keys("l g z p").await; // view order 4, 3, 2 gets 2, 3, 2
    assert_eq!(status(&h), "pasted into qty on 3 rows");
    h.keys("h [").await;
    assert_eq!(view_csv(&h), "id,qty,note\n1,2,a\n2,2,b\n3,3,c\n4,2,d\n");
    h.keys("U U h [").await; // undo the sort and the paste; sort asc again
    assert!(view_csv(&h).contains("2,3,b"), "{}", view_csv(&h));

    // a value that doesn't fit refuses the whole paste
    h.keys("g g l l z y").await;
    h.keys("h g z p").await; // note "a" into qty
    assert_eq!(status(&h), r#"paste: "a" doesn't fit qty (i64)"#);

    // zx: cell to the register, then null
    h.keys("g g z x").await;
    assert!(
        view_csv(&h).starts_with("id,qty,note\n1,,a\n"),
        "{}",
        view_csv(&h)
    );
    let r = h.app.register.clone().unwrap();
    assert_eq!(r.df.column("qty").unwrap().i64().unwrap().get(0), Some(2));
    h.keys("g z x").await; // selected rows
    assert_eq!(h.app.register.as_ref().unwrap().df.height(), 3);
    assert_eq!(view_csv(&h), "id,qty,note\n1,,a\n2,,b\n3,,c\n4,,d\n");
    h.keys("U U").await;
    assert!(
        view_csv(&h).starts_with("id,qty,note\n1,2,a\n2,3,b\n"),
        "{}",
        view_csv(&h)
    );

    // zd / gzd: empty without touching the register
    h.keys("g g z d").await;
    assert!(
        view_csv(&h).starts_with("id,qty,note\n1,,a\n"),
        "{}",
        view_csv(&h)
    );
    assert_eq!(h.app.register.as_ref().unwrap().df.height(), 3);
    h.keys("g z d").await;
    assert_eq!(view_csv(&h), "id,qty,note\n1,,a\n2,,b\n3,,c\n4,,d\n");
}

/// S-18 E: `d` `gd` `x` `gx` delete rows; edits after a delete still hit
/// their rows; `U` brings rows back in place.
#[tokio::test(flavor = "multi_thread")]
async fn delete_and_cut_rows() {
    let mut h = csv(b"id,v\n1,a\n2,b\n3,c\n4,d\n5,e\n").await;
    h.keys("g d").await;
    assert_eq!(status(&h), "no rows selected");
    h.keys("j d").await; // id 2; the cursor stays on the row that moved up
    assert_eq!(view_csv(&h), "id,v\n1,a\n3,c\n4,d\n5,e\n");
    assert_eq!(status(&h), "deleted 1 row");
    assert_eq!(h.app.sheets.active().view.row_cursor, 1);
    h.keys("l e <C-u>").await; // id 3 now under the cursor
    h.type_("C").await;
    assert_eq!(view_csv(&h), "id,v\n1,a\n3,C\n4,d\n5,e\n");

    // gd on a sorted view drops the selection's rows and their selection
    h.keys("h ]").await;
    h.keys("g g s j s").await; // id desc: 5, 4 selected
    h.keys("g d").await;
    assert_eq!(view_csv(&h), "id,v\n3,C\n1,a\n");
    assert!(h.app.sheets.active().view.selected.is_empty());
    h.keys("[").await;
    h.keys("g g x").await; // cut id 1 into the register
    let r = h.app.register.clone().unwrap();
    assert_eq!((r.kind, r.df.height()), (RegisterKind::Rows, 1));
    assert_eq!(view_csv(&h), "id,v\n3,C\n");

    // undo: x, [, gd (with its selection), then the edit stays
    h.keys("U").await;
    assert_eq!(view_csv(&h), "id,v\n1,a\n3,C\n");
    h.keys("U U").await;
    assert_eq!(view_csv(&h), "id,v\n5,e\n4,d\n3,C\n1,a\n");
    assert_eq!(h.app.sheets.active().view.selected.len(), 2);
    h.keys("g x").await;
    assert_eq!(h.app.register.as_ref().unwrap().df.height(), 2);
    assert_eq!(view_csv(&h), "id,v\n3,C\n1,a\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn derived_sheets_refuse_edits_but_copy() {
    let mut h = csv(b"id,g\n1,a\n2,b\n3,a\n").await;
    h.keys("l F").await;
    for keys in [
        "e", "s g e", "g =", "z x", "g z x", "z d", "g z d", "d", "g d", "x", "g x",
    ] {
        h.keys(keys).await;
        assert!(
            status(&h).starts_with("read-only: "),
            "{keys}: {}",
            status(&h)
        );
        assert!(h.app.input.is_none());
    }
    h.keys(":").await;
    h.type_("edit x").await;
    assert!(status(&h).contains("is derived from"), "{}", status(&h));
    h.keys(":").await;
    h.type_("setcol x").await;
    assert!(status(&h).contains("read-only: "), "{}", status(&h));
    h.keys(":").await;
    h.type_("setexpr 1").await;
    assert!(status(&h).contains("read-only: "), "{}", status(&h));
    assert!(h.app.sheets.active().state.steps.is_empty());
    h.keys("y").await;
    assert_eq!(status(&h), "copied 1 row");
}

#[tokio::test(flavor = "multi_thread")]
async fn quit_guard_asks_about_unsaved_edits() {
    let mut h = csv(b"id,g\n1,a\n2,b\n").await;
    h.keys("l e <C-u>").await;
    h.type_("z").await;
    h.keys("F").await; // a child sheet has no edits: q just pops
    h.keys("q").await;
    assert_eq!(h.app.sheets.len(), 1);
    h.keys("q").await;
    let ask = format!("discard 1 edit to {}? y/N", h.app.sheets.active().name);
    assert!(status(&h).contains(&ask), "{}", status(&h));
    h.keys("n").await;
    assert_eq!(status(&h), "kept the edits");
    assert!(!h.app.quit);
    h.keys("g q").await;
    assert!(
        status(&h).contains("quit and discard 1 edit? y/N"),
        "{}",
        status(&h)
    );
    h.keys("<Esc>").await;
    assert!(!h.app.quit);
    // saved: no question
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.csv");
    h.keys(":").await;
    h.type_(&format!("save {}", out.display())).await;
    assert_eq!(std::fs::read_to_string(&out).unwrap(), "id,g\n1,z\n2,b\n");
    h.keys("q").await;
    assert!(h.app.quit);
}

#[tokio::test(flavor = "multi_thread")]
async fn quit_guard_y_and_bang_skip() {
    let mut h = csv(b"id\n1\n").await;
    h.keys("e <C-u>").await;
    h.type_("2").await;
    h.keys("q y").await;
    assert!(h.app.quit);
    let mut h = csv(b"id\n1\n").await;
    h.keys(":").await;
    h.type_("edit 2").await;
    h.keys(":").await;
    h.type_("q!").await;
    assert!(h.app.quit);
}

#[tokio::test(flavor = "multi_thread")]
async fn quit_guard_sees_a_new_edit_after_save_and_undo() {
    let mut h = csv(b"id\n1\n2\n").await;
    let dir = tempfile::tempdir().unwrap();
    h.keys(":").await;
    h.type_("edit 5").await;
    h.keys(":").await;
    h.type_(&format!("save {}", dir.path().join("o.csv").display()))
        .await;
    h.keys("U j :").await; // undo the saved edit, make another: same count, different edit
    h.type_("edit 6").await;
    assert_eq!(h.app.sheets.active().unsaved_edits(), 2);
    h.keys("q").await;
    assert!(status(&h).contains("discard 2 edits"), "{}", status(&h));
    // a job finishing meanwhile doesn't hide the question
    h.app.toast = Some("saved 2 rows".into());
    h.app.draw(&mut h.term).unwrap();
    assert!(status(&h).contains("discard 2 edits"), "{}", status(&h));
    h.keys("n").await;
    assert!(!h.app.quit);
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
async fn filter_expr_shows_in_the_title_undoes_and_unfilters() {
    let mut h = csv(b"id,v\n1,10\n2,20\n3,30\n").await;
    h.keys(":").await;
    h.type_("filter-expr v > 15").await;
    assert!(
        title(&h).contains("filter v > 15") && title(&h).contains("row 1/2"),
        "{}",
        title(&h)
    );
    h.keys(":").await;
    h.type_("filter-expr v || 'x'").await;
    assert!(status(&h).contains("not a boolean"), "{}", status(&h));
    assert!(title(&h).contains("row 1/2"), "{}", title(&h));
    h.keys("U").await;
    assert!(
        !title(&h).contains("filter") && title(&h).contains("row 1/3"),
        "{}",
        title(&h)
    );
    h.keys(":").await;
    h.type_("filter-expr v > 15").await;
    h.keys(":").await;
    h.type_("unfilter").await;
    assert!(title(&h).contains("row 1/3"), "{}", title(&h));
}

/// S-18 G: a terminal paste fills the register (header or not, TSV or CSV)
/// and types into prompts; `zP` `gzP` write the pasted lines into cells,
/// adding rows past the end; one `U` undoes it.
#[tokio::test(flavor = "multi_thread")]
async fn terminal_paste_and_syspaste_cells() {
    let mut h = csv(b"id,v,w\n1,a,x\n2,b,y\n3,c,z\n").await;
    h.paste("v\tid\nq\t9\n").await;
    assert_eq!(status(&h), "clipboard: 1 row × 2 cols");
    h.keys("p").await;
    assert_eq!(view_csv(&h), "id,v,w\n1,a,x\n9,q,\n2,b,y\n3,c,z\n");
    h.keys("U").await;
    h.paste("\"5\",\"r, s\"\r\n").await; // CSV, no header: by position
    h.keys("G p").await;
    assert_eq!(view_csv(&h), "id,v,w\n1,a,x\n2,b,y\n3,c,z\n5,\"r, s\",\n");
    h.keys("U").await;
    h.paste("7\n8\n").await; // one column: cells for zp/gzp
    h.keys("g g z p").await;
    assert_eq!(view_csv(&h), "id,v,w\n7,a,x\n2,b,y\n3,c,z\n");
    h.keys("U").await;

    // zP: from the cursor cell, rightwards and down; past the end adds rows
    h.paste("m\tn\textra\no\n\tp\n").await;
    h.keys("j l z P").await;
    assert_eq!(status(&h), "pasted 5 cells");
    assert_eq!(view_csv(&h), "id,v,w\n1,a,x\n2,m,n\n3,o,z\n,\"\",p\n");
    h.keys("U").await;
    assert_eq!(view_csv(&h), "id,v,w\n1,a,x\n2,b,y\n3,c,z\n");

    // gzP: into the selected rows, in view order; a bad value refuses all
    h.paste("40\n10\n").await;
    h.keys("g g s j j s h h g z P").await;
    assert_eq!(view_csv(&h), "id,v,w\n40,a,x\n2,b,y\n10,c,z\n");
    h.paste("11\nnope\n").await;
    h.keys("g z P").await;
    assert_eq!(status(&h), "paste: \"nope\" doesn't fit id (i64)");
    assert_eq!(view_csv(&h), "id,v,w\n40,a,x\n2,b,y\n10,c,z\n");

    // in a prompt: the first line is typed in
    h.keys("g g l /").await;
    h.paste("c\nignored").await;
    h.keys("<CR>").await;
    assert_eq!(h.app.sheets.active().view.row_cursor, 2);

    // with a paste command, zP reads its output in the background
    h.app.paste_cmd = Some(vec!["printf".into(), "z1\\tz2\\n".into()]);
    h.keys("z P").await;
    h.pump().await;
    assert_eq!(view_csv(&h), "id,v,w\n40,a,x\n2,b,y\n10,z1,z2\n");
}

/// S-20: a change after `U` keeps the undone branch; `:undotree` lists the
/// states and `Enter` goes back to one; `:earlier` / `:later` walk them.
#[tokio::test(flavor = "multi_thread")]
async fn undo_tree_keeps_branches() {
    let mut h = csv(b"id,v\n1,a\n2,b\n").await;
    h.keys("l e <C-u>").await;
    h.type_("x").await; // state 1
    h.keys("U").await;
    h.keys("-").await; // state 2: hide v, a branch beside 1
    assert_eq!(view_csv(&h), "id\n1\n2\n");
    h.keys(":").await;
    h.type_("undotree").await;
    h.pump().await;
    let screen = h.screen();
    for want in ["opened", "  edit v = x", "  hide v"] {
        assert!(screen.contains(want), "{want}\n{screen}");
    }
    assert_eq!(
        h.app.sheets.active().view.row_cursor,
        2,
        "on the current state"
    );
    h.keys("k <CR>").await; // state 1
    assert_eq!(status(&h), "at state 1");
    assert_eq!(view_csv(&h), "id,v\n1,x\n2,b\n");
    h.keys(":").await;
    h.type_("later").await;
    assert_eq!(view_csv(&h), "id\n1\n2\n");
    h.keys(":").await;
    h.type_("earlier 9").await;
    assert_eq!(view_csv(&h), "id,v\n1,a\n2,b\n");
    h.keys(":").await;
    h.type_("earlier").await;
    assert_eq!(status(&h), "at the oldest state");
}

#[tokio::test(flavor = "multi_thread")]
async fn expand_and_contract_a_struct_column() {
    let mut tmp = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .unwrap();
    writeln!(
        tmp,
        r#"{{"id":1,"user":{{"name":"ann","geo":{{"lat":1.5,"lon":2}}}},"tags":["a","b"]}}"#
    )
    .unwrap();
    writeln!(
        tmp,
        r#"{{"id":2,"user":{{"name":"bob","geo":{{"lat":3.0,"lon":4}}}},"tags":[]}}"#
    )
    .unwrap();
    tmp.flush().unwrap();
    let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
    let mut h = Harness::with_source(loaded, tmp).await;
    let header = |h: &Harness| h.screen().lines().nth(1).unwrap().to_string();
    assert!(
        h.screen().contains("{name: ann, geo: {…}}"),
        "{}",
        h.screen()
    );
    assert!(h.screen().contains("[a, b] (2)"), "{}", h.screen());
    assert!(header(&h).contains("user struct"), "{}", header(&h));
    h.keys("l (").await;
    assert_eq!(h.current_column(), "user.name");
    assert!(
        header(&h).contains("user.geo") && !header(&h).contains("user "),
        "{}",
        header(&h)
    );
    assert!(h.screen().contains("{lat: 1.5, lon: 2}"), "{}", h.screen());
    h.keys("l )").await; // from user.geo: back to user
    assert_eq!(h.current_column(), "user");
    assert!(!header(&h).contains("user.name"), "{}", header(&h));
    h.keys("U").await; // the fold undone
    assert_eq!(h.current_column(), "user.geo");
    h.keys("U").await; // and the expansion
    assert!(!header(&h).contains("user.name"), "{}", header(&h));
    h.keys("z (").await;
    h.type_("2").await;
    assert!(header(&h).contains("user.geo.lat"), "{}", header(&h));
    h.keys("g (").await;
    assert_eq!(status(&h), "expanded 0 struct columns");
}

#[tokio::test(flavor = "multi_thread")]
async fn unfurl_pushes_a_read_only_sheet_of_items() {
    let mut tmp = tempfile::Builder::new()
        .suffix(".jsonl")
        .tempfile()
        .unwrap();
    writeln!(tmp, r#"{{"id":1,"tags":["a","b"]}}"#).unwrap();
    writeln!(tmp, r#"{{"id":2,"tags":["a"]}}"#).unwrap();
    writeln!(tmp, r#"{{"id":3,"tags":[]}}"#).unwrap();
    tmp.flush().unwrap();
    let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
    let mut h = Harness::with_source(loaded, tmp).await;
    h.keys("l z M").await;
    assert!(
        title(&h).contains("unfurl(tags)") && title(&h).contains("row 1/3"),
        "{}",
        title(&h)
    );
    assert_eq!(h.current_column(), "tags_value");
    h.keys("e").await;
    assert!(status(&h).starts_with("read-only"), "{}", status(&h));
    h.keys("<Esc> F").await; // counts per tag
    assert_eq!(first_cell(&h), "a", "{}", h.screen());
    assert!(h.screen().contains(" 2 "), "{}", h.screen());
    h.keys("q q").await;
    assert!(
        title(&h).contains("row 1/3") && !title(&h).contains("unfurl"),
        "{}",
        title(&h)
    );
    assert_eq!(h.current_column(), "tags");
}

#[tokio::test(flavor = "multi_thread")]
async fn columns_sheet_writes_back_to_its_sheet() {
    let mut h = Harness::new(5).await;
    let header = |h: &Harness| h.screen().lines().nth(1).unwrap().to_string();
    h.keys("l C").await; // cursor on `name`: the columns sheet starts on its row
    assert!(
        title(&h).contains("_columns") && title(&h).contains("row 2/4"),
        "{}",
        title(&h)
    );
    assert!(h.screen().contains("name_1"), "{}", h.screen()); // the cursor row's value
    h.keys("d").await; // hide `name`
    h.keys("j s g !").await; // key `grp`
    h.keys("e <C-u>").await;
    h.type_("group").await; // rename it
    assert!(h.screen().contains("group"), "{}", h.screen());
    h.keys("q").await;
    let hd = header(&h);
    assert!(
        hd.trim_start().starts_with("group") && !hd.contains("name"),
        "{hd}"
    );
    h.keys("C g g").await; // `group`, the key, comes first
    h.keys("U").await; // undoes the rename, on the sheet
    assert!(
        h.screen().lines().any(|l| l.starts_with("grp ")),
        "{}",
        h.screen()
    );
    h.keys("<CR>").await; // freq of grp
    assert!(title(&h).contains("freq(grp)"), "{}", title(&h));
    h.keys("q g g s j s l g e").await; // back on C: ge on `width` of grp, id hides both
    h.type_("0").await;
    h.keys("q").await;
    let hd = header(&h);
    assert!(!hd.contains("id") && !hd.contains("grp"), "{hd}");
    h.keys("g #").await;
    assert_eq!(status(&h), "only on the columns sheet (C)");
}

#[tokio::test(flavor = "multi_thread")]
async fn aggregators_add_freq_columns_and_memo() {
    let mut h = Harness::new(5).await;
    h.keys("l +").await;
    h.type_("sum").await;
    assert!(status(&h).contains("name isn't numeric"), "{}", status(&h));
    h.keys("l l +").await;
    h.type_("sum mean").await;
    h.keys("z +").await;
    h.type_("sum").await;
    assert_eq!(status(&h), "amt_sum=22.5");
    h.keys("h F").await;
    let hd = h.screen().lines().nth(1).unwrap().to_string();
    assert!(
        hd.contains("amt_sum") && hd.contains("amt_mean") && !hd.contains("percent"),
        "{hd}"
    );
    h.keys("q C").await;
    assert!(h.screen().contains("sum mean"), "{}", h.screen());
    h.keys("g +").await; // nothing selected: every visible non-key column
    h.type_("count").await;
    assert!(h.screen().contains("sum mean count"), "{}", h.screen());
    h.keys("q F").await;
    let hd = h.screen().lines().nth(1).unwrap().to_string();
    assert!(hd.contains("id_count") && hd.contains("amt_count"), "{hd}");
    h.keys("q U F").await; // the columns sheet's g+ is one undo step on the sheet
    let hd = h.screen().lines().nth(1).unwrap().to_string();
    assert!(hd.contains("amt_mean") && !hd.contains("id_count"), "{hd}");
}

#[tokio::test(flavor = "multi_thread")]
async fn all_columns_undo_touches_only_the_sheet_it_wrote() {
    let mut h = Harness::new(5).await;
    h.keys("F [").await; // a freq sheet with its own (sort) change
    h.keys("g C d").await; // all_columns: hide the first sheet's first column
    let hidden = |h: &Harness| {
        let s = h.app.sheets.iter().find(|s| !s.name.contains('(')).unwrap();
        s.view.hidden.len()
    };
    assert_eq!(hidden(&h), 1);
    h.keys("U").await;
    assert_eq!(hidden(&h), 0);
    let freq = h
        .app
        .sheets
        .iter()
        .find(|s| s.name.starts_with("freq("))
        .unwrap();
    assert!(!freq.view.sort.is_empty(), "the freq sheet's sort stays");
    h.keys("R").await;
    assert_eq!(hidden(&h), 1);
    h.keys("U U").await;
    assert_eq!(status(&h), "nothing to undo");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sheet_changed_from_gc_redraws_when_raised_from_the_sheet_list() {
    let mut h = Harness::new(5).await;
    h.keys("F").await; // the base sheet is not under the columns sheet
    h.keys("g C").await;
    h.keys("d").await; // hide the base sheet's `id`
    h.keys("S").await;
    h.keys("<CR>").await; // switcher row 1: the base sheet
    assert!(!title(&h).contains("freq"), "{}", title(&h));
    let hd = h.screen().lines().nth(1).unwrap().to_string();
    assert!(!hd.contains("id") && hd.contains("name"), "{hd}");
}

#[tokio::test(flavor = "multi_thread")]
async fn rejected_or_columns_sheet_edits_leave_no_empty_undo_state() {
    let mut h = Harness::new(5).await;
    let depth = |h: &Harness| {
        let s = h.app.sheets.iter().find(|s| !s.name.contains("_columns"));
        s.unwrap().state.undo.depth()
    };
    h.keys("l +").await;
    h.type_("sum").await; // name isn't numeric: refused
    assert_eq!(depth(&h), 0);
    h.keys("l l +").await;
    h.type_("sum").await;
    assert_eq!(depth(&h), 1);
    h.keys("g g C").await;
    h.keys("l l l e <C-u>").await; // `value` of the cursor column, `id`
    h.type_("9").await;
    assert_eq!(depth(&h), 2, "one state for one edit");
}

#[tokio::test(flavor = "multi_thread")]
async fn macro_records_relative_steps_and_replays_on_another_sheet() {
    let mut h = Harness::new(30).await;
    h.keys("m").await;
    assert!(status(&h).starts_with("● rec"), "{:?}", h.screen());
    h.keys("l l F").await;
    assert!(title(&h).contains("freq(grp)"), "{}", title(&h));
    h.keys("q m").await; // `q` isn't recorded
    h.type_("<F5>").await;
    assert_eq!(status(&h), "macro on <F5>: 3 steps; saved");
    assert!(!h.screen().contains("● rec"));
    h.keys("0 l <F5>").await; // relative (M1): name → amt
    assert!(title(&h).contains("freq(amt)"), "{}", title(&h));
    // `.` repeats the whole macro, not its last step
    assert!(matches!(h.app.last_action.as_deref(), Some([Action::Macro(s)]) if s.len() == 3));
}

#[tokio::test(flavor = "multi_thread")]
async fn macro_playback_waits_for_jobs_between_steps() {
    let mut h = Harness::new(5000).await;
    h.keys("m l l F").await;
    h.keys("<CR>").await; // dive into the freq sheet's first row
    let dived = title(&h);
    assert!(dived.contains("grp="), "{dived}");
    h.keys("q q m").await;
    h.type_("g <F6>").await;
    h.keys("0 g <F6>").await;
    assert_eq!(title(&h), dived);
}

#[tokio::test(flavor = "multi_thread")]
async fn macro_with_derive_and_edit_matches_typing_it() {
    let mut h = csv(b"id,qty\n1,2\n2,3\n").await;
    h.keys("m :").await;
    h.type_("derive dbl = qty * 2").await;
    h.keys("e <C-u>").await;
    h.type_("9").await;
    let typed = view_csv(&h);
    h.keys("m").await;
    h.type_("<F7>").await;
    h.keys("U U").await; // each step its own undo entry
    assert_eq!(view_csv(&h), "id,qty\n1,2\n2,3\n");
    h.keys("<F7>").await;
    assert_eq!(view_csv(&h), typed);
}

#[tokio::test(flavor = "multi_thread")]
async fn macro_refuses_steps_without_ex_form_and_discards_on_empty_key() {
    let mut h = Harness::new(5).await;
    h.keys("m |").await;
    h.type_("name_1").await;
    h.keys("j m <Esc>").await; // Esc keeps recording
    assert_eq!(status(&h), "● rec still recording; m to stop");
    h.keys("m").await;
    h.type_("<F8>").await;
    assert_eq!(
        status(&h),
        "macro on <F8>: 1 steps, not recorded: SelectByRegex; saved"
    );
    h.keys("m j m").await;
    h.type_("").await;
    assert_eq!(status(&h), "macro discarded");
    assert!(h.app.recording.is_none());
}

/// Harness with `lua` loaded before the first sheet's `open` event.
async fn with_lua(rows: usize, lua: &str) -> Harness {
    let mut h = Harness::new(rows).await;
    h.app.plugins = Some(crate::plugins::Plugins::load_from_source(lua).unwrap());
    h.app.keymap = keymap_and_theme(h.app.plugins.as_ref()).0;
    h.app.hooked = None;
    h.pump().await;
    h
}

#[tokio::test(flavor = "multi_thread")]
async fn open_hook_runs_ex_commands_on_the_new_sheet() {
    let h = with_lua(
        30,
        r#"vd.on("open", function(s)
            if s.path:match("%.csv$") and s.format == "csv" then vd.exec(":sort -id") end
        end)"#,
    )
    .await;
    assert_eq!(first_cell(&h), "30", "{}", h.screen());
}

#[tokio::test(flavor = "multi_thread")]
async fn failing_hook_toasts_and_the_rest_still_run() {
    let h = with_lua(
        5,
        r#"vd.on("open", function(s) error("boom") end)
           vd.on("open", function(s) vd.toast("cols " .. #s.column_names) end)"#,
    )
    .await;
    let st = status(&h);
    assert!(st.contains("vd.on open") && st.contains("boom"), "{st}");
    assert!(st.ends_with("cols 4"), "{st}");
    assert_eq!(first_cell(&h), "1");
}

#[tokio::test(flavor = "multi_thread")]
async fn push_hook_sees_the_origin_and_cannot_loop() {
    let mut h = with_lua(
        30,
        r#"vd.on("push", function(s)
            vd.toast(s.origin)
            if s.origin == "freq" then vd.exec("open_freq_table") end
        end)"#,
    )
    .await;
    assert_eq!(status(&h), "file");
    h.keys("l l F").await;
    // the hook's own freq sheet fires nothing: two freq sheets, then quiet
    assert_eq!(h.app.sheets.len(), 3, "{}", h.screen());
    assert!(title(&h).contains("freq(grp): freq(grp)"), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn save_hook_gets_path_and_rows_and_bind_functions_exec() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.csv");
    let mut h = with_lua(
        5,
        r#"vd.on("save", function(s) vd.toast(s.rows .. " " .. s.path) end)
           vd.bind("sheet", "<F9>", function() vd.exec(":sort -id"); vd.exec("goto_bottom") end)"#,
    )
    .await;
    h.keys(":").await;
    h.type_(&format!("save {}", out.display())).await;
    assert_eq!(status(&h), format!("5 {}", out.display()));
    h.keys("<F9>").await;
    assert_eq!(first_cell(&h), "5");
    assert!(title(&h).contains("row 5/5"), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn source_runs_an_ex_script_step_by_step() {
    let dir = tempfile::tempdir().unwrap();
    let other = dir.path().join("other file.csv");
    std::fs::write(&other, "id,name\n1,a\n2,b\n3,b\n").unwrap();
    let script = dir.path().join("s.vdrs");
    std::fs::write(
        &script,
        format!(
            "# a session\n:open \"{}\"\n:sort -id\n:cursor 2 name\nopen_freq_table\n:open {}/gone.csv\n:sort -x\n",
            other.display(),
            dir.path().display()
        ),
    )
    .unwrap();
    let mut h = Harness::new(3).await;
    h.keys(":").await;
    h.type_(&format!("source {}", script.display())).await;
    assert!(
        title(&h).contains("freq(name): ") && title(&h).contains("other file.csv"),
        "{}",
        h.screen()
    );
    assert!(title(&h).contains("sheets 3"), "{}", title(&h));
    assert!(status(&h).contains("skipped (missing)") && status(&h).contains("gone.csv"));
    h.keys("q").await;
    assert_eq!(first_cell(&h), "3");
    assert_eq!(h.current_column(), "name");
}

/// A failed step stops the script: the steps after it would land on the
/// wrong column or sheet.
#[tokio::test(flavor = "multi_thread")]
async fn source_stops_at_a_failed_step() {
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("s.vdrs");
    std::fs::write(&script, ":cursor 1 nope\n:sort -id\n").unwrap();
    let mut h = Harness::new(3).await;
    let first = first_cell(&h);
    h.keys(":").await;
    h.type_(&format!("source {}", script.display())).await;
    assert_eq!(status(&h), "no column: nope; stopped, 1 steps not run");
    assert_eq!(first_cell(&h), first, "the sort didn't run");
    h.keys(":").await;
    h.type_("bogus").await;
    h.keys(":").await;
    h.type_("sort -id").await;
    assert_ne!(
        first_cell(&h),
        first,
        "a typed error doesn't stop later commands"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn open_hook_cannot_open_again() {
    let h = with_lua(
        3,
        r#"vd.on("open", function(s) vd.exec(":open " .. s.path) end)"#,
    )
    .await;
    assert_eq!(h.app.sheets.len(), 1);
    assert!(
        status(&h).contains(":open would open again"),
        "{}",
        status(&h)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn substitute_on_selection_new_column_and_read_only() {
    let mut h = csv(b"id,name\n1,a  \n2,b \n3,c\n").await;
    h.keys("l s j").await; // rows 1 selected, cursor on row 2
    h.keys(":").await;
    h.type_("s/ +$//").await;
    assert_eq!(status(&h), "replaced / +$/ in name on 1 selected rows");
    assert_eq!(view_csv(&h), "id,name\n1,a\n2,b \n3,c\n");
    h.keys("U").await;
    assert_eq!(view_csv(&h), "id,name\n1,a  \n2,b \n3,c\n");
    h.keys("g u :").await;
    h.type_("s/ //g").await; // nothing selected: every row
    assert_eq!(view_csv(&h), "id,name\n1,a\n2,b\n3,c\n");
    h.keys("*").await;
    h.type_("(.)/<\\1>").await;
    assert_eq!(status(&h), "added name_re");
    assert_eq!(view_csv(&h), "id,name,name_re\n1,a,<a>\n2,b,<b>\n3,c,<c>\n");
    h.keys("U").await; // one undo step for derive + replace
    assert_eq!(view_csv(&h), "id,name\n1,a\n2,b\n3,c\n");
    h.keys("s g *").await;
    h.type_("b/B").await;
    assert_eq!(view_csv(&h), "id,name\n1,a\n2,B\n3,c\n");
    // gz*: text columns of the data; `~` on int `id` is a view cast, skipped
    h.keys("h ~ l g z *").await;
    h.type_("[B2]/x").await;
    assert!(
        status(&h).starts_with("replaced in 1 columns"),
        "{}",
        status(&h)
    );
    assert_eq!(view_csv(&h), "id,name\n1,a\n2,x\n3,c\n");
    h.keys("F *").await;
    assert!(status(&h).starts_with("read-only"), "{}", status(&h));
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

#[tokio::test(flavor = "multi_thread")]
async fn selection_stats_follow_selection_and_column() {
    let mut h = csv(b"id,name,amt\n1,a,1.5\n2,b,\n3,a,-4.25\n4,c,1000\n").await;
    assert!(status(&h).starts_with(" hjkl"), "{}", status(&h));
    h.keys("l l s j s j s").await;
    assert_eq!(
        status(&h),
        "sel 3 · sum -2.75 · avg -1.38 · min -4.25 · max 1.5"
    );
    h.keys("h").await; // text column: count, distinct
    assert_eq!(status(&h), "sel 3 · count 3 · distinct 2");
    h.keys("j s").await;
    assert_eq!(status(&h), "sel 4 · count 4 · distinct 3");
    h.keys("g u").await;
    assert!(status(&h).starts_with(" hjkl"), "{}", status(&h));
}

/// S-30 A: `<C-r>` rebuilds a freq sheet from its parent's current view; `U`
/// goes back; `:link` catches up on activation; a closed parent leaves the copy.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_rebuilds_derived_sheets_from_the_parent() {
    let mut h = csv(b"id,region\n1,a\n2,b\n3,a\n4,c\n").await;
    let freq = |h: &Harness| {
        let t = csv_text(h.app.sheets.active(), b',');
        t.lines()
            .skip(1)
            .map(|l| l.split(',').take(2).collect::<Vec<_>>().join("="))
            .collect::<Vec<_>>()
            .join(" ")
    };
    h.keys("l F").await;
    assert_eq!(freq(&h), "a=2 b=1 c=1");
    let base = h
        .app
        .sheets
        .active()
        .name
        .split(": ")
        .nth(1)
        .unwrap()
        .to_string();
    raise(&mut h, &base).await; // freq stays open
    h.keys("g z |").await;
    h.type_("^[ab]$").await;
    raise(&mut h, "freq(").await;
    assert_eq!(freq(&h), "a=2 b=1 c=1", "unlinked: no change by itself");
    h.keys("<C-r>").await;
    assert_eq!(freq(&h), "a=2 b=1");
    h.keys("U").await;
    assert_eq!(freq(&h), "a=2 b=1 c=1");
    h.keys("<C-r> :").await;
    h.type_("link").await;
    assert!(title(&h).contains("⟳"), "{}", title(&h));
    raise(&mut h, &base).await;
    h.keys("g z |").await;
    h.type_("^a$").await;
    raise(&mut h, "freq(").await;
    assert_eq!(freq(&h), "a=2", "linked: caught up on activation");
    raise(&mut h, &base).await;
    h.keys("q").await; // close the parent
    assert!(title(&h).contains("freq("), "{}", title(&h));
    h.keys("<C-r>").await;
    assert_eq!(status(&h), "refresh: parent closed, rebuilt from its copy");
    assert_eq!(freq(&h), "a=2");
    h.keys(":").await;
    h.type_("unlink").await;
    assert!(!title(&h).contains("⟳"), "{}", title(&h));
}

/// A chain refreshes parents first: the freq of a dive follows the base.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_walks_the_lineage_parents_first() {
    let mut h = csv(b"id,region\n1,a\n2,b\n3,a\n4,c\n").await;
    h.keys("g s \"").await; // a copy of all four rows (selected)
    h.keys("l F").await;
    assert!(title(&h).contains("/3 "), "{}", title(&h));
    let base = h.app.sheets.iter().next().unwrap().name.clone();
    raise(&mut h, &base).await;
    h.keys("g z |").await; // filter the base
    h.type_("^a$").await;
    raise(&mut h, "freq(").await;
    assert!(title(&h).contains("freq("), "{}", title(&h));
    h.keys("<C-r>").await;
    assert!(title(&h).contains("/1 "), "{}", title(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn refresh_refuses_a_sheet_with_nothing_to_replay() {
    let mut h = csv(b"id\n1\n").await;
    h.keys("<C-r>").await;
    assert_eq!(status(&h), "refresh: not a derived sheet");
}

/// `S`, then `<CR>` on the first sheet whose name starts with `name`.
async fn raise(h: &mut Harness, name: &str) {
    let row = h
        .app
        .sheets
        .iter()
        .position(|s| s.name.starts_with(name))
        .unwrap();
    h.keys("S").await;
    h.keys(&format!("{}<CR>", "j ".repeat(row))).await;
    assert!(title(h).contains(name), "{}", title(h));
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
    assert!(title(&again).starts_with(" notes: "), "{}", title(&again));
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

#[tokio::test(flavor = "multi_thread")]
async fn notes_on_key_columns_and_refused_on_derived_sheets() {
    let mut h = csv(b"id,name\n1,alpha\n2,beta\n").await;
    h.keys("! l j :").await; // key `id`
    h.type_("note by key").await;
    h.keys(":").await;
    h.type_("notes").await;
    let t = csv_text(h.app.sheets.active(), b'|');
    assert!(t.contains("2|2|name|by key|false|"), "{t}");
    h.keys("q F z n").await;
    assert_eq!(status(&h), "notes: only on sheets read from a file");
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
    assert!(title(&h).contains(&format!("/{want} ")), "{}", title(&h));
    h.keys("q u").await;
    assert!(h.app.sheets.iter().next().unwrap().view.selected.is_empty());
}

/// Foreground of the first screen cell of `text` on the line that holds it.
fn fg_of(h: &Harness, text: &str) -> ratatui::style::Color {
    cell_at(h, text).fg
}

/// The first screen cell of word `text`, on the first data line that holds it.
fn cell_at(h: &Harness, text: &str) -> ratatui::buffer::Cell {
    let screen = h.screen();
    let (y, line) = screen
        .lines()
        .enumerate()
        .skip(1)
        .find(|(_, l)| l.split_whitespace().any(|w| w == text))
        .unwrap_or_else(|| panic!("{text} not on screen:\n{screen}"));
    let x = line
        .match_indices(text)
        .map(|(i, _)| i)
        .find(|&i| {
            (i == 0 || line[..i].ends_with(' '))
                && line[i + text.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| c == ' ')
        })
        .unwrap_or_default();
    h.term.backend().buffer()[(x as u16, y as u16)].clone()
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

/// S-27 C: a renderer's table gives text and style; the style shows under
/// a `:color` rule and never over the cursor row.
#[tokio::test(flavor = "multi_thread")]
async fn lua_renderer_styles_cells_under_color_rules() {
    use ratatui::style::{Color, Modifier};
    let mut h = csv(b"id,amount\n1,-5\n2,7\n3,-2\n").await;
    h.app.plugins = Some(
        crate::plugins::Plugins::load_from_source(
            r#"vd.register_renderer { column = "amount", render = function(v)
                 if v < 0 then return { text = "(" .. -v .. ")", fg = "red", bold = true } end
               end }"#,
        )
        .unwrap(),
    );
    h.pump().await;
    let neg = cell_at(&h, "(2)");
    assert_eq!(neg.fg, Color::Red, "{}", h.screen());
    assert!(neg.modifier.contains(Modifier::BOLD));
    assert_ne!(cell_at(&h, "7").fg, Color::Red);
    assert_ne!(
        cell_at(&h, "(5)").fg,
        Color::Red,
        "the cursor row keeps its colors"
    );
    h.keys(":").await;
    h.type_("color amount id = 3 blue").await;
    let ruled = cell_at(&h, "(2)");
    assert_eq!(ruled.fg, Color::Blue, ":color wins");
    assert!(
        ruled.modifier.contains(Modifier::BOLD),
        "the renderer's bold stays"
    );
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
    assert!(p.mouse());
    assert!(crate::plugins::Plugins::load_from_source(r#"vd.option("mouse", "yes")"#).is_err());
}

/// S-26 A7: big jumps record where they left; `jump-back` / `jump-forward`
/// walk the list (vim's `<C-o>` / `<C-i>`), `:jumps` shows it.
#[tokio::test(flavor = "multi_thread")]
async fn jumplist_walks_back_and_forward() {
    let mut h = Harness::new(30).await;
    let row = |h: &Harness| {
        let t = title(h);
        t.split("row ")
            .nth(1)
            .unwrap()
            .split('/')
            .next()
            .unwrap()
            .to_string()
    };
    h.keys("G").await;
    h.keys(":").await;
    h.type_("cursor 10").await;
    assert_eq!(row(&h), "10");
    for (cmd, want) in [("jump-back", "30"), ("jump-back", "1")] {
        h.keys(":").await;
        h.type_(cmd).await;
        assert_eq!(row(&h), want, "{cmd}");
    }
    h.keys(":").await;
    h.type_("jump-back").await;
    assert_eq!(status(&h), "at the oldest jump");
    for (cmd, want) in [("jump-forward", "30"), ("jump-forward", "10")] {
        h.keys(":").await;
        h.type_(cmd).await;
        assert_eq!(row(&h), want, "{cmd}");
    }
    h.keys(":").await;
    h.type_("jump-forward").await;
    assert_eq!(status(&h), "at the newest jump");
    h.keys(":").await;
    h.type_("jumps").await;
    let t = csv_text(h.app.sheets.active(), b'|');
    let rows: Vec<(bool, &str)> = t
        .lines()
        .skip(1)
        .map(|l| (l.starts_with('>'), l.rsplit('|').nth(1).unwrap()))
        .collect();
    assert_eq!(rows, [(false, "1"), (false, "30"), (true, "10")], "{t}");
    // a new jump after going back drops what was ahead
    h.keys("q :").await;
    h.type_("jump-back").await;
    h.keys("g g").await;
    assert_eq!(h.app.jumps.len(), 2, "{:?}", h.app.jumps);
}

/// S-26 B: ex ranges run a command on rows as if they were selected (the
/// user's selection comes back); `:g` / `:v` pick rows by regex, `:g{}` by
/// SQL; no command selects; `:normal` types keys on each row.
#[tokio::test(flavor = "multi_thread")]
async fn ex_ranges_global_and_normal() {
    let mut h = csv(b"id,name\n1,a1\n2,b2\n3,a3\n4,b4\n5,a5\n").await;
    let ids = |h: &Harness| {
        let t = csv_text(h.app.sheets.active(), b',');
        t.lines()
            .skip(1)
            .map(|l| l.split(',').next().unwrap().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    };
    let sel = |h: &Harness| h.app.sheets.active().view.selected.len();
    h.keys("s").await; // the user's selection: row 1
    h.keys(":").await;
    h.type_("2,3 d").await;
    assert_eq!(ids(&h), "1 4 5", "{}", h.screen());
    assert_eq!(sel(&h), 1, "selection restored");
    h.keys("U").await;
    assert_eq!(ids(&h), "1 2 3 4 5");
    h.keys(":").await;
    h.type_("g/name=^a/d").await;
    assert_eq!(ids(&h), "2 4", "{}", h.screen());
    h.keys("U :").await;
    h.type_("% y").await;
    assert_eq!(h.app.register.as_ref().unwrap().df.height(), 5);
    assert_eq!(sel(&h), 1);
    h.keys(":").await;
    h.type_("v/name=^a/").await; // no command: select
    assert_eq!(status(&h), "selected 2 rows");
    assert_eq!(sel(&h), 3);
    h.keys("g u :").await;
    h.type_("g{id >= 4}").await;
    assert_eq!(sel(&h), 2);
    h.keys("g u l :").await; // the name column
    h.type_("2,4 setcol z").await;
    let t = csv_text(h.app.sheets.active(), b',');
    assert_eq!(t, "id,name\n1,a1\n2,z\n3,z\n4,z\n5,a5\n");
    h.keys(":").await;
    h.type_("4,$ normal s").await;
    assert_eq!(sel(&h), 2, "s typed on rows 4 and 5");
    h.keys(":").await;
    h.type_("2,1000 normal s").await;
    assert!(
        status(&h).starts_with("normal: rows 2–1000 aren't all loaded"),
        "{}",
        status(&h)
    );
}

/// S-26 C: `V` grows a row range with the cursor; a verb runs on it and
/// closes it; `:'<,'>` names it after; `<C-v>` blocks take columns too.
#[tokio::test(flavor = "multi_thread")]
async fn visual_ranges_run_verbs_on_their_rows() {
    let mut h = csv(b"id,name,n\n1,a,1\n2,b,2\n3,c,3\n4,d,4\n5,e,5\n").await;
    let sel = |h: &Harness| h.app.sheets.active().view.selected.len();
    h.keys("j V j j").await; // rows 2–4
    assert!(
        status(&h).starts_with(" -- VISUAL -- rows 2–4"),
        "{}",
        status(&h)
    );
    let screen = h.screen();
    let y = screen.lines().position(|l| l.contains(" 3 c")).unwrap() as u16;
    let rev = |h: &Harness, y| {
        h.term.backend().buffer()[(4, y)]
            .modifier
            .contains(ratatui::style::Modifier::REVERSED)
    };
    assert!(rev(&h, y), "range highlighted:\n{screen}");
    h.keys("s").await;
    assert_eq!(sel(&h), 3);
    assert!(h.app.visual.is_none(), "a verb closes it");
    assert!(!rev(&h, y));
    h.keys("g u :").await;
    h.type_("'<,'> d").await;
    let t = csv_text(h.app.sheets.active(), b',');
    assert_eq!(t, "id,name,n\n1,a,1\n5,e,5\n", "'<,'> = the last range");
    h.keys("U").await;
    h.keys("g g V j <Esc>").await;
    assert!(h.app.visual.is_none() && status(&h) != " -- VISUAL --");
    h.keys("<C-v> l -").await; // block: columns id–name
    let t = csv_text(h.app.sheets.active(), b',');
    assert!(t.starts_with("n\n"), "{t}");
    h.keys(":").await;
    h.type_("reselect").await;
    assert!(h.app.visual.is_some());
}

/// S-26 D: the vim profile's motions and its `u` / `U` swap.
#[tokio::test(flavor = "multi_thread")]
async fn vim_profile_motions_and_undo_keys() {
    let mut h = csv(b"id,a,b,c\n1,x,,z\n1,y,,z\n2,y,q,\n3,,,\n").await;
    h.app.plugins =
        Some(crate::plugins::Plugins::load_from_source(r#"vd.option("keymap", "vim")"#).unwrap());
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    h.keys("w").await;
    assert_eq!(h.current_column(), "a");
    h.keys("w").await;
    assert_eq!(h.current_column(), "c", "w skips the empty b");
    h.keys("b b").await;
    assert_eq!(h.current_column(), "id");
    h.keys("e").await;
    assert_eq!(h.current_column(), "c", "e: the row's last non-empty cell");
    h.keys("0 }").await; // id 1, 1, 2: the next change is row 3
    assert!(title(&h).contains("row 3/4"), "{}", title(&h));
    h.keys("{").await;
    assert!(title(&h).contains("row 2/4"), "{}", title(&h));
    h.keys("L").await;
    assert!(title(&h).contains("row 4/4"), "{}", title(&h));
    h.keys("H").await;
    assert!(title(&h).contains("row 1/4"), "{}", title(&h));
    h.keys("l j *").await; // search a = y
    assert!(title(&h).contains("row 3/4"), "{}", title(&h));
    h.keys("$").await;
    assert_eq!(h.current_column(), "c");
    h.keys("s").await;
    assert_eq!(h.app.sheets.active().view.selected.len(), 1);
    h.keys("U").await;
    assert!(
        h.app.sheets.active().view.selected.is_empty(),
        "U unselects"
    );
    h.keys("s j u").await; // unselect-row would act on the row below
    assert_eq!(
        h.app.sheets.active().view.selected.len(),
        0,
        "u undid the s"
    );
}

/// S-26 D2: in the vim profile `"a` names the register of the next yank or
/// paste; yanks also fill `0`, cuts the `1`–`9` ring; `:registers` lists them.
#[tokio::test(flavor = "multi_thread")]
async fn vim_registers_named_yank_ring_and_paste() {
    let mut h = csv(b"id\n1\n2\n3\n").await;
    h.app.plugins =
        Some(crate::plugins::Plugins::load_from_source(r#"vd.option("keymap", "vim")"#).unwrap());
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    let ids = |h: &Harness| csv_text(h.app.sheets.active(), b',').replace('\n', " ");
    h.keys("\" a y y").await; // row 1 into "a
    h.keys("j y y").await; // row 2 into "" and "0
    let reg = |h: &Harness, c: char| {
        h.app
            .registers
            .get(&c)
            .map(|r| crate::sheet::any_text(&r.df.columns()[0].get(0).unwrap()))
    };
    assert_eq!(reg(&h, 'a').as_deref(), Some("1"));
    assert_eq!(reg(&h, '0').as_deref(), Some("2"));
    h.keys("\" a p").await;
    assert_eq!(ids(&h), "id 1 2 1 3 ", "\"ap pastes row 1 after row 2");
    h.keys("p").await;
    assert_eq!(
        ids(&h),
        "id 1 2 1 2 3 ",
        "p: the unnamed register, still row 2"
    );
    h.keys(":").await;
    h.type_("cut-row").await;
    assert_eq!(reg(&h, '1').as_deref(), Some("2"), "a cut goes to \"1");
    h.keys("\" q p").await;
    assert_eq!(status(&h), "register \"q is empty");
    h.keys(":").await;
    h.type_("registers").await;
    let df = h.app.sheets.active().window().unwrap().clone();
    let names: Vec<String> = df
        .column("register")
        .unwrap()
        .str()
        .unwrap()
        .iter()
        .map(|v| v.unwrap_or_default().to_string())
        .collect();
    assert_eq!(names, ["\"\"", "\"0", "\"1", "\"a"]);
}

/// S-26 E: in the vim profile `Esc` in a prompt is normal mode, `Esc` again
/// cancels, `Enter` submits from it.
#[tokio::test(flavor = "multi_thread")]
async fn vim_prompt_normal_mode() {
    let mut h = csv(b"id,v\n1,a\n").await;
    h.app.plugins =
        Some(crate::plugins::Plugins::load_from_source(r#"vd.option("keymap", "vim")"#).unwrap());
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    let text = |h: &Harness| csv_text(h.app.sheets.active(), b',').replace('\n', " ");
    h.keys("l i").await;
    h.type_no_enter("bc d").await;
    h.keys("<Esc>").await;
    assert!(status(&h).contains("-- NORMAL --"), "{}", status(&h));
    h.keys("b x").await;
    assert!(
        h.app.input.is_some(),
        "normal-mode keys don't leave the prompt"
    );
    h.keys("<CR>").await;
    assert_eq!(text(&h), "id,v 1,bc d ");
    h.keys("i").await;
    h.type_no_enter("zz").await;
    h.keys("<Esc> <Esc>").await;
    assert!(h.app.input.is_none());
    assert_eq!(text(&h), "id,v 1,bc d ", "Esc Esc cancels");
}

/// S-26 D4: vim marks (`ma` / `'a`, across sheets) and register macros
/// (`qa … q`, `@a`, `@@`); `q` no longer pops.
#[tokio::test(flavor = "multi_thread")]
async fn vim_marks_and_register_macros() {
    let mut h = csv(b"id\n1\n2\n3\n4\n5\n6\n").await;
    h.app.plugins =
        Some(crate::plugins::Plugins::load_from_source(r#"vd.option("keymap", "vim")"#).unwrap());
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    let row = |h: &Harness| h.app.sheets.active().view.row_cursor;
    h.keys("j j m a G").await;
    assert_eq!(row(&h), 5);
    h.keys("' a").await;
    assert_eq!(row(&h), 2, "'a goes back to the mark");
    h.keys("' z").await;
    assert_eq!(status(&h), "no mark z");
    h.keys("g g q b j q").await;
    assert_eq!(row(&h), 1);
    assert_eq!(status(&h), "macro @b: 1 steps");
    assert_eq!(h.app.sheets.len(), 1, "q stops recording, doesn't pop");
    h.keys("@ b").await;
    assert_eq!(row(&h), 2);
    h.keys("@ @").await;
    assert_eq!(row(&h), 3, "@@ replays the last macro");
    h.keys("@ c").await;
    assert_eq!(status(&h), "register c holds no macro");
    // a ranged step replays on the cursor row, not on the selection
    h.keys("g g q d d d q").await;
    assert_eq!(h.app.macros[&'d'], [":.,.+0 cut-row"]);
    h.keys("s j s").await;
    h.keys("@ d").await;
    let ids = csv_text(h.app.sheets.active(), b',').replace('\n', " ");
    assert_eq!(ids, "id 2 4 5 6 ", "@d cuts the cursor row only");
    assert_eq!(h.app.sheets.active().view.selected.len(), 1);
}

/// S-26 B: a typed ranged command is what `.` (`:repeat-last`) repeats.
#[tokio::test(flavor = "multi_thread")]
async fn dot_repeats_a_typed_range() {
    let mut h = csv(b"id\n1\n2\n3\n4\n").await;
    h.keys(":").await;
    h.type_(".,.+1 d").await;
    h.keys(":").await;
    h.type_("repeat-last").await;
    assert_eq!(csv_text(h.app.sheets.active(), b','), "id\n");
}

/// S-26 D3: vim operators — `dd`, counts (`2dj` = 3 rows), `dG`, `.`
/// repeats, `yic` copies the column's cells, `ciw`-style `cir` sets a row,
/// `div` takes the run of equal values; a non-motion cancels.
#[tokio::test(flavor = "multi_thread")]
async fn vim_operators_motions_and_text_objects() {
    let mut h = csv(b"id,g\n1,a\n2,a\n3,b\n4,b\n5,b\n6,c\n7,c\n8,d\n").await;
    h.app.plugins =
        Some(crate::plugins::Plugins::load_from_source(r#"vd.option("keymap", "vim")"#).unwrap());
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    let ids = |h: &Harness| {
        let t = csv_text(h.app.sheets.active(), b',');
        t.lines()
            .skip(1)
            .map(|l| l.split(',').next().unwrap().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    };
    h.keys("y i c").await;
    let r = h.app.register.clone().unwrap();
    assert_eq!(
        (r.kind, r.df.height()),
        (RegisterKind::Cells, 8),
        "yic: the column"
    );
    h.keys("d d").await;
    assert_eq!(ids(&h), "2 3 4 5 6 7 8", "{}", h.screen());
    assert_eq!(h.app.register.as_ref().unwrap().df.height(), 1, "dd cuts");
    h.keys(".").await;
    assert_eq!(ids(&h), "3 4 5 6 7 8", ". repeats dd");
    h.keys("2 d j").await;
    assert_eq!(ids(&h), "6 7 8", "2dj: three rows");
    h.keys("j d G").await;
    assert_eq!(ids(&h), "6", "dG: to the end");
    h.keys("u u u u").await; // vim profile: u undoes
    assert_eq!(ids(&h), "1 2 3 4 5 6 7 8");
    h.keys("j j l d i v").await; // g = b on rows 3–5
    let t = csv_text(h.app.sheets.active(), b',');
    assert!(t.contains("3,\n4,\n5,\n"), "div empties the run: {t}");
    h.keys("c i r").await;
    assert_eq!(h.app.input.as_ref().unwrap().1.text(), "'<,'> setcol ");
    h.type_("z").await;
    assert!(
        csv_text(h.app.sheets.active(), b',').contains("3,z\n4,\n"),
        "{}",
        h.screen()
    );
    h.keys("d x").await;
    assert!(status(&h).starts_with("d needs a motion"), "{}", status(&h));
    h.keys("d <Esc> j").await;
    assert!(h.app.operator.is_none());
}
