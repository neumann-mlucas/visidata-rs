//! Centralised colour table. One `Theme` instance drives the whole TUI so we have
//! a single place to change the palette (and, later, a `--theme` CLI flag + a Lua hook).
//!
//! Phase 3 is still "hardcoded colour scheme" per direction §4 — user override lands with
//! the plugin system. This file is the surface that hook will plug into.

use ratatui::style::{Color, Modifier, Style};

#[derive(Debug, Clone, Copy)]
pub struct Theme {
    // Chrome
    pub titlebar_left_fg: Color,
    pub titlebar_left_bg: Color,
    pub titlebar_mid_fg: Color,
    pub titlebar_mid_bg: Color,
    pub titlebar_right_fg: Color,
    pub titlebar_right_bg: Color,
    pub statusbar_fg: Color,
    pub statusbar_bg: Color,
    pub toast_fg: Color,

    // Table
    pub header_fg: Color,
    pub header_bg: Color,
    pub header_cursor_fg: Color,
    pub header_cursor_bg: Color,
    pub row_stripe_bg: Color,
    pub row_cursor_fg: Color,
    pub row_cursor_bg: Color,
    pub cell_cursor_fg: Color,
    pub cell_cursor_bg: Color,
    pub col_cursor_fg: Color,
    pub selected_fg: Color,
    pub selected_bg: Color,
    pub null_fg: Color,

    // Dtype-aware tints (foreground only; applied when cell has no row/col/sel bg override)
    pub num_fg: Color,
    pub date_fg: Color,
    pub bool_true_fg: Color,
    pub bool_false_fg: Color,
    pub dtype_badge_fg: Color,

    // Overlays
    pub overlay_fg: Color,
    pub overlay_bg: Color,
    pub overlay_border_fg: Color,
    pub which_key_fg: Color,
    pub plan_border_fg: Color,
    /// `vd.option("disp_float_fmt", "%.2f")` (S-32): decimals for float
    /// columns without their own precision; `None`: 15 significant digits.
    pub float_digits: Option<u8>,
}

impl Theme {
    pub const fn dark() -> Self {
        Self {
            titlebar_left_fg: Color::Rgb(22, 24, 34),
            titlebar_left_bg: Color::Rgb(122, 162, 247),
            titlebar_mid_fg: Color::Rgb(169, 177, 214),
            titlebar_mid_bg: Color::Rgb(36, 40, 59),
            titlebar_right_fg: Color::Rgb(192, 202, 245),
            titlebar_right_bg: Color::Rgb(65, 72, 104),
            statusbar_fg: Color::Rgb(169, 177, 214),
            statusbar_bg: Color::Rgb(36, 40, 59),
            toast_fg: Color::Rgb(247, 118, 142),
            header_fg: Color::Rgb(122, 162, 247),
            header_bg: Color::Rgb(30, 33, 48),
            header_cursor_fg: Color::Rgb(22, 24, 34),
            header_cursor_bg: Color::Rgb(224, 175, 104),
            row_stripe_bg: Color::Rgb(26, 28, 40),
            row_cursor_fg: Color::Rgb(230, 233, 245),
            row_cursor_bg: Color::Rgb(51, 59, 91),
            cell_cursor_fg: Color::Rgb(22, 24, 34),
            cell_cursor_bg: Color::Rgb(224, 175, 104),
            col_cursor_fg: Color::Rgb(224, 175, 104),
            selected_fg: Color::Rgb(230, 220, 250),
            selected_bg: Color::Rgb(72, 56, 102),
            null_fg: Color::Rgb(86, 95, 137),
            num_fg: Color::Rgb(125, 207, 255),
            date_fg: Color::Rgb(187, 154, 247),
            bool_true_fg: Color::Rgb(158, 206, 106),
            bool_false_fg: Color::Rgb(247, 118, 142),
            dtype_badge_fg: Color::Rgb(86, 95, 137),
            overlay_fg: Color::Rgb(192, 202, 245),
            overlay_bg: Color::Rgb(30, 33, 48),
            overlay_border_fg: Color::Rgb(65, 72, 104),
            which_key_fg: Color::Rgb(224, 175, 104),
            plan_border_fg: Color::Rgb(158, 206, 106),
            float_digits: None,
        }
    }

