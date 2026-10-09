//! Key struct + parser. Yazi-shaped: `<C-a>`, `<S-F1>`, `<Esc>`, `<CR>`, `<Space>`, `<Left>`, `<A-x>`.

use std::fmt;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl From<KeyEvent> for Key {
    fn from(e: KeyEvent) -> Self {
        let shift = e.modifiers.contains(KeyModifiers::SHIFT);
        let ctrl = e.modifiers.contains(KeyModifiers::CONTROL);
        let alt = e.modifiers.contains(KeyModifiers::ALT);
        // Normalise shifted uppercase chars: "A" and "<S-a>" collapse to the same Key.
        let code = match e.code {
            KeyCode::Char(c) if shift && c.is_ascii_lowercase() => {
                KeyCode::Char(c.to_ascii_uppercase())
            }
            KeyCode::Char(c) if shift && c.is_ascii_uppercase() => KeyCode::Char(c),
            other => other,
        };
        Self {
            code,
            ctrl,
            shift,
            alt,
        }
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
            KeyCode::PageUp => "<PgUp>".to_string(),
            KeyCode::PageDown => "<PgDn>".to_string(),
            KeyCode::F(n) => format!("<F{n}>"),
            other => format!("{other:?}"),
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
        if !s.starts_with('<') {
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
            "PgUp" => KeyCode::PageUp,
            "PgDn" => KeyCode::PageDown,
            other if other.starts_with('F') => {
                let n: u8 = other[1..]
                    .parse()
                    .map_err(|_| format!("bad fn key: {other}"))?;
                KeyCode::F(n)
            }
            other if other.chars().count() == 1 => {
                let c = other.chars().next().unwrap();
                if c.is_ascii_uppercase() {
                    shift = true;
                }
                KeyCode::Char(c)
            }
            other => return Err(format!("unknown key: {other}")),
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
