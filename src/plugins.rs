//! mlua plugin system. See `docs/specs/S-09-plugin-system.md`.
//!
//! Shipped shapes: keymap (`vd.bind`/`vd.unbind`), loader (`vd.register_loader`).
//! Pending: aggregator, renderer, hot-reload.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use mlua::{Function as LuaFunction, Lua, Table as LuaTable, Value as LuaValue};

use crate::action::Action;
use crate::key::Key;
use crate::keymap::{Chord, Keymap, Layer};

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
}

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
    config_path: Option<PathBuf>,
}

impl Plugins {
    /// Build a plugin host; immediately executes `init.lua` if it exists.
    /// Missing init.lua is NOT an error — the common case.
    pub fn load() -> Result<Self, PluginError> {
        let lua = Lua::new();
        lua.set_app_data(RefCell::new(Registry::default()));
        install_api(&lua)?;
        let config_path = default_config_path();
        if let Some(ref p) = config_path {
            if p.exists() {
                let src = std::fs::read_to_string(p)?;
                lua.load(&src).set_name("init.lua").exec()?;
            }
        }
        Ok(Self { lua, config_path })
    }

    /// Build with an explicit config string. Used by tests.
    pub fn load_from_source(src: &str) -> Result<Self, PluginError> {
        let lua = Lua::new();
        lua.set_app_data(RefCell::new(Registry::default()));
        install_api(&lua)?;
        lua.load(src).set_name("inline").exec()?;
        Ok(Self {
            lua,
            config_path: None,
        })
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

    pub fn config_path(&self) -> Option<&std::path::Path> {
        self.config_path.as_deref()
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
    pub fn call_loader(
        &self,
        rule: &LoaderRule,
        src_path: &Path,
    ) -> Result<PathBuf, PluginError> {
        let s = src_path.to_string_lossy().into_owned();
        let out: String = rule.open.call(s)?;
        Ok(PathBuf::from(out))
    }
}

fn install_api(lua: &Lua) -> Result<(), mlua::Error> {
    let vd = lua.create_table()?;
    let bind = lua.create_function(bind_fn)?;
    let unbind = lua.create_function(unbind_fn)?;
    let register_loader = lua.create_function(register_loader_fn)?;
    vd.set("bind", bind)?;
    vd.set("unbind", unbind)?;
    vd.set("register_loader", register_loader)?;
    lua.globals().set("vd", vd)?;
    Ok(())
}

/// `vd.bind(layer, keys, action, desc?)`
fn bind_fn(
    lua: &Lua,
    (layer_s, keys_val, action_s, desc): (String, LuaValue, String, Option<String>),
) -> mlua::Result<()> {
    let layer = parse_layer(&layer_s)
        .ok_or_else(|| mlua::Error::RuntimeError(format!("unknown layer: {layer_s}")))?;
    let keys = parse_keys(keys_val)?;
    let action = action_from_name(&action_s)
        .ok_or_else(|| mlua::Error::RuntimeError(format!("unknown action: {action_s}")))?;
    let desc_leaked: &'static str =
        Box::leak(desc.unwrap_or_else(|| action_s.clone()).into_boxed_str());
    let chord = Chord {
        on: keys,
        run: vec![action],
        desc: desc_leaked,
    };
    let reg = lua
        .app_data_ref::<RefCell<Registry>>()
        .ok_or_else(|| mlua::Error::RuntimeError("registry missing".into()))?;
    reg.borrow_mut().patches.push(KeymapPatch { layer, chord });
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
        desc: "unbound",
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
        "search_next" => SearchNext,
        "search_prev" => SearchPrev,
        "clear_filter" => ClearFilter,
        "clear_sort" => ClearSort,
        "sort_asc" => SortAsc,
        "sort_desc" => SortDesc,
        "open_freq_table" => OpenFreqTable,
        "open_sheet_switcher" => OpenSheetSwitcher,
        "submit_row" => SubmitRow,
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