    /// For light terminal backgrounds (`vd.set_theme("light")`).
    pub const fn light() -> Self {
        Self {
            titlebar_left_fg: Color::Rgb(255, 255, 255),
            titlebar_left_bg: Color::Rgb(46, 91, 190),
            titlebar_mid_fg: Color::Rgb(52, 59, 88),
            titlebar_mid_bg: Color::Rgb(225, 228, 238),
            titlebar_right_fg: Color::Rgb(52, 59, 88),
            titlebar_right_bg: Color::Rgb(200, 206, 224),
            statusbar_fg: Color::Rgb(52, 59, 88),
            statusbar_bg: Color::Rgb(225, 228, 238),
            toast_fg: Color::Rgb(180, 30, 60),
            header_fg: Color::Rgb(46, 91, 190),
            header_bg: Color::Rgb(236, 238, 245),
            header_cursor_fg: Color::Rgb(30, 30, 30),
            header_cursor_bg: Color::Rgb(245, 200, 90),
            row_stripe_bg: Color::Rgb(244, 245, 249),
            row_cursor_fg: Color::Rgb(30, 34, 60),
            row_cursor_bg: Color::Rgb(205, 216, 245),
            cell_cursor_fg: Color::Rgb(30, 30, 30),
            cell_cursor_bg: Color::Rgb(245, 200, 90),
            col_cursor_fg: Color::Rgb(160, 90, 0),
            selected_fg: Color::Rgb(60, 30, 100),
            selected_bg: Color::Rgb(225, 210, 245),
            null_fg: Color::Rgb(150, 155, 175),
            num_fg: Color::Rgb(0, 100, 130),
            date_fg: Color::Rgb(130, 40, 130),
            bool_true_fg: Color::Rgb(0, 120, 0),
            bool_false_fg: Color::Rgb(170, 0, 0),
            dtype_badge_fg: Color::Rgb(150, 155, 175),
            overlay_fg: Color::Rgb(52, 59, 88),
            overlay_bg: Color::Rgb(245, 246, 250),
            overlay_border_fg: Color::Rgb(170, 175, 195),
            which_key_fg: Color::Rgb(46, 91, 190),
            plan_border_fg: Color::Rgb(40, 120, 60),
            float_digits: None,
        }
    }

