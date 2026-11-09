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
        while !self.app.settled() {
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
        h.screen().contains("unselect-all unselect-row"),
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
    h.type_no_enter("hel").await;
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
    h.keys(".").await;
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
