use super::*;
use crate::keymap::{Dispatch, Keymap, Layer};

/// The generated `init.lua` loads as is (all comments), and with every
/// `vd.bind` line uncommented it binds exactly what the defaults bind.
#[test]
fn default_init_lua_spells_out_the_defaults() {
    let text = default_init_lua();
    if std::env::var_os("VDRS_PRINT_INIT").is_some() {
        println!("{text}");
    }
    let mut km = Keymap::default();
    Plugins::load_from_source(&text)
        .unwrap()
        .apply_keymap(&mut km);
    assert!(km.chords(Layer::Sheet).is_empty());

    let live: String = text
        .lines()
        .map(|l| {
            l.strip_prefix("-- ")
                .filter(|b| b.starts_with("vd.bind("))
                .unwrap_or(l)
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mut km = Keymap::default();
    Plugins::load_from_source(&live)
        .unwrap()
        .apply_keymap(&mut km);
    let defaults = Keymap::defaults();
    for layer in [Layer::Sheet, Layer::Input] {
        assert_eq!(km.chords(layer).len(), defaults.chords(layer).len());
        for c in defaults.chords(layer) {
            let (last, pending) = c.on.split_last().unwrap();
            let fired = |k: &Keymap| match k.resolve(layer, pending, *last) {
                Dispatch::Fire(a) => Some(a),
                _ => None,
            };
            assert_eq!(fired(&km), fired(&defaults), "{}", c.desc);
        }
    }
}

/// Every `vd.option` (as its unknown-option error lists them) has a
/// commented default in the first-run `init.lua`, which still loads.
#[test]
fn default_init_lua_lists_every_option() {
    let Err(err) = Plugins::load_from_source(r#"vd.option("nope", "x")"#) else {
        panic!("unknown option accepted");
    };
    let err = err.to_string();
    let names = err.split_once('(').unwrap().1.split(')').next().unwrap();
    let init = default_init_lua();
    for name in names.split(", ") {
        assert!(init.contains(&format!("-- vd.option(\"{name}\"")), "{name}");
    }
    Plugins::load_from_source(&init).unwrap();
}

#[test]
fn first_run_writes_init_lua_once() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vdrs").join("init.lua");
    write_default_config(&path).unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), default_init_lua());
    std::fs::write(&path, "-- mine").unwrap();
    assert!(write_default_config(&path).is_err()); // never clobbers
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "-- mine");
}