    pub fn titlebar_left(&self) -> Style {
        Style::default()
            .fg(self.titlebar_left_fg)
            .bg(self.titlebar_left_bg)
            .add_modifier(Modifier::BOLD)
    }
    pub fn titlebar_mid(&self) -> Style {
        Style::default()
            .fg(self.titlebar_mid_fg)
            .bg(self.titlebar_mid_bg)
    }
    pub fn titlebar_right(&self) -> Style {
        Style::default()
            .fg(self.titlebar_right_fg)
            .bg(self.titlebar_right_bg)
            .add_modifier(Modifier::BOLD)
    }
    pub fn statusbar(&self) -> Style {
        Style::default().fg(self.statusbar_fg).bg(self.statusbar_bg)
    }
    pub fn toast(&self) -> Style {
        Style::default().fg(self.toast_fg)
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

/// A `:color` style (S-27): comma-separated `red`, `fg=#ff8800`, `bg=yellow`,
/// `bold`, `italic`, `underline`, `reverse`; a bare color is the foreground.
/// `no_color` (`NO_COLOR` set): colors become reverse video.
pub fn parse_style(s: &str, no_color: bool) -> Result<Style, String> {
    let color = parse_color;
    let mut st = Style::default();
    let mut colored = false;
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        st = match part.split_once('=') {
            Some(("fg", c)) => st.fg(color(c)?),
            Some(("bg", c)) => st.bg(color(c)?),
            Some((k, _)) => return Err(format!("unknown style key: {k} (fg=, bg=)")),
            None => match part {
                "bold" => st.add_modifier(Modifier::BOLD),
                "italic" => st.add_modifier(Modifier::ITALIC),
                "underline" => st.add_modifier(Modifier::UNDERLINED),
                "reverse" => st.add_modifier(Modifier::REVERSED),
                c => st.fg(color(c)?),
            },
        };
        colored |= part.contains('=') || color(part).is_ok();
    }
    if st == Style::default() {
        return Err("empty style".into());
    }
    Ok(match no_color {
        true if colored => Style::default().add_modifier(st.add_modifier | Modifier::REVERSED),
        true => Style::default().add_modifier(st.add_modifier),
        false => st,
    })
}

/// `red`, `grey`, … or `#rrggbb`.
pub fn parse_color(c: &str) -> Result<Color, String> {
    if let Some(hex) = c.strip_prefix('#')
        && hex.len() == 6
        && let Ok(v) = u32::from_str_radix(hex, 16)
    {
        return Ok(Color::Rgb((v >> 16) as u8, (v >> 8) as u8, v as u8));
    }
    Ok(match c {
        "black" => Color::Black,
        "red" => Color::Red,
        "green" => Color::Green,
        "yellow" => Color::Yellow,
        "blue" => Color::Blue,
        "magenta" => Color::Magenta,
        "cyan" => Color::Cyan,
        "gray" | "grey" => Color::Gray,
        "white" => Color::White,
        _ => return Err(format!("unknown color: {c} (red, green, … or #rrggbb)")),
    })
}

/// RGB of a color `parse_color` makes (named ones: the xterm defaults).
pub fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (205, 0, 0),
        Color::Green => (0, 205, 0),
        Color::Yellow => (205, 205, 0),
        Color::Blue => (0, 0, 238),
        Color::Magenta => (205, 0, 205),
        Color::Cyan => (0, 205, 205),
        Color::Gray => (229, 229, 229),
        _ => (255, 255, 255),
    }
}

/// Colors the terminal can show (S-26 A4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorDepth {
    #[default]
    True,
    /// xterm's 256-color palette.
    Ansi256,
    /// `NO_COLOR`: none; the cursor shows in reverse video, the selection bold.
    None,
}

impl ColorDepth {
    /// `NO_COLOR` (non-empty) wins; `COLORTERM=truecolor|24bit` = 24-bit.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        if var("NO_COLOR").is_some_and(|v| !v.is_empty()) {
            return Self::None;
        }
        match var("COLORTERM").as_deref() {
            Some("truecolor" | "24bit") => Self::True,
            _ => Self::Ansi256,
        }
    }
}

/// The frame as `depth` can show it: RGB → nearest 256-color index, or no
/// colors at all with the cursor and selection kept as reverse / bold.
pub fn downgrade(buf: &mut ratatui::buffer::Buffer, depth: ColorDepth, th: &Theme) {
    let to256 = |c: Color| match c {
        Color::Rgb(r, g, b) => Color::Indexed(ansi256(r, g, b)),
        c => c,
    };
    for cell in buf.content.iter_mut() {
        match depth {
            ColorDepth::True => return,
            ColorDepth::Ansi256 => {
                cell.fg = to256(cell.fg);
                cell.bg = to256(cell.bg);
            }
            ColorDepth::None => {
                let bg = cell.bg;
                if bg == th.row_cursor_bg || bg == th.cell_cursor_bg {
                    cell.modifier.insert(Modifier::REVERSED);
                } else if bg == th.selected_bg {
                    cell.modifier.insert(Modifier::BOLD);
                }
                (cell.fg, cell.bg) = (Color::Reset, Color::Reset);
            }
        }
    }
}

