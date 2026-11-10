//! Action enum. See S-02. Phase 0 slice.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    // cursor / scroll — cosmetic
    CursorDown {
        count: Option<usize>,
    },
    CursorUp {
        count: Option<usize>,
    },
    CursorLeft {
        count: Option<usize>,
    },
    CursorRight {
        count: Option<usize>,
    },
    PageDown,
    PageUp,
    GotoTop,
    GotoBottom,
    GotoLeftmost,
    GotoRightmost,
    FreezeColumn,
    UnfreezeAll,
    HideCol,
    UnhideAll,
    KeyCol, // toggle on current column
    UnkeyAll,
    StartRenameColInput,
    RenameCol {
        new_name: String,
    },
    TypeCastInt,
    TypeCastFloat,
    TypeCastString,
    TypeCastDate,
    TypeCastCurrency,
    /// `:type json` (S-22 C): decode JSON text into lists / structs.
    TypeCastJson,

    // search / filter / sort
    SearchNext,
    SearchPrev,
    SetFilter {
        column: String,
        regex: String,
    },
    ClearFilter,
    SortAsc,
    SortDesc,
    SetSort {
        column: String,
        descending: bool,
    },
    ClearSort,

    // derived sheets
    OpenFreqTable,
    OpenSheetSwitcher,
    Pivot {
        index: String,
        on: String,
        values: String,
        agg: String,
    },
    Unpivot {
        on: Vec<String>,
        index: Vec<String>,
    },
    Diff {
        path: String,
        keys: Vec<String>,
    },
    Tail {
        path: String,
        follow: bool,
    },
    SubmitRow,
    /// `g<CR>`: like `SubmitRow` for every selected row (freq dive).
    SubmitSelected,

    // sidebar (S-23 A)
    /// `:plan`, `P`: the sidebar's plan page → optimized plan → closed.
    TogglePlanPanel,
    /// `b`: open the sidebar on its last page, or close it.
    ToggleSidebar,
    /// `Ctrl+G`: next page (column → cell → help → plan), opening it.
    CycleSidebar,
    /// `:sidebar <page>`.
    ShowSidebar {
        page: SidebarPage,
    },

    // selection (VisiData style)
    ToggleSelectRow,
    SelectRow,
    UnselectRow,
    SelectAll,
    UnselectAll,
    SelectByRegex {
        regex: String,
    },
    StartSelectInput, // `|` → prompt for regex
    SelectEqualCell,  // `,` → rows whose current column equals the cursor cell
    DupSelected,      // `"` → new sheet with selected rows only
    /// `:select` / `:unselect` / `z|` / `z\`: rows where a SQL predicate holds (S-21).
    SelectExpr {
        expr: String,
        unselect: bool,
    },
    /// `z|` / `z\` → prompt for a predicate.
    StartSelectExprInput {
        unselect: bool,
    },
    /// Rows whose current-column value already occurred above, in view order.
    SelectColDuplicates,
    /// New sheet keeping each key's first row (all columns when no keys).
    DedupeRows,
    /// `y` `gy` `zy` `gzy` (S-18 A): copy the cursor row / selected rows /
    /// cursor cell / current column of selected rows to the register.
    CopyRow,
    CopySelected,
    CopyCell,
    CopyCells,
    /// `Y` `gY` `zY` `gzY` (S-18 B): the same, as text to the system clipboard.
    SysCopyRow,
    SysCopySelected,
    SysCopyCell,
    SysCopyCells,
    /// `zP` `gzP` (S-18 G): the system clipboard's text (or the last
    /// terminal paste) into cells from the cursor / into selected rows,
    /// columns from the cursor rightwards.
    SysPasteCells,
    SysPasteCellsSelected,
    /// `zp` `gzp` (S-18 D): the copied cell into the cursor cell / the copied
    /// cells into the current column of selected rows (repeated to fill).
    PasteCell,
    PasteCells,
    /// `zx` `gzx`: copy the cursor cell / selected rows' cells, then null them.
    CutCell,
    CutCells,
    /// `zd` `gzd`: null the cursor cell / the current column of selected rows.
    DeleteCell,
    DeleteCells,
    /// `d` `gd` `x` `gx` (S-18 E): delete the cursor row / selected rows;
    /// `x` `gx` copy them to the register first.
    DeleteRow,
    DeleteSelected,
    CutRow,
    CutSelected,
    /// `a` `ga` (S-18 F): blank rows after the cursor row; `ga` asks how many.
    AddRow,
    StartAddRowsInput,
    AddRows {
        count: usize,
    },
    /// `p` `P`: the copied rows (`y` `gy`) after / before the cursor row.
    PasteAfter,
    PasteBefore,
    /// `A`: a new empty sheet with one column, `A`.
    OpenNew,
    /// `:earlier n` / `:later n` (S-20): `n` undo states back (`n < 0`: on)
    /// in the order they were made, across branches.
    TimeWalk {
        n: i64,
    },
    /// `:undotree`: a sheet of the current sheet's undo states.
    OpenUndoTree,
    /// `(` `z(` (S-22): struct column → a column per field, `depth` levels.
    ExpandCol {
        depth: usize,
    },
    /// `z(` `gz(` (`all`): ask how many levels.
    StartExpandDepthInput {
        all: bool,
    },
    /// `g(` `gz(`: every visible struct column, `depth` levels.
    ExpandCols {
        depth: usize,
    },
    /// `)`: fold expanded columns back into their struct.
    ContractCol,
    /// `zM`: a sheet with a row per item of the current column.
    UnfurlCol,
    /// `C` / `gC` (S-25 P3): the columns sheet of this sheet / of all sheets.
    OpenColumns,
    OpenColumnsAll,
    /// Columns sheet `g!` / `gz!`: key / unkey the selected rows' columns.
    KeySelected {
        on: bool,
    },
    /// Columns sheet `g#` `g%` `g~` `g@` `g$`: type the selected rows' columns.
    TypeSelected {
        name: &'static str,
    },
    /// `+` `z+` (`memo`) / columns sheet `g+` (`selected`) (S-25 P4): ask
    /// for aggregator names.
    StartAggregateInput {
        memo: bool,
        selected: bool,
    },
    /// `+`: add aggregators (space-separated) to the current column.
    AggregateCol {
        names: String,
    },
    /// Columns sheet `g+`: to the selected rows' columns; none selected:
    /// every visible non-key column.
    AggregateCols {
        names: String,
    },
    /// `z+`: their values over the selected rows (all when none), in the status line.
    MemoAggregate {
        names: String,
    },

    // ex-line
    StartExInput,
    ExCommand(String), // raw command text, parsed at dispatch
    /// `vd.bind(layer, keys, function() … end)`: index of the Lua callback.
    Lua(usize),
    /// `:name args` for a `vd.command`: its callback and the argument text.
    LuaCommand {
        id: usize,
        args: String,
    },

    // persist current sheet to disk; extension decides writer
    Save {
        path: String,
    },
    /// View as a reproducible Polars (.py) or DuckDB SQL (.sql) script.
    Export {
        path: String,
    },

    // data shaping
    Join {
        source: String,
        keys: Vec<String>,
        how: String,
    },
    Derive {
        name: String,
        expr: String,
    },
    Sql {
        query: String,
    },
    /// `:exec-sql`: statements run in the active sheet's `.duckdb` file.
    ExecSql {
        sql: String,
    },
    /// `:setexpr`, `g=`: current column := SQL expression, selected rows.
    SetExpr {
        expr: String,
    },
    /// `:filter-expr`: keep rows where the predicate holds; `None` clears.
    SetFilterExpr {
        expr: Option<String>,
    },
    /// `:color <col|*> <pred> <style>` (S-27 A): paint cells (or rows, `None`)
    /// where the predicate holds.
    Color {
        column: Option<String>,
        pred: String,
        style: String,
    },
    /// `:nocolor [col|*]`: drop the rules on a column (`*`: row rules); none: all.
    NoColor {
        column: Option<String>,
    },
    SetCol {
        value: String,
    },
    /// `e` / `:edit <value>`: the cursor cell := value (empty = null) (S-18 C).
    EditCell {
        value: String,
    },
    /// `e`: prompt prefilled with the cursor cell.
    StartEditInput,
    /// `ge`: prompt for `:setcol` (current column, selected rows).
    StartSetColInput,
    StartSetExprInput,
    HideUniform,
    DupSheet,
    Describe,
    PivotCurrent,
    StartDiffInput,
    Undo,
    Redo,

    // input modes
    /// `/` `?` (cursor column) and `g/` `g?` (all columns), as VisiData.
    StartSearchInput {
        all_columns: bool,
        backward: bool,
    },
    /// `z/` / `z?`: search by SQL predicate.
    StartSearchExprInput {
        backward: bool,
    },
    /// `=`: new column from a SQL expression.
    StartDeriveInput,
    StartFilterInput,
    SubmitInput,
    CancelInput,
    InputBackspace,

    // sheet stack
    PopSheet,
    /// `:q!`: pop without the unsaved-edits question.
    ForcePopSheet,

    // meta
    Quit,
    /// `:qa!` / `:quit!`: quit without the unsaved-edits question.
    ForceQuit,
    /// `<C-c>`: drop the active sheet's background jobs (VisiData cancel-sheet).
    CancelJobs,
    /// `g<C-c>`: drop every background job (VisiData cancel-all).
    CancelAllJobs,
    RepeatLast,
    Redraw,
    /// `:open <path>` (S-29): a file as a new sheet, as on the command line.
    Open {
        path: String,
    },
    /// `:cursor <row> [column]`: row counts from 1 in the view.
    SetCursor {
        row: u64,
        column: Option<String>,
    },
    /// `:source <path>`: run an ex script (a session), a line per step.
    Source {
        path: String,
    },
    /// `:select-ids 3,7`: select rows by id (sessions, S-29 C).
    SelectIds {
        ids: Vec<u64>,
    },
    /// `:hide <column>`: hide a column by name (already hidden: nothing).
    HideNamed {
        column: String,
    },
    /// `:patch cells <column> <file>` / `:patch rows <file>` (sessions):
    /// pasted cells or added rows, saved by `:mksession` as parquet.
    Patch {
        column: Option<String>,
        path: String,
    },
    /// `:s/pattern/replacement/[g][i]` (S-28): the current column of the
    /// selected rows (none: of the view), Rust regex replacement (`$1`).
    Substitute {
        pattern: String,
        replacement: String,
        all: bool,
    },
    /// `*` `g*` `gz*`: prompt for `search/replace`.
    StartSubstInput {
        mode: SubstMode,
    },
    /// What the `*` prompt answers: replace every match, VisiData style.
    SubstPrompted {
        pattern: String,
        replacement: String,
        mode: SubstMode,
    },
    /// `:hits <text>` (S-28 B): a sheet of the active sheet's search hits.
    Hits {
        needle: String,
    },
    /// `:stats` (S-30 C): the selection's status-line stats to the clipboard, as TSV.
    CopyStats,
    /// `zn` (S-30 B): prompt for the cursor cell's note, the old one filled in.
    StartNoteInput,
    /// `:note <text>`: the cursor cell's note; empty deletes it.
    SetNote {
        text: String,
    },
    /// `:notes`: a sheet of this file's notes; `Enter` jumps to the cell.
    Notes,
    /// `:plot <x> <y…>` (S-23 C1): a plot sheet of the view.
    Plot {
        x: String,
        ys: Vec<String>,
    },
    /// `:hist [bins]` (S-23 C2): histogram of the cursor column.
    Hist {
        bins: Option<u32>,
    },
    /// `:bar`: the frequency table as bars.
    Bars,
    /// On a plot (S-23 C3): `H` `L` `J` `K` make the cursor box thinner,
    /// wider, taller, shorter by a character cell.
    PlotResize {
        wider: i8,
        taller: i8,
    },
    /// `_`: zoom to fit everything.
    ZoomAll,
    /// `zz`: zoom to the cursor box.
    ZoomCursor,
    /// `.` (VisiData plot-column): the cursor column against the numeric key column.
    PlotColumn,
    /// `g.` (plot-numerics): every visible numeric non-key column against it.
    PlotNumerics,
    /// `Z` / `:split`, `:vsplit` (S-23 B): a second pane with the sheet under this one.
    SplitPane {
        vertical: bool,
    },
    /// `gZ`: back to one pane.
    ClosePane,
    /// `Tab`: focus the other pane.
    FocusPane,
    /// `gTab`: swap the panes on screen.
    SwapPanes,
    /// `<C-r>` / `:refresh` (S-30 A): rebuild a derived sheet from its parent's current view.
    Refresh,
    /// `:link` / `:unlink`: refresh on activation when the parent changed.
    Link {
        on: bool,
    },
    /// `:mksession [path]`: write the sheet stack as an ex script.
    MkSession {
        path: Option<String>,
    },
    /// `m` (S-19): start recording a macro; again: stop and ask for its key.
    ToggleMacroRecord,
    /// A macro's steps, played one at a time once background jobs finish.
    Macro(Vec<Action>),
}

