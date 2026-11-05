//! mlua plugin system. See `docs/specs/S-09-plugin-system.md`.
//!
//! Shipped shapes: keymap (`vd.bind`/`vd.unbind`), loader (`vd.register_loader`).
//! Pending: aggregator, renderer, hot-reload.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use mlua::{Function as LuaFunction, Lua, Table as LuaTable, Value as LuaValue};

use crate::action::Action;
use crate::key::Key;
use crate::keymap::{Chord, Keymap, Layer};
use crate::reader::{LoadedSource, OpenError, SourceKind};
use polars::prelude::{AnyValue, DataType, Series};

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
    aggregators: Vec<(String, LuaFunction)>,
    renderers: Vec<RendererRule>,
    /// `vd.bind` functions, indexed by `Action::Lua`.
    callbacks: Vec<LuaFunction>,
    /// While a callback runs: what `vd.sheet()` returns, and `vd.toast` messages.
    current: Option<LuaTable>,
    toasts: Vec<String>,
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

/// Lua API version exposed as `vd.api_version`. Bump on additions; S-10
/// deprecation windows key off it.
pub const API_VERSION: i64 = 2; // 2: function bindings, vd.toast, vd.sheet

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
        (host, err.map(|e| format!("init.lua: {e}")))
    }

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

    /// Whether `vd.register_aggregator` defined `name`.
    pub fn has_aggregator(&self, name: &str) -> bool {
        self.lua
            .app_data_ref::<RefCell<Registry>>()
            .is_some_and(|r| r.borrow().aggregators.iter().any(|(n, _)| n == name))
    }

    /// Run aggregator `name` over one group's values (a Lua array; nulls are nil).
    pub fn aggregate(&self, name: &str, values: &Series) -> Result<AggValue, PluginError> {
        let f = {
            let reg = registry(&self.lua)?;
            let reg = reg.borrow();
            let found = reg
                .aggregators
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, f)| f.clone());
            found.ok_or_else(|| mlua::Error::RuntimeError(format!("no aggregator {name}")))?
        };
        let t = self.lua.create_table()?;
        for (i, v) in values.iter().enumerate() {
            t.raw_set(i + 1, to_lua(&self.lua, &v)?)?;
        }
        Ok(match f.call::<LuaValue>(t)? {
            LuaValue::Integer(n) => AggValue::Num(n as f64),
            LuaValue::Number(n) => AggValue::Num(n),
            LuaValue::String(s) => AggValue::Text(s.to_str()?.to_string()),
            LuaValue::Nil => AggValue::Null,
            other => AggValue::Text(format!("{other:?}")),
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
        let key = (column.to_string(), dt.clone(), value.to_string());
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

    /// Run `vd.bind` function `id` with `vd.sheet()` = `ctx`. Returns the
    /// `vd.toast` messages; a Lua error comes back as Err.
    pub fn call(&self, id: usize, ctx: &SheetCtx) -> Result<Vec<String>, PluginError> {
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
        let f = {
            let r = registry(lua)?;
            let mut reg = r.borrow_mut();
            reg.current = Some(t);
            reg.callbacks.get(id).cloned()
        };
        let result = match f {
            Some(f) => f.call::<()>(()),
            None => Err(mlua::Error::RuntimeError(format!("no lua binding #{id}"))),
        };
        let r = registry(lua)?;
        let mut reg = r.borrow_mut();
        reg.current = None;
        let toasts = std::mem::take(&mut reg.toasts);
        result?;
        Ok(toasts)
    }

    /// Theme picked by `vd.set_theme`, if any.
    pub fn theme(&self) -> Option<crate::theme::Theme> {
        let reg = self.lua.app_data_ref::<RefCell<Registry>>()?;
        match reg.borrow().theme.as_deref()? {
            "light" => Some(crate::theme::Theme::light()),
            _ => Some(crate::theme::Theme::dark()),
        }
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
    lua.globals().set("vd", vd)?;
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
        other => {
            return Err(mlua::Error::RuntimeError(format!(
                "vd.bind: action must be a name, \":command\" or function, not {}",
                other.type_name()
            )));
        }
    };
    // ":sort -id" binds an ex-command (validated now so typos fail at load).
    let action = match action_s.strip_prefix(':') {
        Some(cmd) => {
            crate::exline::parse(cmd).map_err(|e| {
                mlua::Error::RuntimeError(format!("bad ex-command {action_s}: {e}"))
            })?;
            Action::ExCommand(cmd.to_string())
        }
        None => action_from_name(&action_s)
            .ok_or_else(|| mlua::Error::RuntimeError(format!("unknown action: {action_s}")))?,
    };
    let desc = desc.unwrap_or(action_s);
    push_patch(lua, layer, keys, action, desc)
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
    let reg = lua
        .app_data_ref::<RefCell<Registry>>()
        .ok_or_else(|| mlua::Error::RuntimeError("registry missing".into()))?;
    reg.borrow_mut().patches.push(KeymapPatch { layer, chord });
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
    let reg = lua
        .app_data_ref::<RefCell<Registry>>()
        .ok_or_else(|| mlua::Error::RuntimeError("registry missing".into()))?;
    reg.borrow_mut().loaders.push(rule);
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

/// Map a snake_case action name to its default-arg variant.
///
/// S-10 locks the names in the Lua API. Keep this match in sync when adding new actions.
fn action_from_name(s: &str) -> Option<Action> {
    use Action::*;
    Some(match s {
        "cursor_down" => CursorDown { count: None },
        "cursor_up" => CursorUp { count: None },
        "cursor_left" => CursorLeft { count: None },
        "cursor_right" => CursorRight { count: None },
        "page_down" => PageDown,
        "page_up" => PageUp,
        "goto_top" => GotoTop,
        "goto_bottom" => GotoBottom,
        "goto_leftmost" => GotoLeftmost,
        "goto_rightmost" => GotoRightmost,
        "freeze_column" => FreezeColumn,
        "unfreeze_all" => UnfreezeAll,
        "hide_col" => HideCol,
        "unhide_all" => UnhideAll,
        "key_col" => KeyCol,
        "unkey_all" => UnkeyAll,
        "start_rename_col_input" => StartRenameColInput,
        "type_cast_int" => TypeCastInt,
        "type_cast_float" => TypeCastFloat,
        "type_cast_string" => TypeCastString,
        "type_cast_date" => TypeCastDate,
        "type_cast_currency" => TypeCastCurrency,
        "start_search_input" => StartSearchInput,
        "start_filter_input" => StartFilterInput,
        "start_ex_input" => StartExInput,
        // input layer (prompts)
        "submit_input" => SubmitInput,
        "cancel_input" => CancelInput,
        "input_backspace" => InputBackspace,
        "search_next" => SearchNext,
        "search_prev" => SearchPrev,
        "clear_filter" => ClearFilter,
        "clear_sort" => ClearSort,
        "sort_asc" => SortAsc,
        "sort_desc" => SortDesc,
        "open_freq_table" => OpenFreqTable,
        "open_sheet_switcher" => OpenSheetSwitcher,
        "submit_row" => SubmitRow,
        "submit_selected" => SubmitSelected,
        "toggle_plan_panel" => TogglePlanPanel,
        "pop_sheet" => PopSheet,
        "quit" => Quit,
        "redraw" => Redraw,
        "repeat_last" => RepeatLast,
        "toggle_select_row" => ToggleSelectRow,
        "select_row" => SelectRow,
        "unselect_row" => UnselectRow,
        "select_all" => SelectAll,
        "unselect_all" => UnselectAll,
        "start_select_input" => StartSelectInput,
        "dup_selected" => DupSelected,
        "dup_sheet" => DupSheet,
        "select_equal_cell" => SelectEqualCell,
        "hide_uniform" => HideUniform,
        "describe" => Describe,
        "pivot_current" => PivotCurrent,
        "start_diff_input" => StartDiffInput,
        "undo" => Undo,
        "redo" => Redo,
        _ => return None,
    })
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
        assert!(p.has_aggregator("span") && !p.has_aggregator("sum"));
        use polars::prelude::NamedFrom;
        let vals = Series::new("v".into(), [3i64, 9, 4]);
        assert_eq!(p.aggregate("span", &vals).unwrap(), AggValue::Num(6.0));
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
}