/// Nearest xterm-256 index: the 6×6×6 cube or the 24-step gray ramp.
pub fn ansi256(r: u8, g: u8, b: u8) -> u8 {
    let level = |v: u8| match v {
        0..48 => 0,
        48..115 => 1,
        v => (v - 35) / 40,
    };
    let steps = [0u8, 95, 135, 175, 215, 255];
    let (cr, cg, cb) = (level(r), level(g), level(b));
    let dist = |x: (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(x.0, r) + d(x.1, g) + d(x.2, b)
    };
    let avg = ((r as u16 + g as u16 + b as u16) / 3) as u8;
    let gi = avg.saturating_sub(8) / 10;
    let gi = gi.min(23);
    let gv = 8 + 10 * gi;
    let cube = (steps[cr as usize], steps[cg as usize], steps[cb as usize]);
    match dist((gv, gv, gv)) < dist(cube) {
        true => 232 + gi,
        false => 16 + 36 * cr + 6 * cg + cb,
    }
}

#[cfg(test)]
mod style_tests {
    use super::*;

    #[test]
    fn color_depth_from_env_and_256_mapping() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| pairs.iter().find(|p| p.0 == k).map(|p| p.1.to_string())
        };
        let depth = |pairs| ColorDepth::from_env(env(pairs));
        assert_eq!(depth(&[("COLORTERM", "truecolor")]), ColorDepth::True);
        assert_eq!(depth(&[("COLORTERM", "24bit")]), ColorDepth::True);
        assert_eq!(depth(&[]), ColorDepth::Ansi256);
        assert_eq!(
            depth(&[("COLORTERM", "truecolor"), ("NO_COLOR", "1")]),
            ColorDepth::None
        );
        assert_eq!(depth(&[("NO_COLOR", "")]), ColorDepth::Ansi256);
        assert_eq!(ansi256(0, 0, 0), 16);
        assert_eq!(ansi256(255, 255, 255), 231);
        assert_eq!(ansi256(255, 0, 0), 196);
        assert_eq!(ansi256(128, 128, 128), 244);
        assert_eq!(ansi256(95, 135, 175), 67);
    }

    #[test]
    fn downgrade_maps_rgb_or_strips_colors() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        let th = Theme::dark();
        let mut buf = Buffer::empty(Rect::new(0, 0, 2, 1));
        buf[(0, 0)]
            .set_bg(th.row_cursor_bg)
            .set_fg(Color::Rgb(255, 0, 0));
        buf[(1, 0)].set_bg(th.selected_bg);
        let mut b256 = buf.clone();
        downgrade(&mut b256, ColorDepth::Ansi256, &th);
        assert_eq!(b256[(0, 0)].fg, Color::Indexed(196));
        let rgb = |c: Color| matches!(c, Color::Rgb(..));
        assert!(b256.content.iter().all(|c| !rgb(c.fg) && !rgb(c.bg)));
        downgrade(&mut buf, ColorDepth::None, &th);
        assert_eq!(
            (buf[(0, 0)].fg, buf[(0, 0)].bg),
            (Color::Reset, Color::Reset)
        );
        assert!(buf[(0, 0)].modifier.contains(Modifier::REVERSED));
        assert!(buf[(1, 0)].modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn styles_parse_and_bad_names_are_refused() {
        let s = parse_style("bg=yellow,bold", false).unwrap();
        assert_eq!(
            s,
            Style::default()
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        );
        assert_eq!(
            parse_style("red", false).unwrap(),
            Style::default().fg(Color::Red)
        );
        assert_eq!(
            parse_style("fg=#ff8800", false).unwrap(),
            Style::default().fg(Color::Rgb(255, 136, 0))
        );
        assert_eq!(
            parse_style("rouge", false).unwrap_err(),
            "unknown color: rouge (red, green, … or #rrggbb)"
        );
        assert_eq!(
            parse_style("size=3", false).unwrap_err(),
            "unknown style key: size (fg=, bg=)"
        );
        assert!(parse_style("", false).is_err());
        // NO_COLOR: colors read as reverse video
        assert_eq!(
            parse_style("red,bold", true).unwrap(),
            Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED)
        );
    }
}
