use super::*;
use crate::reader::{self, SourceKind};

fn sheet_of(csv: &str) -> (Sheet, tempfile::NamedTempFile) {
    let t = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    std::fs::write(t.path(), csv).unwrap();
    let loaded = reader::open(t.path().as_os_str(), None).unwrap();
    (Sheet::from_source(0, loaded), t)
}
use std::io::Write;

fn make_sheet() -> (Sheet, tempfile::NamedTempFile) {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(tmp, "name,n\nalice,3\nbob,1\ncarol,2").unwrap();
    tmp.flush().unwrap();
    let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
    (Sheet::from_source(0, loaded), tmp)
}

#[test]
fn window_plan_hide_key_reorders() {
    let (mut s, _tmp) = make_sheet();
    s.view.keys.push("n".into());
    s.view.hidden.insert("name".into());
    let df = s.window_plan().collect().unwrap();
    let names: Vec<&str> = df
        .get_column_names()
        .into_iter()
        .map(|s| s.as_ref())
        .collect();
    assert_eq!(names, vec!["n"]);
}

#[test]
fn window_plan_type_cast_int() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(tmp, "n\n42\n17\n5").unwrap();
    tmp.flush().unwrap();
    let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
    let mut s = Sheet::from_source(0, loaded);
    // Even though CSV already inferred Int64, type-cast to Int64 should be a no-op pass.
    s.view.type_casts.push(("n".into(), DataType::Int64));
    let df = s.window_plan().collect().unwrap();
    assert_eq!(df.column("n").unwrap().dtype(), &DataType::Int64);
}

#[test]
fn window_plan_sort_asc() {
    let (mut s, _tmp) = make_sheet();
    s.view.sort.push(SortKey {
        column: "n".into(),
        order: SortOrder::Asc,
    });
    let df = s.window_plan().collect().unwrap();
    let col = df.column("n").unwrap();
    let got: Vec<i64> = (0..col.len())
        .map(|i| col.get(i).unwrap().try_extract::<i64>().unwrap())
        .collect();
    assert_eq!(got, vec![1, 2, 3]);
}

#[test]
fn window_plan_filter_regex() {
    let (mut s, _tmp) = make_sheet();
    s.view.filter = Some(FilterState {
        column: "name".into(),
        regex: "^(alice|bob)$".into(),
    });
    let df = s.window_plan().collect().unwrap();
    assert_eq!(df.height(), 2);
}

#[test]
fn freq_sheet_counts_and_sorts_desc() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(tmp, "color\nred\nblue\nred\ngreen\nred\nblue").unwrap();
    tmp.flush().unwrap();
    let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
    let parent = Sheet::from_source(0, loaded);
    let freq = Sheet::freq_from(&parent, 1, "color");
    let df = freq.plan.clone().collect().unwrap();
    assert_eq!(df.width(), 4); // color + count + percent + histogram
    assert_eq!(df.height(), 3); // red/blue/green
    let counts = df.column("count").unwrap();
    let first: i64 = counts.get(0).unwrap().try_extract().unwrap();
    assert_eq!(first, 3); // red leads
    let pct = df.column("percent").unwrap();
    let first_pct: f64 = pct.get(0).unwrap().try_extract().unwrap();
    assert!((first_pct - 50.0).abs() < 0.01); // 3/6 = 50%
    // bars scale by count, rounding down: red 3 → 38, blue 2 → 25, green 1 → 12
    let bars: Vec<usize> = df
        .column("histogram")
        .unwrap()
        .str()
        .unwrap()
        .iter()
        .map(|b| b.unwrap().chars().count())
        .collect();
    assert_eq!(bars, [38, 25, 12]);
}

#[test]
fn pivot_sum_shape() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        tmp,
        "region,product,qty\nwest,a,1\nwest,b,2\neast,a,3\neast,b,4\nwest,a,5"
    )
    .unwrap();
    tmp.flush().unwrap();
    let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
    let parent = Sheet::from_source(0, loaded);
    let piv = Sheet::pivot_from(&parent, 1, "region", "product", "qty", "sum").unwrap();
    let df = piv.plan.clone().collect().unwrap();
    // columns: region, a, b
    assert_eq!(df.width(), 3);
    assert_eq!(df.height(), 2); // east + west
}

