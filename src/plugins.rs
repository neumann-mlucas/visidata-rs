//! mlua plugin system. See `docs/specs/S-09-plugin-system.md`.
//!
//! Shapes: keymap (`vd.bind`/`vd.unbind`), loader, aggregator, renderer; hot reload.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mlua::{Function as LuaFunction, Lua, Table as LuaTable, Value as LuaValue};

use crate::action::Action;
use crate::key::Key;
use crate::keymap::{Chord, Keymap, Layer};
use crate::reader::{LoadedSource, OpenError, SourceKind};
use crate::widget::Rendered;
use polars::prelude::{AnyValue, Column, DataFrame, DataType, NamedFrom, Series};
use ratatui::style::Style;

mod bars;
pub use bars::{Bar, BarCtx, compose};

#[derive(Default, Clone)]
struct Registry {
    patches: Vec<KeymapPatch>,
    loaders: Vec<LoaderRule>,
    options: Options,
    aggregators: Vec<(String, LuaFunction)>,
    renderers: Vec<RendererRule>,
    /// `vd.bind` / `vd.command` functions, indexed by `Action::Lua`.
    callbacks: Vec<LuaFunction>,
    /// `vd.command` ex commands → callback index.
    commands: Vec<(String, usize)>,
    /// While a callback runs: what `vd.sheet()` returns, and `vd.toast` messages.
    current: Option<LuaTable>,
    toasts: Vec<String>,
    pushed: Vec<(String, DataFrame)>,
    /// `vd.exec` commands, run once the callback returns.
    execs: Vec<String>,
    /// `vd.on` handlers (S-29, S-40 A), in registration order.
    handlers: Vec<Handler>,
    next_handler: i64,
    /// The config file running now (`init.lua`, `.vdrs.lua`, …): where an
    /// option was set, for `O` (S-40 B4).
    loading: String,
    /// `vd.statusline` / `vd.titlebar` (S-40 D), by `Bar`.
    bars: [bars::BarSlot; 2],
    bar_error: Option<String>,
    /// `vd.register_page` sidebar pages (S-40 E), and the last page drawn:
    /// (name + Debug text of its input, lines).
    pages: Vec<(String, LuaFunction)>,
    page_cache: Option<(String, Vec<ratatui::text::Line<'static>>)>,
}

/// A `vd.on` handler (S-40 A).
#[derive(Clone)]
struct Handler {
    id: i64,
    event: String,
    group: Option<String>,
    /// `pattern` as given, and as regexes; empty: every path.
    pattern: Vec<String>,
    globs: Vec<regex::Regex>,
    once: bool,
    f: LuaFunction,
}

impl Handler {
    /// A glob without `/` matches the file name, with `/` the whole path.
    fn matches(&self, path: &str) -> bool {
        self.globs.is_empty() || glob_hit(&self.pattern, &self.globs, path)
    }
}

/// `path` matches one of `pattern` (compiled as `globs`): a glob without `/`
/// on the file name, with `/` on the whole path.
fn glob_hit(pattern: &[String], globs: &[regex::Regex], path: &str) -> bool {
    let name = std::path::Path::new(path)
        .file_name()
        .map_or(String::new(), |n| n.to_string_lossy().into_owned());
    let hit = |(p, re): (&String, &regex::Regex)| match p.contains('/') {
        true => re.is_match(path),
        false => !path.is_empty() && re.is_match(&name),
    };
    pattern.iter().zip(globs).any(hit)
}

/// `*` and `?` (not across `/`), the rest literal.
fn glob_regex(glob: &str) -> Result<regex::Regex, regex::Error> {
    let mut re = String::from("^");
    for c in glob.chars() {
        match c {
            '*' => re.push_str("[^/]*"),
            '?' => re.push_str("[^/]"),
            c => re.push_str(&regex::escape(&c.to_string())),
        }
    }
    re.push('$');
    regex::Regex::new(&re)
}

/// Settings from `vd.set_theme` and `vd.option`.
#[derive(Clone, Default)]
pub struct Options {
    /// `vd.set_theme`, if called.
    pub theme: Option<crate::theme::Theme>,
    /// `vd.option("clipboard_copy_cmd", …)`.
    pub clipboard_copy_cmd: Option<String>,
    /// `vd.option("clipboard_paste_cmd", …)`.
    pub clipboard_paste_cmd: Option<String>,
    /// `vd.option("duckdb_cmd", …)`.
    pub duckdb_cmd: Option<String>,
    /// `vd.option("session_autosave", "false")` turns saving the session on quit off (S-29 D).
    pub no_autosave: bool,
    /// `vd.option("mouse", "true")`: wheel and clicks move the cursor (S-26 A9).
    pub mouse: bool,
    /// `vd.option("disp_float_fmt", …)`: decimals for float columns (S-32).
    pub float_digits: Option<u8>,
    /// `vd.option("numeric_binning", true)` / `("histogram_bins", N)` (S-35 C1; 0 bins: ⌊√rows⌋).
    pub numeric_binning: bool,
    pub histogram_bins: usize,
    /// `vd.option("incr_base", N)` (S-41 E): where `i` starts; None: 1.
    pub incr_base: Option<u64>,
    /// `vd.option("unfurl_empty", true)` (S-42 C).
    pub unfurl_empty: bool,
    /// `vd.option("csv_skipinitialspace", false)` (S-48): CSV cells keep leading blanks.
    pub csv_keep_space: bool,
    /// `vd.option("header", N)` (S-43 A): rows read as column names; None: 1.
    pub header: Option<usize>,
    /// `vd.option("disp_footer", true)` (S-36 C): the totals row.
    pub disp_footer: bool,
    /// Every value set, as checked (S-37 P0c): what `O` shows.
    pub set: std::collections::BTreeMap<String, String>,
    /// The file (or `:set`) each value in `set` came from (S-40 B4).
    pub from: std::collections::BTreeMap<String, String>,
}

impl Options {
    /// Store option `name` = `v`, already checked by [`crate::options::check`].
    pub fn apply(&mut self, name: &str, v: &str) {
        let on = v == "true";
        match name {
            "clipboard_copy_cmd" => self.clipboard_copy_cmd = Some(v.into()),
            "clipboard_paste_cmd" => self.clipboard_paste_cmd = Some(v.into()),
            "duckdb_cmd" => self.duckdb_cmd = Some(v.into()),
            "session_autosave" => self.no_autosave = !on,
            "disp_float_fmt" => self.float_digits = float_fmt_digits(v),
            "numeric_binning" => self.numeric_binning = on,
            "disp_footer" => self.disp_footer = on,
            "histogram_bins" => self.histogram_bins = v.parse().unwrap_or(0),
            "incr_base" => self.incr_base = v.parse().ok(),
            "unfurl_empty" => self.unfurl_empty = on,
            "csv_skipinitialspace" => self.csv_keep_space = !on,
            "header" => self.header = v.parse().ok(),
            "mouse" => self.mouse = on,
            _ => {} // keymap: applied when set (the vim profile's binds)
        }
        self.set.insert(name.into(), v.into());
    }

    /// Option `name`'s value: as set, else its default.
    pub fn value(&self, name: &str) -> String {
        match self.set.get(name) {
            Some(v) => v.clone(),
            None => crate::options::find(name).map_or("", |o| o.default).into(),
        }
    }
}

/// Events `vd.on` takes.
pub const EVENTS: &[&str] = &[
    "open", "push", "save", "quit", "tick", "enter", "loaded", "reload",
];

/// Events with no path: `pattern` is refused on them.
const PATHLESS: &[&str] = &["quit", "tick", "reload"];

/// What a `vd.bind` function asked for: status messages and new sheets.
#[derive(Default)]
pub struct LuaOutput {
    pub toasts: Vec<String>,
    pub sheets: Vec<(String, DataFrame)>,
    /// `vd.exec` ex commands (without the `:`), in call order.
    pub execs: Vec<String>,
}

/// What a `vd.bind` function sees through `vd.sheet()`.
#[derive(Debug)]
pub struct SheetCtx<'a> {
    pub name: &'a str,
    /// 1-based row in the view, 1-based column.
    pub row: u64,
    pub col: usize,
    pub column_names: Vec<String>,
    pub value: AnyValue<'a>,
}

