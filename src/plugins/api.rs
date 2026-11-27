//! The `vd` table a Lua VM gets: `vd.bind`, `vd.on`, `vd.push_sheet`, ….

use super::*;

pub(super) fn install_api(lua: &Lua) -> Result<(), mlua::Error> {
    let vd = lua.create_table()?;
    let bind = lua.create_function(bind_fn)?;
    let unbind = lua.create_function(unbind_fn)?;
    let register_loader = lua.create_function(register_loader_fn)?;
    vd.set("bind", bind)?;
    vd.set("unbind", unbind)?;
    vd.set("command", lua.create_function(command_fn)?)?;
    vd.set("register_loader", register_loader)?;
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
pub(super) fn command_fn(lua: &Lua, (name, f): (String, LuaFunction)) -> mlua::Result<()> {
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
pub(super) fn bind_fn(
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
pub(super) fn parse_step(lua: &Lua, action_s: &str) -> mlua::Result<Action> {
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

pub(super) fn push_patch(
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
pub(super) fn unbind_fn(lua: &Lua, (layer_s, keys_val): (String, LuaValue)) -> mlua::Result<()> {
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
pub(super) fn register_loader_fn(lua: &Lua, spec: LuaTable) -> mlua::Result<()> {
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
pub(super) fn to_lua(lua: &Lua, v: &AnyValue) -> mlua::Result<LuaValue> {
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
pub(super) enum Cell {
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
pub(super) fn array_len(t: &LuaTable) -> Result<usize, String> {
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
pub(super) fn sheet_from_lua(spec: &LuaTable, who: &str) -> mlua::Result<(String, DataFrame)> {
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
    let df = frame_from_rows(&columns, Some(&rows), who)?;
    Ok((name.unwrap_or_else(|| "lua".into()), df))
}

/// `rows` (a list of rows, values by position) as a frame of `columns`, a
/// type per column as `vd.push_sheet` gives them; None: no rows.
pub(super) fn frame_from_rows(
    columns: &[String],
    rows: Option<&LuaTable>,
    who: &str,
) -> mlua::Result<DataFrame> {
    let bad = |m: String| mlua::Error::RuntimeError(format!("{who}: {m}"));
    let width = columns.len();
    let height = match rows {
        Some(rows) => array_len(rows).map_err(|e| bad(format!("rows {e}")))?,
        None => 0,
    };
    let mut cells: Vec<Vec<Cell>> = (0..width)
        .map(|_| (0..height).map(|_| Cell::Null).collect())
        .collect();
    #[allow(clippy::needless_range_loop)] // `r` also addresses the Lua table
    for r in 0..height {
        let Some(LuaValue::Table(row)) = rows.map(|t| t.raw_get(r + 1)).transpose()? else {
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
    DataFrame::new_infer_height(out).map_err(|e| bad(e.to_string()))
}

pub(super) fn reg(lua: &Lua) -> Option<mlua::AppDataRef<'_, RefCell<Registry>>> {
    lua.app_data_ref::<RefCell<Registry>>()
}

pub(super) fn registry(lua: &Lua) -> mlua::Result<mlua::AppDataRef<'_, RefCell<Registry>>> {
    reg(lua).ok_or_else(|| mlua::Error::RuntimeError("registry missing".into()))
}

pub(super) fn parse_layer(s: &str) -> Option<Layer> {
    match s {
        "sheet" => Some(Layer::Sheet),
        "input" => Some(Layer::Input),
        _ => None,
    }
}

/// A string or a table of strings (`what` names the argument in the error).
pub(super) fn strings(v: LuaValue, what: &str) -> mlua::Result<Vec<String>> {
    match v {
        LuaValue::String(s) => Ok(vec![s.to_str()?.to_string()]),
        LuaValue::Table(t) => t.sequence_values().collect(),
        other => Err(mlua::Error::RuntimeError(format!(
            "{what} must be string or table of strings, got {}",
            other.type_name()
        ))),
    }
}

pub(super) fn parse_keys(v: LuaValue) -> mlua::Result<Vec<Key>> {
    strings(v, "keys")?
        .iter()
        .map(|s| s.parse::<Key>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| mlua::Error::RuntimeError(format!("bad key: {e}")))
}

/// A renderer's table: `{ text = …, fg = …, bg = …, bold = true, … }`; a bad
/// color is left out (renderers run every frame, an error there can't stop).
pub(super) fn styled(t: &LuaTable) -> Rendered {
    use crate::style::Modifier;
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
