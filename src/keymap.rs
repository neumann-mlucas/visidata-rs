//! Keymap. Linear-vec per layer + first-key match → fire or pending. See S-04.
//!
//! Phase 0 ships a baked-in default keymap only; TOML config loading lands in Phase 3
//! when Lua is also in play (user explicitly deferred custom color scheme to the plugin
//! system, same rationale for user keymap).

use crate::action::Action;
use crate::key::Key;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Layer {
    Sheet,
    Input, // search / filter text entry
}

#[derive(Debug, Clone)]
pub struct Chord {
    pub on: Vec<Key>,
    pub run: Vec<Action>,
    pub desc: &'static str,
}

#[derive(Debug)]
pub enum Dispatch {
    Fire(Vec<Action>),
    Pending(Vec<Chord>),
    Miss,
}

#[derive(Debug, Default)]
pub struct Keymap {
    sheet: Vec<Chord>,
    input: Vec<Chord>,
}

impl Keymap {
    /// Append a chord to a layer. Plugins call this via `vd.bind`; last-declaration-wins
    /// resolution means the appended chord shadows an identical default.
    pub fn append(&mut self, layer: Layer, chord: Chord) {
        match layer {
            Layer::Sheet => self.sheet.push(chord),
            Layer::Input => self.input.push(chord),
        }
    }

    /// Resolve a key sequence against a layer. `pending` is what the user typed
    /// so far (empty on a fresh chord); `next` is the latest key.
    pub fn resolve(&self, layer: Layer, pending: &[Key], next: Key) -> Dispatch {
        let chords: &[Chord] = match layer {
            Layer::Sheet => &self.sheet,
            Layer::Input => &self.input,
        };
        let mut full_seq = pending.to_vec();
        full_seq.push(next);

        // last-declaration-wins: iterate in reverse so later shadows earlier
        for c in chords.iter().rev() {
            if c.on == full_seq {
                return Dispatch::Fire(c.run.clone());
            }
        }
        let prefix: Vec<Chord> = chords
            .iter()
            .filter(|c| c.on.len() > full_seq.len() && c.on[..full_seq.len()] == full_seq[..])
            .cloned()
            .collect();
        if prefix.is_empty() {
            Dispatch::Miss
        } else {
            Dispatch::Pending(prefix)
        }
    }

