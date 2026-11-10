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
    let color = |c: &str| -> Result<Color, String> {
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
    };
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

#[cfg(test)]
mod style_tests {
    use super::*;

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
