//! Key and mouse input types + key parser. Yazi-shaped: `<C-a>`, `<S-F1>`, `<Esc>`, `<CR>`,
//! `<Space>`, `<Left>`, `<A-x>`. The crossterm conversions here are the only place
//! crossterm's input types reach vdrs (S-37 P0a).

use std::fmt;
use std::str::FromStr;

use crossterm::event as ct;

/// A key vdrs reads. Other terminal keys (media, modifier-only, caps lock)
/// never reach the app: [`Key::from_event`] drops them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    F(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// A bare key, no modifiers.
impl From<KeyCode> for Key {
    fn from(code: KeyCode) -> Self {
        Key {
            code,
            ctrl: false,
            shift: false,
            alt: false,
        }
    }
}

impl Key {
    /// The key a terminal event presses; `None` for a release and for a key
    /// vdrs has no [`KeyCode`] for.
    pub fn from_event(e: ct::KeyEvent) -> Option<Key> {
        if e.kind == ct::KeyEventKind::Release {
            return None;
        }
        let code = match e.code {
            ct::KeyCode::Char(c) => KeyCode::Char(c),
            ct::KeyCode::Enter => KeyCode::Enter,
            ct::KeyCode::Esc => KeyCode::Esc,
            ct::KeyCode::Tab => KeyCode::Tab,
            ct::KeyCode::BackTab => KeyCode::BackTab,
            ct::KeyCode::Backspace => KeyCode::Backspace,
            ct::KeyCode::Left => KeyCode::Left,
            ct::KeyCode::Right => KeyCode::Right,
            ct::KeyCode::Up => KeyCode::Up,
            ct::KeyCode::Down => KeyCode::Down,
            ct::KeyCode::Home => KeyCode::Home,
            ct::KeyCode::End => KeyCode::End,
            ct::KeyCode::Insert => KeyCode::Insert,
            ct::KeyCode::Delete => KeyCode::Delete,
            ct::KeyCode::PageUp => KeyCode::PageUp,
            ct::KeyCode::PageDown => KeyCode::PageDown,
            ct::KeyCode::F(n) => KeyCode::F(n),
            _ => return None,
        };
        // A symbol already encodes its shift ("!" not "<S-1>"), but some terminals
        // (Windows, kitty protocol) still flag SHIFT; drop it so "!" binds match.
        let shift = e.modifiers.contains(ct::KeyModifiers::SHIFT)
            && !matches!(code, KeyCode::Char(c) if !c.is_alphabetic());
        let ctrl = e.modifiers.contains(ct::KeyModifiers::CONTROL);
        let alt = e.modifiers.contains(ct::KeyModifiers::ALT);
        // Normalise shifted uppercase chars: "A" and "<S-a>" collapse to the same Key.
        let code = match code {
            KeyCode::Char(c) if shift => KeyCode::Char(c.to_ascii_uppercase()),
            other => other,
        };
        Some(Key {
            code,
            ctrl,
            shift,
            alt,
        })
    }
}

/// A mouse event vdrs reads (S-26 A9): the left button pressed, or the wheel.
/// `column` and `row` are screen cells from the top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mouse {
    pub kind: MouseKind,
    pub column: u16,
    pub row: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseKind {
    Click,
    ScrollUp,
    ScrollDown,
}

impl Mouse {
    /// `None` for every other button, a release, a drag and a move.
    pub fn from_event(e: ct::MouseEvent) -> Option<Mouse> {
        let kind = match e.kind {
            ct::MouseEventKind::Down(ct::MouseButton::Left) => MouseKind::Click,
            ct::MouseEventKind::ScrollUp => MouseKind::ScrollUp,
            ct::MouseEventKind::ScrollDown => MouseKind::ScrollDown,
            _ => return None,
        };
        Some(Mouse {
            kind,
            column: e.column,
            row: e.row,
        })
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let bare = match self.code {
            KeyCode::Char(' ') => "<Space>".to_string(),
            KeyCode::Char(c) => c.to_string(),
            KeyCode::Enter => "<CR>".to_string(),
            KeyCode::Esc => "<Esc>".to_string(),
            KeyCode::Tab => "<Tab>".to_string(),
            KeyCode::Backspace => "<BS>".to_string(),
            KeyCode::Left => "<Left>".to_string(),
            KeyCode::Right => "<Right>".to_string(),
            KeyCode::Up => "<Up>".to_string(),
            KeyCode::Down => "<Down>".to_string(),
            KeyCode::Home => "<Home>".to_string(),
            KeyCode::End => "<End>".to_string(),
            KeyCode::Delete => "<Del>".to_string(),
            KeyCode::PageUp => "<PgUp>".to_string(),
            KeyCode::PageDown => "<PgDn>".to_string(),
            KeyCode::F(n) => format!("<F{n}>"),
            KeyCode::BackTab => "BackTab".to_string(),
            KeyCode::Insert => "Insert".to_string(),
        };
        if !(self.ctrl || self.alt) {
            return write!(f, "{bare}");
        }
        let mut mods = String::new();
        if self.ctrl {
            mods.push_str("C-");
        }
        if self.alt {
            mods.push_str("A-");
        }
        let inner = bare.trim_matches(|c| c == '<' || c == '>').to_string();
        write!(f, "<{mods}{inner}>")
    }
}