    /// Baked-in default bindings — the VisiData muscle-memory subset relevant to Phase 0.
    pub fn defaults() -> Self {
        use Action::*;
        let k = |s: &str| -> Key { s.parse().unwrap() };
        let ch = |on: Vec<Key>, run: Vec<Action>, desc: &'static str| Chord { on, run, desc };
        let sheet = vec![
            // nav
            ch(vec![k("j")], vec![CursorDown { count: None }], "down"),
            ch(vec![k("k")], vec![CursorUp { count: None }], "up"),
            ch(vec![k("h")], vec![CursorLeft { count: None }], "left"),
            ch(vec![k("l")], vec![CursorRight { count: None }], "right"),
            ch(vec![k("<Down>")], vec![CursorDown { count: None }], "down"),
            ch(vec![k("<Up>")], vec![CursorUp { count: None }], "up"),
            ch(vec![k("<Left>")], vec![CursorLeft { count: None }], "left"),
            ch(
                vec![k("<Right>")],
                vec![CursorRight { count: None }],
                "right",
            ),
            ch(vec![k("<C-d>")], vec![PageDown], "page down"),
            ch(vec![k("<C-u>")], vec![PageUp], "page up"),
            ch(vec![k("<PgDn>")], vec![PageDown], "page down"),
            ch(vec![k("<PgUp>")], vec![PageUp], "page up"),
            ch(vec![k("g"), k("g")], vec![GotoTop], "top"),
            ch(vec![k("G")], vec![GotoBottom], "bottom"),
            ch(vec![k("0")], vec![GotoLeftmost], "leftmost column"),
            ch(vec![k("$")], vec![GotoRightmost], "rightmost column"),
            // freeze
            ch(
                vec![k("z"), k("s")],
                vec![FreezeColumn],
                "freeze current column",
            ),
            ch(vec![k("z"), k("c")], vec![UnfreezeAll], "unfreeze all"),
            // column meta
            ch(vec![k("-")], vec![HideCol], "hide current column"),
            ch(vec![k("g"), k("-")], vec![UnhideAll], "unhide all"),
            ch(vec![k("!")], vec![KeyCol], "toggle current column as key"),
            ch(vec![k("z"), k("!")], vec![UnkeyAll], "unset all keys"),
            ch(
                vec![k("^")],
                vec![StartRenameColInput],
                "rename current column",
            ),
            // type coercion
            ch(vec![k("#")], vec![TypeCastInt], "type: int"),
            ch(vec![k("%")], vec![TypeCastFloat], "type: float"),
            ch(vec![k("~")], vec![TypeCastString], "type: string"),
            ch(vec![k("@")], vec![TypeCastDate], "type: date"),
            // search / filter
            ch(vec![k("/")], vec![StartSearchInput], "search forward"),
            ch(vec![k("n")], vec![SearchNext], "next match"),
            ch(vec![k("N")], vec![SearchPrev], "prev match"),
            ch(
                vec![k("z"), k("|")],
                vec![StartFilterInput],
                "filter current column by regex",
            ),
            ch(vec![k("z"), k("f")], vec![ClearFilter], "clear filter"),
            // selection (VisiData)
            ch(vec![k("s")], vec![ToggleSelectRow], "toggle select row"),
            ch(
                vec![k("t")],
                vec![ToggleSelectRow],
                "toggle select row (alias)",
            ),
            ch(vec![k("u")], vec![UnselectRow], "unselect row"),
            ch(vec![k("g"), k("s")], vec![SelectAll], "select all"),
            ch(vec![k("g"), k("u")], vec![UnselectAll], "unselect all"),
            ch(
                vec![k("|")],
                vec![StartSelectInput],
                "select by regex on current column",
            ),
            ch(
                vec![k("\"")],
                vec![DupSelected],
                "new sheet from selected rows",
            ),
            // sort
            ch(
                vec![k("[")],
                vec![SortAsc],
                "sort ascending by current column",
            ),
            ch(
                vec![k("]")],
                vec![SortDesc],
                "sort descending by current column",
            ),
            ch(vec![k("z"), k("o")], vec![ClearSort], "clear sort"),
            // derived sheets
            ch(
                vec![k("F")],
                vec![OpenFreqTable],
                "frequency table of current column",
            ),
            ch(vec![k("S")], vec![OpenSheetSwitcher], "list open sheets"),
            ch(
                vec![k("<CR>")],
                vec![SubmitRow],
                "activate row (sheet-switcher only)",
            ),
            // plan panel
            ch(
                vec![k("P")],
                vec![TogglePlanPanel],
                "toggle plan side-panel",
            ),
            // ex-line
            ch(vec![k(":")], vec![StartExInput], "ex command"),
            // repeat / sheet stack / quit
            ch(vec![k(".")], vec![RepeatLast], "repeat last action"),
            ch(vec![k("q")], vec![PopSheet], "pop sheet / quit if last"),
            ch(vec![k("<C-c>")], vec![Quit], "quit"),
            ch(vec![k("<C-l>")], vec![Redraw], "redraw"),
        ];
        let input = vec![
            ch(vec![k("<CR>")], vec![SubmitInput], "submit"),
            ch(vec![k("<Esc>")], vec![CancelInput], "cancel"),
            ch(vec![k("<BS>")], vec![InputBackspace], "backspace"),
        ];
        Keymap { sheet, input }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(s: &str) -> Key {
        s.parse().unwrap()
    }

    #[test]
    fn fires_single_key() {
        let km = Keymap::defaults();
        let d = km.resolve(Layer::Sheet, &[], key("j"));
        assert!(matches!(d, Dispatch::Fire(_)));
    }

    #[test]
    fn chord_pends_then_fires() {
        let km = Keymap::defaults();
        let d = km.resolve(Layer::Sheet, &[], key("g"));
        assert!(matches!(d, Dispatch::Pending(_)));
        let d2 = km.resolve(Layer::Sheet, &[key("g")], key("g"));
        assert!(matches!(d2, Dispatch::Fire(v) if matches!(v[0], Action::GotoTop)));
    }

    #[test]
    fn unknown_misses() {
        let km = Keymap::defaults();
        assert!(matches!(
            km.resolve(Layer::Sheet, &[], key("x")),
            Dispatch::Miss
        ));
    }
}
