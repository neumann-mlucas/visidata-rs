//! Files on disk: the default `init.lua`, the vim profile, `macros.lua`,
//! `options.lua`, project `.vdrs.lua` and `:trust`.

use super::*;

/// `vd.option("keymap", "vim")` (S-26 D): the keys where vim and VisiData
/// disagree, as vd.bind patches over the defaults. Copy it into init.lua to
/// change it.
pub const VIM_PROFILE: &str = r##"-- vdrs vim profile
local function b(keys, action) vd.bind("sheet", keys, action, "vim: " .. action) end
b(".", "repeat_last")
b("$", "goto_rightmost")             -- default: currency cast
b("<C-d>", "half_page_down")
b("<C-u>", "half_page_up")
b("<C-r>", "redo")                   -- default: :refresh
b("<C-o>", "jump_back")
b("<C-i>", "jump_forward")           -- apart from <Tab> where the terminal tells them apart
b({"g", "v"}, "reselect")            -- default: unhide columns (:unhide)
b("u", "undo")                       -- default: unselect row
b("U", "unselect_row")               -- default: undo
vd.unbind("sheet", "R")              -- default: redo
b("x", "cut_cell")                   -- default: cut row
b("H", "screen_top")                 -- default: slide column left
b("M", "screen_middle")              -- default: melt (:melt here)
b("L", "screen_bottom")              -- default: slide column right
b({"z", "t"}, "scroll_top")
b({"z", "b"}, "scroll_bottom")
b("w", "next_cell")
b("b", "prev_cell")                  -- default: sidebar (<C-g> cycles it)
b("e", "last_cell")                  -- default: edit (i here)
b("i", "start_edit_input")
b("}", "next_value")                -- default: next selected row
b("{", "prev_value")                -- default: previous selected row
b("*", "search_cell")
b("#", "searchr_cell")               -- default: int cast (z# here)
b({"z", "#"}, "type_cast_int")
b({"<C-w>", "s"}, "split")
b({"<C-w>", "v"}, "vsplit")
b({"<C-w>", "w"}, "focus_pane")
b({"<C-w>", "o"}, "close_pane")
b({"<C-w>", "q"}, "close_pane")
b({"Z", "Z"}, "pop_sheet")
b("d", "operator_delete")             -- default: delete row (dd here)
b("y", "operator_yank")               -- default: copy row (yy here)
b("c", "operator_change")            -- default: go to column by regex
b('"', "register_prefix")            -- default: selected rows as a sheet (:dupsel)
b("m", "set_mark")                   -- default: record macro (q here)
b("'", "go_mark")                   -- default: frozen copy (:freeze-col here)
b("q", "record_macro")               -- default: pop sheet (ZZ, :q here)
b("@", "play_macro")                 -- default: date cast (z@ here)
b({"z", "@"}, "type_cast_date")
b({"z", "s"}, "freeze_column")      -- default: select rows above the cursor
b({"z", "c"}, "unfreeze_all")       -- default: go to column number
b({"z", "o"}, "clear_sort")         -- default: open the cell's file
b("<C-v>", "visual_block")           -- default: show version
vd.bind("input", "<Esc>", "input_normal", "vim: normal mode")  -- Esc again cancels
"##;

/// First TUI start: write [`default_init_lua`] where `init.lua` goes, unless
/// a file is there. A failed write (read-only home) is not worth a warning.
pub fn ensure_default_config() {
    if let Some(p) = default_config_path() {
        let _ = write_default_config(&p);
    }
}

/// Create `path` holding [`default_init_lua`]; never replaces a file.
pub(super) fn write_default_config(path: &Path) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(default_init_lua().as_bytes())
}

