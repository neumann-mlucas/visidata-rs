//! `vd.on` handlers (S-29, S-40 A): events, path patterns, firing.

use super::*;

/// A `vd.on` handler (S-40 A).
#[derive(Clone)]
pub(super) struct Handler {
    pub(super) id: i64,
    pub(super) event: String,
    pub(super) group: Option<String>,
    /// `pattern` as given, and as regexes; empty: every path.
    pub(super) pattern: Vec<String>,
    pub(super) globs: Vec<regex::Regex>,
    pub(super) once: bool,
    pub(super) f: LuaFunction,
}

impl Handler {
    /// A glob without `/` matches the file name, with `/` the whole path.
    fn matches(&self, path: &str) -> bool {
        self.globs.is_empty() || glob_hit(&self.pattern, &self.globs, path)
    }
}

/// `path` matches one of `pattern` (compiled as `globs`): a glob without `/`
/// on the file name, with `/` on the whole path.
pub(super) fn glob_hit(pattern: &[String], globs: &[regex::Regex], path: &str) -> bool {
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
pub(super) fn glob_regex(glob: &str) -> Result<regex::Regex, regex::Error> {
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

/// Events `vd.on` takes.
pub const EVENTS: &[&str] = &[
    "open", "push", "save", "quit", "tick", "enter", "loaded", "reload",
];

/// Events with no path: `pattern` is refused on them.
pub(super) const PATHLESS: &[&str] = &["quit", "tick", "reload"];

impl Plugins {
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
}