/// Where `*` / `g*` / `gz*` replace (VisiData `addcol-regex-subst`,
/// `setcol-regex-subst`, `setcol-regex-subst-all`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubstMode {
    /// A new column `<col>_re`.
    NewColumn,
    /// The current column of the selected rows.
    Selected,
    /// Every visible text column of the selected rows.
    SelectedAll,
}

/// What the sidebar shows (S-23 A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarPage {
    /// Stats of the cursor column (one row of `I`).
    Column,
    /// The cursor cell's full value, wrapped.
    Cell,
    /// The sheet layer's key bindings.
    Help,
    /// The query plan; `PlanOpt` after the optimizer.
    Plan,
    PlanOpt,
}

impl SidebarPage {
    pub fn name(self) -> &'static str {
        match self {
            Self::Column => "column",
            Self::Cell => "cell",
            Self::Help => "help",
            Self::Plan => "plan",
            Self::PlanOpt => "plan-opt",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "column" => Self::Column,
            "cell" => Self::Cell,
            "help" => Self::Help,
            "plan" => Self::Plan,
            "plan-opt" | "optimized" => Self::PlanOpt,
            _ => return None,
        })
    }

    /// `Ctrl+G` order.
    pub fn next(self) -> Self {
        match self {
            Self::Column => Self::Cell,
            Self::Cell => Self::Help,
            Self::Help => Self::Plan,
            Self::Plan | Self::PlanOpt => Self::Column,
        }
    }
}

