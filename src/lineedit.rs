//! Single-line text editor behind every prompt (`/`, `:`, `z|`, `|`, `^`, `D`).
//! Readline subset: Left/Right, Home/End (`<C-a>`/`<C-e>`), Backspace/Delete,
//! `<C-u>` kill to start, `<C-w>` kill previous word. Vim profile: `Esc`
//! enters a normal mode (S-26 E).

use crossterm::event::KeyCode;

use crate::key::Key;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LineEdit {
    text: String,
    /// Byte offset into `text`, always on a char boundary.
    cursor: usize,
    /// Vim normal mode: keys move and change instead of typing.
    normal: bool,
    /// `d` / `c` waiting for `w`.
    pending: Option<char>,
    /// Text + cursor before each normal-mode change, for `u`.
    undo: Vec<(String, usize)>,
}

impl LineEdit {
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Text before / after the cursor, for rendering the cursor cell.
    pub fn split(&self) -> (&str, &str) {
        self.text.split_at(self.cursor)
    }

    /// Replace the whole line (history recall), cursor at the end.
    pub fn set(&mut self, text: &str) {
        self.text = text.to_string();
        self.cursor = self.text.len();
    }

    pub fn insert(&mut self, c: char) {
        self.text.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    pub fn backspace(&mut self) {
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
            self.text.remove(self.cursor);
        }
    }

    pub fn is_normal(&self) -> bool {
        self.normal
    }

    /// `Esc` in the vim profile: normal mode, cursor back onto the last char typed.
    pub fn enter_normal(&mut self) {
        self.normal = true;
        self.pending = None;
        if let Some(c) = self.text[..self.cursor].chars().next_back() {
            self.cursor -= c.len_utf8();
        }
    }

    fn save(&mut self) {
        self.undo.push((self.text.clone(), self.cursor));
    }

    /// A normal-mode key: `h l w b e 0 $ x dw cw D C i a A I u`; others do
    /// nothing. Words are runs of non-space (vim's WORDs).
    pub fn normal_key(&mut self, key: Key) {
        let KeyCode::Char(k) = key.code else { return };
        if key.ctrl {
            return;
        }
        let mut cs: Vec<char> = self.text.chars().collect();
        let i = self.text[..self.cursor].chars().count();
        let n = cs.len();
        let last = n.saturating_sub(1);
        let ws = |c: char| c.is_whitespace();
        let next_word = |cs: &[char]| {
            let mut j = i;
            while j < n && !ws(cs[j]) {
                j += 1;
            }
            while j < n && ws(cs[j]) {
                j += 1;
            }
            j
        };
        let word_end = |cs: &[char]| {
            let mut j = i + 1;
            while j < n && ws(cs[j]) {
                j += 1;
            }
            while j + 1 < n && !ws(cs[j + 1]) {
                j += 1;
            }
            j.min(last)
        };
        if let Some(op) = self.pending.take() {
            if k != 'w' || n == 0 {
                return;
            }
            self.save();
            let end = match op {
                'd' => next_word(&cs),
                _ => word_end(&cs) + 1,
            };
            cs.drain(i..end);
            self.normal = op == 'd';
            let at = match op {
                'd' => i.min(cs.len().saturating_sub(1)),
                _ => i,
            };
            self.put(&cs, at);
            return;
        }
        let (at, insert) = match k {
            'h' => (i.saturating_sub(1), false),
            'l' => ((i + 1).min(last), false),
            '0' => (0, false),
            '$' => (last, false),
            'w' => (next_word(&cs).min(last), false),
            'b' => {
                let mut j = i;
                while j > 0 && ws(cs[j - 1]) {
                    j -= 1;
                }
                while j > 0 && !ws(cs[j - 1]) {
                    j -= 1;
                }
                (j, false)
            }
            'e' => (word_end(&cs), false),
            'x' if i < n => {
                self.save();
                cs.remove(i);
                (i.min(n.saturating_sub(2)), false)
            }
            'D' | 'C' => {
                self.save();
                cs.truncate(i);
                match k {
                    'D' => (i.saturating_sub(1), false),
                    _ => (i, true),
                }
            }
            'i' | 'a' | 'A' | 'I' => {
                self.save();
                let at = match k {
                    'i' => i,
                    'a' => (i + 1).min(n),
                    'A' => n,
                    _ => 0,
                };
                (at, true)
            }
            'u' => {
                if let Some((t, c)) = self.undo.pop() {
                    (self.text, self.cursor) = (t, c);
                }
                return;
            }
            'd' | 'c' => {
                self.pending = Some(k);
                return;
            }
            _ => return,
        };
        self.normal = !insert;
        self.put(&cs, at);
    }

