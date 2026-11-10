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
                | Action::SelectEqualCell
                | Action::SelectColDuplicates
                | Action::HideUniform
                | Action::AggregateCol { .. }
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
            | OpenUndoTree => false,
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

    /// Prompt editing actions: bindable in the input layer, not ex commands.
    pub fn is_input_layer(&self) -> bool {
        matches!(
            self,
            Action::SubmitInput | Action::CancelInput | Action::InputBackspace
        )
    }
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