#[test]
fn bind_registers_a_single_key_chord() {
    let p =
        Plugins::load_from_source(r#"vd.bind("sheet", "x", "goto_top", "jump to top")"#).unwrap();
    let mut km = Keymap::defaults();
    p.apply_keymap(&mut km);
    let d = km.resolve(Layer::Sheet, &[], "x".parse().unwrap());
    assert!(matches!(d, Dispatch::Fire(v) if matches!(v[0], Action::GotoTop)));
}

#[test]
fn bind_registers_a_chord_from_table() {
    let p =
        Plugins::load_from_source(r#"vd.bind("sheet", {"<Space>", "f"}, "search_next")"#).unwrap();
    let mut km = Keymap::defaults();
    p.apply_keymap(&mut km);
    let pending = km.resolve(Layer::Sheet, &[], "<Space>".parse().unwrap());
    assert!(matches!(pending, Dispatch::Pending(_)));
    let fired = km.resolve(
        Layer::Sheet,
        &["<Space>".parse().unwrap()],
        "f".parse().unwrap(),
    );
    assert!(matches!(fired, Dispatch::Fire(v) if matches!(v[0], Action::SearchNext)));
}

#[test]
fn bind_accepts_ex_commands_and_rejects_bad_ones() {
    let p = Plugins::load_from_source(r#"vd.bind("sheet", "x", ":sort -id")"#).unwrap();
    let mut km = Keymap::defaults();
    p.apply_keymap(&mut km);
    let d = km.resolve(Layer::Sheet, &[], "x".parse().unwrap());
    assert!(matches!(d, Dispatch::Fire(v) if v[0] == Action::ExCommand("sort -id".into())));
    assert!(Plugins::load_from_source(r#"vd.bind("sheet", "x", ":frobnicate")"#).is_err());
}

#[test]
fn broken_init_keeps_earlier_registrations_and_reloads_on_change() {
    let dir = tempfile::tempdir().unwrap();
    let init = dir.path().join("init.lua");
    std::fs::write(
        &init,
        "vd.bind('sheet', 'x', 'goto_top')\nvd.set_theme('light')\nerror('boom')",
    )
    .unwrap();
    let (p, warn) = Plugins::load_from(Some(init.clone()), None);
    assert!(warn.unwrap().contains("boom"));
    let mut km = Keymap::defaults();
    p.apply_keymap(&mut km);
    assert!(matches!(
        km.resolve(Layer::Sheet, &[], "x".parse().unwrap()),
        Dispatch::Fire(_)
    ));
    assert!(p.options().theme.is_some());
    assert!(p.reload_if_changed().is_none());
    std::fs::remove_file(&init).unwrap(); // deleting counts as a change
    let (fresh, warn) = p.reload_if_changed().unwrap();
    assert!(warn.is_none() && fresh.options().theme.is_none());
}

#[test]
fn aggregator_and_renderer_run_lua() {
    let p = Plugins::load_from_source(
        r#"vd.register_aggregator("span", function(vs)
             local lo, hi = math.huge, -math.huge
             for _, v in ipairs(vs) do lo = math.min(lo, v); hi = math.max(hi, v) end
             return hi - lo end)
           vd.register_renderer { column = "price", render = function(v) return "$" .. v end }
           vd.register_renderer { dtype = "bool", render = function(v) return v and "yes" or "no" end }"#,
    )
    .unwrap();
    assert!(p.aggregator("sum").is_none());
    use polars::prelude::NamedFrom;
    let vals = Series::new("v".into(), [3i64, 9, 4]);
    let span = p.aggregator("span").unwrap();
    let on_job = std::thread::spawn(move || span.call(&vals).unwrap());
    assert_eq!(on_job.join().unwrap(), AggValue::Num(6.0));
    assert_eq!(
        p.render_text("price", &DataType::Float64, &AnyValue::Float64(2.5)),
        Some("$2.5".into())
    );
    assert_eq!(
        p.render_text("ok", &DataType::Boolean, &AnyValue::Boolean(false)),
        Some("no".into())
    );
    assert_eq!(
        p.render_text("other", &DataType::Int64, &AnyValue::Int64(1)),
        None
    );
    assert!(Plugins::load_from_source("vd.register_renderer { render = print }").is_err());
}

#[test]
fn option_sets_clipboard_cmd_and_rejects_unknown() {
    let p = Plugins::load_from_source(r#"vd.option("clipboard_copy_cmd", "wl-copy -n")"#).unwrap();
    assert_eq!(
        p.options().clipboard_copy_cmd.as_deref(),
        Some("wl-copy -n")
    );
    let p = Plugins::load_from_source(r#"vd.option("clipboard_paste_cmd", "wl-paste")"#).unwrap();
    assert_eq!(p.options().clipboard_paste_cmd.as_deref(), Some("wl-paste"));
    let p = Plugins::load_from_source(r#"vd.option("duckdb_cmd", "/opt/duckdb")"#).unwrap();
    assert_eq!(p.options().duckdb_cmd.as_deref(), Some("/opt/duckdb"));
    for bad in [
        r#"vd.option("nope", "x")"#,
        r#"vd.option("clipboard_copy_cmd", " ")"#,
    ] {
        assert!(Plugins::load_from_source(bad).is_err(), "{bad}");
    }
}

#[test]
fn bind_takes_a_list_of_steps() {
    let p = Plugins::load_from_source(
        r#"vd.bind("sheet", "<F5>", { "goto_rightmost", ":filter region ^n", "open_freq_table" })"#,
    )
    .unwrap();
    let mut km = Keymap::default();
    p.apply_keymap(&mut km);
    let run = &km.chords(Layer::Sheet)[0].run;
    assert_eq!(
        run,
        &vec![Action::Macro(vec![
            Action::GotoRightmost,
            Action::ExCommand("filter region ^n".into()),
            Action::OpenFreqTable,
        ])]
    );
    for bad in [r#"{ "no_such" }"#, r#"{ ":filter" }"#, "{}"] {
        let src = format!(r#"vd.bind("sheet", "x", {bad})"#);
        assert!(Plugins::load_from_source(&src).is_err(), "{bad}");
    }
}

/// What `m` writes loads back through the real loader, a later macro on
/// the same keys replaces the earlier, and other lines stay.
#[test]
fn saved_macros_load_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("vdrs").join("macros.lua");
    let keys: Vec<Key> = vec!["g".parse().unwrap(), "<F5>".parse().unwrap()];
    let steps = vec!["cursor_right".to_string(), ":edit \"a\\\"b\"\n".to_string()];
    save_macro(&path, &keys, &["redraw".to_string()]).unwrap();
    save_macro(&path, &["x".parse().unwrap()], &["redraw".to_string()]).unwrap();
    save_macro(&path, &keys, &steps).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert_eq!(text.lines().count(), 2, "{text}");
    let (p, warn) = Plugins::load_from(Some(dir.path().join("vdrs").join("init.lua")), None);
    assert_eq!(warn, None);
    let mut km = Keymap::default();
    p.apply_keymap(&mut km);
    let chord = km
        .chords(Layer::Sheet)
        .iter()
        .find(|c| c.on == keys)
        .unwrap();
    assert_eq!(
        chord.run,
        vec![Action::Macro(vec![
            Action::CursorRight { count: None },
            Action::ExCommand("edit \"a\\\"b\"\n".into()),
        ])]
    );
}

#[test]
fn float_fmt_takes_visidata_forms() {
    assert_eq!(float_fmt_digits("%.2f"), Some(2));
    assert_eq!(float_fmt_digits("{:.0f}"), Some(0));
    assert_eq!(float_fmt_digits("%.16f"), None);
    assert_eq!(float_fmt_digits("%d"), None);
    assert!(
        Plugins::load_from_source(r#"vd.option("disp_float_fmt", "%.3f")"#)
            .unwrap()
            .options()
            .float_digits
            .is_some_and(|d| d == 3)
    );
    assert!(Plugins::load_from_source(r#"vd.option("disp_float_fmt", "2")"#).is_err());
}

#[test]
fn set_theme_refuses_an_unknown_name() {
    assert!(Plugins::load_from_source("vd.set_theme('neon')").is_err());
}

/// S-40 G: a table patches `dark` (or its `base`); a bad key or colour
/// is an error naming it.
#[test]
fn set_theme_table_patches_a_base() {
    use crate::theme::Theme;
    use ratatui::style::Color;
    let th = |src: &str| Plugins::load_from_source(src).map(|p| p.options().theme.unwrap());
    let t = th(r##"vd.set_theme { statusbar_bg = "#010203", header_fg = "blue" }"##).unwrap();
    assert_eq!(
        (t.statusbar_bg, t.header_fg),
        (Color::Rgb(1, 2, 3), Color::Blue)
    );
    assert_eq!(t.row_cursor_bg, Theme::dark().row_cursor_bg);
    let t = th(r#"vd.set_theme { base = "light", null_fg = "red" }"#).unwrap();
    assert_eq!(t.null_fg, Color::Red);
    assert_eq!(t.statusbar_bg, Theme::light().statusbar_bg);
    let e = th(r#"vd.set_theme { statusbar = "red" }"#)
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("unknown key statusbar"), "{e}");
    let e = th(r#"vd.set_theme { null_fg = "nope" }"#)
        .err()
        .unwrap()
        .to_string();
    assert!(e.contains("null_fg"), "{e}");
}

#[test]
fn bind_rejects_unknown_action() {
    let err = Plugins::load_from_source(r#"vd.bind("sheet", "x", "no_such_action")"#);
    assert!(err.is_err());
}

#[test]
fn bind_rejects_unknown_layer() {
    let err = Plugins::load_from_source(r#"vd.bind("floppy", "x", "goto_top")"#);
    assert!(err.is_err());
}

#[test]
fn register_loader_matches_by_extension_case_insensitive() {
    let p = Plugins::load_from_source(
        r#"vd.register_loader {
             name = "mylog",
             extensions = { "mylog", "ml" },
             open = function(path) return path end,
           }"#,
    )
    .unwrap();
    assert!(p.resolve_loader(Path::new("x.mylog")).is_some());
    assert!(p.resolve_loader(Path::new("x.ML")).is_some());
    assert!(p.resolve_loader(Path::new("x.csv")).is_none());
    assert!(p.resolve_loader(Path::new("noext")).is_none());
}

#[test]
fn register_loader_accepts_single_string_extension() {
    let p = Plugins::load_from_source(
        r#"vd.register_loader {
             extensions = "foo",
             open = function(path) return path end,
           }"#,
    )
    .unwrap();
    assert!(p.resolve_loader(Path::new("x.foo")).is_some());
}

#[test]
fn call_loader_passes_path_and_returns_result() {
    let p = Plugins::load_from_source(
        r#"vd.register_loader {
             extensions = { "mylog" },
             open = function(path) return path .. ".csv" end,
           }"#,
    )
    .unwrap();
    let rule = p.resolve_loader(Path::new("input.mylog")).unwrap();
    let out = p.call_loader(&rule, Path::new("input.mylog")).unwrap();
    assert!(matches!(out, LoaderOutput::Path(p) if p == Path::new("input.mylog.csv")));
}

#[test]
fn loader_may_return_rows_with_types() {
    let p = Plugins::load_from_source(
        r#"vd.register_loader { name = "pods", extensions = "pods", open = function(path)
             return { columns = {"name", "ready", "n"},
                      rows = { {path, true, 1}, {"db", false, nil} } }
           end }"#,
    )
    .unwrap();
    let loaded = open(Some(&p), "x.pods".as_ref(), None).unwrap();
    assert!(loaded.via_loader && loaded.path.is_none());
    assert_eq!(loaded.display_name, "x.pods");
    let df = loaded.lazy.collect().unwrap();
    let dtypes: Vec<_> = df.dtypes();
    assert_eq!(
        dtypes,
        [DataType::String, DataType::Boolean, DataType::Int64]
    );
    assert_eq!(
        df.column("name").unwrap().str().unwrap().get(0),
        Some("x.pods")
    );
    assert_eq!(df.column("n").unwrap().null_count(), 1);
    // a bad table names the loader, not vd.push_sheet
    let bad = Plugins::load_from_source(
        r#"vd.register_loader { extensions = "bad", open = function() return { rows = {} } end }"#,
    )
    .unwrap();
    let err = open(Some(&bad), "x.bad".as_ref(), None)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("loader open: columns"), "{err}");
    let num = Plugins::load_from_source(
        r#"vd.register_loader { extensions = "num", open = function() return 3 end }"#,
    )
    .unwrap();
    let err = open(Some(&num), "x.num".as_ref(), None)
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("path or a { columns, rows } table"), "{err}");
}

/// S-40 C: a loader's parts (paths and rows mixed) become a list, one
/// part opens directly, `file:part` picks one, no parts is an error
/// naming the loader; a name beats a pattern, a pattern beats the
/// built-in reader, and the later of two patterns wins.
#[test]
fn loader_parts_names_and_patterns() {
    let dir = tempfile::tempdir().unwrap();
    let csv = dir.path().join("a.csv");
    std::fs::write(&csv, "x\n1\n2\n").unwrap();
    let blocks = dir.path().join("b.cvsv");
    std::fs::write(&blocks, "").unwrap();
    let src = format!(
        r#"vd.register_loader {{ name = "cvsv", extensions = "cvsv", open = function(p)
             return {{ parts = {{ {{ name = "one", path = {csv:?} }},
                                 {{ name = "two", columns = {{"y"}}, rows = {{ {{7}} }} }} }} }}
           end }}
           vd.register_loader {{ name = "none", open = function(p) return {{ parts = {{}} }} end }}
           vd.register_loader {{ name = "g1", patterns = "export_*.csv", open = function(p) return {{ columns = {{"g"}}, rows = {{ {{1}} }} }} end }}
           vd.register_loader {{ name = "g2", patterns = "export_*.csv", open = function(p) return {{ columns = {{"g2"}}, rows = {{ {{1}} }} }} end }}"#
    );
    let p = Plugins::load_from_source(&src).unwrap();
    let path = blocks.display().to_string();
    let list = open(Some(&p), path.as_ref(), None).unwrap();
    let names: Vec<&str> = list.parts.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["one", "two"]);
    assert_eq!(list.parts[0].1.lazy.clone().collect().unwrap().height(), 2);
    let two = open(Some(&p), format!("{path}:two").as_ref(), None).unwrap();
    assert_eq!(two.display_name, format!("{path}:two"));
    assert_eq!(two.lazy.collect().unwrap().get_column_names(), ["y"]);
    let err = open(Some(&p), path.as_ref(), Some("none"))
        .err()
        .unwrap()
        .to_string();
    assert!(err.contains("none: open returned no parts"), "{err}");
    let err = open(Some(&p), path.as_ref(), Some("nope"))
        .err()
        .unwrap()
        .to_string();
    assert!(
        err.contains("unknown format nope") && err.contains("cvsv"),
        "{err}"
    );
    // named beats the file name; a pattern beats the built-in csv reader
    let export = dir.path().join("export_1.csv");
    std::fs::write(&export, "x\n1\n").unwrap();
    let e = export.display().to_string();
    let cols = |l: LoadedSource| {
        l.lazy
            .collect()
            .unwrap()
            .get_column_names()
            .iter()
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
    };
    assert_eq!(cols(open(Some(&p), e.as_ref(), None).unwrap()), ["g2"]);
    assert_eq!(cols(open(Some(&p), e.as_ref(), Some("g1")).unwrap()), ["g"]);
    assert_eq!(
        cols(open(Some(&p), e.as_ref(), Some("csv")).unwrap()),
        ["x"]
    );
}

#[test]
fn register_loader_rejects_empty_extensions() {
    let err = Plugins::load_from_source(
        r#"vd.register_loader {
             extensions = {},
             open = function(path) return path end,
           }"#,
    );
    assert!(err.is_err());
}

#[test]
fn loader_roundtrip_via_tempfile_and_reader() {
    // Lua reads a `.ml` file (a KEY=VAL line format), writes CSV to a tempfile,
    // returns the tempfile path. We then scan it with the native reader.
    use std::io::Write;
    let input = tempfile::NamedTempFile::new().unwrap();
    {
        let mut f = std::fs::File::create(input.path().with_extension("ml")).unwrap();
        writeln!(f, "a=1 b=x").unwrap();
        writeln!(f, "a=2 b=y").unwrap();
    }
    let input_path = input.path().with_extension("ml");

    let out = tempfile::NamedTempFile::new().unwrap();
    let out_path = out.path().with_extension("csv");

    let src = format!(
        r#"vd.register_loader {{
             extensions = {{ "ml" }},
             open = function(path)
               local out = {out:?}
               local f = io.open(out, "w")
               f:write("a,b\n")
               for line in io.lines(path) do
                 local a, b = line:match("a=(%S+) b=(%S+)")
                 f:write(a .. "," .. b .. "\n")
               end
               f:close()
               return out
             end,
           }}"#,
        out = out_path.to_string_lossy().replace('\\', "/"),
    );
    let p = Plugins::load_from_source(&src).unwrap();
    let rule = p.resolve_loader(&input_path).unwrap();
    let LoaderOutput::Path(got) = p.call_loader(&rule, &input_path).unwrap() else {
        panic!("expected a path")
    };
    assert_eq!(got, out_path);

    let loaded = crate::reader::open(got.as_os_str(), None).unwrap();
    let df = loaded.lazy.collect().unwrap();
    assert_eq!(df.height(), 2);
    assert_eq!(df.width(), 2);
    let _ = std::fs::remove_file(&input_path);
    let _ = std::fs::remove_file(&out_path);
}

/// S-40 F: each guide recipe's loader on its `docs/fixtures/` sample.
#[test]
fn guide_recipes_open_their_fixtures() {
    let html = include_str!("../../docs/index.html");
    let recipe = |name: &str| {
        let start = format!("<pre data-recipe=\"{name}\"><code>");
        let b = html.split(&start).nth(1).expect(name);
        let b = b.split("</code></pre>").next().unwrap();
        let src = b
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&amp;", "&");
        Plugins::load_from_source(&src).unwrap_or_else(|e| panic!("{name}: {e}"))
    };
    let fixture = |f: &str| format!("{}/docs/fixtures/{f}", env!("CARGO_MANIFEST_DIR"));
    let frame = |s: &LoadedSource| s.lazy.clone().collect().unwrap();
    let names = |df: &DataFrame| -> Vec<String> {
        df.get_column_names()
            .iter()
            .map(|n| n.to_string())
            .collect()
    };

    let p = recipe("cvsv");
    let s = open(Some(&p), fixture("blocks.cvsv").as_ref(), None).unwrap();
    let parts: Vec<&str> = s.parts.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(parts, ["people", "pets"]);
    assert_eq!(frame(&s.parts[0].1).shape(), (3, 2));

    let p = recipe("pdb");
    let df = frame(&open(Some(&p), fixture("crambin.pdb").as_ref(), None).unwrap());
    assert_eq!(df.height(), 50);
    assert_eq!(names(&df)[..3], ["serial", "atom", "residue"]);
    assert_eq!(df.column("x").unwrap().dtype(), &DataType::Float64);
    assert_eq!(df.column("serial").unwrap().dtype(), &DataType::Int64);
    assert_eq!(
        df.column("residue").unwrap().str().unwrap().get(0),
        Some("THR")
    );

    let p = recipe("fasta");
    let df = frame(&open(Some(&p), fixture("sample.fasta").as_ref(), None).unwrap());
    assert_eq!(
        names(&df),
        ["id", "description", "length", "gc", "sequence"]
    );
    assert_eq!(df.height(), 4);
    let len = df.column("length").unwrap().i64().unwrap();
    assert_eq!(len.get(0), Some(24)); // two lines joined
    let gc = df.column("gc").unwrap().f64().unwrap();
    assert_eq!((gc.get(2), gc.get(3)), (Some(0.5), None)); // protein: no gc

    let p = recipe("access-log");
    let s = open(Some(&p), fixture("access.log").as_ref(), None).unwrap();
    let (req, bad) = (frame(&s.parts[0].1), frame(&s.parts[1].1));
    assert_eq!((req.height(), bad.height()), (6, 1));
    assert_eq!(names(&req)[..2], ["host", "time"]);
    let time = req.column("time").unwrap().str().unwrap();
    assert_eq!(time.get(0), Some("2026-10-10T20:55:36Z"));
    assert_eq!(time.get(2), Some("2026-10-10T21:10:02Z")); // +0200
    let bytes = req.column("bytes").unwrap().i64().unwrap();
    assert_eq!(bytes.get(3), None); // "-"
    assert_eq!(bad.column("line").unwrap().i64().unwrap().get(0), Some(6));
}

/// Every Lua block in docs/index.html loads.
#[test]
fn guide_lua_blocks_load() {
    let html = include_str!("../../docs/index.html");
    let blocks: Vec<String> = html
        .split("<pre><code>")
        .skip(1)
        .filter_map(|b| b.split("</code></pre>").next())
        .filter(|b| ["--", "vd.", "local "].iter().any(|p| b.starts_with(p)))
        .map(|b| {
            b.replace("&lt;", "<")
                .replace("&gt;", ">")
                .replace("&amp;", "&")
        })
        .collect();
    assert!(blocks.len() >= 5, "{}", blocks.len());
    for b in blocks {
        if let Err(e) = Plugins::load_from_source(&b) {
            panic!("{e}\n{b}");
        }
    }
}

/// The `init.lua` example in docs/index.html loads and does what it says.
#[test]
fn guide_example_runs() {
    let html = include_str!("../../docs/index.html");
    let start = "<pre><code>-- ~/.config/vdrs/init.lua\n";
    let code = html
        .split(start)
        .nth(1)
        .unwrap()
        .split("</code></pre>")
        .next()
        .unwrap();
    let code = code
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&");
    let p = Plugins::load_from_source(&code).unwrap();
    use polars::prelude::NamedFrom;
    let range = p.aggregator("range").unwrap();
    let vals = Series::new("v".into(), [Some(3.0f64), None, Some(9.0)]);
    assert_eq!(range.call(&vals).unwrap(), AggValue::Num(6.0));
    let empty = Series::new("v".into(), [None::<f64>]);
    assert_eq!(range.call(&empty).unwrap(), AggValue::Null);
    let price = |v| p.render_text("price", &DataType::Float64, &v);
    assert_eq!(price(AnyValue::Float64(2.5)), Some("$2.50".into()));
    assert_eq!(price(AnyValue::Null), None);
}

#[test]
fn push_sheet_infers_types_and_rejects_bad_input() {
    let src = |rows: &str| {
        format!(
            r#"vd.bind("sheet", "x", function()
                 vd.push_sheet {{ name = "t", columns = {{ "i", "f", "b", "s", "e" }}, rows = {rows} }}
               end)"#
        )
    };
    let ctx = SheetCtx {
        name: "s",
        row: 1,
        col: 1,
        column_names: vec![],
        value: AnyValue::Null,
    };
    let p = Plugins::load_from_source(&src(r#"{ { 1, 1.5, true, "a" }, { nil, 2, false, 3 } }"#))
        .unwrap();
    let mut out = p.call(0, &ctx, None).unwrap();
    let (name, df) = out.sheets.pop().unwrap();
    assert_eq!(name, "t");
    let types: Vec<String> = df.dtypes().iter().map(|d| d.to_string()).collect();
    assert_eq!(types, ["i64", "f64", "bool", "str", "str"]);
    assert_eq!(df.column("i").unwrap().null_count(), 1);
    assert_eq!(df.column("s").unwrap().str().unwrap().get(1), Some("3"));
    for bad in [r#"{ { 1, 2, 3, 4, 5, 6 } }"#, r#"{ { {} } }"#] {
        let p = Plugins::load_from_source(&src(bad)).unwrap();
        assert!(p.call(0, &ctx, None).is_err(), "{bad}");
    }
    assert!(Plugins::load_from_source("vd.push_sheet { columns = { 'a' }, rows = {} }").is_err());
    let dup = r#"vd.bind("sheet", "x", function() vd.push_sheet { columns = { "a", "a" }, rows = {} } end)"#;
    assert!(
        Plugins::load_from_source(dup)
            .unwrap()
            .call(0, &ctx, None)
            .is_err()
    );
}

#[test]
fn vim_profile_binds_known_actions_and_twice_is_once() {
    use crate::keymap::{Dispatch, Layer};
    let km = |src: &str| {
        let p = Plugins::load_from_source(src).unwrap();
        let mut km = Keymap::defaults();
        p.apply_keymap(&mut km);
        km
    };
    let once = km(r#"vd.option("keymap", "vim")"#);
    let twice = km(r#"vd.option("keymap", "vim") vd.option("keymap", "vim")"#);
    let fire = |km: &Keymap, keys: &[&str]| {
        let mut pending = Vec::new();
        for k in keys {
            match km.resolve(Layer::Sheet, &pending, k.parse().unwrap()) {
                Dispatch::Fire(a) => return Some(a),
                Dispatch::Pending(_) => pending.push(k.parse().unwrap()),
                Dispatch::Miss => return None,
            }
        }
        None
    };
    let keys: [&[&str]; 6] = [&["u"], &["U"], &["$"], &["g", "v"], &["<C-w>", "s"], &["x"]];
    for k in keys {
        assert_eq!(fire(&once, k), fire(&twice, k), "{k:?}");
    }
    assert_eq!(fire(&once, &["u"]), Some(vec![Action::Undo]));
    assert_eq!(fire(&once, &["$"]), Some(vec![Action::GotoRightmost]));
    assert_eq!(fire(&once, &["R"]), Some(vec![Action::Redraw]), "unbound");
    assert_eq!(
        fire(&Keymap::defaults(), &["u"]),
        Some(vec![Action::UnselectRow])
    );
    assert!(Plugins::load_from_source(r#"vd.option("keymap", "emacs")"#).is_err());
}

#[test]
fn user_guide_lists_every_vim_profile_key() {
    let html = include_str!("../../docs/index.html");
    let p = Plugins::load_from_source(r#"vd.option("keymap", "vim")"#).unwrap();
    let mut km = Keymap::defaults();
    let n = km.chords(crate::keymap::Layer::Sheet).len();
    p.apply_keymap(&mut km);
    for c in &km.chords(crate::keymap::Layer::Sheet)[n..] {
        let keys: Vec<String> = c.on.iter().map(|k| k.to_string()).collect();
        let attr = format!(
            "data-vim-keys=\"{}\"",
            keys.join(" ")
                .replace('&', "&amp;")
                .replace('"', "&quot;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        );
        assert!(html.contains(&attr), "docs/index.html misses {attr}");
    }
}

#[test]
fn renderer_tables_carry_style_and_bad_colors_are_dropped() {
    use ratatui::style::{Color, Modifier};
    let p = Plugins::load_from_source(
        r##"vd.register_renderer{column="x",render=function(v)
             if v == 1 then return { fg = "red", bg = "#000010", underline = true } end
             if v == 2 then return { text = "two", fg = "rouge" } end
             return v * 10 end}"##,
    )
    .unwrap();
    let r = |n| {
        p.render("x", &DataType::Int64, &AnyValue::Int64(n))
            .unwrap()
    };
    let one = r(1);
    assert_eq!(one.text, None, "no text: the default display");
    assert_eq!(
        one.style,
        Style::default()
            .fg(Color::Red)
            .bg(Color::Rgb(0, 0, 16))
            .add_modifier(Modifier::UNDERLINED)
    );
    assert_eq!(
        r(2),
        Rendered {
            text: Some("two".into()),
            style: Style::default()
        }
    );
    assert_eq!(r(3).text.as_deref(), Some("30"));
}

#[test]
fn renderer_memo_keeps_close_floats_apart() {
    let p = Plugins::load_from_source(
        r#"vd.register_renderer{column="x",render=function(v) return string.format("%.8f",v) end}"#,
    )
    .unwrap();
    let r = |f| p.render_text("x", &DataType::Float64, &AnyValue::Float64(f));
    assert_eq!(r(1.00000001).as_deref(), Some("1.00000001"));
    assert_eq!(r(1.00000002).as_deref(), Some("1.00000002"));
}

/// S-40 A: ids, `once`, `vd.off`, groups that clear, globs on the name
/// or the path, `pattern` refused where there's no path.
#[test]
fn handler_options_filter_and_remove() {
    let src = r#"
        local g = vd.augroup("logs")
        vd.on("open", { group = g, pattern = "*.log.csv" }, function(s) vd.toast("first " .. s.path) end)
        g = vd.augroup("logs")  -- as if sourced again: replaces
        vd.on("open", { group = g, pattern = { "*.log.csv", "/data/*.tsv" } }, function(s) vd.toast("log") end)
        vd.on("save", { once = true }, function() vd.toast("once") end)
        gone = vd.on("save", function() vd.toast("gone") end)
        assert(vd.off(gone) and not vd.off(gone))
        ok, err = pcall(vd.on, "quit", { pattern = "*" }, function() end)
        assert(not ok and tostring(err):find("no path"))
    "#;
    let p = Plugins::load_from_source(src).unwrap();
    let ctx = SheetCtx {
        name: "s",
        row: 1,
        col: 1,
        column_names: vec![],
        value: AnyValue::Null,
    };
    let open = |path: &str| p.fire("open", &ctx, &[("path", path.into())]).0.toasts;
    assert_eq!(open("/x/app.log.csv"), ["log"]);
    assert_eq!(open("/data/b.tsv"), ["log"]);
    assert!(open("/x/b.tsv").is_empty());
    assert!(open("").is_empty());
    let save = || p.fire("save", &ctx, &[]).0.toasts;
    assert_eq!(save(), ["once"]);
    assert!(save().is_empty());
    let rows = p.handler_rows();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(
        rows[0][1..],
        ["open", "logs", "*.log.csv /data/*.tsv", "false"]
    );
}

/// S-40 B1: the walk up stops at `$HOME`, which counts only when it is
/// where vdrs starts.
#[test]
fn project_file_walk_stops_at_home() {
    let home = tempfile::tempdir().unwrap();
    let deep = home.path().join("a/b");
    std::fs::create_dir_all(&deep).unwrap();
    let h = Some(home.path().to_path_buf());
    std::fs::write(home.path().join(".vdrs.lua"), "").unwrap();
    assert_eq!(find_project(&deep, h.clone()), None);
    assert_eq!(
        find_project(home.path(), h.clone()),
        Some(home.path().join(".vdrs.lua"))
    );
    std::fs::write(home.path().join("a/.vdrs.lua"), "").unwrap();
    assert_eq!(
        find_project(&deep, h),
        Some(home.path().join("a/.vdrs.lua"))
    );
}

/// S-40 B2, B3: an untrusted file doesn't run; trusted, it runs after
/// init.lua (its option and loader win); an edit untrusts it; `vd.reset`
/// clears what init.lua set, and is refused inside a function.
#[test]
fn project_file_trust_and_merge_order() {
    let dir = tempfile::tempdir().unwrap();
    let init = dir.path().join("init.lua");
    std::fs::write(
        &init,
        r#"vd.option("histogram_bins", "3"); vd.option("mouse", "true")
           vd.register_loader { extensions = "foo", open = function(p) return "global" end }"#,
    )
    .unwrap();
    let project = dir.path().join(".vdrs.lua");
    std::fs::write(
        &project,
        r#"vd.option("histogram_bins", "5")
           vd.register_loader { extensions = "foo", open = function(p) return "project.csv" end }"#,
    )
    .unwrap();
    let load = || Plugins::load_with(Some(init.clone()), Some(project.clone()));
    let (p, warn) = load();
    assert!(warn.unwrap().contains("not trusted: :trust"));
    assert_eq!(p.options().histogram_bins, 3);
    set_trust(&project, true).unwrap();
    let (p, warn) = load();
    assert_eq!(warn, None);
    assert_eq!(p.project_file().map(|(_, t)| t), Some(true));
    let o = p.options();
    assert_eq!((o.histogram_bins, o.mouse), (5, true));
    assert_eq!(o.from["histogram_bins"], ".vdrs.lua");
    assert_eq!(o.from["mouse"], "init.lua");
    let rule = p.resolve_loader(Path::new("x.foo")).unwrap();
    match p.call_loader(&rule, Path::new("x.foo")).unwrap() {
        LoaderOutput::Path(p) => assert_eq!(p, PathBuf::from("project.csv")),
        LoaderOutput::Rows(_) | LoaderOutput::Parts(_) => panic!("not a path"),
    }
    std::fs::write(
        &project,
        "vd.reset()\nvd.bind('sheet', 'x', function() vd.reset() end)",
    )
    .unwrap();
    let (p, warn) = load();
    assert!(
        warn.unwrap().contains("not trusted"),
        "an edit needs :trust again"
    );
    assert_eq!(p.options().histogram_bins, 3);
    set_trust(&project, true).unwrap();
    let (p, warn) = load();
    assert_eq!(warn, None);
    assert!(!p.options().mouse, "vd.reset dropped init.lua's options");
    assert!(p.resolve_loader(Path::new("x.foo")).is_none());
    let ctx = SheetCtx {
        name: "s",
        row: 1,
        col: 1,
        column_names: vec![],
        value: AnyValue::Null,
    };
    let e = p.call(0, &ctx, None).err().unwrap().to_string();
    assert!(e.contains("top level"), "{e}");
    set_trust(&project, false).unwrap();
    assert!(!is_trusted(&project));
}

fn push_error(body: &str) -> String {
    let src = format!(r#"vd.bind("sheet", "x", function() vd.push_sheet {{ {body} }} end)"#);
    let p = Plugins::load_from_source(&src).unwrap();
    let ctx = SheetCtx {
        name: "s",
        row: 1,
        col: 1,
        column_names: vec![],
        value: AnyValue::Null,
    };
    p.call(0, &ctx, None)
        .err()
        .map_or(String::new(), |e| e.to_string())
}

#[test]
fn push_sheet_rejects_quiet_data_loss() {
    for (body, want) in [
        (r#"columns={"a"}, rows={{1},nil,{3}}"#, "rows has gaps"),
        (r#"columns={"a",nil,"c"}, rows={}"#, "columns has gaps"),
        (r#"columns={"a"}, rows={{a=1}}"#, "values go by position"),
        (
            r#"columns={"a","b"}, rows={{1,2,[5]=9}}"#,
            "more values than columns",
        ),
        (r#"columns={"__vdrs_rid"}, rows={{1}}"#, "reserved"),
        (r#"rows={{1}}"#, "columns must be a table"),
        (r#"columns={{"a"}}, rows={}"#, "expected text"),
        (r#"name={}, columns={"a"}, rows={}"#, "expected text"),
        (r#"columns={"a"}"#, "rows must be a table"),
        (r#"columns={"a"}, rows={1}"#, "row 1 isn't a table"),
    ] {
        let got = push_error(body);
        assert!(got.contains(want), "{body}: {got}");
    }
}

#[test]
fn push_sheet_takes_bad_utf8_and_big_sheets() {
    assert_eq!(push_error(r#"columns={"a"}, rows={{"caf\xe9"}}"#), "");
    // More cells than mlua has reference slots if each were held as a value.
    let big = r#"columns={"a","b"}, rows=(function()
        local r = {}
        for i = 1, 600000 do r[i] = {"x" .. i, "y" .. i} end
        return r
    end)()"#;
    assert_eq!(push_error(big), "");
}

#[test]
fn a_failing_binding_discards_its_pushed_sheets() {
    let p = Plugins::load_from_source(
        r#"vd.bind("sheet", "x", function()
            vd.push_sheet { columns = {"a"}, rows = {{1}} }
            error("boom")
        end)"#,
    )
    .unwrap();
    let ctx = SheetCtx {
        name: "s",
        row: 1,
        col: 1,
        column_names: vec![],
        value: AnyValue::Null,
    };
    assert!(p.call(0, &ctx, None).is_err());
}
