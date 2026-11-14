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
    #[expect(
        clippy::unwrap_used,
        reason = "literal key names; every test builds the defaults"
    )]
    #[rustfmt::skip]
    pub fn defaults() -> Self {
        use Action::*;
        // `on` is space-separated key names: "g g".
        let ch = |on: &str, run: Action, desc: &'static str| Chord {
            on: on.split(' ').map(|k| k.parse().unwrap()).collect(),
            run: vec![run],
            desc: Cow::Borrowed(desc),
        };
        let sheet = vec![
            // nav
            ch("j", CursorDown { count: None }, "down"),
            ch("k", CursorUp { count: None }, "up"),
            ch("h", CursorLeft { count: None }, "left"),
            ch("l", CursorRight { count: None }, "right"),
            ch("<Down>", CursorDown { count: None }, "down"),
            ch("<Up>", CursorUp { count: None }, "up"),
            ch("<Left>", CursorLeft { count: None }, "left"),
            ch("<Right>", CursorRight { count: None }, "right"),
            ch("<C-f>", PageDown, "page down"),
            ch("<C-b>", PageUp, "page up"),
            ch("<C-d>", PageDown, "page down"),
            ch("<C-u>", PageUp, "page up"),
            ch("<PgDn>", PageDown, "page down"),
            ch("<PgUp>", PageUp, "page up"),
            ch("g g", GotoTop, "top"),
            ch("G", GotoBottom, "bottom"),
            ch("g h", GotoLeftmost, "leftmost column"),
            ch("0", GotoLeftmost, "leftmost column"),
            ch("g l", GotoRightmost, "rightmost column"),
            // freeze
            ch("z s", FreezeColumn, "freeze columns up to the cursor"),
            ch("z c", UnfreezeAll, "unfreeze all"),
            // column meta
            ch("-", HideCol, "hide current column"),
            ch("g v", UnhideAll, "unhide all columns"),
            ch("g -", HideUniform, "hide columns with one distinct value"),
            ch("!", KeyCol, "toggle current column as key"),
            ch("z !", UnkeyAll, "unset all keys"),
            ch("^", StartRenameColInput, "rename current column"),
            // type coercion
            ch("#", TypeCastInt, "type: int"),
            ch("%", TypeCastFloat, "type: float"),
            ch("~", TypeCastString, "type: string"),
            ch("@", TypeCastDate, "type: date"),
            ch("$", TypeCastCurrency, "type: currency"),
            // search / filter
            ch("/", StartSearchInput { all_columns: false, backward: false }, "search current column (regex)"),
            ch("?", StartSearchInput { all_columns: false, backward: true }, "search current column backward"),
            ch("g /", StartSearchInput { all_columns: true, backward: false }, "search all columns"),
            ch("g ?", StartSearchInput { all_columns: true, backward: true }, "search all columns backward"),
            ch("z /", StartSearchExprInput { backward: false }, "search by SQL predicate"),
            ch("z ?", StartSearchExprInput { backward: true }, "search by SQL predicate backward"),
            ch("n", SearchNext, "next match"),
            ch("N", SearchPrev, "prev match"),
            ch("g z |", StartFilterInput, "filter current column by regex"),
            ch("z f", ClearFilter, "clear filter"),
            // selection (VisiData)
            ch("s", ToggleSelectRow, "toggle select row"),
            ch("t", ToggleSelectRow, "toggle select row (alias)"),
            ch("u", UnselectRow, "unselect row"),
            ch("g s", SelectAll, "select all"),
            ch("g u", UnselectAll, "unselect all"),
            ch("|", StartSelectInput, "select by regex on current column"),
            ch("z |", StartSelectExprInput { unselect: false }, "select rows by SQL predicate"),
            ch("z \\", StartSelectExprInput { unselect: true }, "unselect rows by SQL predicate"),
            ch("\"", DupSelected, "new sheet from selected rows"),
            // sort
            ch("[", SortAsc, "sort ascending by current column"),
            ch("]", SortDesc, "sort descending by current column"),
            ch("z o", ClearSort, "clear sort"),
            // derived sheets
            ch("F", OpenFreqTable, "frequency table of current column"),
            ch("S", OpenSheetSwitcher, "list open sheets"),
            ch("g <CR>", SubmitSelected, "open rows of all selected values (freq)"),
            ch("g \"", DupSheet, "copy of this sheet"),
            ch("g '", DupSheet, "frozen copy of this sheet"),
            // column builders (S-31)
            ch("z a", StartAddColInput, "add an empty column"),
            ch("i", AddColIncr, "add a column numbering rows"),
            ch("g i", SetColIncr, "number the selected rows"),
            ch("f", SetColFill, "fill empty cells from above"),
            ch(";", StartCaptureInput, "add a column of regex captures"),
            ch("<A-+>", PrecisionMore, "one more decimal in this float column"),
            ch("<A-->", PrecisionLess, "one fewer decimal in this float column"),
            ch("I", Describe, "describe columns"),
            ch(",", SelectEqualCell, "select rows equal to this cell"),
            ch("W", PivotCurrent, "pivot: key cols × current col"),
            ch("D", StartDiffInput, "diff with file (key cols)"),
            // copy (S-18 A)
            ch("y", CopyRow, "copy current row"),
            ch("g y", CopySelected, "copy selected rows"),
            ch("z y", CopyCell, "copy current cell"),
            ch("g z y", CopyCells, "copy current column of selected rows"),
            // add / paste rows (S-18 F)
            ch("a", AddRow, "add a blank row after current row"),
            ch("g a", StartAddRowsInput, "add N blank rows after current row"),
            ch("p", PasteAfter, "paste copied rows after current row"),
            ch("P", PasteBefore, "paste copied rows before current row"),
            ch("A", OpenNew, "new empty sheet"),
            // delete / cut rows (S-18 E)
            ch("d", DeleteRow, "delete current row"),
            ch("g d", DeleteSelected, "delete selected rows"),
            ch("x", CutRow, "cut current row"),
            ch("g x", CutSelected, "cut selected rows"),
            // paste / cut cells (S-18 D)
            ch("z p", PasteCell, "paste into current cell"),
            ch("g z p", PasteCells, "paste into current column of selected rows"),
            ch("z d", DeleteCell, "empty current cell"),
            ch("g z d", DeleteCells, "empty current column of selected rows"),
            ch("z x", CutCell, "cut current cell"),
            ch("g z x", CutCells, "cut current column of selected rows"),
            // system clipboard (S-18 B)
            ch("Y", SysCopyRow, "copy current row to clipboard"),
            ch("g Y", SysCopySelected, "copy selected rows to clipboard"),
            ch("z Y", SysCopyCell, "copy current cell to clipboard"),
            ch("g z Y", SysCopyCells, "copy current column of selected rows to clipboard"),
            ch("z P", SysPasteCells, "paste clipboard into cells from the cursor"),
            ch("g z P", SysPasteCellsSelected, "paste clipboard into selected rows from the cursor column"),
            // columns sheet (S-25 P3)
            ch("C", OpenColumns, "open the columns sheet"),
            // aggregators (S-25 P4)
            ch("+", StartAggregateInput { memo: false, selected: false }, "add aggregators to this column"),
            ch("z +", StartAggregateInput { memo: true, selected: false }, "aggregate this column in the status line"),
            ch("g +", StartAggregateInput { memo: false, selected: true }, "columns sheet: add aggregators to selected columns"),
            ch("g C", OpenColumnsAll, "open the columns of all sheets"),
            ch("g !", KeySelected { on: true }, "columns sheet: key selected columns"),
            ch("g z !", KeySelected { on: false }, "columns sheet: unkey selected columns"),
            ch("g #", TypeSelected { name: "int" }, "columns sheet: selected columns int"),
            ch("g %", TypeSelected { name: "float" }, "columns sheet: selected columns float"),
            ch("g ~", TypeSelected { name: "str" }, "columns sheet: selected columns text"),
            ch("g @", TypeSelected { name: "date" }, "columns sheet: selected columns date"),
            ch("g $", TypeSelected { name: "currency" }, "columns sheet: selected columns currency"),
            // nested data (S-22)
            ch("(", ExpandCol { depth: 1 }, "expand struct column into a column per field"),
            ch("z (", StartExpandDepthInput { all: false }, "expand struct column N levels deep"),
            ch("g (", ExpandCols { depth: 1 }, "expand every visible struct column"),
            ch("g z (", StartExpandDepthInput { all: true }, "expand every visible struct column N levels deep"),
            ch(")", ContractCol, "fold expanded columns back into their struct"),
            ch("z M", UnfurlCol, "open a row per item of the current column's lists"),
            // editing (S-18 C)
            ch("e", StartEditInput, "edit current cell"),
            ch("g e", StartSetColInput, "set current column of selected rows"),
            ch("g =", StartSetExprInput, "set current column of selected rows to SQL expression"),
            ch("U", Undo, "undo"),
            ch("R", Redo, "redo"),
            ch("<CR>", SubmitRow, "open row: sheet list → sheet, freq → matching rows"),
            // sidebar (S-23 A)
            ch("b", ToggleSidebar, "toggle sidebar"),
            ch("<C-g>", CycleSidebar, "sidebar: next page (column, cell, help, plan)"),
            // ex-line
            ch(":", StartExInput, "ex command"),
            ch("<Space>", StartExInput, "ex command (any command by name)"),
            ch("=", StartDeriveInput, "new column from SQL expression"),
            // repeat / sheet stack / quit
            ch(".", PlotColumn, "plot this column against the key column"),
            ch("g .", PlotNumerics, "plot the numeric columns against the key column"),
            ch("m", ToggleMacroRecord, "record macro / stop"),
            ch("*", StartSubstInput { mode: crate::action::SubstMode::NewColumn }, "new column: regex replace of this one"),
            ch("g *", StartSubstInput { mode: crate::action::SubstMode::Selected }, "regex replace in selected rows"),
            ch("g z *", StartSubstInput { mode: crate::action::SubstMode::SelectedAll }, "regex replace in selected rows, all columns"),
            ch("q", PopSheet, "pop sheet / quit if last"),
            ch("g q", Quit, "quit all sheets"),
            ch("<C-q>", Quit, "quit"),
            ch("z n", StartNoteInput, "note on this cell"),
            ch("Z", SplitPane { vertical: false }, "split: the sheet under this one below"),
            ch("g Z", ClosePane, "close the split"),
            ch("H", SlideCol { by: -1 }, "slide the column left (plot: thinner cursor box)"),
            ch("L", SlideCol { by: 1 }, "slide the column right (plot: wider cursor box)"),
            ch("g H", SlideCol { by: i64::MIN }, "slide the column to the left end of its section"),
            ch("g L", SlideCol { by: i64::MAX }, "slide the column to the right end of its section"),
            ch("z H", StartSlideInput { left: true }, "slide the column left N places"),
            ch("z L", StartSlideInput { left: false }, "slide the column right N places"),
            ch("J", PlotResize { wider: 0, taller: 1 }, "plot: taller cursor box"),
            ch("K", PlotResize { wider: 0, taller: -1 }, "plot: shorter cursor box"),
            ch("_", ZoomAll, "plot: zoom to fit"),
            ch("z z", ScrollTo { at: 1 }, "scroll the cursor row to the middle; plot: zoom to the cursor box"),
            ch("<Tab>", FocusPane, "focus the other pane"),
            ch("g <Tab>", SwapPanes, "swap the panes"),
            ch("<C-r>", Refresh, "refresh a derived sheet from its parent"),
            ch("<C-c>", CancelJobs, "cancel this sheet's background jobs"),
            ch("g <C-c>", CancelAllJobs, "cancel all background jobs"),
            ch("<C-l>", Redraw, "redraw"),
            ch("z <C-h>", HelpCommands, "list the key bindings"),
            ch("z <F1>", HelpCommands, "list the key bindings"),
            ch("z <BS>", HelpCommands, "list the key bindings"),
            ch("<C-^>", JumpPrev, "go to the previous sheet"),
            // terminals send Ctrl+^ as Ctrl+6
            ch("<C-6>", JumpPrev, "go to the previous sheet"),
            ch("<C-z>", Suspend, "suspend to the shell"),
            ch("V", Visual { block: false }, "visual: a row range"),
            ch("<C-v>", Visual { block: true }, "visual: a cell block"),
        ];
        let input = vec![
            ch("<CR>", SubmitInput, "submit"),
            ch("<Esc>", CancelInput, "cancel"),
            ch("<BS>", InputBackspace, "backspace"),
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
