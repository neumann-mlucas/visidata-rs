//! `vd.statusline` / `vd.titlebar` (S-40 D): Lua functions that draw the
//! status line and the title bar from segments.

use super::*;
use crate::action::SidebarPage;
use ratatui::text::{Line, Span};

/// Which bar a Lua function draws.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bar {
    Status,
    Title,
}

impl Bar {
    pub(super) const ALL: [Bar; 2] = [Bar::Status, Bar::Title];

    pub(super) fn name(self) -> &'static str {
        match self {
            Bar::Status => "statusline",
            Bar::Title => "titlebar",
        }
    }
}

/// What a bar function's `ctx` holds, besides `ctx.sheet` (`vd.sheet()`).
#[derive(Debug)]
pub struct BarCtx<'a> {
    pub sheet: SheetCtx<'a>,
    /// `normal`, `input`, `confirm`, `visual` or `plot`.
    pub mode: &'static str,
    /// None until counted.
    pub rows: Option<u64>,
    pub selected: usize,
    pub jobs: Vec<&'static str>,
    /// 1: the focused pane, 2: the other pane of a split.
    pub pane: u8,
    pub message: Option<&'a str>,
    pub keys: String,
}

/// One bar's function, its last result and whether it broke.
#[derive(Default, Clone)]
pub(super) struct BarSlot {
    f: Option<LuaFunction>,
    /// Debug text of the last input, and the `(left, right)` it gave.
    cache: Option<(String, Segments)>,
    broken: bool,
}

type Segments = (Vec<Span<'static>>, Vec<Span<'static>>);

/// A call longer than this puts the built-in bar back.
// ponytail: wall clock, so a loaded test machine could trip 5 ms; tests get 1 s.
const SLOW: std::time::Duration = if cfg!(test) {
    std::time::Duration::from_secs(1)
} else {
    std::time::Duration::from_millis(5)
};

pub(super) fn install(lua: &Lua, vd: &LuaTable) -> mlua::Result<()> {
    for bar in Bar::ALL {
        vd.set(
            bar.name(),
            lua.create_function(move |lua, f: Option<LuaFunction>| {
                registry(lua)?.borrow_mut().bars[bar as usize] = BarSlot {
                    f,
                    ..Default::default()
                };
                Ok(())
            })?,
        )?;
    }
    // `vd.bar.statusline(ctx)`: the built-in bar's segments, as `ctx` carries them.
    let builtin = lua.create_table()?;
    for bar in Bar::ALL {
        builtin.set(
            bar.name(),
            lua.create_function(move |lua, ctx: LuaTable| {
                match ctx.get::<Option<LuaTable>>(format!("_{}", bar.name()))? {
                    Some(t) => Ok(t),
                    None => {
                        let t = lua.create_table()?;
                        t.set("left", lua.create_table()?)?;
                        t.set("right", lua.create_table()?)?;
                        Ok(t)
                    }
                }
            })?,
        )?;
    }
    vd.set("bar", builtin)?;
    vd.set(
        "register_page",
        lua.create_function(|lua, spec: LuaTable| {
            let name: String = spec.get("name")?;
            let render: LuaFunction = spec.get("render")?;
            if SidebarPage::parse(&name) != Some(SidebarPage::Lua(name.clone())) {
                return Err(mlua::Error::RuntimeError(format!(
                    "vd.register_page: {name:?} is a built-in page or empty"
                )));
            }
            let r = registry(lua)?;
            let mut reg = r.borrow_mut();
            reg.pages.retain(|(n, _)| *n != name); // redefining replaces
            reg.pages.push((name, render));
            reg.page_cache = None;
            Ok(())
        })?,
    )?;
    Ok(())
}

impl Plugins {
    pub fn has_bar(&self, bar: Bar) -> bool {
        reg(&self.lua).is_some_and(|r| {
            let slot = &r.borrow().bars[bar as usize];
            slot.f.is_some() && !slot.broken
        })
    }

    /// `(left, right)` from the `vd.statusline` / `vd.titlebar` function; None
    /// draws the built-in bar (`builtin`: its own left and right). Called again
    /// only when the input changed. An error or a slow call breaks the
    /// function for the session; `take_bar_error` says why, once.
    pub fn bar(
        &self,
        bar: Bar,
        ctx: &BarCtx,
        builtin: (&[Span<'_>], &[Span<'_>]),
    ) -> Option<Segments> {
        let r = reg(&self.lua)?;
        let key = format!("{ctx:?}{builtin:?}");
        let f = {
            let reg = r.borrow();
            let slot = &reg.bars[bar as usize];
            if slot.broken {
                return None;
            }
            if let Some((k, out)) = &slot.cache
                && *k == key
            {
                return Some(out.clone());
            }
            slot.f.clone()?
        };
        let start = std::time::Instant::now();
        let out = self
            .bar_ctx(bar, ctx, builtin)
            .and_then(|t| f.call::<LuaTable>(t))
            .and_then(|t| Ok((segments(&t.get("left")?)?, segments(&t.get("right")?)?)));
        let took = start.elapsed();
        let mut reg = r.borrow_mut();
        let err = match out {
            Ok(_) if took > SLOW => format!(
                "{} took {} ms (over {}); built-in bar back",
                bar.name(),
                took.as_millis(),
                SLOW.as_millis()
            ),
            Ok(out) => {
                reg.bars[bar as usize].cache = Some((key, out.clone()));
                return Some(out);
            }
            Err(e) => format!(
                "{}: {}; built-in bar back",
                bar.name(),
                e.to_string().lines().next().unwrap_or_default()
            ),
        };
        reg.bars[bar as usize].broken = true;
        reg.bar_error.get_or_insert(err);
        None
    }