/// The `init.lua` written on first run: every default, commented out, so a
/// later release's defaults still apply until a line is uncommented.
pub fn default_init_lua() -> String {
    let quote = |s: &str| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""));
    let mut out = String::from(
        "-- vdrs config: loaded at startup, reloaded when saved.\n\
         -- vdrs wrote this file because there was none. Every line below is a\n\
         -- default, commented out: uncomment and edit one to change it. Deleting\n\
         -- the file is safe; the next start writes a fresh one.\n\
         -- Pre-alpha: the Lua API may change in any release until beta.\n\
         -- Guide: https://neumann-mlucas.github.io/visidata-rs/\n\
         \n\
         -- Theme: \"dark\" or \"light\".\n\
         -- vd.set_theme(\"dark\")\n\
         \n\
         -- System clipboard (Y, gY, zY, gzY): a command that reads stdin, or \"osc52\".\n\
         -- Default: osc52 over SSH, else wl-copy / xclip / pbcopy / clip.exe.\n\
         -- vd.option(\"clipboard_copy_cmd\", \"xclip -selection clipboard -filter\")\n\
         -- zP, gzP read it back with a command that prints it. Default: none over SSH\n\
         -- (paste with the terminal), else wl-paste / xclip -o / pbpaste / powershell.\n\
         -- vd.option(\"clipboard_paste_cmd\", \"xclip -selection clipboard -o\")\n\
         \n\
         -- .duckdb files: the DuckDB program vdrs runs. Default: duckdb on PATH.\n\
         -- vd.option(\"duckdb_cmd\", \"/usr/local/bin/duckdb\")\n\
         \n\
         -- .csv files: drop blanks at the start of each cell and name (VisiData's).\n\
         -- vd.option(\"csv_skipinitialspace\", \"true\")\n\
         \n\
         -- Keys: \"visidata\" (default) or \"vim\": rebinds the keys where vim and VisiData\n\
         -- disagree (u undo, d/y/c operators, q macros, m/' marks, w/b/e cell motions).\n\
         -- vd.option(\"keymap\", \"vim\")\n\
         \n\
         -- Decimals of every float column (\"%.Nf\" or \"{:.Nf}\"). Default: shortest\n\
         -- exact text, up to 15 significant digits. Alt++ / Alt+- / :precision per column.\n\
         -- vd.option(\"disp_float_fmt\", \"%.2f\")\n\
         \n\
         -- F on a number column: ranges instead of values. Default off. histogram_bins:\n\
         -- how many (0: the square root of the row count).\n\
         -- vd.option(\"numeric_binning\", \"true\")\n\
         -- vd.option(\"histogram_bins\", \"0\")\n\
         \n\
         -- First number of i, gi and :addcol-incr-key. Default 1.\n\
         -- vd.option(\"incr_base\", \"1\")\n\
         \n\
         -- Rows read as column names (csv, tsv, xlsx, fixed width, usv); 0 names\n\
         -- them column_1, column_2, … Default 1.\n\
         -- vd.option(\"header\", \"1\")\n\
         \n\
         -- zM on an empty list keeps the row, key and value empty. Default off.\n\
         -- vd.option(\"unfurl_empty\", \"true\")\n\
         \n\
         -- :join-cols puts name_joiner between the column names and value_joiner\n\
         -- between the values. Defaults \"_\" and one space.\n\
         -- vd.option(\"name_joiner\", \"_\")\n\
         -- vd.option(\"value_joiner\", \" \")\n\
         \n\
         -- A Lua loader's poll sheet keeps this many rows, the newest. 0: all.\n\
         -- vd.option(\"source_max_rows\", \"1000000\")\n\
         \n\
         -- A totals row under the table: each column's + aggregators. Default off; :footer\n\
         -- turns it on or off.\n\
         -- vd.option(\"disp_footer\", \"true\")\n\
         \n\
         -- A ─ rule under the header row. Default off.\n\
         -- vd.option(\"disp_header_rule\", \"true\")\n\
         \n\
         -- An edit that fails a :validate rule: \"refuse\" (default) or \"warn\" (it\n\
         -- lands, with a toast; failing cells show red either way).\n\
         -- vd.option(\"validation\", \"warn\")\n\
         \n\
         -- Mouse: wheel moves the cursor, a click picks a cell. Default off, so the\n\
         -- terminal's own text selection keeps working.\n\
         -- vd.option(\"mouse\", \"true\")\n\
         \n\
         -- Save the open sheets on quit, for vdrs -S. Default: true.\n\
         -- vd.option(\"session_autosave\", \"false\")\n\
         \n\
         -- Keys: vd.bind(layer, keys, action, description?)\n\
         --   keys: \"x\", or {\"g\", \"g\"} for a sequence; <C-d>, <CR>, <Esc>, <Tab>, …\n\
         --   action: a name below, \":ex command\", or a Lua function.\n\
         -- vd.unbind(layer, keys) removes a binding.\n",
    );
    let keymap = crate::keymap::Keymap::defaults();
    for (layer, title) in [(Layer::Sheet, "sheet"), (Layer::Input, "input")] {
        out += &format!("\n-- {title} layer\n");
        for chord in keymap.chords(layer) {
            let keys: Vec<String> = chord.on.iter().map(|k| quote(&k.to_string())).collect();
            let keys = match keys.as_slice() {
                [one] => one.clone(),
                many => format!("{{{}}}", many.join(", ")),
            };
            let name = match chord.run.as_slice() {
                [a] => Action::NAMED.iter().find(|(_, n)| n == a).map(|(n, _)| *n),
                _ => None,
            };
            out += &match name {
                Some(n) => format!(
                    "-- vd.bind(\"{title}\", {keys}, \"{n}\")  -- {}\n",
                    chord.desc
                ),
                None => format!("-- {keys}: {} (built in)\n", chord.desc),
            };
        }
    }
    let unbound: Vec<&str> = Action::NAMED
        .iter()
        .filter(|(_, a)| {
            !keymap
                .chords(Layer::Sheet)
                .iter()
                .chain(keymap.chords(Layer::Input))
                .any(|c| c.run == [a.clone()])
        })
        .map(|(n, _)| *n)
        .collect();
    let wrap = |text: String| -> String {
        let (mut lines, mut line) = (Vec::new(), String::from("--"));
        for word in text.split(' ') {
            if line.len() + word.len() > 77 {
                lines.push(std::mem::replace(&mut line, String::from("--")));
            }
            line.push(' ');
            line.push_str(word);
        }
        lines.push(line);
        lines.join("\n") + "\n"
    };
    out += "\n";
    out += &wrap(format!(
        "Actions without a default key: {}.",
        unbound.join(", ")
    ));
    out += "\n";
    out += &wrap(format!(
        "Ex commands (`:`, Tab completes): {}, and every action name above with - for _ (:select-all). Add your own:",
        crate::exline::COMMANDS.join(" ")
    ));
    out += "-- vd.command(\"hello\", function(args) vd.toast(\"hello \" .. args) end)\n\n";
    out += &wrap(
        "More (see the guide): vd.register_loader { name, extensions, open }, vd.register_aggregator(name, fn), vd.register_renderer { column, render }, vd.sheet(), vd.toast(msg), vd.push_sheet { name, columns, rows }, vd.on(event, [opts,] fn), vd.off(id), vd.augroup(name), vd.exec(\":ex command\")."
            .into(),
    );
    out
}