/// `vd.register_renderer { column = "price" | dtype = "f64", render = fn(v) }`.
#[derive(Clone)]
struct RendererRule {
    column: Option<String>,
    dtype: Option<String>,
    render: LuaFunction,
}

/// Lua aggregator result, ready to become a Polars value.
#[derive(Debug, Clone, PartialEq)]
pub enum AggValue {
    Num(f64),
    Text(String),
    Null,
}

/// A Lua aggregator, callable from any thread.
#[derive(Clone)]
pub struct Aggregator {
    lua: Lua,
    f: LuaFunction,
}

impl Aggregator {
    /// Reduce one group's values (a Lua array; nulls are nil).
    pub fn call(&self, values: &Series) -> mlua::Result<AggValue> {
        let t = self.lua.create_table()?;
        for (i, v) in values.iter().enumerate() {
            t.raw_set(i + 1, to_lua(&self.lua, &v)?)?;
        }
        Ok(match self.f.call::<LuaValue>(t)? {
            LuaValue::Integer(n) => AggValue::Num(n as f64),
            LuaValue::Number(n) => AggValue::Num(n),
            LuaValue::String(s) => AggValue::Text(s.to_str()?.to_string()),
            LuaValue::Nil => AggValue::Null,
            other => AggValue::Text(format!("{other:?}")),
        })
    }
}

/// Lua API version exposed as `vd.api_version`. Bump on additions; S-10
/// deprecation windows key off it.
pub const API_VERSION: i64 = 21; // 2: function bindings, vd.toast, vd.sheet; 3: vd.push_sheet; 4: vd.command; 5: vd.option; 6: duckdb_cmd; 7: clipboard_paste_cmd; 8: vd.bind lists (macros); 9: vd.on, vd.exec; 10: styled renderers; 11: disp_float_fmt; 12: keymap, mouse, session_autosave, numeric_binning, histogram_bins options; 13: loader open may return rows; 14: disp_footer; 15: validation; 16: vd.on opts, vd.off, vd.augroup, enter/loaded/tick/reload; 17: .vdrs.lua, vd.reset, vd.config_dirs, require paths; 18: loader parts, patterns, by name, vd.loaders; 19: vd.set_theme table; 20: vd.statusline, vd.titlebar, vd.bar; 21: vd.register_page

#[derive(Clone, Debug)]
struct KeymapPatch {
    layer: Layer,
    chord: Chord,
}

/// Lua loader registration. `open(path) -> string` converts a source file into
/// a path to a format we natively scan (CSV/TSV/Parquet/Arrow/Jsonl). The Lua
/// side owns the tempfile lifecycle; we read the returned path once.
#[derive(Clone)]
pub struct LoaderRule {
    /// `--format` / `:open path name` pick it by this (S-40 C2).
    pub name: Option<String>,
    pub extensions: Vec<String>,
    /// File-name globs (`export_*.txt`), as `vd.on`'s `pattern`.
    pub patterns: Vec<String>,
    globs: Vec<regex::Regex>,
    pub open: LuaFunction,
}

impl LoaderRule {
    fn claims(&self, path: &Path) -> bool {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        ext.is_some_and(|x| self.extensions.contains(&x))
            || glob_hit(&self.patterns, &self.globs, &path.to_string_lossy())
    }
}

/// What a loader's `open` gave back.
pub enum LoaderOutput {
    Path(PathBuf),
    Rows(DataFrame),
    /// `{ parts = { { name, path | columns+rows }, … } }` (S-40 C1).
    Parts(Vec<(String, LoaderOutput)>),
}

pub struct Plugins {
    lua: Lua,
    /// `init.lua` this host was loaded from and its mtime then (hot reload).
    config: Option<(PathBuf, Option<std::time::SystemTime>)>,
    /// The project's `.vdrs.lua` (S-40 B), its mtime then, and whether it ran.
    project: Option<(PathBuf, Option<std::time::SystemTime>, bool)>,
    /// Renderer results by (column, dtype, value): renderers run on the UI
    /// thread, so each distinct cell is rendered once, not every frame.
    rendered: RefCell<HashMap<(String, String, String), Option<Rendered>>>,
}

impl Plugins {
    /// Build a plugin host from `$XDG_CONFIG_HOME/vdrs/init.lua`. Never fails:
    /// a broken `init.lua` keeps everything registered before the error and the
    /// error comes back as a warning. Missing init.lua is the common case.
    pub fn load() -> (Self, Option<String>) {
        // tests never pick up a `.vdrs.lua` from where they run
        let project = match cfg!(test) {
            true => None,
            false => std::env::current_dir()
                .ok()
                .and_then(|cwd| find_project(&cwd, std::env::var_os("HOME").map(PathBuf::from))),
        };
        Self::load_with(default_config_path(), project)
    }

    /// [`Self::load`] with `init.lua` at `path` and the project file `project`.
    pub fn load_with(path: Option<PathBuf>, project: Option<PathBuf>) -> (Self, Option<String>) {
        let (mut host, mut warn) = Self::load_from(path, project.as_deref());
        let Some(p) = project else {
            return (host, warn);
        };
        let mtime = std::fs::metadata(&p).and_then(|m| m.modified()).ok();
        let trusted = is_trusted(&p);
        if trusted {
            match std::fs::read_to_string(&p)
                .map_err(|e| e.to_string())
                .and_then(|src| host.exec(&src, ".vdrs.lua").map_err(|e| e.to_string()))
            {
                Ok(()) => {}
                Err(e) => {
                    warn =
                        Some(warn.map_or(String::new(), |w| w + "; ") + &format!(".vdrs.lua: {e}"))
                }
            }
        } else {
            let msg = format!(
                "{} not trusted: :trust to run it, :trust view to read it",
                p.display()
            );
            warn = Some(warn.map_or(String::new(), |w| w + "; ") + &msg);
        }
        host.project = Some((p, mtime, trusted));
        (host, warn)
    }

    /// The project `.vdrs.lua` found at start, and whether it ran.
    pub fn project_file(&self) -> Option<(&Path, bool)> {
        self.project.as_ref().map(|(p, _, t)| (p.as_path(), *t))
    }

    fn load_from(path: Option<PathBuf>, project: Option<&Path>) -> (Self, Option<String>) {
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let config = path.map(|p| {
            let m = mtime(&p);
            (p, m)
        });
        let src = match &config {
            Some((p, Some(_))) => match std::fs::read_to_string(p) {
                Ok(s) => Some(s),
                Err(e) => return (Self::empty(config), Some(format!("init.lua: {e}"))),
            },
            _ => None,
        };
        let host = Self::empty(config);
        let global = host
            .config
            .as_ref()
            .and_then(|(p, _)| p.parent().map(Path::to_path_buf));
        let project = project.and_then(Path::parent).map(Path::to_path_buf);
        if let Err(e) = host.set_dirs(global, project) {
            return (host, Some(format!("package.path: {e}")));
        }
        let err = src.and_then(|s| host.exec(&s, "init.lua").err());
        let mut warn = err.map(|e| format!("init.lua: {e}"));
        // options.lua (`O`, S-37 P0c) then macros.lua (S-19) sit next to
        // init.lua and run after it.
        for name in ["options.lua", "macros.lua"] {
            let file = host.config.as_ref().map(|(p, _)| p.with_file_name(name));
            if let Some(Ok(src)) = file.map(std::fs::read_to_string)
                && let Err(e) = host.exec(&src, name)
            {
                warn = Some(warn.map_or(String::new(), |w| w + "; ") + &format!("{name}: {e}"));
            }
        }
        (host, warn)
    }

    #[expect(clippy::expect_used, reason = "a fresh VM always takes the vd table")]
    fn empty(config: Option<(PathBuf, Option<std::time::SystemTime>)>) -> Self {
        let lua = Lua::new();
        lua.set_app_data(RefCell::new(Registry::default()));
        install_api(&lua).expect("installing the vd table on a fresh VM");
        Self {
            lua,
            config,
            project: None,
            rendered: Default::default(),
        }
    }

    /// `vd.config_dirs()`, and `require` from `<global>/lua` and
    /// `<project>/.vdrs/lua` (K4).
    fn set_dirs(&self, global: Option<PathBuf>, project: Option<PathBuf>) -> mlua::Result<()> {
        let mut prefix = String::new();
        if let Some(g) = &global {
            prefix += &format!("{}/lua/?.lua;", g.display());
        }
        if let Some(p) = &project {
            prefix += &format!("{}/.vdrs/lua/?.lua;", p.display());
        }
        let package: LuaTable = self.lua.globals().get("package")?;
        let path: String = package.get("path")?;
        package.set("path", prefix + &path)?;
        self.lua.set_app_data(ConfigDirs { global, project });
        Ok(())
    }