    /// `vd.register_page` names, in registration order.
    pub fn page_names(&self) -> Vec<String> {
        reg(&self.lua).map_or_else(Vec::new, |r| {
            r.borrow().pages.iter().map(|(n, _)| n.clone()).collect()
        })
    }

    /// Lines of the Lua sidebar page `name`: its `render(ctx)` with
    /// `ctx.cell` = `cell`, cached until the input changes. Err: its error.
    pub fn page(&self, name: &str, ctx: &BarCtx, cell: &str) -> Result<Vec<Line<'static>>, String> {
        let r = reg(&self.lua).ok_or("no Lua")?;
        let key = format!("{name}{ctx:?}{cell}");
        let f = {
            let reg = r.borrow();
            if let Some((k, lines)) = &reg.page_cache
                && *k == key
            {
                return Ok(lines.clone());
            }
            let page = reg.pages.iter().find(|(n, _)| n == name);
            page.ok_or(format!("no sidebar page {name}"))?.1.clone()
        };
        let lines = self
            .bar_ctx(Bar::Status, ctx, (&[], &[]))
            .and_then(|t| {
                t.set("cell", cell)?;
                f.call::<LuaValue>(t)
            })
            .and_then(|v| page_lines(&v))
            .map_err(|e| {
                format!(
                    "{name}: {}",
                    e.to_string().lines().next().unwrap_or_default()
                )
            })?;
        r.borrow_mut().page_cache = Some((key, lines.clone()));
        Ok(lines)
    }

    /// Why a bar function was dropped, once.
    pub fn take_bar_error(&self) -> Option<String> {
        reg(&self.lua)?.borrow_mut().bar_error.take()
    }

    fn bar_ctx(
        &self,
        bar: Bar,
        ctx: &BarCtx,
        builtin: (&[Span<'_>], &[Span<'_>]),
    ) -> mlua::Result<LuaTable> {
        let lua = &self.lua;
        let t = lua.create_table()?;
        t.set("sheet", self.ctx_table(&ctx.sheet)?)?;
        t.set("mode", ctx.mode)?;
        t.set("row", ctx.sheet.row)?;
        t.set("rows", ctx.rows)?;
        t.set(
            "column",
            ctx.sheet
                .column_names
                .get(ctx.sheet.col.wrapping_sub(1))
                .cloned(),
        )?;
        t.set("selected", ctx.selected)?;
        t.set("jobs", ctx.jobs.clone())?;
        t.set("pane", ctx.pane)?;
        t.set("message", ctx.message)?;
        t.set("keys", ctx.keys.as_str())?;
        let side = |spans: &[Span<'_>]| -> mlua::Result<LuaTable> {
            lua.create_sequence_from(
                spans
                    .iter()
                    .map(|s| span_table(lua, s))
                    .collect::<mlua::Result<Vec<_>>>()?,
            )
        };
        let b = lua.create_table()?;
        b.set("left", side(builtin.0)?)?;
        b.set("right", side(builtin.1)?)?;
        t.set(format!("_{}", bar.name()), b)?;
        Ok(t)
    }
}

/// A span as a segment table: `{ text, fg, bg, bold, … }`.
fn span_table(lua: &Lua, s: &Span<'_>) -> mlua::Result<LuaTable> {
    use ratatui::style::Modifier;
    let t = lua.create_table()?;
    t.set("text", s.content.as_ref())?;
    let hex = |c: ratatui::style::Color| {
        let (r, g, b) = crate::theme::rgb(c);
        format!("#{r:02x}{g:02x}{b:02x}")
    };
    t.set("fg", s.style.fg.map(hex))?;
    t.set("bg", s.style.bg.map(hex))?;
    for (k, m) in [
        ("bold", Modifier::BOLD),
        ("italic", Modifier::ITALIC),
        ("underline", Modifier::UNDERLINED),
        ("reverse", Modifier::REVERSED),
    ] {
        if s.style.add_modifier.contains(m) {
            t.set(k, true)?;
        }
    }
    Ok(t)
}

