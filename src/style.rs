//! vdrs colour and style types (S-37 P0b). The theme, `:color` rules and Lua
//! renderers build these; paint code converts them to ratatui's with `into()`.
//! The builder names copy ratatui's, so a style reads the same in both.

use std::ops::{BitOr, BitOrAssign};

use ratatui::style as rt;

/// A colour: the terminal's default, one of the eight named ANSI colours, or RGB.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Color {
    #[default]
    Reset,
    Black,
    Red,
    Green,
    Yellow,
    Blue,
    Magenta,
    Cyan,
    Gray,
    White,
    Rgb(u8, u8, u8),
}

/// Text attributes, combined with `|`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Modifier(u8);

impl Modifier {
    pub const BOLD: Self = Self(1);
    pub const DIM: Self = Self(1 << 1);
    pub const ITALIC: Self = Self(1 << 2);
    pub const UNDERLINED: Self = Self(1 << 3);
    pub const REVERSED: Self = Self(1 << 4);

    pub const fn empty() -> Self {
        Self(0)
    }
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }
}

impl BitOr for Modifier {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Modifier {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

/// Colours and attributes for a span of cells. `None` keeps what is under it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Style {
    pub fg: Option<Color>,
    pub bg: Option<Color>,
    pub add_modifier: Modifier,
}

impl Style {
    pub const fn new() -> Self {
        Self {
            fg: None,
            bg: None,
            add_modifier: Modifier::empty(),
        }
    }
    pub const fn fg(mut self, c: Color) -> Self {
        self.fg = Some(c);
        self
    }
    pub const fn bg(mut self, c: Color) -> Self {
        self.bg = Some(c);
        self
    }
    pub const fn add_modifier(mut self, m: Modifier) -> Self {
        self.add_modifier = Modifier(self.add_modifier.0 | m.0);
        self
    }
    /// `other` over `self`: its colours win where set, the attributes add up.
    pub const fn patch(self, other: Style) -> Self {
        Self {
            fg: match other.fg {
                Some(c) => Some(c),
                None => self.fg,
            },
            bg: match other.bg {
                Some(c) => Some(c),
                None => self.bg,
            },
            add_modifier: Modifier(self.add_modifier.0 | other.add_modifier.0),
        }
    }
}

impl From<Color> for rt::Color {
    fn from(c: Color) -> Self {
        match c {
            Color::Reset => rt::Color::Reset,
            Color::Black => rt::Color::Black,
            Color::Red => rt::Color::Red,
            Color::Green => rt::Color::Green,
            Color::Yellow => rt::Color::Yellow,
            Color::Blue => rt::Color::Blue,
            Color::Magenta => rt::Color::Magenta,
            Color::Cyan => rt::Color::Cyan,
            Color::Gray => rt::Color::Gray,
            Color::White => rt::Color::White,
            Color::Rgb(r, g, b) => rt::Color::Rgb(r, g, b),
        }
    }
}

/// The way back, for a ratatui span vdrs reads (the built-in bars Lua sees);
/// `None` for a colour vdrs never makes (indexed, light variants).
impl Color {
    pub fn from_ratatui(c: rt::Color) -> Option<Color> {
        Some(match c {
            rt::Color::Reset => Color::Reset,
            rt::Color::Black => Color::Black,
            rt::Color::Red => Color::Red,
            rt::Color::Green => Color::Green,
            rt::Color::Yellow => Color::Yellow,
            rt::Color::Blue => Color::Blue,
            rt::Color::Magenta => Color::Magenta,
            rt::Color::Cyan => Color::Cyan,
            rt::Color::Gray => Color::Gray,
            rt::Color::White => Color::White,
            rt::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
            _ => return None,
        })
    }
}

impl From<Modifier> for rt::Modifier {
    fn from(m: Modifier) -> Self {
        [
            (Modifier::BOLD, rt::Modifier::BOLD),
            (Modifier::DIM, rt::Modifier::DIM),
            (Modifier::ITALIC, rt::Modifier::ITALIC),
            (Modifier::UNDERLINED, rt::Modifier::UNDERLINED),
            (Modifier::REVERSED, rt::Modifier::REVERSED),
        ]
        .into_iter()
        .filter(|(ours, _)| m.contains(*ours))
        .fold(rt::Modifier::empty(), |acc, (_, theirs)| acc | theirs)
    }
}

impl From<Style> for rt::Style {
    fn from(s: Style) -> Self {
        let mut out = rt::Style::default().add_modifier(s.add_modifier.into());
        if let Some(c) = s.fg {
            out = out.fg(c.into());
        }
        if let Some(c) = s.bg {
            out = out.bg(c.into());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn styles_convert_to_ratatui_and_patch_as_it_does() {
        let a = Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
        let b = Style::default()
            .bg(Color::Rgb(1, 2, 3))
            .add_modifier(Modifier::UNDERLINED | Modifier::REVERSED);
        let want = rt::Style::from(a).patch(rt::Style::from(b));
        assert_eq!(rt::Style::from(a.patch(b)), want);
        assert_eq!(rt::Style::from(Style::default()), rt::Style::default());
        let all = Modifier::BOLD
            | Modifier::DIM
            | Modifier::ITALIC
            | Modifier::UNDERLINED
            | Modifier::REVERSED;
        assert_eq!(
            rt::Modifier::from(all),
            rt::Modifier::BOLD
                | rt::Modifier::DIM
                | rt::Modifier::ITALIC
                | rt::Modifier::UNDERLINED
                | rt::Modifier::REVERSED
        );
        assert!(all.contains(Modifier::DIM) && !Modifier::BOLD.contains(all));
    }
}