    fn exec(&self, src: &str, name: &str) -> mlua::Result<()> {
        if let Ok(r) = registry(&self.lua) {
            r.borrow_mut().loading = name.to_string();
        }
        let res = self.lua.load(src).set_name(name).exec();
        if let Ok(r) = registry(&self.lua) {
            r.borrow_mut().loading = ":set".into(); // what sets options from now on
        }
        res
    }

    /// Build with an explicit config string. Used by tests; errors on any Lua error.
    pub fn load_from_source(src: &str) -> mlua::Result<Self> {
        let host = Self::empty(None);
        host.exec(src, "inline")?;
        Ok(host)
    }

    /// `init.lua` changed on disk (created, edited or deleted) since this host
    /// loaded it → a freshly loaded host plus its warning, if any.
    pub fn reload_if_changed(&self) -> Option<(Self, Option<String>)> {
        let mtime = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
        let init = self.config.as_ref().is_some_and(|(p, m)| mtime(p) != *m);
        let project = self
            .project
            .as_ref()
            .is_some_and(|(p, m, _)| mtime(p) != *m);
        (init || project).then(|| self.reload())
    }

    /// A fresh host from the same files (`:trust`).
    pub fn reload(&self) -> (Self, Option<String>) {
        let init = self.config.as_ref().map(|(p, _)| p.clone());
        Self::load_with(init, self.project.as_ref().map(|(p, _, _)| p.clone()))
    }

    /// The aggregator `vd.register_aggregator` defined as `name`. `Send`: pivots
    /// run it on a background job; each call holds the VM lock only for one cell.
    pub fn aggregator(&self, name: &str) -> Option<Aggregator> {
        let reg = reg(&self.lua)?;
        let f = reg
            .borrow()
            .aggregators
            .iter()
            .find(|(n, _)| n == name)?
            .1
            .clone();
        Some(Aggregator {
            lua: self.lua.clone(),
            f,
        })
    }

    pub fn has_renderers(&self) -> bool {
        reg(&self.lua).is_some_and(|r| !r.borrow().renderers.is_empty())
    }

    /// Display text from the first renderer matching the cell's column name or
    /// dtype (`i64`, `f64`, `str`, `date`, … as shown in headers). Lua errors
    /// fall back to the default display.
    /// Renderers are assumed pure: results are memoized per distinct value.
    pub fn render(&self, column: &str, dtype: &DataType, value: &AnyValue) -> Option<Rendered> {
        let dt = crate::widget::dtype_badge(dtype);
        // `Debug`, not `Display`: Display rounds floats, merging distinct cells.
        let key = (column.to_string(), dt.clone(), format!("{value:?}"));
        if let Some(hit) = self.rendered.borrow().get(&key) {
            return hit.clone();
        }
        let out = self.render_uncached(column, &dt, value);
        let mut memo = self.rendered.borrow_mut();
        if memo.len() >= 100_000 {
            memo.clear(); // ponytail: crude bound, LRU if anyone scrolls that far
        }
        memo.insert(key, out.clone());
        out
    }

    fn render_uncached(&self, column: &str, dt: &str, value: &AnyValue) -> Option<Rendered> {
        let reg = reg(&self.lua)?;
        let f = {
            let reg = reg.borrow();
            let rule = reg
                .renderers
                .iter()
                .find(|r| r.column.as_deref() == Some(column) || r.dtype.as_deref() == Some(dt))?;
            rule.render.clone()
        };
        let v = to_lua(&self.lua, value).ok()?;
        match f.call::<LuaValue>(v).ok()? {
            LuaValue::Table(t) => Some(styled(&t)),
            other => Some(Rendered {
                text: Some(
                    self.lua
                        .coerce_string(other)
                        .ok()??
                        .to_str()
                        .ok()?
                        .to_string(),
                ),
                style: Style::default(),
            }),
        }
    }

    #[cfg(test)]
    fn render_text(&self, column: &str, dtype: &DataType, value: &AnyValue) -> Option<String> {
        self.render(column, dtype, value)?.text
    }

    /// Callback index of the `vd.command` named `name`.
    pub fn command(&self, name: &str) -> Option<usize> {
        let reg = reg(&self.lua)?;
        reg.borrow()
            .commands
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
    }

    /// Names of the `vd.command` ex commands.
    pub fn command_names(&self) -> Vec<String> {
        reg(&self.lua)
            .map(|r| r.borrow().commands.iter().map(|(n, _)| n.clone()).collect())
            .unwrap_or_default()
    }

    /// Run callback `id` with `vd.sheet()` = `ctx`; a `vd.command` function gets
    /// the text after its name (`args`), a `vd.bind` one nil. Returns its toasts
    /// and pushed sheets; a Lua error comes back as Err and discards both.
    pub fn call(&self, id: usize, ctx: &SheetCtx, args: Option<&str>) -> mlua::Result<LuaOutput> {
        let lua = &self.lua;
        let t = self.ctx_table(ctx)?;
        let f = {
            let r = registry(lua)?;
            let mut reg = r.borrow_mut();
            reg.current = Some(t);
            reg.callbacks.get(id).cloned()
        };
        let result = match f {
            Some(f) => f.call::<()>(args),
            None => Err(mlua::Error::RuntimeError(format!("no lua binding #{id}"))),
        };
        let out = self.take_output()?;
        result?;
        Ok(out)
    }

    /// The `init.lua` this VM loaded, if any.
    pub fn config_file(&self) -> Option<&Path> {
        self.config.as_ref().map(|(p, _)| p.as_path())
    }

    /// `:handlers`: id, event, group, pattern, once per `vd.on` handler.
    pub fn handler_rows(&self) -> Vec<Vec<String>> {
        let Some(r) = reg(&self.lua) else {
            return Vec::new();
        };
        let r = r.borrow();
        r.handlers
            .iter()
            .map(|h| {
                vec![
                    h.id.to_string(),
                    h.event.clone(),
                    h.group.clone().unwrap_or_default(),
                    h.pattern.join(" "),
                    h.once.to_string(),
                ]
            })
            .collect()
    }

    /// Some `vd.on(event, …)` handler is registered.
    pub fn handles(&self, event: &str) -> bool {
        reg(&self.lua).is_some_and(|r| r.borrow().handlers.iter().any(|h| h.event == event))
    }

    /// Run `event`'s handlers in registration order. Each gets the
    /// `vd.sheet()` table of `ctx` plus `extra` fields (also what `vd.sheet()`
    /// returns inside). A failing handler doesn't stop the rest: its error
    /// comes back in the list, and what the others asked for in the output.
    pub fn fire(
        &self,
        event: &str,
        ctx: &SheetCtx,
        extra: &[(&str, String)],
    ) -> (LuaOutput, Vec<String>) {
        let mut errors = Vec::new();
        let mut out = LuaOutput::default();
        let path = extra
            .iter()
            .find(|(k, _)| *k == "path")
            .map_or("", |(_, v)| v.as_str());
        let handlers: Vec<LuaFunction> = match registry(&self.lua) {
            Ok(r) => {
                let mut reg = r.borrow_mut();
                let hit = |h: &Handler| h.event == event && h.matches(path);
                let fs = reg
                    .handlers
                    .iter()
                    .filter(|h| hit(h))
                    .map(|h| h.f.clone())
                    .collect();
                reg.handlers.retain(|h| !(h.once && hit(h)));
                fs
            }
            Err(e) => return (out, vec![e.to_string()]),
        };
        for f in handlers {
            let run = || -> mlua::Result<LuaOutput> {
                let t = self.ctx_table(ctx)?;
                for (k, v) in extra {
                    t.set(*k, v.as_str())?;
                }
                registry(&self.lua)?.borrow_mut().current = Some(t.clone());
                let result = f.call::<()>(t);
                let got = self.take_output()?;
                result?;
                Ok(got)
            };
            match run() {
                Ok(o) => {
                    out.toasts.extend(o.toasts);
                    out.sheets.extend(o.sheets);
                    out.execs.extend(o.execs);
                }
                // the message, not the traceback
                Err(e) => errors.push(format!(
                    "vd.on {event}: {}",
                    e.to_string().lines().next().unwrap_or_default()
                )),
            }
        }
        (out, errors)
    }