    /// `cs` as the text, cursor before char `at`.
    fn put(&mut self, cs: &[char], at: usize) {
        self.text = cs.iter().collect();
        self.cursor = cs[..at.min(cs.len())].iter().map(|c| c.len_utf8()).sum();
    }

    /// Apply an editing key. Returns false when the key isn't an editing key,
    /// so the caller can treat it as text or a binding.
    pub fn edit(&mut self, key: Key) -> bool {
        let before = &self.text[..self.cursor];
        match (key.code, key.ctrl) {
            (KeyCode::Left, false) => {
                if let Some(c) = before.chars().next_back() {
                    self.cursor -= c.len_utf8();
                }
            }
            (KeyCode::Right, false) => {
                if let Some(c) = self.text[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
            }
            (KeyCode::Home, false) | (KeyCode::Char('a'), true) => self.cursor = 0,
            (KeyCode::End, false) | (KeyCode::Char('e'), true) => self.cursor = self.text.len(),
            (KeyCode::Delete, false) => {
                if self.cursor < self.text.len() {
                    self.text.remove(self.cursor);
                }
            }
            (KeyCode::Char('u'), true) => {
                self.text.drain(..self.cursor);
                self.cursor = 0;
            }
            (KeyCode::Char('w'), true) => {
                // previous word = trailing spaces + the non-space run before them
                let trimmed = before.trim_end_matches(' ');
                let start = trimmed.rfind(' ').map_or(0, |i| i + 1);
                self.text.drain(start..self.cursor);
                self.cursor = start;
            }
            _ => return false,
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn typed(s: &str) -> LineEdit {
        let mut l = LineEdit::default();
        s.chars().for_each(|c| l.insert(c));
        l
    }

    fn key(s: &str) -> Key {
        s.parse().unwrap()
    }

    #[test]
    fn cursor_moves_and_inserts_mid_line() {
        let mut l = typed("héllo");
        l.edit(key("<Left>"));
        l.edit(key("<Left>"));
        l.insert('X');
        assert_eq!(l.text(), "hélXlo");
        l.edit(key("<Home>"));
        l.insert('>');
        l.edit(key("<C-e>"));
        l.insert('!');
        assert_eq!(l.text(), ">hélXlo!");
        l.edit(key("<Home>"));
        for _ in 0..3 {
            l.edit(key("<Right>")); // past '>', 'h', then the 2-byte 'é'
        }
        l.backspace();
        assert_eq!(l.text(), ">hlXlo!");
    }

    #[test]
    fn kill_word_and_line() {
        let mut l = typed("sort -id  ");
        l.edit(key("<C-w>"));
        assert_eq!(l.text(), "sort ");
        l.edit(key("<C-u>"));
        assert_eq!(l.text(), "");
        let mut l = typed("abc def");
        l.edit(key("<Left>"));
        l.edit(key("<C-u>"));
        assert_eq!((l.text(), l.split().0), ("f", ""));
    }

    #[test]
    fn normal_mode_motions_and_changes() {
        let mut l = typed("sort -id name");
        let keys =
            |l: &mut LineEdit, ks: &str| ks.chars().for_each(|c| l.normal_key(key(&c.to_string())));
        l.enter_normal();
        assert!(l.is_normal());
        assert_eq!(l.split().1, "e", "Esc steps back onto the last char");
        keys(&mut l, "0w");
        assert_eq!(l.split().1, "-id name");
        keys(&mut l, "dw");
        assert_eq!((l.text(), l.split().1), ("sort name", "name"));
        keys(&mut l, "u");
        assert_eq!(l.text(), "sort -id name");
        keys(&mut l, "e");
        assert_eq!(l.split().1, "d name");
        keys(&mut l, "bcw");
        assert!(!l.is_normal(), "cw types");
        l.insert('+');
        assert_eq!(l.text(), "sort + name");
        l.enter_normal();
        keys(&mut l, "$xhD");
        assert_eq!(l.text(), "sort + n");
        keys(&mut l, "0x");
        assert_eq!(l.text(), "ort + n");
        keys(&mut l, "A");
        l.insert('!');
        keys(&mut l, "");
        assert_eq!(l.text(), "ort + n!");
        l.enter_normal();
        keys(&mut l, "IC");
        assert_eq!(l.text(), "");
        let mut e = LineEdit::default();
        e.enter_normal();
        keys(&mut e, "hlwbexDdwu$0");
        assert_eq!(e.text(), "", "an empty line takes every key");
    }

    #[test]
    fn non_editing_keys_pass_through() {
        let mut l = typed("x");
        assert!(!l.edit(key("q")));
        assert!(!l.edit(key("<CR>")));
    }
}
