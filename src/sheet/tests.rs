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
    assert_eq!(s.search_hits("AR", None).unwrap(), vec![(1, 0)]);
    assert_eq!(s.search_hits("3", None).unwrap(), vec![(2, 1)]);
    assert!(s.search_hits("zzz", None).unwrap().is_empty());
    // one column, regex (VisiData `/`)
    assert!(s.search_hits("3", Some("name")).unwrap().is_empty());
    assert_eq!(
        s.search_hits("^(b|c)", Some("name")).unwrap(),
        vec![(0, 0), (1, 0)]
    );
}

#[test]
fn expr_hits_and_rids_where_take_boolean_sql() {
    let (mut s, _tmp) = make_sheet(); // alice,3 bob,1 carol,2
    s.view.sort.push(SortKey {
        column: "n".into(),
        order: SortOrder::Asc,
    });
    s.view.hidden.insert("n".into()); // predicates may name hidden columns
    // view: bob,1 carol,2 alice,3
    assert_eq!(s.expr_hits("n >= 2").unwrap(), vec![1, 2]);
    // rids are base positions: alice 0, carol 2
    let mut rids = s.rids_where("n >= 2").unwrap();
    rids.sort();
    assert_eq!(rids, vec![0, 2]);
    assert_eq!(s.rids_where("name = 'bob'").unwrap(), vec![1]);
    let e = s.rids_where("n + 1").unwrap_err();
    assert!(e.contains("not a boolean"), "{e}");
    assert!(s.rids_where("nope > 1").unwrap_err().starts_with("expr:"));
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

#[test]
fn edit_value_fits_display_and_base_types() {
    let (mut s, _tmp) = sheet_of("i,f,b,t,d\n1,1.5,true,x,2024-01-31\n");
    s.view.type_casts.push(("d".into(), DataType::Date)); // `@` on text
    s.view.type_casts.push(("t".into(), DataType::Float64)); // `%` on text
    let ok = |c: &str, v: &str| s.edit_value(c, v).map(|(v, _)| v);
    for (c, v) in [
        ("i", "7"),
        ("i", "-3"),
        ("f", "2"),
        ("f", "1e3"),
        ("b", "false"),
    ] {
        assert_eq!(ok(c, v), Ok(Some(v.into())), "{c}={v}");
    }
    assert_eq!(ok("t", "2.5"), Ok(Some("2.5".into())), "stored as text");
    assert_eq!(ok("d", "2024-02-29"), Ok(Some("2024-02-29".into())));
    assert_eq!(ok("i", ""), Ok(None), "empty = null");
    assert_eq!(ok("i", "1.5"), Err(r#""1.5" doesn't fit i (i64)"#.into()));
    assert_eq!(ok("f", "abc"), Err(r#""abc" doesn't fit f (f64)"#.into()));
    assert_eq!(ok("t", "abc"), Err(r#""abc" doesn't fit t (f64)"#.into()));
    assert_eq!(
        ok("d", "nope"),
        Err(r#""nope" doesn't fit d (date)"#.into())
    );
    assert_eq!(ok("b", "TRUE"), Ok(Some("true".into())));
    assert!(ok("b", "yes").unwrap_err().contains("bool"));
    assert!(ok("zz", "1").unwrap_err().starts_with("no column"));
    s.edit_cell("b", 0, "false").unwrap();
    let df = s.view_plan().collect().unwrap();
    assert_eq!(df.column("b").unwrap().bool().unwrap().get(0), Some(false));
}

#[test]
fn tail_keeps_edits_as_the_file_grows() {
    let mut tmp = tempfile::NamedTempFile::new().unwrap();
    writeln!(tmp, r#"{{"a":1}}"#).unwrap();
    tmp.flush().unwrap();
    let loaded = reader::open(tmp.path().as_os_str(), Some(SourceKind::Jsonl)).unwrap();
    let mut s = Sheet::tail_from(1, loaded, false).unwrap();
    s.edit_cell("a", 0, "9").unwrap();
    writeln!(tmp, r#"{{"a":2}}"#).unwrap();
    tmp.flush().unwrap();
    assert!(s.refresh_tail().unwrap());
    let a = s.plan.clone().collect().unwrap();
    let a: Vec<_> = a.column("a").unwrap().i64().unwrap().iter().collect();
    assert_eq!(a, [Some(9), Some(2)]);
}

/// Edits are plan steps and sort/filter are view state, so interleaving
/// them in any order gives the same rows (seeded loop, S-18 §4 C).
#[test]
fn edits_and_view_changes_commute() {
    let csv: String = std::iter::once("id,v\n".to_string())
        .chain((0..40).map(|i| format!("{i},{}\n", (i * 7) % 11)))
        .collect();
    let mut seed = 0x2545_f491_u64;
    let mut next = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    for _ in 0..20 {
        let edits: Vec<(u64, String)> = (0..5).map(|_| (next(40), next(100).to_string())).collect();
        let order = if next(2) == 1 {
            SortOrder::Desc
        } else {
            SortOrder::Asc
        };
        let sort = vec![
            SortKey {
                column: "v".into(),
                order,
            },
            SortKey {
                column: "id".into(),
                order: SortOrder::Asc,
            },
        ];
        let filter = format!("{}", next(10));
        let (mut a, _ta) = sheet_of(&csv);
        let (mut b, _tb) = sheet_of(&csv);
        for (rid, v) in &edits {
            a.edit_cell("v", *rid, v).unwrap();
        }
        a.view.sort = sort.clone();
        a.view.filter = Some(FilterState {
            column: "v".into(),
            regex: filter.clone(),
        });
        b.view.sort = sort;
        b.view.filter = Some(FilterState {
            column: "v".into(),
            regex: filter,
        });
        for (rid, v) in &edits {
            b.edit_cell("v", *rid, v).unwrap();
        }
        let rows = |s: &Sheet| s.view_plan().collect().unwrap();
        assert!(rows(&a).equals_missing(&rows(&b)), "{edits:?}");
    }
}

#[test]
fn describe_counts_failed_casts_as_errors() {
    let df = df!("n" => [Some("3"), Some("1"), Some("x"), None, Some("3")], "s" => ["a", "b", "a", "b", "c"]).unwrap();
    let mut parent = Sheet::from_source(
        0,
        LoadedSource::from_df(SourceKind::Csv, "t".into(), None, df),
    );
    parent.set_type_cast("n".into(), DataType::Int32);
    let d = Sheet::describe_from(&parent, 1).unwrap();
    let out = d.view_plan().collect().unwrap();
    let get = |c: &str| out.column(c).unwrap().get(0).unwrap().to_string();
    assert_eq!(
        ["errors", "nulls", "distinct", "mode", "sum", "median"].map(get),
        ["1", "1", "3", "\"3\"", "\"7\"", "\"3\""]
    );
    assert_eq!(
        out.get_column_names(),
        [
            "column", "errors", "nulls", "distinct", "mode", "min", "max", "sum", "median", "mean",
            "stdev"
        ]
    );
}

#[test]
fn describe_handles_every_column_kind() {
    let df = df!(
        "b" => [true, false, true],
        "t" => [1i64, 2, 3],
        "l" => [Series::new("".into(), [1i64]), Series::new("".into(), [2i64]), Series::new("".into(), [1i64])],
    )
    .unwrap()
    .lazy()
    .with_columns([
        col("t").cast(DataType::Decimal(10, 2)).alias("dec"),
        (col("t") * lit(1_000_000_000i64)).cast(DataType::Time).alias("time"),
        col("t").cast(DataType::Duration(TimeUnit::Milliseconds)).alias("dur"),
        col("t").cast(DataType::String).cast(DataType::from_categories(Categories::global())).alias("cat"),
    ])
    .collect()
    .unwrap();
    let parent = Sheet::from_source(
        0,
        LoadedSource::from_df(SourceKind::Csv, "t".into(), None, df),
    );
    let d = Sheet::describe_from(&parent, 1).unwrap();
    let out = d.view_plan().collect().unwrap();
    assert_eq!(out.height(), 7, "{out}");
}

fn ints(s: &Sheet, c: &str) -> Vec<Option<i64>> {
    let df = s.view_plan().collect().unwrap();
    df.column(c).unwrap().i64().unwrap().iter().collect()
}

#[test]
fn set_expr_writes_selected_rows_only_and_checks_types() {
    let (mut s, _t) = sheet_of("id,v\n1,10\n2,\n3,30\n");
    assert_eq!(s.set_expr("v", "id * 100"), Err("no rows selected".into()));
    Arc::make_mut(&mut s.view.selected).extend([0u64, 2]);
    assert_eq!(s.set_expr("v", "id * 100"), Ok(2));
    assert_eq!(ints(&s, "v"), [Some(100), None, Some(300)]); // row 1 keeps its null
    assert!(s.edits().len() == 1 && s.unsaved_edits() == 1);
    let e = s.set_expr("v", "'x' || id").unwrap_err();
    assert_eq!(e, "expr: str doesn't fit v (i64)");
    assert!(
        s.set_expr("v", "nope + 1")
            .unwrap_err()
            .starts_with("expr:")
    );
    assert_eq!(s.edits().len(), 1, "a refused expression adds no step");
    // Datetime of another unit (and Date ↔ Datetime) fits; text into a date doesn't.
    let (mut d, _td) = sheet_of("t\n2024-01-02T03:04:05\n");
    Arc::make_mut(&mut d.view.selected).insert(0);
    d.set_expr("t", "t + INTERVAL '1 day'").unwrap();
    d.set_expr("t", "CAST(t AS DATE)").unwrap();
    assert!(d.set_expr("t", "'x'").unwrap_err().contains("doesn't fit"));
    assert!(s.time_travel(true));
    assert_eq!(ints(&s, "v"), [Some(10), None, Some(30)]);
}

#[test]
fn paste_cells_cycles_over_selected_rows_in_view_order() {
    let (mut s, _t) = sheet_of("id,v\n1,10\n2,20\n3,30\n4,40\n");
    let vals = Column::new("c".into(), ["7", "8"]);
    assert_eq!(s.paste_cells("v", &vals), Err("no rows selected".into()));
    Arc::make_mut(&mut s.view.selected).extend([0u64, 2, 3]);
    s.set_filter_expr(Some("id <> 3")).unwrap(); // hides rid 2: it goes last
    assert_eq!(s.paste_cells("v", &vals), Ok(3));
    s.set_filter_expr(None).unwrap();
    assert_eq!(ints(&s, "v"), [Some(7), Some(20), Some(7), Some(8)]);
    let bad = Column::new("c".into(), [Some("9"), None, Some("x")]);
    assert_eq!(
        s.paste_cells("v", &bad),
        Err(r#""x" doesn't fit v (i64)"#.into())
    );
    assert_eq!(s.edits().len(), 1, "a refused paste adds no step");
}

#[test]
fn set_expr_reading_a_derived_column_goes_after_its_derive() {
    let (mut s, _t) = sheet_of("id,v\n1,10\n2,20\n");
    s.derive("twice", "v * 2").unwrap();
    Arc::make_mut(&mut s.view.selected).extend([1u64]);
    s.set_expr("id", "twice + 1").unwrap();
    assert!(matches!(
        s.state.steps.last(),
        Some(PlanStep::SetExpr { .. })
    ));
    assert_eq!(ints(&s, "id"), [Some(1), Some(41)]);
    // An edit `twice` doesn't read still lands before it.
    s.set_expr("v", "0").unwrap();
    assert!(matches!(s.state.steps[0], PlanStep::SetExpr { .. }));
    assert_eq!(ints(&s, "twice"), [Some(20), Some(0)]);
}

#[test]
fn filter_expr_keeps_matching_rows_with_the_regex_filter() {
    let (mut s, _t) = sheet_of("id,v\n1,10\n2,20\n3,30\n4,40\n");
    s.set_filter_expr(Some("v >= 20")).unwrap();
    assert_eq!(ints(&s, "id"), [Some(2), Some(3), Some(4)]);
    s.view.filter = Some(FilterState {
        column: "id".into(),
        regex: "[34]".into(),
    });
    assert_eq!(ints(&s, "id"), [Some(3), Some(4)]);
    assert_eq!(
        s.set_filter_expr(Some("v + 1")),
        Err("expr: not a boolean: v + 1".into())
    );
    assert_eq!(
        s.view.filter_expr.as_deref(),
        Some("v >= 20"),
        "a bad predicate keeps the old one"
    );
    assert!(
        s.rename_col("v", "w")
            .unwrap_err()
            .contains(":filter-expr reads v")
    );
    s.rename_col("id", "key").unwrap();
    s.set_filter_expr(None).unwrap();
    assert_eq!(ints(&s, "key"), [Some(3), Some(4)]);
}

#[test]
fn a_stale_window_or_count_never_lands() {
    let (mut s, _t) = sheet_of("id\n1\n2\n");
    let old = s.begin_window();
    let new = s.begin_window();
    let df = s.window_plan_with_rids().collect().unwrap();
    assert!(!s.land_window(old, df.clone()));
    assert!(s.window().is_none() && !s.is_loaded());
    assert!(!s.land_error(old));
    assert!(s.land_window(new, df));
    assert!(s.is_loaded());
    assert_eq!(s.window_rids(), [0, 1]);
    assert_eq!(s.window().unwrap().get_column_names(), ["id"]);

    let key = s.start_count().unwrap();
    assert_eq!(s.start_count(), None, "already counting this key");
    assert!(!s.land_count("stale", Some(9)));
    assert!(s.is_counting() && s.row_count().is_none());
    assert!(s.land_count(&key, Some(2)));
    assert_eq!(s.row_count(), Some(2));
    s.invalidate(Reason::Plan);
    assert!(s.window().is_none() && !s.is_loaded());
}

/// Every way a plan step lands makes the row count stale (S-24 R2).
#[test]
fn every_plan_step_recounts() {
    let (mut s, _t) = sheet_of("id,v\n1,$10\n2,$20\n");
    Arc::make_mut(&mut s.view.selected).insert(0);
    let k = s.start_count().unwrap();
    s.land_count(&k, Some(2));
    type Step = (&'static str, fn(&mut Sheet));
    let steps: [Step; 8] = [
        ("currency", |s| {
            s.apply_step(PlanStep::Currency { column: "v".into() })
                .unwrap()
        }),
        ("derive", |s| s.derive("w", "id * 2").unwrap()),
        ("rename", |s| s.rename_col("w", "x").unwrap()),
        ("setcol", |s| _ = s.set_col("id", "5").unwrap()),
        ("edit", |s| s.edit_cell("id", 1, "6").unwrap()),
        ("setexpr", |s| _ = s.set_expr("id", "id + 1").unwrap()),
        ("paste", |s| {
            _ = s
                .paste_cells("id", &Column::new("c".into(), ["7", "8"]))
                .unwrap()
        }),
        ("undo", |s| assert!(s.time_travel(true))),
    ];
    for (what, step) in steps {
        step(&mut s);
        let k = s
            .start_count()
            .unwrap_or_else(|| panic!("{what} left the count current"));
        s.land_count(&k, Some(2));
    }
}

/// S-18 E: deleted rows leave every view of the rows; row ids don't shift.
#[test]
fn deleted_rows_are_masked_and_ids_stay() {
    let (mut s, _t) = sheet_of("id,v\n1,10\n2,20\n3,30\n4,40\n");
    Arc::make_mut(&mut s.view.selected).extend([1u64, 3]);
    assert_eq!(s.delete_rows(vec![1]), Ok(1));
    assert_eq!(s.view.selected.iter().copied().collect::<Vec<_>>(), [3]);
    assert_eq!(s.view_rids().unwrap(), [0, 2, 3]);
    let n = s.count_plan().collect().unwrap();
    assert_eq!(
        n.columns()[0].get(0).unwrap().try_extract::<u64>().unwrap(),
        3
    );
    // an edit after the delete still lands on its row
    s.edit_cell("v", 2, "33").unwrap();
    assert_eq!(ints(&s, "v"), [Some(10), Some(33), Some(40)]);
    assert_eq!(s.delete_rows(vec![]), Err("no rows selected".into()));
    assert!(s.time_travel(true) && s.time_travel(true));
    assert_eq!(ints(&s, "v"), [Some(10), Some(20), Some(30), Some(40)]);
    assert_eq!(s.view.selected.len(), 2, "undo brings the selection back");
}

/// The rid column (S-18 §2.3): source ids are positions, carried through
/// steps, never shown or saved.
#[test]
fn source_rows_carry_their_ids_out_of_sight() {
    let (mut s, _t) = sheet_of("id,v\n1,10\n2,20\n3,30\n");
    s.view.sort = vec![SortKey {
        column: "v".into(),
        order: SortOrder::Desc,
    }];
    assert_eq!(
        rid_values(&s.sorted_plan(true).collect().unwrap()).unwrap(),
        [2, 1, 0]
    );
    s.derive("w", "v + 1").unwrap();
    assert_eq!(
        rid_values(&s.sorted_plan(true).collect().unwrap()).unwrap(),
        [2, 1, 0]
    );
    let names = s.view_plan().collect_schema().unwrap();
    assert!(!names.contains(ROW_ID), "{names:?}");
    assert!(!s.data_plan().collect_schema().unwrap().contains(ROW_ID));
    assert!(s.derive(ROW_ID, "1").unwrap_err().contains("reserved"));
    assert!(s.rename_col("w", ROW_ID).unwrap_err().contains("reserved"));
}

fn view_ids(s: &Sheet) -> Vec<i64> {
    ints(s, "id").into_iter().map(|v| v.unwrap_or(-1)).collect()
}

/// S-18 F: added rows go between their neighbours in base order, at any
/// depth, without renumbering; ids never meet source ids.
#[test]
fn added_rows_land_between_neighbours() {
    let (mut s, _t) = sheet_of("id\n1\n2\n3\n");
    let a = s.add_rows(Some(0), true, None, 1).unwrap(); // after row 1
    assert!(a[0] >= ADDED_RID);
    s.set_col("id", "9").unwrap_err(); // nothing selected
    s.edit_cell("id", a[0], "10").unwrap();
    let b = s.add_rows(Some(0), true, None, 1).unwrap(); // between 1 and 10
    s.edit_cell("id", b[0], "11").unwrap();
    let c = s.add_rows(Some(b[0]), false, None, 2).unwrap(); // before 11
    s.edit_cell("id", c[0], "12").unwrap();
    s.edit_cell("id", c[1], "13").unwrap();
    s.add_rows(Some(0), false, None, 1).unwrap(); // at the top
    let d = s.add_rows(Some(2), true, None, 1).unwrap(); // after the last
    s.edit_cell("id", d[0], "14").unwrap();
    assert_eq!(view_ids(&s), [-1, 1, 12, 13, 11, 10, 2, 3, 14]);
    // undo takes the last add away, the rest stay put
    assert!(s.time_travel(true) && s.time_travel(true));
    assert_eq!(view_ids(&s), [-1, 1, 12, 13, 11, 10, 2, 3]);
}

#[test]
fn pasted_rows_match_columns_by_name_and_type() {
    let (mut s, _t) = sheet_of("id,v,note\n1,10,a\n2,20,b\n");
    s.rename_col("note", "memo").unwrap();
    s.derive("w", "v * 2").unwrap();
    let rows = df!("memo" => ["x"], "v" => [5i64], "w" => [0i64], "other" => [1i64]).unwrap();
    s.add_rows(Some(1), true, Some(&rows), 0).unwrap();
    let out = s.view_plan().collect().unwrap();
    assert_eq!(out.height(), 3);
    let last = out.slice(2, 1);
    assert_eq!(last.column("id").unwrap().get(0).unwrap(), AnyValue::Null);
    assert_eq!(
        last.column("memo").unwrap().get(0).unwrap(),
        AnyValue::String("x")
    );
    assert_eq!(
        last.column("w").unwrap().get(0).unwrap(),
        AnyValue::Int64(10)
    ); // recomputed
    let bad = df!("v" => ["nope"]).unwrap();
    let e = s.add_rows(None, true, Some(&bad), 0).unwrap_err();
    assert!(e.contains("don't fit"), "{e}");
    assert_eq!(s.view_plan().collect().unwrap().height(), 3);
}

#[test]
fn rows_added_one_after_another_keep_their_order() {
    // Each `a` lands after the row it made: the gap halves every time.
    let (mut s, _t) = sheet_of("id\n0\n1\n");
    let mut at = 0;
    for i in 2..120 {
        at = s.add_rows(Some(at), true, None, 1).unwrap()[0];
        s.edit_cell("id", at, &i.to_string()).unwrap();
    }
    let want: Vec<i64> = std::iter::once(0).chain(2..120).chain([1]).collect();
    assert_eq!(view_ids(&s), want);
}

#[test]
fn many_edits_stay_collectable() {
    let (mut s, _t) = sheet_of("id\n0\n1\n");
    for i in 0..200 {
        s.edit_cell("id", 0, &i.to_string()).unwrap();
    }
    assert_eq!(view_ids(&s), [199, 1]);
    assert_eq!(s.edits().len(), 1, "one join, not 200");
}

/// S-18 G: a refused paste leaves no added rows and no undo step behind.
#[test]
fn a_refused_region_paste_changes_nothing() {
    let (mut s, _t) = sheet_of("id,v\n1,a\n2,b\n");
    let lines = |t: &str| crate::clipboard::fields(t);
    let cols = ["id".to_string(), "v".into()];
    let e = s
        .paste_region(&cols, vec![1], &lines("5\tx\nbad\ty\n"))
        .unwrap_err();
    assert!(e.contains("bad"), "{e}");
    assert_eq!(view_ids(&s), [1, 2]);
    assert!(!s.time_travel(true), "no undo step");
    assert_eq!(s.paste_region(&cols, vec![1], &lines("5\tx\n6\n")), Ok(3));
    assert_eq!(view_ids(&s), [1, 5, 6]);
    assert!(s.time_travel(true));
    assert_eq!(view_ids(&s), [1, 2]);
}

/// S-20: what a state looks like, to compare on revisits.
fn fingerprint(s: &Sheet) -> String {
    let mut df = s.view_plan().collect().unwrap();
    let mut out = Vec::new();
    CsvWriter::new(&mut out).finish(&mut df).unwrap();
    format!(
        "{:?}|{:?}|{:?}|{}",
        s.state.steps,
        s.view.hidden,
        s.view.selected,
        String::from_utf8(out).unwrap()
    )
}

#[test]
fn a_change_after_undo_branches_and_earlier_reaches_the_other_branch() {
    let (mut s, _t) = sheet_of("id,v\n1,a\n2,b\n");
    let open = fingerprint(&s);
    s.edit_cell("v", 0, "x").unwrap(); // state 1
    let one = fingerprint(&s);
    assert!(s.time_travel(true));
    s.checkpoint(); // state 2, a sibling of 1
    s.view.hidden.insert("v".into());
    let two = fingerprint(&s);
    let labels: Vec<String> = s.undo_states().into_iter().map(|u| u.label).collect();
    assert_eq!(labels, ["opened", "edit v = x", "hide v"]);
    assert!(s.time_travel(true));
    assert_eq!(fingerprint(&s), open);
    assert!(s.time_travel(false)); // the child last come back from
    assert_eq!(fingerprint(&s), two);
    assert!(s.time_walk(1)); // :earlier: state 1, on the other branch
    assert_eq!(fingerprint(&s), one);
    assert!(s.time_walk(-1)); // :later
    assert_eq!(fingerprint(&s), two);
    assert!(!s.time_walk(-1), "newest");
    assert!(s.time_walk(5)); // clamps to the oldest
    assert_eq!(fingerprint(&s), open);
    assert!(!s.time_walk(1), "oldest");
    assert!(s.goto_state(1));
    assert_eq!(fingerprint(&s), one);
    assert!(s.time_travel(true) && s.time_travel(false));
    assert_eq!(fingerprint(&s), one, "R retraces a jump");
}

#[test]
fn eviction_drops_old_branches_first_and_keeps_the_current_path() {
    let (mut s, _t) = sheet_of("id\n1\n");
    for _ in 0..5 {
        s.checkpoint(); // a branch that will be undone
    }
    for _ in 0..5 {
        assert!(s.time_travel(true));
    }
    for i in 0..120 {
        s.edit_cell("id", 0, &i.to_string()).unwrap();
    }
    let states = s.undo_states();
    assert_eq!(states.len(), 100);
    assert_eq!(s.state.undo.depth(), 99, "one line: the current path");
    assert!(
        states.iter().all(|u| u.id > 5),
        "the undone branch went first"
    );
    for _ in 0..99 {
        assert!(s.time_travel(true));
    }
    assert!(!s.time_travel(true));
    assert_eq!(view_ids(&s), [20]); // the 21st edit's state is the oldest kept
}

/// Seeded random walk: every state, revisited by any route, is as it was.
#[test]
fn every_state_comes_back_exactly() {
    let (mut s, _t) = sheet_of("id,v\n1,a\n2,b\n3,c\n");
    let mut seen: std::collections::HashMap<u64, String> = Default::default();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut rand = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    seen.insert(s.state.undo.current(), fingerprint(&s));
    for step in 0..400 {
        match rand(8) {
            0 => drop(s.edit_cell("v", rand(3), &step.to_string())),
            1 => {
                s.checkpoint();
                let c = ["id", "v"][rand(2) as usize].to_string();
                if !s.view.hidden.remove(&c) {
                    s.view.hidden.insert(c);
                }
            }
            2 => drop(s.delete_rows(vec![rand(3)])),
            3 => {
                s.checkpoint();
                std::sync::Arc::make_mut(&mut s.view.selected).insert(rand(3));
            }
            4 => drop(s.time_travel(true)),
            5 => drop(s.time_travel(false)),
            6 => drop(s.time_walk(rand(4) as i64 - 2)),
            _ => {
                let ids: Vec<u64> = s.undo_states().iter().map(|u| u.id).collect();
                s.goto_state(ids[rand(ids.len() as u64) as usize]);
            }
        }
        let fp = fingerprint(&s);
        let id = s.state.undo.current();
        if let Some(was) = seen.get(&id) {
            assert_eq!(*was, fp, "state {id} changed (step {step})");
        }
        seen.insert(id, fp);
    }
}

#[test]
fn freq_adds_aggregator_columns_in_view_order() {
    let (mut s, _tmp) = sheet_of("k,a,b\nx,1,\nx,3,a\ny,,b\n");
    s.add_aggregators("b", "count distinct").unwrap();
    s.add_aggregators("a", "sum mean avg min max median mode stdev sum")
        .unwrap();
    assert!(
        s.add_aggregators("b", "sum")
            .unwrap_err()
            .contains("isn't numeric")
    );
    assert!(
        s.add_aggregators("a", "nope")
            .unwrap_err()
            .contains("no aggregator")
    );
    let freq = Sheet::freq_from(&s, 1, "k");
    let df = freq.plan.clone().collect().unwrap();
    let names: Vec<String> = df
        .get_column_names_owned()
        .iter()
        .map(|n| n.to_string())
        .collect();
    assert_eq!(
        names[2..12],
        [
            "a_sum",
            "a_mean",
            "a_avg",
            "a_min",
            "a_max",
            "a_median",
            "a_mode",
            "a_stdev",
            "b_count",
            "b_distinct"
        ]
    );
    assert!(freq.view.hidden.contains("percent") && freq.view.hidden.contains("histogram"));
    let row = |i: usize| -> Vec<String> {
        df.columns()[2..12]
            .iter()
            .map(|c| any_text(&c.get(i).unwrap()))
            .collect()
    };
    // x: a = 1, 3; b = null, "a". y: a all null (sum 0, the rest null).
    assert_eq!(
        row(0),
        [
            "4",
            "2.0",
            "2.0",
            "1",
            "3",
            "2.0",
            "1",
            "1.4142135623730951",
            "1",
            "1"
        ]
    );
    assert_eq!(row(1)[..2], ["0", "null"]);
    assert_eq!(
        s.memo_aggregate("a", "sum count").unwrap(),
        "a_sum=4  a_count=2"
    );
    Arc::make_mut(&mut s.view.selected).insert(1);
    assert_eq!(s.memo_aggregate("a", "sum").unwrap(), "a_sum=3");
    s.rename_col("a", "z").unwrap(); // aggregators follow the rename
    assert!(s.view.aggregators.iter().all(|(c, _)| c != "a"));
}

#[test]
fn copied_rows_keep_base_values_under_a_view_cast() {
    let (mut s, _t) = sheet_of("name,price\na,2.75\nb,1.5\n");
    s.set_type_cast("price".into(), DataType::Int64);
    s.view.sort.push(SortKey {
        column: "price".into(),
        order: SortOrder::Asc,
    });
    let row = s.copy_rows(Some(0), true).unwrap(); // sorted: b first
    let get = |df: &DataFrame, c: &str| df.column(c).unwrap().get(0).unwrap().into_static();
    assert_eq!(get(&row, "name"), AnyValue::String("b").into_static());
    assert_eq!(get(&row, "price"), AnyValue::Float64(1.5));
    let shown = s.copy_rows(Some(0), false).unwrap();
    assert_eq!(get(&shown, "price"), AnyValue::Int64(1));
    let names: Vec<&str> = row.get_column_names().iter().map(|n| n.as_str()).collect();
    assert_eq!(names, ["name", "price"]);
    // derived sheets number their rows the same way
    let mut d = Sheet::child(&s, 1, "d".into(), s.data_plan(), Origin::Copy { parent: 0 });
    d.set_type_cast("price".into(), DataType::Int64);
    d.view.selected = Arc::new([0u64].into());
    let row = d.copy_rows(None, true).unwrap();
    assert_eq!(get(&row, "price"), AnyValue::Float64(2.75));
}