    /// End of a callback: what it asked for, and `vd.sheet()` unset.
    fn take_output(&self) -> mlua::Result<LuaOutput> {
        let r = registry(&self.lua)?;
        let mut reg = r.borrow_mut();
        reg.current = None;
        Ok(LuaOutput {
            toasts: std::mem::take(&mut reg.toasts),
            sheets: std::mem::take(&mut reg.pushed),
            execs: std::mem::take(&mut reg.execs),
        })
    }

    /// What `vd.sheet()` returns for `ctx`.
    fn ctx_table(&self, ctx: &SheetCtx) -> mlua::Result<LuaTable> {
        let lua = &self.lua;
        let t = lua.create_table()?;
        t.set("name", ctx.name)?;
        let cursor = lua.create_table()?;
        cursor.set("row", ctx.row)?;
        cursor.set("col", ctx.col)?;
        t.set("cursor", cursor)?;
        t.set(
            "column",
            ctx.column_names.get(ctx.col.wrapping_sub(1)).cloned(),
        )?;
        t.set("column_names", ctx.column_names.clone())?;
        t.set("value", to_lua(lua, &ctx.value)?)?;
        Ok(t)
    }

    /// What `vd.set_theme` and `vd.option` set.
    /// `:set` / `O` (S-37 P0c): store a checked option value.
    pub fn set_option(&self, name: &str, v: &str) {
        if let Some(r) = reg(&self.lua) {
            let mut reg = r.borrow_mut();
            reg.options.apply(name, v);
            reg.options.from.insert(name.into(), ":set".into());
        }
    }

    pub fn options(&self) -> Options {
        reg(&self.lua)
            .map(|r| r.borrow().options.clone())
            .unwrap_or_default()
    }

    /// Mutate `keymap` by appending the Lua-registered patches.
    /// Last declaration wins at `Keymap::resolve` time (S-04 rule), so user binds shadow defaults.
    pub fn apply_keymap(&self, keymap: &mut Keymap) {
        let reg = match reg(&self.lua) {
            Some(r) => r.borrow().clone(),
            None => return,
        };
        for patch in reg.patches {
            keymap.append(patch.layer, patch.chord);
        }
    }

    /// Return a registered loader matching this path's extension (case-insensitive),
    /// if any. First match wins. Lookup is O(n·k) over loaders × their extensions —
    /// fine for the handful a user registers.
    pub fn resolve_loader(&self, path: &Path) -> Option<LoaderRule> {
        let reg = reg(&self.lua)?;
        // the last registered wins: a project loader beats a global one (S-40 B3)
        reg.borrow()
            .loaders
            .iter()
            .rev()
            .find(|r| r.claims(path))
            .cloned()
    }

    /// The loader registered as `name` (the last one of that name).
    pub fn loader_named(&self, name: &str) -> Option<LoaderRule> {
        let reg = reg(&self.lua)?;
        let r = reg.borrow();
        r.loaders
            .iter()
            .rev()
            .find(|r| r.name.as_deref() == Some(name))
            .cloned()
    }

    /// Names of the named loaders, for error messages.
    pub fn loader_names(&self) -> Vec<String> {
        reg(&self.lua).map_or_else(Vec::new, |r| {
            r.borrow()
                .loaders
                .iter()
                .filter_map(|l| l.name.clone())
                .collect()
        })
    }

    /// The loader that opens `path` when `format` names none: by name first,
    /// then by file name, also for `file:part` of a loader's file.
    pub fn loader_for(&self, path: &str, format: Option<&str>) -> Option<LoaderRule> {
        if let Some(f) = format {
            return self.loader_named(f);
        }
        let p = Path::new(path);
        self.resolve_loader(p).or_else(|| {
            let (file, _) = path.rsplit_once(':')?;
            (!p.exists() && Path::new(file).is_file())
                .then(|| self.resolve_loader(Path::new(file)))
                .flatten()
        })
    }

    /// Call a loader's `open(path)`. It returns the path of a file it wrote in a
    /// format we scan natively (the plugin owns that tempfile), or the rows
    /// themselves as a `{ columns, rows }` table (S-39 A).
    pub fn call_loader(&self, rule: &LoaderRule, src_path: &Path) -> mlua::Result<LoaderOutput> {
        let s = src_path.to_string_lossy().into_owned();
        let out = rule.open.call::<LuaValue>(s)?;
        let one = |v: LuaValue| -> mlua::Result<LoaderOutput> {
            match v {
                LuaValue::String(p) => {
                    Ok(LoaderOutput::Path(PathBuf::from(p.to_str()?.to_string())))
                }
                LuaValue::Table(t) => Ok(LoaderOutput::Rows(sheet_from_lua(&t, "loader open")?.1)),
                other => Err(mlua::Error::RuntimeError(format!(
                    "loader open must return a path or a {{ columns, rows }} table (or {{ parts = … }}), not {}",
                    other.type_name()
                ))),
            }
        };
        let LuaValue::Table(t) = &out else {
            return one(out);
        };
        let Some(parts) = t.get::<Option<LuaTable>>("parts")? else {
            return one(out);
        };
        let who = rule.name.as_deref().unwrap_or("loader");
        let mut list = Vec::new();
        for (i, p) in parts.sequence_values::<LuaTable>().enumerate() {
            let p = p?;
            let name = p
                .get::<Option<String>>("name")?
                .unwrap_or_else(|| (i + 1).to_string());
            let v = match p.get::<Option<String>>("path")? {
                Some(path) => LoaderOutput::Path(PathBuf::from(path)),
                None => one(LuaValue::Table(p))?,
            };
            list.push((name, v));
        }
        if list.is_empty() {
            return Err(mlua::Error::RuntimeError(format!(
                "{who}: open returned no parts"
            )));
        }
        Ok(LoaderOutput::Parts(list))
    }

    /// `vd.loaders()` as rows: name, extensions, patterns.
    pub fn loader_rows(&self) -> Vec<Vec<String>> {
        reg(&self.lua).map_or_else(Vec::new, |r| {
            r.borrow()
                .loaders
                .iter()
                .map(|l| {
                    vec![
                        l.name.clone().unwrap_or_default(),
                        l.extensions.join(" "),
                        l.patterns.join(" "),
                    ]
                })
                .collect()
        })
    }
}

/// A built-in format by its `--format` name (`csv`, `xlsx`, …).
pub fn builtin_format(name: &str) -> Option<SourceKind> {
    <SourceKind as clap::ValueEnum>::from_str(name, true).ok()
}

