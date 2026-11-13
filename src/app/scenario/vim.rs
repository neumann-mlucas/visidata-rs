//! Vim grammar, visual and ex ranges, macros, the jumplist.

use super::*;

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

/// Keys pressed while a replay waits on a job queue behind it: three
/// quick F5s of "edit, move down" check three different rows.
#[tokio::test(flavor = "multi_thread")]
async fn macro_pressed_again_mid_replay_queues_behind_it() {
    let mut h = csv(b"s\na\nb\nc\nd\ne\n").await;
    h.keys("m e <C-u>").await;
    h.type_("x").await;
    h.keys("j m").await;
    h.type_("<F5>").await;
    h.keys("<F5> <F5> <F5>").await;
    assert_eq!(view_csv(&h), "s\nx\nx\nx\nx\ne\n");
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

/// Default `zz` is VisiData's scroll-middle on a sheet (zoom on a plot).
#[tokio::test(flavor = "multi_thread")]
async fn zz_scrolls_the_cursor_row_to_the_middle() {
    let mut h = Harness::new(100).await;
    h.keys("4 0 j z z").await;
    let v = &h.app.sheets.active().view;
    assert!(v.row_offset > 0);
    assert_eq!(v.row_cursor - v.row_offset, h.app.viewport_rows / 2);
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
    h.keys("\" + p").await;
    assert!(status(&h).starts_with("\"+ pastes cells"), "{}", status(&h));
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
    // the cell's value is the line: change its first word, drop the last
    h.keys("i <C-u>").await;
    h.type_("hello big world").await;
    h.keys("i <Esc> 0 c w").await;
    h.type_no_enter("bye").await;
    h.keys("<Esc> $ b D u x <CR>").await;
    assert_eq!(text(&h), "id,v 1,bye big orld ", "u undid D, x cut the w");
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

/// A macro over a visual range replays from the cursor, not on the rows it
/// was recorded on (vim records the keys).
#[tokio::test(flavor = "multi_thread")]
async fn vim_macro_over_a_visual_range_is_relative() {
    let mut h = csv(b"id\n1\n2\n3\n4\n5\n6\n7\n").await;
    h.app.plugins =
        Some(crate::plugins::Plugins::load_from_source(r#"vd.option("keymap", "vim")"#).unwrap());
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    let ids = |h: &Harness| csv_text(h.app.sheets.active(), b',').replace('\n', " ");
    h.keys("j q a V j d q").await;
    assert_eq!(ids(&h), "id 1 4 5 6 7 ", "rows 2 and 3 cut");
    assert_eq!(h.app.macros[&'a'], ["cursor_down", ":.-1,.+0 cut-row"]);
    h.keys("j @ a").await;
    assert_eq!(
        ids(&h),
        "id 1 4 5 ",
        "replayed from the cursor (row 6): 6 and 7"
    );
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