#[test]
fn unpivot_longform() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(tmp, "id,a,b\n1,10,20\n2,30,40").unwrap();
    tmp.flush().unwrap();
    let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
    let parent = Sheet::from_source(0, loaded);
    let on = vec!["a".to_string(), "b".to_string()];
    let index = vec!["id".to_string()];
    let s = Sheet::unpivot_from(&parent, 1, &on, &index);
    let df = s.plan.clone().collect().unwrap();
    assert_eq!(df.height(), 4); // 2 rows × 2 unpivoted columns
}

#[test]
fn schema_diff_catches_missing_and_mismatched() {
    let mut ta = tempfile::NamedTempFile::new().unwrap();
    let mut tb = tempfile::NamedTempFile::new().unwrap();
    writeln!(ta, "id,name,age\n1,alice,30").unwrap();
    writeln!(tb, "id,name,salary\n1,alice,100.5").unwrap();
    ta.flush().unwrap();
    tb.flush().unwrap();
    let la = reader::open(ta.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
    let lb = reader::open(tb.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
    let parent = Sheet::from_source(0, la);
    let d = Sheet::schema_diff_from(&parent, 1, &lb).unwrap();
    let df = d.plan.clone().collect().unwrap();
    // 4 columns: id, name, age, salary
    assert_eq!(df.height(), 4);
    let statuses = df.column("status").unwrap();
    let v: Vec<String> = (0..statuses.len())
        .map(|i| match statuses.get(i).unwrap() {
            AnyValue::String(s) => s.to_string(),
            AnyValue::StringOwned(s) => s.as_str().to_string(),
            _ => String::new(),
        })
        .collect();
    assert!(v.contains(&"only_a".to_string())); // age
    assert!(v.contains(&"only_b".to_string())); // salary
    assert!(v.contains(&"same".to_string())); // id, name
}

#[test]
fn row_diff_flags_only_a_only_b_changed() {
    let mut ta = tempfile::NamedTempFile::new().unwrap();
    let mut tb = tempfile::NamedTempFile::new().unwrap();
    // A: 1=alice,30 ; 2=bob,25 ; 3=carol,40
    // B: 1=alice,30 ; 2=bob,99 ; 4=dave,10
    writeln!(ta, "id,name,age\n1,alice,30\n2,bob,25\n3,carol,40").unwrap();
    writeln!(tb, "id,name,age\n1,alice,30\n2,bob,99\n4,dave,10").unwrap();
    ta.flush().unwrap();
    tb.flush().unwrap();
    let la = reader::open(ta.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
    let lb = reader::open(tb.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
    let parent = Sheet::from_source(0, la);
    let d =
        Sheet::row_diff_from(&parent, 1, &Sheet::from_source(2, lb), &["id".to_string()]).unwrap();
    let df = d.plan.clone().collect().unwrap();
    // Expect 3 rows (1 unchanged suppressed): id=2 changed, id=3 only_a, id=4 only_b
    assert_eq!(df.height(), 3);
}

#[test]
fn tail_refresh_picks_up_appended_rows() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(tmp, r#"{{"a":1}}"#).unwrap();
    writeln!(tmp, r#"{{"a":2}}"#).unwrap();
    tmp.flush().unwrap();
    // Re-open to force the JSONL kind
    let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Jsonl)).unwrap();
    let mut s = Sheet::tail_from(42, loaded, false).unwrap();
    let before = s.plan.clone().collect().unwrap();
    assert_eq!(before.height(), 2);
    // append more rows
    use std::io::Write as _;
    let mut handle = std::fs::OpenOptions::new()
        .append(true)
        .open(tmp.path())
        .unwrap();
    writeln!(handle, r#"{{"a":3}}"#).unwrap();
    writeln!(handle, r#"{{"a":4}}"#).unwrap();
    handle.flush().unwrap();
    drop(handle);
    assert!(s.refresh_tail().unwrap());
    let after = s.plan.clone().collect().unwrap();
    assert_eq!(after.height(), 4);
    assert!(!s.refresh_tail().unwrap(), "unchanged file → no reload");
    // an unparsable partial line waits; truncation starts over
    let mut handle = std::fs::OpenOptions::new()
        .append(true)
        .open(tmp.path())
        .unwrap();
    write!(handle, r#"{{"a":5"#).unwrap();
    drop(handle);
    s.refresh_tail().unwrap();
    assert_eq!(s.plan.clone().collect().unwrap().height(), 4);
    std::fs::write(tmp.path(), "{\"a\":9}\n").unwrap();
    assert!(s.refresh_tail().unwrap());
    assert_eq!(s.plan.clone().collect().unwrap().height(), 1);
}

#[test]
fn write_df_roundtrip_csv_parquet_tsv() {
    let mut df = df!(
        "a" => [1i64, 2, 3],
        "b" => ["x", "y", "z"],
    )
    .unwrap();
    for (ext, want_sep) in [
        ("csv", b','),
        ("tsv", b'\t'),
        ("parquet", 0u8),
        ("json", 0),
        ("jsonl", 0),
    ] {
        let path = tempfile::NamedTempFile::new()
            .unwrap()
            .path()
            .with_extension(ext);
        write_df_by_extension(&mut df, &path).unwrap();
        let loaded = reader::open(path.as_os_str(), None).unwrap();
        let back = loaded.lazy.collect().unwrap();
        assert_eq!(back.height(), 3, "ext={ext}");
        assert_eq!(back.width(), 2, "ext={ext}");
        // Separator sanity for text formats
        if want_sep != 0 {
            let bytes = std::fs::read(&path).unwrap();
            assert!(bytes.contains(&want_sep), "ext={ext} missing separator");
        }
        let _ = std::fs::remove_file(&path);
    }
}

#[test]
fn write_df_unknown_extension_errors() {
    let mut df = df!("a" => [1i64]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("existing.xyz");
    std::fs::write(&path, b"keep me").unwrap();
    let err = write_df_by_extension(&mut df, &path).unwrap_err();
    assert!(err.to_string().contains("unknown extension"));
    assert_eq!(std::fs::read(&path).unwrap(), b"keep me"); // not truncated
}

#[test]
fn window_plan_filter_non_string_column() {
    let (mut s, _tmp) = make_sheet();
    s.view.filter = Some(FilterState {
        column: "n".into(),
        regex: "^3$".into(),
    });
    assert_eq!(s.window_plan().collect().unwrap().height(), 1);
}

#[test]
fn derived_sheets_follow_view() {
    let (mut s, _tmp) = make_sheet(); // alice,3 bob,1 carol,2
    s.view.sort.push(SortKey {
        column: "n".into(),
        order: SortOrder::Asc,
    });
    Arc::make_mut(&mut s.view.selected).insert(1); // row id 1 = bob (base order), first in the sorted view
    let dup = Sheet::dup_selected_from(&s, 1).unwrap();
    let df = dup.plan.collect().unwrap();
    let name = df.column("name").unwrap().get(0).unwrap();
    assert_eq!(name.get_str(), Some("bob"));

    s.view.filter = Some(FilterState {
        column: "name".into(),
        regex: "^a".into(),
    });
    let freq = Sheet::freq_from(&s, 2, "name").plan.collect().unwrap();
    assert_eq!(freq.height(), 1); // only alice survives the filter
    // freq of a freq's `count` column: stat columns step aside
    let ff = Sheet::freq_from(&Sheet::freq_from(&s, 3, "name"), 4, "count");
    let names = ff.plan.collect().unwrap().get_column_names_owned();
    assert_eq!(
        names,
        ["count", "freq_count", "freq_percent", "freq_histogram"]
    );
}

#[test]
fn count_plan_counts_filtered_rows() {
    let (mut s, _tmp) = make_sheet();
    let count = |s: &Sheet| {
        let df = s.count_plan().collect().unwrap();
        df.columns()[0]
            .get(0)
            .unwrap()
            .try_extract::<u64>()
            .unwrap()
    };
    assert_eq!(count(&s), 3);
    let before = s.count_key();
    s.view.filter = Some(FilterState {
        column: "name".into(),
        regex: "^(alice|bob)$".into(),
    });
    assert_ne!(s.count_key(), before);
    assert_eq!(count(&s), 2);
}

#[test]
fn search_hits_scan_full_view_in_view_order() {
    let (mut s, _tmp) = make_sheet(); // alice,3 bob,1 carol,2
    s.view.window_rows = 1; // search must not be limited to the window
    s.view.sort.push(SortKey {
        column: "n".into(),
        order: SortOrder::Asc,
    });
    // sorted: bob,1 carol,2 alice,3 ; "AR" hits carol (name) only, "3" hits alice (n)
    assert_eq!(s.search_hits("AR").unwrap(), vec![(1, 0)]);
    assert_eq!(s.search_hits("3").unwrap(), vec![(2, 1)]);
    assert!(s.search_hits("zzz").unwrap().is_empty());
}

#[test]
fn lenient_date_cast_reads_common_layouts() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(
        tmp,
        "d\n7/3/2018 1:47p\n2024-01-02\n03.04.2020\n\"Jan 5, 2021\"\n7/3/18\nnope"
    )
    .unwrap();
    tmp.flush().unwrap();
    let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Csv)).unwrap();
    let mut s = Sheet::from_source(0, loaded);
    s.view.type_casts.push(("d".into(), DataType::Date));
    let df = s.view_plan().collect().unwrap();
    let got: Vec<Option<String>> = df
        .column("d")
        .unwrap()
        .cast(&DataType::String)
        .unwrap()
        .str()
        .unwrap()
        .iter()
        .map(|v| v.map(String::from))
        .collect();
    let want = [
        "2018-07-03",
        "2024-01-02",
        "2020-04-03",
        "2021-01-05",
        "2018-07-03",
    ];
    let want: Vec<Option<String>> = want
        .iter()
        .map(|d| Some(d.to_string()))
        .chain([None])
        .collect();
    assert_eq!(got, want);
}

#[test]
fn view_plan_is_unsliced() {
    let (mut s, _tmp) = make_sheet();
    s.view.window_rows = 1;
    assert_eq!(s.window_plan().collect().unwrap().height(), 1);
    assert_eq!(s.view_plan().collect().unwrap().height(), 3);
}

#[test]
fn sheets_stack_push_pop() {
    let (parent, _tmp) = make_sheet();
    let mut sheets = Sheets::new(parent);
    assert_eq!(sheets.len(), 1);
    assert!(!sheets.pop()); // can't pop last
    let id = sheets.alloc_id();
    let child = Sheet::freq_from(sheets.active(), id, "n");
    sheets.push(child);
    assert_eq!(sheets.len(), 2);
    assert!(sheets.pop());
    assert_eq!(sheets.len(), 1);
}

#[test]
fn failed_save_keeps_the_existing_file() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("keep.csv");
    std::fs::write(&p, "precious\n").unwrap();
    let list = Series::new("l".into(), [1i32, 2]).implode().unwrap();
    let mut df = DataFrame::new_infer_height(vec![list.into_series().into()]).unwrap();
    assert!(write_df_by_extension(&mut df, &p).is_err());
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "precious\n");
    assert_eq!(
        std::fs::read_dir(dir.path()).unwrap().count(),
        1,
        "no temp left"
    );
    let mut ok = df!("a" => [1, 2]).unwrap();
    write_df_by_extension(&mut ok, &p).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "a\n1\n2\n");
}

#[test]
fn save_keeps_mode_and_refuses_read_only_files() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("t.csv");
    let mode = |m| std::fs::Permissions::from_mode(m);
    std::fs::write(&p, "old\n").unwrap();
    std::fs::set_permissions(&p, mode(0o664)).unwrap();
    let mut df = df!("a" => [1]).unwrap();
    write_df_by_extension(&mut df, &p).unwrap();
    let got = std::fs::metadata(&p).unwrap().permissions().mode();
    assert_eq!(got & 0o777, 0o664);
    std::fs::set_permissions(&p, mode(0o444)).unwrap();
    if !std::fs::OpenOptions::new().write(true).open(&p).is_ok() {
        assert!(write_df_by_extension(&mut df, &p).is_err());
    } // root ignores modes
}