/// Open `path`. `format`: a built-in kind or a Lua loader's name (S-40 C2);
/// without one, a loader whose extension or pattern claims the file (or the
/// file of a `file:part`), else the built-in readers.
pub fn open(
    plugins: Option<&Plugins>,
    path: &std::ffi::OsStr,
    format: Option<&str>,
) -> Result<LoadedSource, OpenError> {
    let builtin = format.and_then(builtin_format);
    let text = path.to_string_lossy().into_owned();
    let rule = match (format, builtin) {
        (_, Some(_)) => None,
        (Some(f), None) => {
            let rule = plugins.and_then(|p| p.loader_named(f));
            if rule.is_none() {
                let mut names: Vec<String> = <SourceKind as clap::ValueEnum>::value_variants()
                    .iter()
                    .filter_map(|k| {
                        clap::ValueEnum::to_possible_value(k).map(|v| v.get_name().to_string())
                    })
                    .collect();
                names.extend(plugins.map(Plugins::loader_names).unwrap_or_default());
                return Err(OpenError::Plugin(format!(
                    "unknown format {f} (built-in or loader: {})",
                    names.join(", ")
                )));
            }
            rule
        }
        (None, _) => plugins.and_then(|p| p.loader_for(&text, None)),
    };
    let (Some(p), Some(rule)) = (plugins, rule) else {
        let header = plugins.and_then(|p| p.options().header).unwrap_or(1);
        return crate::reader::open_with(path, builtin, header);
    };
    if text == "-" {
        // ponytail: a loader gets a path; stdin would need a kept tempfile
        return Err(OpenError::Plugin(
            "a Lua loader reads a file, not stdin".into(),
        ));
    }
    // `file:part` of a loader's file: the loader reads the file
    let (file, want) = match Path::new(path).exists() {
        true => (text.clone(), None),
        false => match text.rsplit_once(':') {
            Some((f, part)) if Path::new(f).is_file() => (f.to_string(), Some(part.to_string())),
            _ => (text.clone(), None),
        },
    };
    let out = p
        .call_loader(&rule, Path::new(&file))
        .map_err(|e| OpenError::Plugin(e.to_string()))?;
    let one = |out: LoaderOutput, shown: String| -> Result<LoadedSource, OpenError> {
        let mut loaded = match out {
            LoaderOutput::Path(converted) => crate::reader::open(converted.as_os_str(), None)?,
            LoaderOutput::Rows(df) => {
                LoadedSource::from_df(SourceKind::Csv, shown.clone(), None, df)
            }
            LoaderOutput::Parts(_) => return Err(OpenError::Plugin("parts inside parts".into())),
        };
        loaded.display_name = shown; // what the user opened
        loaded.via_loader = true;
        Ok(loaded)
    };
    let parts = match out {
        LoaderOutput::Parts(parts) => parts,
        out => return one(out, file),
    };
    if let Some(want) = want {
        let Some((name, out)) = parts.into_iter().find(|(n, _)| *n == want) else {
            return Err(OpenError::Plugin(format!("{file}: no part {want}")));
        };
        return one(out, format!("{file}:{name}"));
    }
    let mut parts = parts;
    if parts.len() == 1 {
        let (name, out) = parts.remove(0);
        return one(out, format!("{file}:{name}"));
    }
    let names: Vec<String> = parts.iter().map(|(n, _)| n.clone()).collect();
    let mut list = Vec::new();
    for (name, out) in parts {
        let loaded = one(out, format!("{file}:{name}"))?;
        list.push((name, std::sync::Arc::new(loaded)));
    }
    let df = polars::prelude::Column::new("part".into(), names).into_frame();
    let mut loaded = LoadedSource::from_df(SourceKind::Csv, file, None, df);
    loaded.via_loader = true;
    loaded.parts = list;
    Ok(loaded)
}

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
b("'", "go_mark")
b("q", "record_macro")               -- default: pop sheet (ZZ, :q here)
b("@", "play_macro")                 -- default: date cast (z@ here)
b({"z", "@"}, "type_cast_date")
b({"z", "s"}, "freeze_column")      -- default: select rows above the cursor
b({"z", "c"}, "unfreeze_all")       -- default: go to column number
b({"z", "o"}, "clear_sort")         -- default: open the cell's file
b("<C-v>", "visual_block")           -- default: show version
vd.bind("input", "<Esc>", "input_normal", "vim: normal mode")  -- Esc again cancels
"##;

fn install_api(lua: &Lua) -> Result<(), mlua::Error> {
    let vd = lua.create_table()?;
    let bind = lua.create_function(bind_fn)?;
    let unbind = lua.create_function(unbind_fn)?;
    let register_loader = lua.create_function(register_loader_fn)?;
    vd.set("bind", bind)?;
    vd.set("unbind", unbind)?;
    vd.set("command", lua.create_function(command_fn)?)?;
    vd.set("register_loader", register_loader)?;
    vd.set("api_version", API_VERSION)?;
    vd.set(
        "toast",
        lua.create_function(|lua, msg: String| {
            let r = registry(lua)?;
            let mut reg = r.borrow_mut();
            if reg.current.is_none() {
                return Err(mlua::Error::RuntimeError(
                    "vd.toast works inside a vd.bind, vd.command or vd.on function".into(),
                ));
            }
            reg.toasts.push(msg);
            Ok(())
        })?,
    )?;
    vd.set(
        "sheet",
        lua.create_function(|lua, ()| Ok(registry(lua)?.borrow().current.clone()))?,
    )?;
    vd.set(
        "on",
        lua.create_function(
            |lua, (event, opts, f): (String, LuaValue, Option<LuaFunction>)| {
                let err = |m: String| mlua::Error::RuntimeError(format!("vd.on: {m}"));
                if !EVENTS.contains(&event.as_str()) {
                    return Err(err(format!(
                        "unknown event {event} ({})",
                        EVENTS.join(", ")
                    )));
                }
                let (opts, f) = match (opts, f) {
                    (LuaValue::Function(f), None) => (None, f),
                    (LuaValue::Table(t), Some(f)) => (Some(t), f),
                    _ => return Err(err("vd.on(event, [opts,] fn)".into())),
                };
                let (mut group, mut pattern, mut once) = (None, Vec::new(), false);
                if let Some(t) = opts {
                    group = t.get::<Option<String>>("group")?;
                    once = t.get::<Option<bool>>("once")?.unwrap_or(false);
                    pattern = match t.get::<LuaValue>("pattern")? {
                        LuaValue::Nil => Vec::new(),
                        LuaValue::String(s) => vec![s.to_str()?.to_string()],
                        LuaValue::Table(l) => {
                            l.sequence_values::<String>().collect::<mlua::Result<_>>()?
                        }
                        _ => return Err(err("pattern is a glob or a list of globs".into())),
                    };
                }
                if !pattern.is_empty() && PATHLESS.contains(&event.as_str()) {
                    return Err(err(format!("{event} has no path to match a pattern")));
                }
                let globs = pattern
                    .iter()
                    .map(|p| glob_regex(p).map_err(|e| err(e.to_string())))
                    .collect::<mlua::Result<_>>()?;
                let r = registry(lua)?;
                let mut reg = r.borrow_mut();
                reg.next_handler += 1;
                let id = reg.next_handler;
                reg.handlers.push(Handler {
                    id,
                    event,
                    group,
                    pattern,
                    globs,
                    once,
                    f,
                });
                Ok(id)
            },
        )?,
    )?;
    vd.set(
        "off",
        lua.create_function(|lua, id: i64| {
            let r = registry(lua)?;
            let mut reg = r.borrow_mut();
            let n = reg.handlers.len();
            reg.handlers.retain(|h| h.id != id);
            Ok(reg.handlers.len() < n)
        })?,
    )?;
    let clear_group = |lua: &Lua, name: &str| -> mlua::Result<()> {
        let r = registry(lua)?;
        r.borrow_mut()
            .handlers
            .retain(|h| h.group.as_deref() != Some(name));
        Ok(())
    };
    vd.set(
        "clear_group",
        lua.create_function(move |lua, name: String| clear_group(lua, &name))?,
    )?;
    vd.set(
        "augroup",
        lua.create_function(move |lua, (name, opts): (String, Option<LuaTable>)| {
            let clear = match &opts {
                Some(t) => t.get::<Option<bool>>("clear")?.unwrap_or(true),
                None => true,
            };
            if clear {
                clear_group(lua, &name)?;
            }
            Ok(name)
        })?,
    )?;
    vd.set(
        "handlers",
        lua.create_function(|lua, ()| {
            let r = registry(lua)?;
            let reg = r.borrow();
            let out = lua.create_table()?;
            for (i, h) in reg.handlers.iter().enumerate() {
                let t = lua.create_table()?;
                t.set("id", h.id)?;
                t.set("event", h.event.as_str())?;
                t.set("group", h.group.as_deref())?;
                t.set("pattern", h.pattern.join(" "))?;
                t.set("once", h.once)?;
                out.set(i + 1, t)?;
            }
            Ok(out)
        })?,
    )?;
    vd.set(
        "exec",
        lua.create_function(|lua, cmd: String| {
            let r = registry(lua)?;
            if r.borrow().current.is_none() {
                return Err(mlua::Error::RuntimeError(
                    "vd.exec works inside a vd.bind, vd.command or vd.on function".into(),
                ));
            }
            let cmd = cmd.trim().trim_start_matches(':').trim().to_string();
            r.borrow_mut().execs.push(cmd);
            Ok(())
        })?,
    )?;
    vd.set(
        "push_sheet",
        lua.create_function(|lua, spec: LuaTable| {
            let r = registry(lua)?;
            if r.borrow().current.is_none() {
                return Err(mlua::Error::RuntimeError(
                    "vd.push_sheet works inside a vd.bind, vd.command or vd.on function".into(),
                ));
            }
            let sheet = sheet_from_lua(&spec, "vd.push_sheet")?;
            r.borrow_mut().pushed.push(sheet);
            Ok(())
        })?,
    )?;
    vd.set(
        "register_aggregator",
        lua.create_function(|lua, (name, f): (String, LuaFunction)| {
            let r = registry(lua)?;
            let mut reg = r.borrow_mut();
            reg.aggregators.retain(|(n, _)| *n != name); // redefining replaces
            reg.aggregators.push((name, f));
            Ok(())
        })?,
    )?;
    vd.set(
        "register_renderer",
        lua.create_function(|lua, spec: LuaTable| {
            let rule = RendererRule {
                column: spec.get("column")?,
                dtype: spec.get("dtype")?,
                render: spec.get("render")?,
            };
            if rule.column.is_none() && rule.dtype.is_none() {
                return Err(mlua::Error::RuntimeError(
                    "vd.register_renderer: give column or dtype".into(),
                ));
            }
            registry(lua)?.borrow_mut().renderers.push(rule);
            Ok(())
        })?,
    )?;
    bars::install(lua, &vd)?;
    vd.set(
        "set_theme",
        lua.create_function(|lua, spec: LuaValue| {
            use crate::theme::Theme;
            let err = |m: String| mlua::Error::RuntimeError(format!("vd.set_theme: {m}"));
            let base = |name: &str| match name {
                "dark" => Ok(Theme::dark()),
                "light" => Ok(Theme::light()),
                _ => Err(err(format!("unknown theme: {name} (dark|light)"))),
            };
            let theme = match spec {
                LuaValue::String(s) => base(&s.to_str()?)?,
                // S-40 G: `{ base = "light", statusbar_bg = "#24283b", … }`
                LuaValue::Table(t) => {
                    let mut th = base(&t.get::<Option<String>>("base")?.unwrap_or("dark".into()))?;
                    for pair in t.pairs::<String, String>() {
                        let (k, v) = pair?;
                        if k == "base" {
                            continue;
                        }
                        let Some(slot) = th.color_mut(&k) else {
                            return Err(err(format!(
                                "unknown key {k} ({})",
                                Theme::COLOR_KEYS.join(", ")
                            )));
                        };
                        *slot =
                            crate::theme::parse_color(&v).map_err(|e| err(format!("{k}: {e}")))?;
                    }
                    th
                }
                _ => return Err(err("a theme name or a table of colours".into())),
            };
            registry(lua)?.borrow_mut().options.theme = Some(theme);
            Ok(())
        })?,
    )?;
    vd.set(
        "option",
        lua.create_function(|lua, (name, value): (String, String)| {
            let v = crate::options::check(&name, &value)
                .map_err(|e| mlua::Error::RuntimeError(format!("vd.option: {e}")))?;
            if name == "keymap" && v == "vim" {
                lua.load(VIM_PROFILE).set_name("vim profile").exec()?;
            }
            let r = registry(lua)?;
            let mut reg = r.borrow_mut();
            reg.options.apply(&name, &v);
            let from = reg.loading.clone();
            reg.options.from.insert(name, from);
            Ok(())
        })?,
    )?;
    vd.set(
        "loaders",
        lua.create_function(|lua, ()| {
            let out = lua.create_table()?;
            for (i, l) in registry(lua)?.borrow().loaders.iter().enumerate() {
                let t = lua.create_table()?;
                t.set("name", l.name.clone())?;
                t.set("extensions", l.extensions.clone())?;
                t.set("patterns", l.patterns.clone())?;
                out.set(i + 1, t)?;
            }
            Ok(out)
        })?,
    )?;
    vd.set(
        "reset",
        lua.create_function(|lua, ()| {
            let r = registry(lua)?;
            let mut reg = r.borrow_mut();
            if reg.current.is_some() {
                return Err(mlua::Error::RuntimeError(
                    "vd.reset works at the top level of a config file, not in a function".into(),
                ));
            }
            let loading = std::mem::take(&mut reg.loading);
            *reg = Registry {
                loading,
                ..Default::default()
            };
            Ok(())
        })?,
    )?;
    vd.set(
        "config_dirs",
        lua.create_function(|lua, ()| {
            let t = lua.create_table()?;
            if let Some(d) = lua.app_data_ref::<ConfigDirs>() {
                t.set("global", d.global.as_ref().map(|p| p.display().to_string()))?;
                t.set(
                    "project",
                    d.project.as_ref().map(|p| p.display().to_string()),
                )?;
            }
            Ok(t)
        })?,
    )?;
    lua.globals().set("vd", vd)?;
    Ok(())
}