impl Action {
    /// True = re-materialize the window after dispatch; false = cosmetic.
    /// Paging/`G` aren't listed: their handlers re-materialize only when the window slides.
    pub fn affects_data(&self) -> bool {
        matches!(
            self,
            Action::SetFilter { .. }
                | Action::SetFilterExpr { .. }
                | Action::Color { .. }
                | Action::NoColor { .. }
                | Action::ClearFilter
                | Action::SortAsc
                | Action::SortDesc
                | Action::SetSort { .. }
                | Action::ClearSort
        )
    }

    /// Whether `dispatch` checkpoints before this action for `U`. Rename,
    /// `:derive` and `:setcol` are undoable too, but checkpoint themselves after
    /// validating, so a rejected command leaves no empty undo step.
    pub fn undoable(&self) -> bool {
        matches!(
            self,
            Action::HideCol
                | Action::HideNamed { .. }
                | Action::Patch { .. }
                | Action::UnhideAll
                | Action::KeyCol
                | Action::UnkeyAll
                | Action::TypeCastInt
                | Action::TypeCastFloat
                | Action::TypeCastString
                | Action::TypeCastDate
                | Action::TypeCastCurrency
                | Action::FreezeColumn
                | Action::UnfreezeAll
                | Action::SetFilter { .. }
                | Action::SetFilterExpr { .. }
                | Action::Color { .. }
                | Action::NoColor { .. }
                | Action::ClearFilter
                | Action::SortAsc
                | Action::SortDesc
                | Action::SetSort { .. }
                | Action::ClearSort
                | Action::ToggleSelectRow
                | Action::SelectRow
                | Action::UnselectRow
                | Action::SelectAll
                | Action::UnselectAll
                | Action::SelectByRegex { .. }
                | Action::SelectExpr { .. }
                | Action::SelectIds { .. }
                | Action::SelectEqualCell
                | Action::SelectColDuplicates
                | Action::HideUniform
        )
    }