#[test]
fn failed_save_to_a_new_path_leaves_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("new.csv");
    let list = Series::new("l".into(), [1i32]).implode().unwrap();
    let mut df = DataFrame::new_infer_height(vec![list.into_series().into()]).unwrap();
    assert!(write_df_by_extension(&mut df, &p).is_err());
    assert!(!p.exists());
}

#[test]
fn w_ignores_hidden_key_columns() {
    let (mut s, _t) = sheet_of("a,b\nx,1\ny,2\n");
    s.view.hidden.insert("a".into());
    let keys = vec!["a".to_string()];
    let w = Sheet::pivot_current_from(&s, 1, &keys, "b").unwrap();
    assert_eq!(w.plan.clone().collect().unwrap().height(), 1);
}

#[test]
fn failed_save_through_a_dangling_symlink_keeps_the_link() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("out.csv");
    std::os::unix::fs::symlink(dir.path().join("later.csv"), &link).unwrap();
    let list = Series::new("l".into(), [1i32]).implode().unwrap();
    let mut df = DataFrame::new_infer_height(vec![list.into_series().into()]).unwrap();
    assert!(write_df_by_extension(&mut df, &link).is_err());
    assert!(std::fs::symlink_metadata(&link).is_ok());
    assert!(!dir.path().join("later.csv").exists());
}
