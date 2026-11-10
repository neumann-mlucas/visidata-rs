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
use polars::prelude::{AnyValue, Column, DataFrame, DataType, NamedFrom, Series};

#[derive(thiserror::Error, Debug)]
pub enum PluginError {
    #[error("lua: {0}")]
    Lua(#[from] mlua::Error),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Default, Clone)]
struct Registry {
    patches: Vec<KeymapPatch>,
    loaders: Vec<LoaderRule>,
    theme: Option<String>,
    /// `vd.option("clipboard_copy_cmd", …)`.
    clipboard_copy_cmd: Option<String>,
    /// `vd.option("clipboard_paste_cmd", …)`.
    clipboard_paste_cmd: Option<String>,
    /// `vd.option("duckdb_cmd", …)`.
    duckdb_cmd: Option<String>,
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
    /// `vd.on` handlers (S-29), in registration order.
    handlers: Vec<(String, LuaFunction)>,
}

/// Events `vd.on` takes.
pub const EVENTS: &[&str] = &["open", "push", "save", "quit"];

/// What a `vd.bind` function asked for: status messages and new sheets.
#[derive(Default)]
pub struct LuaOutput {
    pub toasts: Vec<String>,
    pub sheets: Vec<(String, DataFrame)>,
    /// `vd.exec` ex commands (without the `:`), in call order.
    pub execs: Vec<String>,
}

/// What a `vd.bind` function sees through `vd.sheet()`.
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
    pub fn call(&self, values: &Series) -> Result<AggValue, PluginError> {
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
pub const API_VERSION: i64 = 9; // 2: function bindings, vd.toast, vd.sheet; 3: vd.push_sheet; 4: vd.command; 5: vd.option; 6: duckdb_cmd; 7: clipboard_paste_cmd; 8: vd.bind lists (macros); 9: vd.on, vd.exec

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
    pub name: String,
    pub extensions: Vec<String>,
    pub open: LuaFunction,
}

impl std::fmt::Debug for LoaderRule {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoaderRule")
            .field("name", &self.name)
            .field("extensions", &self.extensions)
            .finish_non_exhaustive()
    }
}

pub struct Plugins {
    lua: Lua,
    /// `init.lua` this host was loaded from and its mtime then (hot reload).
    config: Option<(PathBuf, Option<std::time::SystemTime>)>,
    /// Renderer results by (column, dtype, value): renderers run on the UI
    /// thread, so each distinct cell is rendered once, not every frame.
    rendered: RefCell<HashMap<(String, String, String), Option<String>>>,
}

impl Plugins {
    /// Build a plugin host from `$XDG_CONFIG_HOME/vdrs/init.lua`. Never fails:
    /// a broken `init.lua` keeps everything registered before the error and the
    /// error comes back as a warning. Missing init.lua is the common case.
    pub fn load() -> (Self, Option<String>) {
        Self::load_from(default_config_path())
    }

    fn load_from(path: Option<PathBuf>) -> (Self, Option<String>) {
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
        let err = src.and_then(|s| host.exec(&s, "init.lua").err());
        let mut warn = err.map(|e| format!("init.lua: {e}"));
        // macros.lua (S-19) sits next to init.lua and runs after it.
        let macros = host
            .config
            .as_ref()
            .map(|(p, _)| p.with_file_name("macros.lua"));
        if let Some(Ok(src)) = macros.map(std::fs::read_to_string)
            && let Err(e) = host.exec(&src, "macros.lua")
        {
            warn = Some(warn.map_or(String::new(), |w| w + "; ") + &format!("macros.lua: {e}"));
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
            rendered: Default::default(),
        }
    }

    fn exec(&self, src: &str, name: &str) -> Result<(), PluginError> {
        Ok(self.lua.load(src).set_name(name).exec()?)
    }

    /// Build with an explicit config string. Used by tests; errors on any Lua error.
    pub fn load_from_source(src: &str) -> Result<Self, PluginError> {
        let host = Self::empty(None);
        host.exec(src, "inline")?;
        Ok(host)
    }