/// `vd.command(name, fn)`: `:name args` calls `fn(args)` ("" without args).
/// Names are `[a-z0-9_-]`, and can't shadow a built-in or an action name.
fn command_fn(lua: &Lua, (name, f): (String, LuaFunction)) -> mlua::Result<()> {
    let ok = !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'_');
    if !ok {
        return Err(mlua::Error::RuntimeError(format!(
            "vd.command: bad name {name:?} (use a-z, 0-9, - and _)"
        )));
    }
    if crate::exline::command_names()
        .iter()
        .any(|c| c.replace('-', "_") == name.replace('-', "_"))
    {
        return Err(mlua::Error::RuntimeError(format!(
            "vd.command: :{name} is built in"
        )));
    }
    let r = registry(lua)?;
    let mut reg = r.borrow_mut();
    reg.callbacks.push(f);
    let id = reg.callbacks.len() - 1;
    reg.commands.retain(|(n, _)| *n != name); // redefining replaces
    reg.commands.push((name, id));
    Ok(())
}

/// `vd.bind(layer, keys, action, desc?)`
fn bind_fn(
    lua: &Lua,
    (layer_s, keys_val, action_v, desc): (String, LuaValue, LuaValue, Option<String>),
) -> mlua::Result<()> {
    let layer = parse_layer(&layer_s)
        .ok_or_else(|| mlua::Error::RuntimeError(format!("unknown layer: {layer_s}")))?;
    let keys = parse_keys(keys_val)?;
    let action_s = match action_v {
        LuaValue::Function(f) => {
            let id = {
                let r = registry(lua)?;
                let mut reg = r.borrow_mut();
                reg.callbacks.push(f);
                reg.callbacks.len() - 1
            };
            return push_patch(
                lua,
                layer,
                keys,
                Action::Lua(id),
                desc.unwrap_or("lua function".into()),
            );
        }
        LuaValue::String(s) => s.to_str()?.to_string(),
        // A list of steps (S-19 macro): played in order, each after the
        // previous one's background work.
        LuaValue::Table(t) => {
            let steps = t
                .sequence_values::<String>()
                .map(|s| parse_step(lua, &s?))
                .collect::<mlua::Result<Vec<_>>>()?;
            if steps.is_empty() {
                return Err(mlua::Error::RuntimeError(
                    "vd.bind: empty action list".into(),
                ));
            }
            return push_patch(
                lua,
                layer,
                keys,
                Action::Macro(steps),
                desc.unwrap_or("macro".into()),
            );
        }
        other => {
            return Err(mlua::Error::RuntimeError(format!(
                "vd.bind: action must be a name, \":command\", list or function, not {}",
                other.type_name()
            )));
        }
    };
    let action = parse_step(lua, &action_s)?;
    let desc = desc.unwrap_or(action_s);
    push_patch(lua, layer, keys, action, desc)
}

/// An action name or a `":command"` (validated now so typos fail at load;
/// a `vd.command` must be defined before it is bound).
fn parse_step(lua: &Lua, action_s: &str) -> mlua::Result<Action> {
    match action_s.strip_prefix(':') {
        Some(cmd) => {
            let name = cmd.split_whitespace().next().unwrap_or("");
            let lua_cmd = registry(lua)?
                .borrow()
                .commands
                .iter()
                .any(|(n, _)| n == name);
            if !lua_cmd {
                crate::exline::parse(cmd).map_err(|e| {
                    mlua::Error::RuntimeError(format!("bad ex-command {action_s}: {e}"))
                })?;
            }
            Ok(Action::ExCommand(cmd.to_string()))
        }
        None => Action::from_name(action_s)
            .ok_or_else(|| mlua::Error::RuntimeError(format!("unknown action: {action_s}"))),
    }
}

