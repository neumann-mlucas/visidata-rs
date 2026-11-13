//! Edits, undo, clipboard, columns sheet, column builders.

use super::*;

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
async fn derive_over_the_filtered_column_recounts() {
    let mut h = csv(b"n\n1\n1\n2\n").await;
    h.keys("g z |").await;
    h.type_("^1$").await;
    assert!(title(&h).contains("row 1/2"), "{}", title(&h));
    h.keys(":").await;
    h.type_("derive n = n + 1").await;
    assert!(title(&h).contains("row 0/0"), "{}", title(&h));
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
    h.keys("l l l l e <C-u>").await; // `value` of the cursor column, `id`
    h.type_("9").await;
    assert_eq!(depth(&h), 2, "one state for one edit");
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

#[tokio::test(flavor = "multi_thread")]
async fn refresh_refuses_a_sheet_with_nothing_to_replay() {
    let mut h = csv(b"id\n1\n").await;
    h.keys("<C-r>").await;
    assert_eq!(status(&h), "refresh: not a derived sheet");
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

#[tokio::test(flavor = "multi_thread")]
async fn column_builders_fill_number_add_and_capture() {
    let mut h = csv(b"id,name,grp\n3,c-1,x\n1,a-2,\n2,b-3,\n4,d-4,y\n").await;
    // f: empty cells take the value above, in view order
    h.keys("l l f").await;
    assert_eq!(status(&h), "filled 2 values");
    assert_eq!(
        view_csv(&h),
        "id,name,grp\n3,c-1,x\n1,a-2,x\n2,b-3,x\n4,d-4,y\n"
    );
    h.keys("U").await;
    assert_eq!(
        view_csv(&h),
        "id,name,grp\n3,c-1,x\n1,a-2,\n2,b-3,\n4,d-4,y\n"
    );
    // i: numbers in view order (sorted), kept when the sort goes
    h.keys("h h [ i").await;
    assert_eq!(h.current_column(), "incr");
    h.keys(":").await;
    h.type_("unsort").await;
    assert_eq!(
        view_csv(&h),
        "id,name,grp,incr\n3,c-1,x,3\n1,a-2,,1\n2,b-3,,2\n4,d-4,y,4\n"
    );
    h.keys("U U").await; // :unsort, then the column and its numbers in one step
    assert!(!view_csv(&h).contains("incr"), "{}", view_csv(&h));
    h.keys("U").await; // the sort
    assert_eq!(
        view_csv(&h),
        "id,name,grp\n3,c-1,x\n1,a-2,\n2,b-3,\n4,d-4,y\n"
    );
    // gi: the selected rows of this column, in view order
    h.keys("g g s G s g i").await;
    assert_eq!(status(&h), "set id to 1…2");
    assert_eq!(
        view_csv(&h),
        "id,name,grp\n1,c-1,x\n1,a-2,\n2,b-3,\n2,d-4,y\n"
    );
    h.keys("U g u").await;
    // za: an empty text column, the cursor on it, editable
    h.keys("z a").await;
    h.type_("comment").await;
    assert_eq!(h.current_column(), "comment");
    h.keys("g g j e").await;
    h.type_("check").await;
    assert_eq!(
        view_csv(&h),
        "id,name,grp,comment\n3,c-1,x,\n1,a-2,,check\n2,b-3,,\n4,d-4,y,\n"
    );
    h.keys("z a").await;
    h.type_("comment").await;
    assert_eq!(status(&h), "addcol: column already exists: comment");
    // ;: one struct column of the groups; ( expands it
    h.keys("g h l ;").await;
    h.type_("([a-z])-(?P<n>\\d)").await;
    assert_eq!(status(&h), "added name_re; ( expands it");
    h.keys("(").await;
    let names = h.screen().lines().nth(1).unwrap().to_string();
    assert!(
        names.contains("name_re.0") && names.contains("name_re.n"),
        "{names}"
    );
    h.keys(";").await;
    h.type_("[a-z]").await;
    assert_eq!(status(&h), "specify a capture group using parentheses");
    // derived sheets are read-only
    h.keys("F i").await;
    assert!(status(&h).contains(": read-only:"), "{}", status(&h));
}

#[tokio::test(flavor = "multi_thread")]
async fn copies_keep_column_attributes() {
    let mut h = csv(b"id,amt\n1,2.5\n2,4\n3,8\n").await;
    h.keys("l % :").await;
    h.type_("aggregate sum").await;
    h.keys(":").await;
    h.type_("color amt amt > 3 red").await;
    h.keys("s j s \"").await;
    let v = &h.app.sheets.active().view;
    assert_eq!(v.aggregators, [("amt".to_string(), "sum".to_string())]);
    assert_eq!(v.type_casts, [("amt".to_string(), DataType::Float64)]);
    assert_eq!(v.formats.len(), 1);
    h.keys("q g '").await;
    assert_eq!(h.app.sheets.active().view.aggregators.len(), 1);
    // a freq sheet's columns mean something else: nothing carried over
    h.keys("q F").await;
    assert!(h.app.sheets.active().view.aggregators.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn float_precision_more_less_set_and_columns_sheet() {
    let mut h = csv(b"id,amt\n1,3.14159\n2,2.5\n").await;
    let rows = |h: &Harness| h.screen().lines().nth(2).unwrap_or_default().to_string();
    h.keys("l <A-+>").await;
    assert_eq!(status(&h), "amt: 3 decimals");
    assert!(rows(&h).contains("3.142"), "{}", h.screen());
    h.keys("<A--> <A--> <A--> <A--> <A-->").await;
    assert_eq!(status(&h), "amt: 0 decimals");
    assert!(rows(&h).ends_with(" 3"), "{}", rows(&h));
    h.keys(":").await;
    h.type_("precision 1").await;
    assert!(
        rows(&h).contains("3.1") && !rows(&h).contains("3.14"),
        "{}",
        rows(&h)
    );
    // display only: the data keeps every digit
    assert_eq!(view_csv(&h), "id,amt\n1,3.14159\n2,2.5\n");
    h.keys("C").await;
    assert!(h.screen().contains("%.1f"), "{}", h.screen());
    h.keys("q :").await;
    h.type_("precision").await;
    assert_eq!(status(&h), "amt: default precision");
    assert!(rows(&h).contains("3.14159"), "{}", rows(&h));
    h.keys("h <A-+>").await;
    assert_eq!(status(&h), "precision: id is i (float columns only)");
}