    /// Whether `.` should repeat this action. No wildcard: a new action must
    /// say. Prompts stay out: what `.` repeats is the submitted result.
    pub fn repeatable(&self) -> bool {
        use Action::*;
        match self {
            Quit
            | ForceQuit
            | ForcePopSheet
            | CancelJobs
            | CancelAllJobs
            | PopSheet
            | Redraw
            | RepeatLast
            | Save { .. }
            | Export { .. }
            | ExecSql { .. }
            | Undo
            | Redo
            | TimeWalk { .. }
            | OpenUndoTree
            | ToggleMacroRecord => false,
            Macro(_) => true,
            Open { .. } | SetCursor { .. } | Source { .. } | SelectIds { .. } => true,
            MkSession { .. } | CopyStats | Refresh | Link { .. } => false,
            SplitPane { .. } | ClosePane | FocusPane | SwapPanes => false,
            Plot { .. } | PlotColumn | PlotNumerics | Hist { .. } | Bars => true,
            PlotResize { .. } | ZoomAll | ZoomCursor => false,
            HideNamed { .. } | Patch { .. } => true,
            Substitute { .. } | SubstPrompted { .. } | Hits { .. } => true,
            StartSubstInput { .. } | StartNoteInput | Notes => false,
            SetNote { .. } | Color { .. } | NoColor { .. } => true,
            OpenSheetSwitcher => false,
            StartSearchInput { .. }
            | StartSearchExprInput { .. }
            | StartDeriveInput
            | StartSelectExprInput { .. }
            | StartFilterInput
            | StartExInput
            | StartSelectInput
            | StartDiffInput
            | StartRenameColInput
            | StartEditInput
            | StartSetColInput
            | StartSetExprInput
            | StartAddRowsInput
            | StartExpandDepthInput { .. }
            | StartAggregateInput { .. } => false,
            SubmitInput | CancelInput | InputBackspace => false,
            CursorDown { .. }
            | CursorUp { .. }
            | CursorLeft { .. }
            | CursorRight { .. }
            | PageDown
            | PageUp
            | GotoTop
            | GotoBottom
            | GotoLeftmost
            | GotoRightmost => true,
            FreezeColumn
            | UnfreezeAll
            | HideCol
            | UnhideAll
            | KeyCol
            | UnkeyAll
            | RenameCol { .. } => true,
            TypeCastInt | TypeCastFloat | TypeCastString | TypeCastDate | TypeCastCurrency => true,
            TypeCastJson => true,
            SearchNext | SearchPrev | SetFilter { .. } | ClearFilter => true,
            SortAsc | SortDesc | SetSort { .. } | ClearSort => true,
            OpenFreqTable | Pivot { .. } | Unpivot { .. } | Diff { .. } | Tail { .. } => true,
            SubmitRow | SubmitSelected | TogglePlanPanel => true,
            ToggleSidebar | CycleSidebar | ShowSidebar { .. } => true,
            ToggleSelectRow
            | SelectRow
            | UnselectRow
            | SelectAll
            | UnselectAll
            | SelectByRegex { .. }
            | SelectExpr { .. }
            | SelectEqualCell
            | SelectColDuplicates
            | DupSelected => true,
            ExCommand(_)
            | Lua(_)
            | LuaCommand { .. }
            | Join { .. }
            | Derive { .. }
            | Sql { .. }
            | SetCol { .. }
            | SetExpr { .. }
            | SetFilterExpr { .. }
            | EditCell { .. } => true,
            HideUniform | DupSheet | Describe | PivotCurrent | DedupeRows => true,
            CopyRow | CopySelected | CopyCell | CopyCells => true,
            SysCopyRow | SysCopySelected | SysCopyCell | SysCopyCells => true,
            SysPasteCells | SysPasteCellsSelected => true,
            PasteCell | PasteCells | CutCell | CutCells | DeleteCell | DeleteCells => true,
            DeleteRow | DeleteSelected | CutRow | CutSelected => true,
            AddRow | AddRows { .. } | PasteAfter | PasteBefore | OpenNew => true,
            ExpandCol { .. } | ExpandCols { .. } | ContractCol | UnfurlCol => true,
            OpenColumns | OpenColumnsAll | KeySelected { .. } | TypeSelected { .. } => true,
            AggregateCol { .. } | AggregateCols { .. } | MemoAggregate { .. } => true,
        }
    }
}