fn push_patch(
    lua: &Lua,
    layer: Layer,
    on: Vec<Key>,
    action: Action,
    desc: String,
) -> mlua::Result<()> {
    let chord = Chord {
        on,
        run: vec![action],
        desc: desc.into(),
    };
    registry(lua)?
        .borrow_mut()
        .patches
        .push(KeymapPatch { layer, chord });
    Ok(())
}

/// `vd.unbind(layer, keys)` — pushes a chord that fires `redraw` (noop-ish).
/// A proper remove-from-vec would need to touch the base keymap; last-wins handles the common case.
fn unbind_fn(lua: &Lua, (layer_s, keys_val): (String, LuaValue)) -> mlua::Result<()> {
    let layer = parse_layer(&layer_s)
        .ok_or_else(|| mlua::Error::RuntimeError(format!("unknown layer: {layer_s}")))?;
    let keys = parse_keys(keys_val)?;
    let chord = Chord {
        on: keys,
        run: vec![Action::Redraw],
        desc: "unbound".into(),
    };
    registry(lua)?
        .borrow_mut()
        .patches
        .push(KeymapPatch { layer, chord });
    Ok(())
}

/// `vd.register_loader { name = "...", extensions = {"...", ...}, open = fn(path) end }`
/// (`name` is accepted and unused).
///
/// `open(path)` returns a path to a file in a format we scan natively (the
/// plugin owns that tempfile), or a `{ columns, rows }` table as `vd.push_sheet`
/// takes. Heavy-dep or huge formats belong in `vdrs-ext-*` Rust crates (S-09).
fn register_loader_fn(lua: &Lua, spec: LuaTable) -> mlua::Result<()> {
    let open: LuaFunction = spec.get("open")?;
    let name: Option<String> = spec.get("name")?;
    let list = |key: &str| -> mlua::Result<Vec<String>> {
        match spec.get::<LuaValue>(key)? {
            LuaValue::Nil => Ok(Vec::new()),
            v => strings(v, key),
        }
    };
    let extensions: Vec<String> = list("extensions")?
        .iter()
        .map(|e| e.to_ascii_lowercase())
        .collect();
    let patterns = list("patterns")?;
    if extensions.is_empty() && patterns.is_empty() && name.is_none() {
        return Err(mlua::Error::RuntimeError(
            "vd.register_loader: give extensions, patterns or a name".into(),
        ));
    }
    let globs = patterns
        .iter()
        .map(|p| glob_regex(p).map_err(|e| mlua::Error::RuntimeError(e.to_string())))
        .collect::<mlua::Result<_>>()?;
    let rule = LoaderRule {
        name,
        extensions,
        patterns,
        globs,
        open,
    };
    registry(lua)?.borrow_mut().loaders.push(rule);
    Ok(())
}

/// Polars value → Lua: numbers stay numbers, null is nil, the rest is text.
fn to_lua(lua: &Lua, v: &AnyValue) -> mlua::Result<LuaValue> {
    Ok(match v {
        AnyValue::Null => LuaValue::Nil,
        AnyValue::Boolean(b) => LuaValue::Boolean(*b),
        AnyValue::Int8(_)
        | AnyValue::Int16(_)
        | AnyValue::Int32(_)
        | AnyValue::Int64(_)
        | AnyValue::UInt8(_)
        | AnyValue::UInt16(_)
        | AnyValue::UInt32(_) => LuaValue::Integer(v.extract::<i64>().unwrap_or_default()),
        AnyValue::UInt64(_) | AnyValue::Float32(_) | AnyValue::Float64(_) => {
            LuaValue::Number(v.extract::<f64>().unwrap_or_default())
        }
        AnyValue::String(s) => LuaValue::String(lua.create_string(s)?),
        other => LuaValue::String(lua.create_string(other.to_string())?),
    })
}

/// One cell of a pushed sheet. Held as plain data: keeping a `LuaValue` per
/// cell would exhaust mlua's reference stack on a big sheet, and that panics.
enum Cell {
    Null,
    Bool(bool),
    Int(i64),
    Num(f64),
    Text(String),
}

impl Cell {
    fn text(&self) -> Option<String> {
        match self {
            Cell::Null => None,
            Cell::Bool(b) => Some(b.to_string()),
            Cell::Int(i) => Some(i.to_string()),
            Cell::Num(f) => Some(format!("{f:?}")),
            Cell::Text(t) => Some(t.clone()),
        }
    }
}

/// Length of array `t`: every key an integer 1..=n, with no gaps (`ipairs` and
/// raw length both stop quietly at a gap, losing whatever follows).
fn array_len(t: &LuaTable) -> Result<usize, String> {
    let (mut n, mut max) = (0usize, 0i64);
    for pair in t.pairs::<LuaValue, LuaValue>() {
        let (k, _) = pair.map_err(|e| e.to_string())?;
        let Some(i) = k.as_integer().filter(|i| *i >= 1) else {
            return Err(format!("key {k:?} isn't a position"));
        };
        n += 1;
        max = max.max(i);
    }
    if max as usize != n {
        return Err(format!("has gaps (position {max} of {n} values)"));
    }
    Ok(n)
}

/// `{ name?, columns = {…}, rows = { {…}, … } }` → a DataFrame. Per column:
/// all integers → i64, numbers → f64, all booleans → bool, else text; nil = null.
fn sheet_from_lua(spec: &LuaTable, who: &str) -> mlua::Result<(String, DataFrame)> {
    let bad = |m: String| mlua::Error::RuntimeError(format!("{who}: {m}"));
    let table = |key: &str| match spec.get::<LuaValue>(key)? {
        LuaValue::Table(t) => Ok(t),
        other => Err(bad(format!(
            "{key} must be a table, not {}",
            other.type_name()
        ))),
    };
    let text = |v: LuaValue| match v {
        LuaValue::String(s) => Ok(s.to_string_lossy()),
        v @ (LuaValue::Integer(_) | LuaValue::Number(_)) => v.to_string(),
        other => Err(bad(format!("expected text, not {}", other.type_name()))),
    };
    let name = match spec.get::<LuaValue>("name")? {
        LuaValue::Nil => None,
        v => Some(text(v)?),
    };
    let cols = table("columns")?;
    let width = array_len(&cols).map_err(|e| bad(format!("columns {e}")))?;
    let columns: Vec<String> = (1..=width)
        .map(|i| text(cols.raw_get(i)?))
        .collect::<mlua::Result<_>>()?;
    if columns.is_empty() {
        return Err(bad("columns is empty".into()));
    }
    if let Some(d) = columns
        .iter()
        .enumerate()
        .find(|(i, c)| columns[..*i].contains(c))
    {
        return Err(bad(format!("duplicate column {:?}", d.1)));
    }
    if columns.iter().any(|c| c == crate::sheet::ROW_ID) {
        return Err(bad(format!("{} is reserved", crate::sheet::ROW_ID)));
    }
    let rows = table("rows")?;
    let height = array_len(&rows).map_err(|e| bad(format!("rows {e}")))?;
    let mut cells: Vec<Vec<Cell>> = (0..width)
        .map(|_| (0..height).map(|_| Cell::Null).collect())
        .collect();
    #[allow(clippy::needless_range_loop)] // `r` also addresses the Lua table
    for r in 0..height {
        let LuaValue::Table(row) = rows.raw_get(r + 1)? else {
            return Err(bad(format!("row {} isn't a table", r + 1)));
        };
        for pair in row.pairs::<LuaValue, LuaValue>() {
            let (k, v) = pair?;
            let Some(c) = k.as_integer().filter(|c| *c >= 1) else {
                return Err(bad(format!(
                    "row {}: values go by position, found key {k:?}",
                    r + 1
                )));
            };
            let c = c as usize - 1;
            if c >= width {
                return Err(bad(format!("row {} has more values than columns", r + 1)));
            }
            cells[c][r] = match v {
                LuaValue::Nil => Cell::Null,
                LuaValue::Boolean(b) => Cell::Bool(b),
                LuaValue::Integer(i) => Cell::Int(i),
                LuaValue::Number(f) => Cell::Num(f),
                v @ (LuaValue::Table(_) | LuaValue::Function(_)) => {
                    let name = &columns[c];
                    return Err(bad(format!(
                        "column {name:?}: can't show a {}",
                        v.type_name()
                    )));
                }
                LuaValue::String(s) => Cell::Text(s.to_string_lossy()),
                v => Cell::Text(v.to_string()?),
            };
        }
    }
    let mut out = Vec::with_capacity(width);
    for (name, vals) in columns.iter().zip(&cells) {
        let n = name.as_str().into();
        let all = |f: fn(&Cell) -> bool| vals.iter().all(|v| matches!(v, Cell::Null) || f(v));
        let series = if vals.iter().all(|v| matches!(v, Cell::Null)) {
            Series::full_null(n, vals.len(), &DataType::String)
        } else if all(|v| matches!(v, Cell::Int(_))) {
            let int = |v: &Cell| if let Cell::Int(i) = v { Some(*i) } else { None };
            Series::new(n, vals.iter().map(int).collect::<Vec<_>>())
        } else if all(|v| matches!(v, Cell::Int(_) | Cell::Num(_))) {
            let num = |v: &Cell| match v {
                Cell::Int(i) => Some(*i as f64),
                Cell::Num(f) => Some(*f),
                _ => None,
            };
            Series::new(n, vals.iter().map(num).collect::<Vec<_>>())
        } else if all(|v| matches!(v, Cell::Bool(_))) {
            let flag = |v: &Cell| {
                if let Cell::Bool(b) = v {
                    Some(*b)
                } else {
                    None
                }
            };
            Series::new(n, vals.iter().map(flag).collect::<Vec<_>>())
        } else {
            Series::new(n, vals.iter().map(Cell::text).collect::<Vec<_>>())
        };
        out.push(Column::from(series));
    }
    let df = DataFrame::new_infer_height(out).map_err(|e| bad(e.to_string()))?;
    Ok((name.unwrap_or_else(|| "lua".into()), df))
}