/// Where `m` saves macros: `macros.lua` next to `init.lua` (a temp dir under
/// `cfg!(test)`, so tests never touch the user's config).
pub fn macros_path() -> Option<PathBuf> {
    if cfg!(test) {
        return Some(crate::state_path("macros.lua"));
    }
    Some(default_config_path()?.with_file_name("macros.lua"))
}

/// Where `O`'s `z<C-s>` saves options: `options.lua` next to `init.lua`
/// (a temp dir under `cfg!(test)`).
pub fn options_path() -> Option<PathBuf> {
    if cfg!(test) {
        return Some(crate::state_path("options.lua"));
    }
    Some(default_config_path()?.with_file_name("options.lua"))
}

/// Save options to `path`: a `vd.option` line each, replacing one for the
/// same option; other lines (hand edits) stay.
pub fn save_options(path: &Path, opts: &[(String, String)]) -> std::io::Result<()> {
    let old = std::fs::read_to_string(path).unwrap_or_default();
    let prefix = |n: &str| format!("vd.option({}, ", lua_str(n));
    let mut out: Vec<String> = old
        .lines()
        .filter(|l| !opts.iter().any(|(n, _)| l.starts_with(&prefix(n))))
        .map(String::from)
        .collect();
    out.extend(
        opts.iter()
            .map(|(n, v)| format!("{}{})", prefix(n), lua_str(v))),
    );
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out.join("\n") + "\n")
}

