//! Action enum. See S-02. Phase 0 slice.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
    FilterCurrentColumn {
        regex: String,
    },
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
    SwitchToSheet(u32),
    SubmitRow,

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
    DupSelected,      // `"` → new sheet with selected rows only

    // ex-line
    StartExInput,
    ExCommand(String), // raw command text, parsed at dispatch

    // persist current sheet to disk; extension decides writer
    Save {
        path: String,
    },

    // input modes
    StartSearchInput,
    StartFilterInput,
    SubmitInput,
    CancelInput,
    InputChar(char),
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
            Action::FilterCurrentColumn { .. }
                | Action::SetFilter { .. }
                | Action::ClearFilter
                | Action::SortAsc
                | Action::SortDesc
                | Action::SetSort { .. }
                | Action::ClearSort
        )
    }

    /// Whether `.` should repeat this action.
    pub fn repeatable(&self) -> bool {
        !matches!(
            self,
            Action::Quit
                | Action::PopSheet
                | Action::Redraw
                | Action::RepeatLast
                | Action::StartSearchInput
                | Action::StartFilterInput
                | Action::StartExInput
                | Action::SubmitInput
                | Action::CancelInput
                | Action::InputChar(_)
                | Action::InputBackspace
                | Action::OpenSheetSwitcher
                | Action::StartRenameColInput
                | Action::Save { .. }
        )
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