fn reg(lua: &Lua) -> Option<mlua::AppDataRef<'_, RefCell<Registry>>> {
    lua.app_data_ref::<RefCell<Registry>>()
}

fn registry(lua: &Lua) -> mlua::Result<mlua::AppDataRef<'_, RefCell<Registry>>> {
    reg(lua).ok_or_else(|| mlua::Error::RuntimeError("registry missing".into()))
}

fn parse_layer(s: &str) -> Option<Layer> {
    match s {
        "sheet" => Some(Layer::Sheet),
        "input" => Some(Layer::Input),
        _ => None,
    }
}

/// A string or a table of strings (`what` names the argument in the error).
fn strings(v: LuaValue, what: &str) -> mlua::Result<Vec<String>> {
    match v {
        LuaValue::String(s) => Ok(vec![s.to_str()?.to_string()]),
        LuaValue::Table(t) => t.sequence_values().collect(),
        other => Err(mlua::Error::RuntimeError(format!(
            "{what} must be string or table of strings, got {}",
            other.type_name()
        ))),
    }
}

fn parse_keys(v: LuaValue) -> mlua::Result<Vec<Key>> {
    strings(v, "keys")?
        .iter()
        .map(|s| s.parse::<Key>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| mlua::Error::RuntimeError(format!("bad key: {e}")))
}

/// First TUI start: write [`default_init_lua`] where `init.lua` goes, unless
/// a file is there. A failed write (read-only home) is not worth a warning.
pub fn ensure_default_config() {
    if let Some(p) = default_config_path() {
        let _ = write_default_config(&p);
    }
}

/// Create `path` holding [`default_init_lua`]; never replaces a file.
fn write_default_config(path: &Path) -> std::io::Result<()> {
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
    let mut out = format!(
        "-- vdrs config: loaded at startup, reloaded when saved.\n\
         -- vdrs wrote this file because there was none. Every line below is a\n\
         -- default, commented out: uncomment and edit one to change it. Deleting\n\
         -- the file is safe; the next start writes a fresh one.\n\
         -- Lua API version {API_VERSION}. Guide: https://neumann-mlucas.github.io/visidata-rs/\n\
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
         -- Decimals of every float column (\"%.Nf\" or \"{{:.Nf}}\"). Default: shortest\n\
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
         -- A totals row under the table: each column's + aggregators. Default off; :footer\n\
         -- turns it on or off.\n\
         -- vd.option(\"disp_footer\", \"true\")\n\
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
         --   keys: \"x\", or {{\"g\", \"g\"}} for a sequence; <C-d>, <CR>, <Esc>, <Tab>, …\n\
         --   action: a name below, \":ex command\", or a Lua function.\n\
         -- vd.unbind(layer, keys) removes a binding.\n"
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
fn lua_str(s: &str) -> String {
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
struct ConfigDirs {
    global: Option<PathBuf>,
    project: Option<PathBuf>,
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
fn trust_copy(project: &Path) -> PathBuf {
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

fn default_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("vdrs").join("init.lua"))
}

/// A renderer's table: `{ text = …, fg = …, bg = …, bold = true, … }`; a bad
/// color is left out (renderers run every frame, an error there can't stop).
fn styled(t: &LuaTable) -> Rendered {
    use ratatui::style::Modifier;
    let color = |k: &str| {
        t.get::<Option<String>>(k)
            .ok()
            .flatten()
            .and_then(|c| crate::theme::parse_color(&c).ok())
    };
    let mut style = Style {
        fg: color("fg"),
        bg: color("bg"),
        ..Style::default()
    };
    for (k, m) in [
        ("bold", Modifier::BOLD),
        ("italic", Modifier::ITALIC),
        ("underline", Modifier::UNDERLINED),
        ("reverse", Modifier::REVERSED),
    ] {
        if t.get::<Option<bool>>(k).ok().flatten() == Some(true) {
            style = style.add_modifier(m);
        }
    }
    Rendered {
        text: t.get::<Option<String>>("text").ok().flatten(),
        style,
    }
}

/// `"true"` / `"false"`, for `vd.option` and `:set` (S-38 A).
pub fn parse_bool_option(name: &str, value: &str) -> Result<bool, String> {
    match value {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("{name} is \"true\" or \"false\"")),
    }
}

/// `histogram_bins`: a count, 0 for ⌊√rows⌋.
pub fn parse_histogram_bins(value: &str) -> Result<usize, String> {
    value
        .parse()
        .map_err(|_| format!("histogram_bins is a count, 0 for √rows (got {value:?})"))
}

/// VisiData's float format, `%.2f` or `{:.2f}` (S-32): its decimals, 0–15.
pub fn float_fmt_digits(fmt: &str) -> Option<u8> {
    let n = fmt
        .strip_prefix("%.")
        .and_then(|r| r.strip_suffix('f'))
        .or_else(|| fmt.strip_prefix("{:.").and_then(|r| r.strip_suffix("f}")))?;
    n.parse().ok().filter(|d| *d <= 15)
}

#[cfg(test)]
mod tests {
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
        let p = Plugins::load_from_source(r#"vd.bind("sheet", "x", "goto_top", "jump to top")"#)
            .unwrap();
        let mut km = Keymap::defaults();
        p.apply_keymap(&mut km);
        let d = km.resolve(Layer::Sheet, &[], "x".parse().unwrap());
        assert!(matches!(d, Dispatch::Fire(v) if matches!(v[0], Action::GotoTop)));
    }

    #[test]
    fn bind_registers_a_chord_from_table() {
        let p = Plugins::load_from_source(r#"vd.bind("sheet", {"<Space>", "f"}, "search_next")"#)
            .unwrap();
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
        let p =
            Plugins::load_from_source(r#"vd.option("clipboard_copy_cmd", "wl-copy -n")"#).unwrap();
        assert_eq!(
            p.options().clipboard_copy_cmd.as_deref(),
            Some("wl-copy -n")
        );
        let p =
            Plugins::load_from_source(r#"vd.option("clipboard_paste_cmd", "wl-paste")"#).unwrap();
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
    fn api_version_is_exposed() {
        let p = Plugins::load_from_source("assert(vd.api_version >= 1)");
        assert!(p.is_ok());
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
        let html = include_str!("../docs/index.html");
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
        let html = include_str!("../docs/index.html");
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
        let html = include_str!("../docs/index.html");
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
        let p =
            Plugins::load_from_source(&src(r#"{ { 1, 1.5, true, "a" }, { nil, 2, false, 3 } }"#))
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
        assert!(
            Plugins::load_from_source("vd.push_sheet { columns = { 'a' }, rows = {} }").is_err()
        );
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
        let html = include_str!("../docs/index.html");
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
}
