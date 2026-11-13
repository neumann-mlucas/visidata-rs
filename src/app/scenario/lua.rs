//! Lua: bindings, commands, hooks, renderers, `:source`.

use super::*;

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

/// The guide's `vd.command` and `<F3>` examples do what the guide says.
#[tokio::test(flavor = "multi_thread")]
async fn guide_lua_examples_run() {
    let html = include_str!("../../../docs/index.html");
    let block = |start: &str| {
        let b = html.split(&format!("<pre><code>{start}")).nth(1).unwrap();
        let b = b.split("</code></pre>").next().unwrap();
        format!("{start}{b}")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&")
    };
    let src = block("vd.command(\"hello\"") + "\n" + &block("vd.bind(\"sheet\", \"&lt;F3&gt;\"");
    let mut h = Harness::new(3).await;
    h.app.plugins = Some(crate::plugins::Plugins::load_from_source(&src).unwrap());
    (h.app.keymap, h.app.theme) = keymap_and_theme(h.app.plugins.as_ref());
    h.keys(":").await;
    h.type_("hello world").await;
    assert_eq!(status(&h), "hello world");
    h.keys("<F3>").await;
    assert!(h.screen().contains(" columns "), "{}", h.screen());
    assert!(
        view_csv(&h).starts_with("n,name,length\n1,id,2\n"),
        "{}",
        view_csv(&h)
    );
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