impl FromStr for Key {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err("empty key".into());
        }
        if !s.starts_with('<') || s == "<" {
            let mut cs = s.chars();
            let c = cs.next().ok_or("empty")?;
            if cs.next().is_some() {
                return Err(format!("multi-char without < >: {s:?}"));
            }
            let (code, shift) = if c.is_ascii_uppercase() {
                (KeyCode::Char(c), true)
            } else {
                (KeyCode::Char(c), false)
            };
            return Ok(Key {
                code,
                ctrl: false,
                shift,
                alt: false,
            });
        }
        let inner = s.trim_start_matches('<').trim_end_matches('>');
        let mut ctrl = false;
        let mut shift = false;
        let mut alt = false;
        let mut rest = inner;
        loop {
            if let Some(r) = rest.strip_prefix("C-") {
                ctrl = true;
                rest = r;
                continue;
            }
            if let Some(r) = rest.strip_prefix("S-") {
                shift = true;
                rest = r;
                continue;
            }
            if let Some(r) = rest.strip_prefix("A-") {
                alt = true;
                rest = r;
                continue;
            }
            break;
        }
        let code = match rest {
            "Esc" => KeyCode::Esc,
            "CR" | "Enter" => KeyCode::Enter,
            "Tab" => KeyCode::Tab,
            "BS" | "Backspace" => KeyCode::Backspace,
            "Space" => KeyCode::Char(' '),
            "Left" => KeyCode::Left,
            "Right" => KeyCode::Right,
            "Up" => KeyCode::Up,
            "Down" => KeyCode::Down,
            "Home" => KeyCode::Home,
            "End" => KeyCode::End,
            "Del" | "Delete" => KeyCode::Delete,
            "PgUp" => KeyCode::PageUp,
            "PgDn" => KeyCode::PageDown,
            other if other.starts_with('F') => {
                let n: u8 = other[1..]
                    .parse()
                    .map_err(|_| format!("bad fn key: {other}"))?;
                KeyCode::F(n)
            }
            other => {
                let mut cs = other.chars();
                let (Some(c), None) = (cs.next(), cs.next()) else {
                    return Err(format!("unknown key: {other}"));
                };
                if c.is_ascii_uppercase() {
                    shift = true;
                }
                KeyCode::Char(c)
            }
        };
        Ok(Key {
            code,
            ctrl,
            shift,
            alt,
        })
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn shifted_symbol_matches_its_binding() {
        let shifted = |c| ct::KeyEvent::new(ct::KeyCode::Char(c), ct::KeyModifiers::SHIFT);
        for c in ['!', '$', '^', '#', '|', '"', ':', '@', '%', '~'] {
            let pressed = Key::from_event(shifted(c)).unwrap();
            assert_eq!(pressed, c.to_string().parse::<Key>().unwrap(), "{c}");
        }
        let upper = Key::from_event(shifted('G')).unwrap();
        assert_eq!(upper, "G".parse::<Key>().unwrap());
        let lower = Key::from_event(shifted('g')).unwrap();
        assert_eq!(lower, "G".parse::<Key>().unwrap());
    }

    #[test]
    fn terminal_events_vdrs_has_no_name_for_are_dropped() {
        let ev = |code| ct::KeyEvent::new(code, ct::KeyModifiers::NONE);
        assert_eq!(Key::from_event(ev(ct::KeyCode::CapsLock)), None);
        assert_eq!(Key::from_event(ev(ct::KeyCode::Null)), None);
        let mut up = ev(ct::KeyCode::Char('j'));
        up.kind = ct::KeyEventKind::Release;
        assert_eq!(Key::from_event(up), None);
        let mouse = |kind| ct::MouseEvent {
            kind,
            column: 3,
            row: 4,
            modifiers: ct::KeyModifiers::NONE,
        };
        let click = mouse(ct::MouseEventKind::Down(ct::MouseButton::Left));
        let want = Mouse {
            kind: MouseKind::Click,
            column: 3,
            row: 4,
        };
        assert_eq!(Mouse::from_event(click), Some(want));
        let right = mouse(ct::MouseEventKind::Down(ct::MouseButton::Right));
        assert_eq!(Mouse::from_event(right), None);
        assert_eq!(Mouse::from_event(mouse(ct::MouseEventKind::Moved)), None);
    }
    use super::*;

    #[test]
    fn parse_bare_lower() {
        let k: Key = "j".parse().unwrap();
        assert_eq!(
            k,
            Key {
                code: KeyCode::Char('j'),
                ctrl: false,
                shift: false,
                alt: false
            }
        );
    }
    #[test]
    fn parse_ctrl() {
        let k: Key = "<C-s>".parse().unwrap();
        assert_eq!(
            k,
            Key {
                code: KeyCode::Char('s'),
                ctrl: true,
                shift: false,
                alt: false
            }
        );
    }
    #[test]
    fn parse_special() {
        assert_eq!("<Esc>".parse::<Key>().unwrap().code, KeyCode::Esc);
        assert_eq!("<Space>".parse::<Key>().unwrap().code, KeyCode::Char(' '));
        assert_eq!("<Left>".parse::<Key>().unwrap().code, KeyCode::Left);
    }
    #[test]
    fn uppercase_sets_shift() {
        let k: Key = "G".parse().unwrap();
        assert!(k.shift);
    }
}
