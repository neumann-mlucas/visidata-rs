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

    // plan panel
    TogglePlanPanel,

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
    /// Rows whose current-column value already occurred above, in view order.
    SelectColDuplicates,
    /// New sheet keeping each key's first row (all columns when no keys).
    DedupeRows,

    // ex-line
    StartExInput,
    ExCommand(String), // raw command text, parsed at dispatch
    /// `vd.bind(layer, keys, function() … end)`: index of the Lua callback.
    Lua(usize),

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
    SetCol {
        value: String,
    },
    HideUniform,
    DupSheet,
    Describe,
    PivotCurrent,
    StartDiffInput,
    Undo,
    Redo,

    // input modes
    StartSearchInput,
    StartFilterInput,
    SubmitInput,
    CancelInput,
    InputBackspace,

    // sheet stack
    PopSheet,

    // meta
    Quit,
    RepeatLast,
    Redraw,
}

impl Action {
    /// True = re-materialize the window after dispatch; false = cosmetic.
    /// Paging/`G` aren't listed: their handlers re-materialize only when the window slides.
    pub fn affects_data(&self) -> bool {
        matches!(
            self,
            Action::SetFilter { .. }
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
            Quit | PopSheet | Redraw | RepeatLast | Save { .. } | Export { .. } | Undo | Redo => {
                false
            }
            OpenSheetSwitcher => false,
            StartSearchInput | StartFilterInput | StartExInput | StartSelectInput
            | StartDiffInput | StartRenameColInput => false,
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
            SearchNext | SearchPrev | SetFilter { .. } | ClearFilter => true,
            SortAsc | SortDesc | SetSort { .. } | ClearSort => true,
            OpenFreqTable | Pivot { .. } | Unpivot { .. } | Diff { .. } | Tail { .. } => true,
            SubmitRow | SubmitSelected | TogglePlanPanel => true,
            ToggleSelectRow
            | SelectRow
            | UnselectRow
            | SelectAll
            | UnselectAll
            | SelectByRegex { .. }
            | SelectEqualCell
            | SelectColDuplicates
            | DupSelected => true,
            ExCommand(_) | Lua(_) | Join { .. } | Derive { .. } | Sql { .. } | SetCol { .. } => {
                true
            }
            HideUniform | DupSheet | Describe | PivotCurrent | DedupeRows => true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
