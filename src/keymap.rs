//! Keymap. Linear-vec per layer + first-key match → fire or pending. See S-04.
//!
//! Phase 0 ships a baked-in default keymap only; TOML config loading lands in Phase 3
//! when Lua is also in play (user explicitly deferred custom color scheme to the plugin
//! system, same rationale for user keymap).

use std::borrow::Cow;

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
    pub desc: Cow<'static, str>,
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
    /// A layer's chords, in declaration order (later ones shadow earlier).
    pub fn chords(&self, layer: Layer) -> &[Chord] {
        match layer {
            Layer::Sheet => &self.sheet,
            Layer::Input => &self.input,
        }
    }

    /// No bindings at all.
    pub fn empty() -> Self {
        Keymap {
            sheet: Vec::new(),
            input: Vec::new(),
        }
    }

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

        // Last declaration wins, also between an exact chord and longer ones it
        // prefixes: `vd.bind("sheet", "<Space> f", …)` makes `<Space>` a leader
        // even though the defaults bind `<Space>` alone.
        let is_prefix =
            |c: &Chord| c.on.len() > full_seq.len() && c.on[..full_seq.len()] == full_seq[..];
        let exact = chords.iter().rposition(|c| c.on == full_seq);
        let last_prefix = chords.iter().rposition(is_prefix);
        if let Some(i) = exact
            && last_prefix.is_none_or(|p| p < i)
        {
            return Dispatch::Fire(chords[i].run.clone());
        }
        let prefix: Vec<Chord> = chords.iter().filter(|c| is_prefix(c)).cloned().collect();
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
        let ch = |on: Vec<Key>, run: Vec<Action>, desc: &'static str| Chord {
            on,
            run,
            desc: Cow::Borrowed(desc),
        };
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
            ch(vec![k("<C-f>")], vec![PageDown], "page down"),
            ch(vec![k("<C-b>")], vec![PageUp], "page up"),
            ch(vec![k("<C-d>")], vec![PageDown], "page down"),
            ch(vec![k("<C-u>")], vec![PageUp], "page up"),
            ch(vec![k("<PgDn>")], vec![PageDown], "page down"),
            ch(vec![k("<PgUp>")], vec![PageUp], "page up"),
            ch(vec![k("g"), k("g")], vec![GotoTop], "top"),
            ch(vec![k("G")], vec![GotoBottom], "bottom"),
            ch(vec![k("g"), k("h")], vec![GotoLeftmost], "leftmost column"),
            ch(vec![k("0")], vec![GotoLeftmost], "leftmost column"),
            ch(
                vec![k("g"), k("l")],
                vec![GotoRightmost],
                "rightmost column",
            ),
            // freeze
            ch(
                vec![k("z"), k("s")],
                vec![FreezeColumn],
                "freeze current column",
            ),
            ch(vec![k("z"), k("c")], vec![UnfreezeAll], "unfreeze all"),
            // column meta
            ch(vec![k("-")], vec![HideCol], "hide current column"),
            ch(vec![k("g"), k("v")], vec![UnhideAll], "unhide all columns"),
            ch(
                vec![k("g"), k("-")],
                vec![HideUniform],
                "hide columns with one distinct value",
            ),
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
            ch(vec![k("$")], vec![TypeCastCurrency], "type: currency"),
            // search / filter
            ch(
                vec![k("/")],
                vec![StartSearchInput {
                    all_columns: false,
                    backward: false,
                }],
                "search current column (regex)",
            ),
            ch(
                vec![k("?")],
                vec![StartSearchInput {
                    all_columns: false,
                    backward: true,
                }],
                "search current column backward",
            ),
            ch(
                vec![k("g"), k("/")],
                vec![StartSearchInput {
                    all_columns: true,
                    backward: false,
                }],
                "search all columns",
            ),
            ch(
                vec![k("g"), k("?")],
                vec![StartSearchInput {
                    all_columns: true,
                    backward: true,
                }],
                "search all columns backward",
            ),
            ch(
                vec![k("z"), k("/")],
                vec![StartSearchExprInput { backward: false }],
                "search by SQL predicate",
            ),
            ch(
                vec![k("z"), k("?")],
                vec![StartSearchExprInput { backward: true }],
                "search by SQL predicate backward",
            ),
            ch(vec![k("n")], vec![SearchNext], "next match"),
            ch(vec![k("N")], vec![SearchPrev], "prev match"),
            ch(
                vec![k("g"), k("z"), k("|")],
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
                vec![k("z"), k("|")],
                vec![StartSelectExprInput { unselect: false }],
                "select rows by SQL predicate",
            ),
            ch(
                vec![k("z"), k("\\")],
                vec![StartSelectExprInput { unselect: true }],
                "unselect rows by SQL predicate",
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
                vec![k("g"), k("<CR>")],
                vec![SubmitSelected],
                "open rows of all selected values (freq)",
            ),
            ch(vec![k("g"), k("\"")], vec![DupSheet], "copy of this sheet"),
            ch(vec![k("I")], vec![Describe], "describe columns"),
            ch(
                vec![k(",")],
                vec![SelectEqualCell],
                "select rows equal to this cell",
            ),
            ch(
                vec![k("W")],
                vec![PivotCurrent],
                "pivot: key cols × current col",
            ),
            ch(
                vec![k("D")],
                vec![StartDiffInput],
                "diff with file (key cols)",
            ),
            // copy (S-18 A)
            ch(vec![k("y")], vec![CopyRow], "copy current row"),
            ch(
                vec![k("g"), k("y")],
                vec![CopySelected],
                "copy selected rows",
            ),
            ch(vec![k("z"), k("y")], vec![CopyCell], "copy current cell"),
            ch(
                vec![k("g"), k("z"), k("y")],
                vec![CopyCells],
                "copy current column of selected rows",
            ),
            // delete / cut rows (S-18 E)
            ch(vec![k("d")], vec![DeleteRow], "delete current row"),
            ch(
                vec![k("g"), k("d")],
                vec![DeleteSelected],
                "delete selected rows",
            ),
            ch(vec![k("x")], vec![CutRow], "cut current row"),
            ch(vec![k("g"), k("x")], vec![CutSelected], "cut selected rows"),
            // paste / cut cells (S-18 D)
            ch(
                vec![k("z"), k("p")],
                vec![PasteCell],
                "paste into current cell",
            ),
            ch(
                vec![k("g"), k("z"), k("p")],
                vec![PasteCells],
                "paste into current column of selected rows",
            ),
            ch(vec![k("z"), k("d")], vec![DeleteCell], "empty current cell"),
            ch(
                vec![k("g"), k("z"), k("d")],
                vec![DeleteCells],
                "empty current column of selected rows",
            ),
            ch(vec![k("z"), k("x")], vec![CutCell], "cut current cell"),
            ch(
                vec![k("g"), k("z"), k("x")],
                vec![CutCells],
                "cut current column of selected rows",
            ),
            // system clipboard (S-18 B)
            ch(
                vec![k("Y")],
                vec![SysCopyRow],
                "copy current row to clipboard",
            ),
            ch(
                vec![k("g"), k("Y")],
                vec![SysCopySelected],
                "copy selected rows to clipboard",
            ),
            ch(
                vec![k("z"), k("Y")],
                vec![SysCopyCell],
                "copy current cell to clipboard",
            ),
            ch(
                vec![k("g"), k("z"), k("Y")],
                vec![SysCopyCells],
                "copy current column of selected rows to clipboard",
            ),
            // editing (S-18 C)
            ch(vec![k("e")], vec![StartEditInput], "edit current cell"),
            ch(
                vec![k("g"), k("e")],
                vec![StartSetColInput],
                "set current column of selected rows",
            ),
            ch(
                vec![k("g"), k("=")],
                vec![StartSetExprInput],
                "set current column of selected rows to SQL expression",
            ),
            ch(vec![k("U")], vec![Undo], "undo"),
            ch(vec![k("R")], vec![Redo], "redo"),
            ch(
                vec![k("<CR>")],
                vec![SubmitRow],
                "open row: sheet list → sheet, freq → matching rows",
            ),
            // sidebar (S-23 A)
            ch(vec![k("b")], vec![ToggleSidebar], "toggle sidebar"),
            ch(
                vec![k("<C-g>")],
                vec![CycleSidebar],
                "sidebar: next page (column, cell, help, plan)",
            ),
            ch(
                vec![k("P")],
                vec![TogglePlanPanel],
                "toggle plan side-panel",
            ),
            // ex-line
            ch(vec![k(":")], vec![StartExInput], "ex command"),
            ch(
                vec![k("<Space>")],
                vec![StartExInput],
                "ex command (any command by name)",
            ),
            ch(
                vec![k("=")],
                vec![StartDeriveInput],
                "new column from SQL expression",
            ),
            // repeat / sheet stack / quit
            ch(vec![k(".")], vec![RepeatLast], "repeat last action"),
            ch(vec![k("q")], vec![PopSheet], "pop sheet / quit if last"),
            ch(vec![k("g"), k("q")], vec![Quit], "quit all sheets"),
            ch(vec![k("<C-q>")], vec![Quit], "quit"),
            ch(
                vec![k("<C-c>")],
                vec![CancelJobs],
                "cancel this sheet's background jobs",
            ),
            ch(
                vec![k("g"), k("<C-c>")],
                vec![CancelAllJobs],
                "cancel all background jobs",
            ),
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

    /// docs/index.html lists every default sheet binding (`data-keys="…"`).
    #[test]
    fn user_guide_documents_every_default_key() {
        let html = include_str!("../docs/index.html");
        let esc = |s: String| {
            s.replace('&', "&amp;")
                .replace('"', "&quot;")
                .replace('<', "&lt;")
                .replace('>', "&gt;")
        };
        for chord in &Keymap::defaults().sheet {
            let keys: Vec<String> = chord.on.iter().map(|k| k.to_string()).collect();
            let attr = format!("data-keys=\"{}\"", esc(keys.join(" ")));
            assert!(
                html.contains(&attr),
                "docs/index.html misses {attr} ({})",
                chord.desc
            );
        }
    }

    #[test]
    fn unknown_misses() {
        let km = Keymap::defaults();
        assert!(matches!(
            km.resolve(Layer::Sheet, &[], key("X")),
            Dispatch::Miss
        ));
    }
}
