//! mlua plugin system. See `docs/specs/S-09-plugin-system.md`.
//!
//! Phase 3 minimum: Lua VM + `vd.bind(layer, keys, action, desc)` keymap patches.
//! Hot-reload, loader/aggregator/renderer shapes land in follow-up iterations.

use std::cell::RefCell;
use std::path::PathBuf;

use mlua::{Lua, Value as LuaValue};

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
}

#[derive(Clone, Debug)]
struct KeymapPatch {
    layer: Layer,
    chord: Chord,
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
}

fn install_api(lua: &Lua) -> Result<(), mlua::Error> {
    let vd = lua.create_table()?;
    let bind = lua.create_function(bind_fn)?;
    let unbind = lua.create_function(unbind_fn)?;
    vd.set("bind", bind)?;
    vd.set("unbind", unbind)?;
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
}
