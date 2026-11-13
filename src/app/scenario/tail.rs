//! `:tail`: following files as they grow.

use super::*;

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