/// A side's segments: strings (or numbers) and `{ text, fg, … }` tables.
fn segments(v: &LuaValue) -> mlua::Result<Vec<Span<'static>>> {
    match v {
        LuaValue::Nil => Ok(Vec::new()),
        LuaValue::Table(t) => t
            .sequence_values::<LuaValue>()
            .map(|s| segment(s?))
            .collect(),
        _ => Err(mlua::Error::RuntimeError("left/right must be lists".into())),
    }
}

/// A string, a number or a `{ text, fg, … }` table as a span.
fn segment(v: LuaValue) -> mlua::Result<Span<'static>> {
    match v {
        LuaValue::String(s) => Ok(Span::raw(s.to_str()?.to_string())),
        LuaValue::Integer(n) => Ok(Span::raw(n.to_string())),
        LuaValue::Number(n) => Ok(Span::raw(n.to_string())),
        LuaValue::Table(t) => {
            let r = styled(&t);
            Ok(Span::styled(r.text.unwrap_or_default(), r.style))
        }
        other => Err(mlua::Error::RuntimeError(format!(
            "a segment is a string or a table, not {}",
            other.type_name()
        ))),
    }
}

/// A page's lines: each a segment, or a list of segments.
fn page_lines(v: &LuaValue) -> mlua::Result<Vec<Line<'static>>> {
    let LuaValue::Table(t) = v else {
        return Err(mlua::Error::RuntimeError(
            "render must return a list of lines".into(),
        ));
    };
    t.sequence_values::<LuaValue>()
        .map(|line| match line? {
            LuaValue::Table(t) if !t.contains_key("text")? => {
                Ok(Line::from(segments(&LuaValue::Table(t))?))
            }
            v => Ok(Line::from(segment(v)?)),
        })
        .collect()
}

/// `left`, a gap, then `right` at the far end of `width` columns.
pub fn compose(left: Vec<Span<'static>>, right: Vec<Span<'static>>, width: u16) -> Line<'static> {
    let used: usize = left.iter().chain(&right).map(Span::width).sum();
    let mut spans = left;
    spans.push(Span::raw(" ".repeat((width as usize).saturating_sub(used))));
    spans.extend(right);
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::style::{Color, Modifier};

    fn ctx(name: &str) -> BarCtx<'_> {
        BarCtx {
            sheet: SheetCtx {
                name,
                row: 2,
                col: 1,
                column_names: vec!["a".into()],
                value: AnyValue::Int64(7),
            },
            mode: "normal",
            rows: Some(9),
            selected: 3,
            jobs: vec![],
            pane: 1,
            message: None,
            keys: String::new(),
        }
    }

    #[test]
    fn segments_cache_and_fallback() {
        let p = Plugins::load_from_source(
            r##"
            calls = 0
            vd.statusline(function(ctx)
              calls = calls + 1
              return { left = { { text = ctx.mode, bold = true, fg = "#ff0000" }, " ", ctx.sheet.name },
                       right = { ctx.selected, "/", ctx.rows, vd.bar.statusline(ctx).left[1].text } }
            end)
            vd.titlebar(function(ctx) error("boom") end)
            "##,
        )
        .unwrap();
        let builtin = [Span::styled("hi", Style::default().fg(Color::Blue))];
        let (l, r) = p.bar(Bar::Status, &ctx("s"), (&builtin, &[])).unwrap();
        assert_eq!(l[0].content, "normal");
        assert_eq!(l[0].style.fg, Some(Color::Rgb(255, 0, 0)));
        assert!(l[0].style.add_modifier.contains(Modifier::BOLD));
        let text = |v: &[Span]| v.iter().map(|s| s.content.to_string()).collect::<String>();
        assert_eq!((text(&l), text(&r)), ("normal s".into(), "3/9hi".into()));
        // same input: cached; other input: called again
        p.bar(Bar::Status, &ctx("s"), (&builtin, &[])).unwrap();
        assert_eq!(p.lua.globals().get::<i64>("calls").unwrap(), 1);
        p.bar(Bar::Status, &ctx("t"), (&builtin, &[])).unwrap();
        assert_eq!(p.lua.globals().get::<i64>("calls").unwrap(), 2);
        // an error: built-in bar for the session, the reason once
        assert!(p.has_bar(Bar::Title));
        assert!(p.bar(Bar::Title, &ctx("s"), (&[], &[])).is_none());
        assert!(!p.has_bar(Bar::Title));
        let e = p.take_bar_error().unwrap();
        assert!(e.starts_with("titlebar:") && e.contains("boom"), "{e}");
        assert_eq!(p.take_bar_error(), None);
        assert!(p.has_bar(Bar::Status));
    }

    #[test]
    fn compose_puts_right_at_the_end() {
        let line = compose(vec![Span::raw("ab")], vec![Span::raw("cd")], 8);
        assert_eq!(line.to_string(), "ab    cd");
    }
}