impl Action {
    /// Every action with a name, at its default arguments. The name is the
    /// Lua API's (`vd.bind`, S-10: locked) and, kebab-cased, an ex command
    /// (`:select-all`). Input-layer actions only make sense inside a prompt.
    #[rustfmt::skip]
    pub const NAMED: &[(&'static str, Action)] = {
        use Action::*;
        &[
        ("cursor_down", CursorDown { count: None }),
        ("cursor_up", CursorUp { count: None }),
        ("cursor_left", CursorLeft { count: None }),
        ("cursor_right", CursorRight { count: None }),
        ("page_down", PageDown),
        ("page_up", PageUp),
        ("goto_top", GotoTop),
        ("goto_bottom", GotoBottom),
        ("goto_leftmost", GotoLeftmost),
        ("goto_rightmost", GotoRightmost),
        ("freeze_column", FreezeColumn),
        ("unfreeze_all", UnfreezeAll),
        ("hide_col", HideCol),
        ("unhide_all", UnhideAll),
        ("key_col", KeyCol),
        ("unkey_all", UnkeyAll),
        ("start_rename_col_input", StartRenameColInput),
        ("type_cast_int", TypeCastInt),
        ("type_cast_float", TypeCastFloat),
        ("type_cast_string", TypeCastString),
        ("type_cast_date", TypeCastDate),
        ("type_cast_currency", TypeCastCurrency),
        ("type_cast_json", TypeCastJson),
        ("start_search_input", StartSearchInput { all_columns: false, backward: false }),
        ("start_searchr_input", StartSearchInput { all_columns: false, backward: true }),
        ("start_search_all_input", StartSearchInput { all_columns: true, backward: false }),
        ("start_searchr_all_input", StartSearchInput { all_columns: true, backward: true }),
        ("start_search_expr_input", StartSearchExprInput { backward: false }),
        ("start_searchr_expr_input", StartSearchExprInput { backward: true }),
        ("start_derive_input", StartDeriveInput),
        ("start_select_expr_input", StartSelectExprInput { unselect: false }),
        ("start_unselect_expr_input", StartSelectExprInput { unselect: true }),
        ("start_filter_input", StartFilterInput),
        ("start_ex_input", StartExInput),
        ("submit_input", SubmitInput),
        ("cancel_input", CancelInput),
        ("input_backspace", InputBackspace),
        ("search_next", SearchNext),
        ("search_prev", SearchPrev),
        ("clear_filter", ClearFilter),
        ("clear_sort", ClearSort),
        ("sort_asc", SortAsc),
        ("sort_desc", SortDesc),
        ("open_freq_table", OpenFreqTable),
        ("open_sheet_switcher", OpenSheetSwitcher),
        ("submit_row", SubmitRow),
        ("submit_selected", SubmitSelected),
        ("toggle_plan_panel", TogglePlanPanel),
        ("toggle_sidebar", ToggleSidebar),
        ("cycle_sidebar", CycleSidebar),
        ("pop_sheet", PopSheet),
        ("start_edit_input", StartEditInput),
        ("start_setcol_input", StartSetColInput),
        ("start_setexpr_input", StartSetExprInput),
        ("quit", Quit),
        ("cancel_jobs", CancelJobs),
        ("cancel_all_jobs", CancelAllJobs),
        ("redraw", Redraw),
        ("repeat_last", RepeatLast),
        ("toggle_select_row", ToggleSelectRow),
        ("select_row", SelectRow),
        ("unselect_row", UnselectRow),
        ("select_all", SelectAll),
        ("unselect_all", UnselectAll),
        ("start_select_input", StartSelectInput),
        ("dup_selected", DupSelected),
        ("dup_sheet", DupSheet),
        ("select_equal_cell", SelectEqualCell),
        ("select_col_duplicates", SelectColDuplicates),
        ("dedupe_rows", DedupeRows),
        ("copy_row", CopyRow),
        ("copy_selected", CopySelected),
        ("copy_cell", CopyCell),
        ("copy_cells", CopyCells),
        ("syscopy_row", SysCopyRow),
        ("syscopy_selected", SysCopySelected),
        ("syscopy_cell", SysCopyCell),
        ("syscopy_cells", SysCopyCells),
        ("syspaste_cells", SysPasteCells),
        ("syspaste_cells_selected", SysPasteCellsSelected),
        ("paste_cell", PasteCell),
        ("paste_cells", PasteCells),
        ("cut_cell", CutCell),
        ("cut_cells", CutCells),
        ("delete_cell", DeleteCell),
        ("delete_cells", DeleteCells),
        ("delete_row", DeleteRow),
        ("delete_selected", DeleteSelected),
        ("cut_row", CutRow),
        ("cut_selected", CutSelected),
        ("add_row", AddRow),
        ("start_add_rows_input", StartAddRowsInput),
        ("paste_after", PasteAfter),
        ("paste_before", PasteBefore),
        ("open_new", OpenNew),
        ("hide_uniform", HideUniform),
        ("describe", Describe),
        ("pivot_current", PivotCurrent),
        ("start_diff_input", StartDiffInput),
        ("undo", Undo),
        ("redo", Redo),
        ("open_undo_tree", OpenUndoTree),
        ("expand_col", ExpandCol { depth: 1 }),
        ("start_expand_depth_input", StartExpandDepthInput { all: false }),
        ("start_expand_all_depth_input", StartExpandDepthInput { all: true }),
        ("expand_cols", ExpandCols { depth: 1 }),
        ("contract_col", ContractCol),
        ("unfurl_col", UnfurlCol),
        ("open_columns", OpenColumns),
        ("open_columns_all", OpenColumnsAll),
        ("key_selected", KeySelected { on: true }),
        ("key_off_selected", KeySelected { on: false }),
        ("type_int_selected", TypeSelected { name: "int" }),
        ("type_float_selected", TypeSelected { name: "float" }),
        ("type_string_selected", TypeSelected { name: "str" }),
        ("type_date_selected", TypeSelected { name: "date" }),
        ("type_currency_selected", TypeSelected { name: "currency" }),
        ("start_aggregate_input", StartAggregateInput { memo: false, selected: false }),
        ("start_memo_aggregate_input", StartAggregateInput { memo: true, selected: false }),
        ("start_aggregate_cols_input", StartAggregateInput { memo: false, selected: true }),
        ("macro_record", ToggleMacroRecord),
        ("refresh", Refresh),
        ("plot_column", PlotColumn),
        ("zoom_all", ZoomAll),
        ("plot_thinner", PlotResize { wider: -1, taller: 0 }),
        ("plot_wider", PlotResize { wider: 1, taller: 0 }),
        ("plot_taller", PlotResize { wider: 0, taller: 1 }),
        ("plot_shorter", PlotResize { wider: 0, taller: -1 }),
        ("zoom_cursor", ZoomCursor),
        ("bar", Bars),
        ("plot_numerics", PlotNumerics),
        ("split", SplitPane { vertical: false }),
        ("vsplit", SplitPane { vertical: true }),
        ("close_pane", ClosePane),
        ("focus_pane", FocusPane),
        ("swap_panes", SwapPanes),
        ("start_note_input", StartNoteInput),
        ("notes", Notes),
        ("link", Link { on: true }),
        ("unlink", Link { on: false }),
        ("start_subst_input", StartSubstInput { mode: SubstMode::NewColumn }),
        ("start_subst_selected_input", StartSubstInput { mode: SubstMode::Selected }),
        ("start_subst_all_input", StartSubstInput { mode: SubstMode::SelectedAll }),
        ]
    };

    /// Look up `name` in [`Action::NAMED`]; `-` and `_` are interchangeable.
    pub fn from_name(name: &str) -> Option<Action> {
        let name = name.replace('-', "_");
        Self::NAMED
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, a)| a.clone())
    }

    /// A cursor move takes a count (`3j`); others ignore it.
    pub fn with_count(self, count: Option<usize>) -> Action {
        match self {
            Action::CursorDown { .. } => Action::CursorDown { count },
            Action::CursorUp { .. } => Action::CursorUp { count },
            Action::CursorLeft { .. } => Action::CursorLeft { count },
            Action::CursorRight { .. } => Action::CursorRight { count },
            other => other,
        }
    }

    /// The ex command that runs this action (S-19 macros): its name when it
    /// takes no arguments, else `:` syntax without the colon. `None`: no ex
    /// form (a Lua function, a macro, an argument the ex syntax can't spell).
    pub fn to_ex(&self) -> Option<String> {
        use Action::*;
        if let Some((n, _)) = Self::NAMED.iter().find(|(_, a)| a == self) {
            return Some(n.to_string());
        }
        let q = quote_arg;
        // `,` and ` -- ` separate list items; names holding them can't be spelled.
        let list = |v: &[String]| {
            v.iter()
                .all(|s| !s.contains(',') && !s.contains(" -- ") && s.trim() == s && !s.is_empty())
                .then(|| v.join(","))
        };
        Some(match self {
            CursorDown { count: Some(n) } => format!("cursor_down {n}"),
            CursorUp { count: Some(n) } => format!("cursor_up {n}"),
            CursorLeft { count: Some(n) } => format!("cursor_left {n}"),
            CursorRight { count: Some(n) } => format!("cursor_right {n}"),
            RenameCol { new_name } => format!("rename {}", q(new_name)),
            SetFilter { column, regex } if !regex.is_empty() && regex.trim() == regex => {
                format!("filter {} {regex}", q(column))
            }
            SetSort { column, descending } => {
                let sign = if *descending { "-" } else { "+" };
                format!("sort {}", q(&format!("{sign}{column}")))
            }
            Pivot {
                index,
                on,
                values,
                agg,
            } => {
                format!("pivot {} {} {} {}", q(index), q(on), q(values), q(agg))
            }
            Unpivot { on, index } if index.is_empty() => format!("unpivot {}", list(on)?),
            Unpivot { on, index } => format!("unpivot {} -- {}", list(on)?, list(index)?),
            Diff { path, keys } if keys.is_empty() => format!("diff {}", q(path)),
            Diff { path, keys } => format!("diff {} {}", q(path), q(&list(keys)?)),
            Tail { path, follow } => {
                format!("tail {}{}", q(path), if *follow { " --follow" } else { "" })
            }
            ShowSidebar { page } => format!("sidebar {}", page.name()),
            SelectExpr { expr, unselect } => {
                format!("{} {expr}", if *unselect { "unselect" } else { "select" })
            }
            TimeWalk { n } if *n > 0 => format!("earlier {n}"),
            TimeWalk { n } if *n < 0 => format!("later {}", -n),
            Save { path } => format!("save {}", q(path)),
            Export { path } => format!("export {}", q(path)),
            Join { source, keys, how } if keys.is_empty() => format!("join {} {how}", q(source)),
            Join { source, keys, how } => format!("join {} {} {how}", q(source), q(&list(keys)?)),
            Derive { name, expr } if !name.contains('=') => format!("derive {} = {expr}", q(name)),
            Sql { query } => format!("sql {query}"),
            ExecSql { sql } => format!("exec-sql {sql}"),
            SetExpr { expr } => format!("setexpr {expr}"),
            SetFilterExpr { expr: Some(e) } => format!("filter-expr {e}"),
            SetFilterExpr { expr: None } => "filter-expr".into(),
            Color {
                column,
                pred,
                style,
            } => format!(
                "color {} {pred} {style}",
                column.as_deref().map_or("*".into(), q)
            ),
            NoColor { column: None } => "nocolor".into(),
            NoColor { column: Some(c) } => format!("nocolor {}", q(c)),
            SetCol { value } => format!("setcol {}", q(value)),
            EditCell { value } => format!("edit {}", q(value)),
            ExCommand(s) => s.clone(),
            Open { path } => format!("open {}", q(path)),
            SetCursor { row, column: None } => format!("cursor {row}"),
            SetCursor {
                row,
                column: Some(c),
            } => format!("cursor {row} {}", q(c)),
            Source { path } => format!("source {}", q(path)),
            SelectIds { ids } => format!(
                "select-ids {}",
                ids.iter().map(u64::to_string).collect::<Vec<_>>().join(",")
            ),
            MkSession { path: None } => "mksession".into(),
            MkSession { path: Some(p) } => format!("mksession {}", q(p)),
            AggregateCol { names } => format!("aggregate {names}"),
            HideNamed { column } => format!("hide {}", q(column)),
            Patch { column: None, path } => format!("patch rows {}", q(path)),
            Patch {
                column: Some(c),
                path,
            } => format!("patch cells {} {}", q(c), q(path)),
            ExpandCol { depth } => format!("expand {depth}"),
            Hits { needle } => format!("hits {needle}"),
            Hist { bins } => bins.map_or("hist".into(), |b| format!("hist {b}")),
            Plot { x, ys } => std::iter::once(x)
                .chain(ys)
                .fold("plot".to_string(), |s, c| s + " " + &q(c)),
            SetNote { text } => format!("note {text}").trim_end().to_string(),
            Substitute {
                pattern,
                replacement,
                all,
            } => {
                let d = ['/', '|', '#', ',', ';', '!', '@', '%', '~']
                    .into_iter()
                    .find(|d| !pattern.contains(*d) && !replacement.contains(*d))?;
                // `\` in the replacement is literal: doubled, as vim spells it
                let r = replacement.replace('\\', "\\\\");
                format!("s{d}{pattern}{d}{r}{d}{}", if *all { "g" } else { "" })
            }
            _ => return None,
        })
    }

    /// Prompt editing actions: bindable in the input layer, not ex commands.
    pub fn is_input_layer(&self) -> bool {
        matches!(
            self,
            Action::SubmitInput | Action::CancelInput | Action::InputBackspace
        )
    }
}

