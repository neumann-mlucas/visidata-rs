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
            titlebar_left_fg: Color::Black,
            titlebar_left_bg: Color::Cyan,
            titlebar_mid_fg: Color::White,
            titlebar_mid_bg: Color::Blue,
            titlebar_right_fg: Color::Black,
            titlebar_right_bg: Color::Green,
            statusbar_fg: Color::White,
            statusbar_bg: Color::DarkGray,
            toast_fg: Color::Red,

            header_fg: Color::Cyan,
            header_cursor_fg: Color::Black,
            header_cursor_bg: Color::Yellow,
            row_stripe_bg: Color::Rgb(32, 32, 32),
            row_cursor_fg: Color::White,
            row_cursor_bg: Color::Blue,
            cell_cursor_fg: Color::Black,
            cell_cursor_bg: Color::Yellow,
            col_cursor_fg: Color::Yellow,
            selected_fg: Color::Black,
            selected_bg: Color::Magenta,
            null_fg: Color::DarkGray,

            num_fg: Color::LightCyan,
            date_fg: Color::LightMagenta,
            bool_true_fg: Color::LightGreen,
            bool_false_fg: Color::LightRed,
            dtype_badge_fg: Color::DarkGray,

            overlay_fg: Color::White,
            overlay_bg: Color::Black,
            overlay_border_fg: Color::DarkGray,
            which_key_fg: Color::Yellow,
            plan_border_fg: Color::Green,
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