    /// `init.lua` changed on disk (created, edited or deleted) since this host
    /// loaded it → a freshly loaded host plus its warning, if any.
    pub fn reload_if_changed(&self) -> Option<(Self, Option<String>)> {
        let (path, loaded) = self.config.as_ref()?;
        let now = std::fs::metadata(path).and_then(|m| m.modified()).ok();
        (now != *loaded).then(|| Self::load_from(Some(path.clone())))
    }

    /// The aggregator `vd.register_aggregator` defined as `name`. `Send`: pivots
    /// run it on a background job; each call holds the VM lock only for one cell.
    pub fn aggregator(&self, name: &str) -> Option<Aggregator> {
        let reg = self.lua.app_data_ref::<RefCell<Registry>>()?;
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
        self.lua
            .app_data_ref::<RefCell<Registry>>()
            .is_some_and(|r| !r.borrow().renderers.is_empty())
    }

    /// Display text from the first renderer matching the cell's column name or
    /// dtype (`i64`, `f64`, `str`, `date`, … as shown in headers). Lua errors
    /// fall back to the default display.
    /// Renderers are assumed pure: results are memoized per distinct value.
    pub fn render(&self, column: &str, dtype: &DataType, value: &AnyValue) -> Option<String> {
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

    fn render_uncached(&self, column: &str, dt: &str, value: &AnyValue) -> Option<String> {
        let reg = self.lua.app_data_ref::<RefCell<Registry>>()?;
        let f = {
            let reg = reg.borrow();
            let rule = reg
                .renderers
                .iter()
                .find(|r| r.column.as_deref() == Some(column) || r.dtype.as_deref() == Some(dt))?;
            rule.render.clone()
        };
        let v = to_lua(&self.lua, value).ok()?;
        f.call::<Option<String>>(v).ok().flatten()
    }

    /// Callback index of the `vd.command` named `name`.
    pub fn command(&self, name: &str) -> Option<usize> {
        let reg = self.lua.app_data_ref::<RefCell<Registry>>()?;
        reg.borrow()
            .commands
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
    }

    /// Names of the `vd.command` ex commands.
    pub fn command_names(&self) -> Vec<String> {
        self.lua
            .app_data_ref::<RefCell<Registry>>()
            .map(|r| r.borrow().commands.iter().map(|(n, _)| n.clone()).collect())
            .unwrap_or_default()
    }

    /// Run callback `id` with `vd.sheet()` = `ctx`; a `vd.command` function gets
    /// the text after its name (`args`), a `vd.bind` one nil. Returns its toasts
    /// and pushed sheets; a Lua error comes back as Err and discards both.
    pub fn call(
        &self,
        id: usize,
        ctx: &SheetCtx,
        args: Option<&str>,
    ) -> Result<LuaOutput, PluginError> {
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

    /// Some `vd.on(event, …)` handler is registered.
    pub fn handles(&self, event: &str) -> bool {
        self.lua
            .app_data_ref::<RefCell<Registry>>()
            .is_some_and(|r| r.borrow().handlers.iter().any(|(e, _)| e == event))
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
        let handlers: Vec<LuaFunction> = match registry(&self.lua) {
            Ok(r) => r
                .borrow()
                .handlers
                .iter()
                .filter(|(e, _)| e == event)
                .map(|(_, f)| f.clone())
                .collect(),
            Err(e) => return (out, vec![e.to_string()]),
        };
        for f in handlers {
            let run = || -> Result<LuaOutput, PluginError> {
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
    fn take_output(&self) -> Result<LuaOutput, PluginError> {
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
    fn ctx_table(&self, ctx: &SheetCtx) -> Result<LuaTable, PluginError> {
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

    /// Theme picked by `vd.set_theme`, if any.
    pub fn theme(&self) -> Option<crate::theme::Theme> {
        let reg = self.lua.app_data_ref::<RefCell<Registry>>()?;
        match reg.borrow().theme.as_deref()? {
            "light" => Some(crate::theme::Theme::light()),
            _ => Some(crate::theme::Theme::dark()),
        }
    }

    /// `vd.option("clipboard_copy_cmd", …)`, if set.
    pub fn clipboard_copy_cmd(&self) -> Option<String> {
        let reg = self.lua.app_data_ref::<RefCell<Registry>>()?;
        reg.borrow().clipboard_copy_cmd.clone()
    }

    /// `vd.option("clipboard_paste_cmd", …)`, if set.
    pub fn clipboard_paste_cmd(&self) -> Option<String> {
        let reg = self.lua.app_data_ref::<RefCell<Registry>>()?;
        reg.borrow().clipboard_paste_cmd.clone()
    }

    /// `vd.option("duckdb_cmd", …)`, if set.
    pub fn duckdb_cmd(&self) -> Option<String> {
        let reg = self.lua.app_data_ref::<RefCell<Registry>>()?;
        reg.borrow().duckdb_cmd.clone()
    }

    /// Mutate `keymap` by appending the Lua-registered patches.
    /// Last declaration wins at `Keymap::resolve` time (S-04 rule), so user binds shadow defaults.
    pub fn apply_keymap(&self, keymap: &mut Keymap) {
        let reg = match self.lua.app_data_ref::<RefCell<Registry>>() {
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
        let ext = path.extension()?.to_str()?.to_ascii_lowercase();
        let reg = self.lua.app_data_ref::<RefCell<Registry>>()?;
        reg.borrow()
            .loaders
            .iter()
            .find(|r| r.extensions.iter().any(|e| e == &ext))
            .cloned()
    }

    /// Call a loader's `open(path)` and return the path it emitted. The Lua side
    /// is expected to parse `src_path`, convert into a format we scan natively
    /// (CSV/TSV/Parquet/Arrow/Jsonl), and return the destination path. Tempfile
    /// cleanup is the plugin's responsibility — typically via `os.tmpname()`.
    pub fn call_loader(&self, rule: &LoaderRule, src_path: &Path) -> Result<PathBuf, PluginError> {
        let s = src_path.to_string_lossy().into_owned();
        let out: String = rule.open.call(s)?;
        Ok(PathBuf::from(out))
    }
}

/// Open `path` through a registered Lua loader when one claims its extension
/// (the loader converts to a native format, which is then scanned), else natively.
pub fn open(
    plugins: Option<&Plugins>,
    path: &std::ffi::OsStr,
    fmt: Option<SourceKind>,
) -> Result<LoadedSource, OpenError> {
    if let Some((p, rule)) = plugins.and_then(|p| Some((p, p.resolve_loader(Path::new(path))?))) {
        let converted = p
            .call_loader(&rule, Path::new(path))
            .map_err(|e| OpenError::Plugin(e.to_string()))?;
        let mut loaded = crate::reader::open(converted.as_os_str(), fmt)?;
        loaded.display_name = path.to_string_lossy().into_owned(); // what the user opened
        return Ok(loaded);
    }
    crate::reader::open(path, fmt)
}

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
                    "vd.toast works inside a vd.bind function".into(),
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
        lua.create_function(|lua, (event, f): (String, LuaFunction)| {
            if !EVENTS.contains(&event.as_str()) {
                return Err(mlua::Error::RuntimeError(format!(
                    "vd.on: unknown event {event} ({})",
                    EVENTS.join(", ")
                )));
            }
            registry(lua)?.borrow_mut().handlers.push((event, f));
            Ok(())
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
                    "vd.push_sheet works inside a vd.bind function".into(),
                ));
            }
            let sheet = sheet_from_lua(&spec)?;
            r.borrow_mut().pushed.push(sheet);
            Ok(())
        })?,
    )?;
    vd.set(
        "register_aggregator",
        lua.create_function(|lua, (name, f): (String, LuaFunction)| {
            registry(lua)?.borrow_mut().aggregators.push((name, f));
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
    vd.set(
        "set_theme",
        lua.create_function(|lua, name: String| {
            if !matches!(name.as_str(), "dark" | "light") {
                return Err(mlua::Error::RuntimeError(format!(
                    "unknown theme: {name} (dark|light)"
                )));
            }
            registry(lua)?.borrow_mut().theme = Some(name);
            Ok(())
        })?,
    )?;
    vd.set(
        "option",
        lua.create_function(|lua, (name, value): (String, String)| {
            match name.as_str() {
                "clipboard_copy_cmd" | "clipboard_paste_cmd" | "duckdb_cmd"
                    if value.trim().is_empty() =>
                {
                    return Err(mlua::Error::RuntimeError(format!(
                        "vd.option: {name} is empty"
                    )));
                }
                "clipboard_copy_cmd" => {
                    registry(lua)?.borrow_mut().clipboard_copy_cmd = Some(value)
                }
                "clipboard_paste_cmd" => {
                    registry(lua)?.borrow_mut().clipboard_paste_cmd = Some(value)
                }
                "duckdb_cmd" => registry(lua)?.borrow_mut().duckdb_cmd = Some(value),
                _ => {
                    return Err(mlua::Error::RuntimeError(format!(
                        "vd.option: unknown option {name} (clipboard_copy_cmd, clipboard_paste_cmd, duckdb_cmd)"
                    )));
                }
            }
            Ok(())
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
///
/// `open(path)` must return a path string pointing at a file in a format we scan
/// natively (CSV/TSV/Parquet/Arrow/Jsonl). The plugin owns the tempfile lifecycle.
///
/// Returning a DataFrame directly from Lua is intentionally **not supported** —
/// it would require marshalling Lua tables → Polars Series with per-value type
/// inference. Heavy-dep formats (SPSS, pcap, …) belong in `vdrs-ext-*` Rust crates
/// per the S-09 extension ADR; everything else can convert to CSV in Lua.
fn register_loader_fn(lua: &Lua, spec: LuaTable) -> mlua::Result<()> {
    let name: String = spec.get::<Option<String>>("name")?.unwrap_or_default();
    let open: LuaFunction = spec.get("open")?;
    let exts_val: LuaValue = spec.get("extensions")?;
    let extensions: Vec<String> = match exts_val {
        LuaValue::String(s) => vec![s.to_str()?.to_ascii_lowercase()],
        LuaValue::Table(t) => {
            let mut out = Vec::new();
            for pair in t.sequence_values::<String>() {
                out.push(pair?.to_ascii_lowercase());
            }
            out
        }
        other => {
            return Err(mlua::Error::RuntimeError(format!(
                "extensions must be string or table of strings, got {}",
                other.type_name()
            )));
        }
    };
    if extensions.is_empty() {
        return Err(mlua::Error::RuntimeError(
            "vd.register_loader: at least one extension required".into(),
        ));
    }
    let rule = LoaderRule {
        name,
        extensions,
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
fn sheet_from_lua(spec: &LuaTable) -> mlua::Result<(String, DataFrame)> {
    let bad = |m: String| mlua::Error::RuntimeError(format!("vd.push_sheet: {m}"));
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

fn registry(lua: &Lua) -> mlua::Result<mlua::AppDataRef<'_, RefCell<Registry>>> {
    lua.app_data_ref::<RefCell<Registry>>()
        .ok_or_else(|| mlua::Error::RuntimeError("registry missing".into()))
}

fn parse_layer(s: &str) -> Option<Layer> {
    match s {
        "sheet" => Some(Layer::Sheet),
        "input" => Some(Layer::Input),
        _ => None,
    }
}

fn parse_keys(v: LuaValue) -> mlua::Result<Vec<Key>> {
    let strs: Vec<String> = match v {
        LuaValue::String(s) => vec![s.to_str()?.to_string()],
        LuaValue::Table(t) => {
            let mut out = Vec::new();
            for pair in t.sequence_values::<String>() {
                out.push(pair?);
            }
            out
        }
        other => {
            return Err(mlua::Error::RuntimeError(format!(
                "keys must be string or table of strings, got {}",
                other.type_name()
            )));
        }
    };
    strs.iter()
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
        "More (see the guide): vd.register_loader { name, extensions, open }, vd.register_aggregator(name, fn), vd.register_renderer { column, render }, vd.sheet(), vd.toast(msg), vd.push_sheet { name, columns, rows }."
            .into(),
    );
    out
}

/// Where `m` saves macros: `macros.lua` next to `init.lua` (a temp dir under
/// `cfg!(test)`, so tests never touch the user's config).
pub fn macros_path() -> Option<PathBuf> {
    if cfg!(test) {
        let dir = std::env::temp_dir().join(format!("vdrs-test-{}", std::process::id()));
        return Some(dir.join("macros.lua"));
    }
    Some(default_config_path()?.with_file_name("macros.lua"))
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
    let prefix = &line[..line.find("}, {").map_or(line.len(), |i| i + 3)];
    let old = std::fs::read_to_string(path).unwrap_or_default();
    let mut out: Vec<&str> = old.lines().filter(|l| !l.starts_with(prefix)).collect();
    out.push(&line);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, out.join("\n") + "\n")
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

fn default_config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("vdrs").join("init.lua"))
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
        let mut km = Keymap::empty();
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
        let mut km = Keymap::empty();
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
        let (p, warn) = Plugins::load_from(Some(init.clone()));
        assert!(warn.unwrap().contains("boom"));
        let mut km = Keymap::defaults();
        p.apply_keymap(&mut km);
        assert!(matches!(
            km.resolve(Layer::Sheet, &[], "x".parse().unwrap()),
            Dispatch::Fire(_)
        ));
        assert!(p.theme().is_some());
        assert!(p.reload_if_changed().is_none());
        std::fs::remove_file(&init).unwrap(); // deleting counts as a change
        let (fresh, warn) = p.reload_if_changed().unwrap();
        assert!(warn.is_none() && fresh.theme().is_none());
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
            p.render("price", &DataType::Float64, &AnyValue::Float64(2.5)),
            Some("$2.5".into())
        );
        assert_eq!(
            p.render("ok", &DataType::Boolean, &AnyValue::Boolean(false)),
            Some("no".into())
        );
        assert_eq!(
            p.render("other", &DataType::Int64, &AnyValue::Int64(1)),
            None
        );
        assert!(Plugins::load_from_source("vd.register_renderer { render = print }").is_err());
    }

    #[test]
    fn option_sets_clipboard_cmd_and_rejects_unknown() {
        let p =
            Plugins::load_from_source(r#"vd.option("clipboard_copy_cmd", "wl-copy -n")"#).unwrap();
        assert_eq!(p.clipboard_copy_cmd().as_deref(), Some("wl-copy -n"));
        let p =
            Plugins::load_from_source(r#"vd.option("clipboard_paste_cmd", "wl-paste")"#).unwrap();
        assert_eq!(p.clipboard_paste_cmd().as_deref(), Some("wl-paste"));
        let p = Plugins::load_from_source(r#"vd.option("duckdb_cmd", "/opt/duckdb")"#).unwrap();
        assert_eq!(p.duckdb_cmd().as_deref(), Some("/opt/duckdb"));
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
        let mut km = Keymap::empty();
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
        let (p, warn) = Plugins::load_from(Some(dir.path().join("vdrs").join("init.lua")));
        assert_eq!(warn, None);
        let mut km = Keymap::empty();
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
    fn api_version_is_exposed() {
        let p = Plugins::load_from_source("assert(vd.api_version >= 1)");
        assert!(p.is_ok());
        assert!(Plugins::load_from_source("vd.set_theme('neon')").is_err());
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
        assert_eq!(out, PathBuf::from("input.mylog.csv"));
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
        let got = p.call_loader(&rule, &input_path).unwrap();
        assert_eq!(got, out_path);

        let loaded = crate::reader::open(got.as_os_str(), None).unwrap();
        let df = loaded.lazy.collect().unwrap();
        assert_eq!(df.height(), 2);
        assert_eq!(df.width(), 2);
        let _ = std::fs::remove_file(&input_path);
        let _ = std::fs::remove_file(&out_path);
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
        let price = |v| p.render("price", &DataType::Float64, &v);
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
    fn renderer_memo_keeps_close_floats_apart() {
        let p = Plugins::load_from_source(
            r#"vd.register_renderer{column="x",render=function(v) return string.format("%.8f",v) end}"#,
        )
        .unwrap();
        let r = |f| p.render("x", &DataType::Float64, &AnyValue::Float64(f));
        assert_eq!(r(1.00000001).as_deref(), Some("1.00000001"));
        assert_eq!(r(1.00000002).as_deref(), Some("1.00000002"));
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