/// One ex argument: bare when it splits back to itself, else double-quoted.
fn quote_arg(s: &str) -> String {
    let bare = !s.is_empty()
        && !s
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '"' | '\'' | '\\'));
    if bare {
        return s.to_string();
    }
    let mut out = String::from('"');
    for c in s.chars() {
        if matches!(c, '"' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The guide's vd.bind section lists every action name.
    #[test]
    fn user_guide_lists_every_action_name() {
        let html = include_str!("../docs/index.html");
        for (name, _) in Action::NAMED {
            assert!(
                html.contains(&format!("<code>{name}</code>")),
                "docs/index.html misses {name}"
            );
        }
    }

    /// Every payload action with an ex form parses back to itself (S-19).
    /// A new payload action adds an entry here.
    #[test]
    fn to_ex_round_trips_through_the_ex_parser() {
        use Action::*;
        let s = |v: &str| v.to_string();
        let cases = [
            CursorDown { count: Some(3) },
            CursorUp { count: Some(1) },
            CursorLeft { count: Some(2) },
            CursorRight { count: Some(10) },
            RenameCol {
                new_name: s("new name"),
            },
            RenameCol {
                new_name: s("q\"uote"),
            },
            SetFilter {
                column: s("my col"),
                regex: s("^n.*\\d"),
            },
            SetSort {
                column: s("amt"),
                descending: true,
            },
            SetSort {
                column: s("-odd"),
                descending: false,
            },
            Pivot {
                index: s("a"),
                on: s("b c"),
                values: s("v"),
                agg: s("mean"),
            },
            Unpivot {
                on: vec![s("x"), s("y")],
                index: vec![],
            },
            Unpivot {
                on: vec![s("x")],
                index: vec![s("id"), s("k")],
            },
            Diff {
                path: s("other file.csv"),
                keys: vec![],
            },
            Diff {
                path: s("b.csv"),
                keys: vec![s("id"), s("k")],
            },
            Tail {
                path: s("log.csv"),
                follow: true,
            },
            Tail {
                path: s("log.csv"),
                follow: false,
            },
            ShowSidebar {
                page: SidebarPage::PlanOpt,
            },
            SelectExpr {
                expr: s("amt > 3"),
                unselect: false,
            },
            SelectExpr {
                expr: s("amt > 3"),
                unselect: true,
            },
            TimeWalk { n: 2 },
            TimeWalk { n: -3 },
            Save {
                path: s("out dir/x.csv"),
            },
            Export { path: s("x.py") },
            Join {
                source: s("b.csv"),
                keys: vec![s("id")],
                how: s("left"),
            },
            Join {
                source: s("b.csv"),
                keys: vec![],
                how: s("append"),
            },
            Derive {
                name: s("dbl"),
                expr: s("amt * 2"),
            },
            Derive {
                name: s("two words"),
                expr: s("1"),
            },
            Sql {
                query: s("SELECT * FROM self"),
            },
            ExecSql {
                sql: s("DELETE FROM t"),
            },
            SetExpr { expr: s("amt + 1") },
            SetFilterExpr {
                expr: Some(s("amt > 1")),
            },
            SetFilterExpr { expr: None },
            Color {
                column: Some(s("a b")),
                pred: s("amt > 100"),
                style: s("bg=yellow,bold"),
            },
            Color {
                column: None,
                pred: s("x = 'a b'"),
                style: s("red"),
            },
            NoColor { column: None },
            NoColor {
                column: Some(s("*")),
            },
            SetCol { value: s("") },
            SetCol { value: s("a b") },
            EditCell { value: s("") },
            EditCell { value: s("x\\y") },
            ExpandCol { depth: 1 },
            KeySelected { on: false },
            Open {
                path: s("data/a b.csv"),
            },
            SetCursor {
                row: 12,
                column: Some(s("amount")),
            },
            SetCursor {
                row: 1,
                column: None,
            },
            Source { path: s("s.vdrs") },
            SelectIds { ids: vec![3, 7] },
            MkSession { path: None },
            MkSession {
                path: Some(s("my session.vdrs")),
            },
            AggregateCol {
                names: s("sum mean"),
            },
            HideNamed { column: s("a b") },
            Patch {
                column: None,
                path: s("/x/s.vdrs.d/0-1.parquet"),
            },
            Patch {
                column: Some(s("my col")),
                path: s("p.parquet"),
            },
            ExpandCol { depth: 3 },
            Hits {
                needle: s("foo bar"),
            },
            Substitute {
                pattern: s("a/b +$"),
                replacement: s("${1}x"),
                all: true,
            },
            Substitute {
                pattern: s("x"),
                replacement: s(""),
                all: false,
            },
            Substitute {
                pattern: s(r"\d"),
                replacement: s(r"a\1"),
                all: false,
            },
        ];
        for a in cases {
            let ex = a.to_ex().unwrap_or_else(|| panic!("no ex form: {a:?}"));
            let back = crate::exline::parse(&ex).unwrap_or_else(|e| panic!("{ex}: {e}"));
            assert_eq!(back, vec![a], "{ex}");
        }
        for none in [SelectByRegex { regex: s("x") }, Lua(0)] {
            assert_eq!(none.to_ex(), None, "{none:?}");
        }
    }

    #[test]
    fn repeatable_excludes_quit() {
        assert!(!Action::Quit.repeatable());
        assert!(Action::CursorDown { count: None }.repeatable());
    }

    #[test]
    fn affects_data_distinguishes() {
        assert!(!Action::CursorDown { count: None }.affects_data());
        assert!(Action::ClearFilter.affects_data());
    }
}
