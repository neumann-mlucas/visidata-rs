//! Single-line text editor behind every prompt (`/`, `:`, `z|`, `|`, `^`, `D`).
//! Readline subset: Left/Right, Home/End (`<C-a>`/`<C-e>`), Backspace/Delete,
//! `<C-u>` kill to start, `<C-w>` kill previous word.

use crossterm::event::KeyCode;

use crate::key::Key;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LineEdit {
    text: String,
    /// Byte offset into `text`, always on a char boundary.
    cursor: usize,
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
    fn non_editing_keys_pass_through() {
        let mut l = typed("x");
        assert!(!l.edit(key("q")));
        assert!(!l.edit(key("<CR>")));
    }
}