/// The `vd.bind` line `m` writes for a macro on `keys`.
pub fn macro_line(keys: &[Key], steps: &[String]) -> String {
    let list = |v: Vec<String>| v.iter().map(|s| lua_str(s)).collect::<Vec<_>>().join(", ");
    let keys = list(keys.iter().map(Key::to_string).collect());
    format!(
        "vd.bind(\"sheet\", {{{keys}}}, {{{}}}, \"macro\")",
        list(steps.to_vec())
    )
}

/// Save a macro to `path`, replacing an earlier one on the same keys; other
/// lines (hand edits) stay.
pub fn save_macro(path: &Path, keys: &[Key], steps: &[String]) -> std::io::Result<()> {
    let line = macro_line(keys, steps);
    let prefix = macro_prefix(keys);
    let old = std::fs::read_to_string(path).unwrap_or_default();
    let mut out: Vec<&str> = old.lines().filter(|l| !l.starts_with(&prefix)).collect();
    out.push(&line);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out.join("\n") + "\n")
}

/// The start of the line `m` writes for a macro on `keys`, up to its steps.
pub fn macro_prefix(keys: &[Key]) -> String {
    let line = macro_line(keys, &[]);
    line[..line.find("}, {").map_or(line.len(), |i| i + 3)].to_string()
}

/// Drop the macro on `keys` from `path` (`gm`, S-36 E); other lines stay.
pub fn remove_macro(path: &Path, keys: &[Key]) -> std::io::Result<()> {
    let prefix = macro_prefix(keys);
    let old = std::fs::read_to_string(path).unwrap_or_default();
    let out: Vec<&str> = old.lines().filter(|l| !l.starts_with(&prefix)).collect();
    std::fs::write(
        path,
        out.iter().map(|l| format!("{l}\n")).collect::<String>(),
    )
}

/// A Lua string literal.
pub(super) fn lua_str(s: &str) -> String {
    let mut out = String::from('"');
    for c in s.chars() {
        match c {
            '"' | '\\' => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            c if c.is_ascii_control() => out.push_str(&format!("\\{:03}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// `vd.config_dirs()`: the folders of `init.lua` and of the project file.
pub(super) struct ConfigDirs {
    pub(super) global: Option<PathBuf>,
    pub(super) project: Option<PathBuf>,
}

/// The first `.vdrs.lua` from `cwd` up (S-40 B1, K1). The walk stops at
/// `home`, which is searched only when it is `cwd`.
pub fn find_project(cwd: &Path, home: Option<PathBuf>) -> Option<PathBuf> {
    for dir in cwd.ancestors() {
        if home.as_deref() == Some(dir) && dir != cwd {
            return None;
        }
        let f = dir.join(".vdrs.lua");
        if f.is_file() {
            return Some(f);
        }
        if home.as_deref() == Some(dir) {
            return None;
        }
    }
    None
}

/// Where `:trust` keeps the bytes it trusted for `project`.
pub(super) fn trust_copy(project: &Path) -> PathBuf {
    let abs = std::fs::canonicalize(project).unwrap_or_else(|_| project.to_path_buf());
    let name: String = abs
        .display()
        .to_string()
        .chars()
        .map(|c| if c == '/' || c == '\\' { '%' } else { c })
        .collect();
    crate::state_path(&format!("trust/{name}"))
}

/// `project` runs: its bytes are the ones `:trust` saw (K2: any edit needs a
/// new `:trust`).
pub fn is_trusted(project: &Path) -> bool {
    match (std::fs::read(project), std::fs::read(trust_copy(project))) {
        (Ok(now), Ok(seen)) => now == seen,
        _ => false,
    }
}

/// `:trust` (`on`) / `:trust off` for `project`.
pub fn set_trust(project: &Path, on: bool) -> std::io::Result<()> {
    let copy = trust_copy(project);
    if !on {
        return match std::fs::remove_file(&copy) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            r => r,
        };
    }
    if let Some(d) = copy.parent() {
        std::fs::create_dir_all(d)?;
    }
    std::fs::write(copy, std::fs::read(project)?)
}

pub(super) fn default_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("vdrs").join("init.lua"))
}
