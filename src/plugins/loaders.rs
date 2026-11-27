//! Lua loaders (S-09, S-39, S-40 C) and `open`, which picks one or a built-in reader.

use super::*;

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
    pub(super) globs: Vec<regex::Regex>,
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
    /// `{ columns, rows, poll }` (S-39 B): the first rows, then `poll()`
    /// on each tail tick for more.
    Live {
        columns: Vec<String>,
        df: DataFrame,
        poll: LuaFunction,
    },
}

/// A loader's `poll` (S-39 B): it returns new rows (a list, as `rows`) or
/// nil. It runs on the UI thread, which owns the Lua VM.
pub struct LuaSource {
    /// Keeps the VM `poll` lives in: an `init.lua` reload drops the app's.
    _lua: Lua,
    who: String,
    columns: Vec<String>,
    poll: LuaFunction,
    df: DataFrame,
    /// `source_max_rows`: older rows go past it; 0 keeps all.
    max_rows: usize,
    dropped: usize,
}

impl crate::source::Source for LuaSource {
    fn poll(&mut self) -> Result<bool, String> {
        let who = format!("{}: poll", self.who);
        let rows = match self.poll.call::<LuaValue>(()) {
            Ok(LuaValue::Nil) => return Ok(false),
            Ok(LuaValue::Table(t)) => t,
            Ok(other) => {
                return Err(format!(
                    "{who} must return a list of rows or nil, not {}",
                    other.type_name()
                ));
            }
            Err(e) => return Err(format!("{who}: {e}")),
        };
        let batch = frame_from_rows(&self.columns, Some(&rows), &who).map_err(|e| e.to_string())?;
        if batch.height() == 0 {
            return Ok(false);
        }
        let all = crate::source::append(&self.df, batch).map_err(|e| format!("{who}: {e}"))?;
        let (kept, dropped) = crate::source::cap(all, self.max_rows);
        self.df = kept;
        self.dropped += dropped;
        Ok(true)
    }

    fn rows(&self) -> polars::prelude::PolarsResult<DataFrame> {
        Ok(self.df.clone())
    }

    fn take_note(&mut self) -> Option<String> {
        let n = std::mem::take(&mut self.dropped);
        (n > 0).then(|| {
            format!(
                "{}: dropped the oldest {n} rows (source_max_rows {})",
                self.who, self.max_rows
            )
        })
    }
}

impl Plugins {
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
                LuaValue::Table(t) => match t.get::<Option<LuaFunction>>("poll")? {
                    Some(poll) => live_output(&t, poll),
                    None => Ok(LoaderOutput::Rows(sheet_from_lua(&t, "loader open")?.1)),
                },
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
            LoaderOutput::Live { columns, df, poll } => {
                let max_rows = p.options().value("source_max_rows").parse().unwrap_or(0);
                let source = LuaSource {
                    _lua: p.lua.clone(),
                    who: rule.name.clone().unwrap_or_else(|| "loader".into()),
                    columns,
                    poll,
                    df: df.clone(),
                    max_rows,
                    dropped: 0,
                };
                let mut l = LoadedSource::from_df(SourceKind::Csv, shown.clone(), None, df);
                l.live = Some(crate::source::Live::new(source));
                l
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

/// `{ columns, rows, poll }`: `rows` is optional, the first poll may bring them.
fn live_output(t: &LuaTable, poll: LuaFunction) -> mlua::Result<LoaderOutput> {
    let who = "loader open";
    let cols: LuaTable = t.get("columns").map_err(|_| {
        mlua::Error::RuntimeError(format!("{who}: a source with poll needs columns"))
    })?;
    let columns: Vec<String> = cols.sequence_values().collect::<mlua::Result<_>>()?;
    if columns.is_empty() {
        return Err(mlua::Error::RuntimeError(format!(
            "{who}: columns is empty"
        )));
    }
    let df = match t.get::<Option<LuaTable>>("rows")? {
        Some(_) => sheet_from_lua(t, who)?.1,
        None => frame_from_rows(&columns, None, who)?,
    };
    Ok(LoaderOutput::Live { columns, df, poll })
}
