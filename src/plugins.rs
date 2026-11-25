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

mod api;
mod bars;
mod config;
mod events;
mod loaders;
#[cfg(test)]
mod tests;

use api::*;
pub use bars::{Bar, BarCtx, Segments, compose};
pub use config::*;
pub use events::*;
pub use loaders::*;

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
    /// `vd.option("disp_header_rule", true)` (S-59): a rule under the header.
    pub disp_header_rule: bool,
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
            "disp_header_rule" => self.disp_header_rule = on,
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

#[derive(Clone, Debug)]
struct KeymapPatch {
    layer: Layer,
    chord: Chord,
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
}

/// VisiData's float format, `%.2f` or `{:.2f}` (S-32): its decimals, 0–15.
pub fn float_fmt_digits(fmt: &str) -> Option<u8> {
    let n = fmt
        .strip_prefix("%.")
        .and_then(|r| r.strip_suffix('f'))
        .or_else(|| fmt.strip_prefix("{:.").and_then(|r| r.strip_suffix("f}")))?;
    n.parse().ok().filter(|d| *d <= 15)
}
