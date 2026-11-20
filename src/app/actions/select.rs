//! Row selection by value, by position, between selected rows.

use super::*;

impl App {
    pub(super) fn select_col_duplicates(&mut self) -> ControlFlow<()> {
        let Some(column) = self.current_column_name() else {
            return Break(());
        };
        let sheet = self.sheets.active().clone();
        self.spawn_job("select", move || {
            let rids = sheet
                .rids_col_duplicates(&column)
                .map_err(|e| e.to_string())?;
            let note = format!("selected {} repeated {column} values", rids.len());
            Ok(JobDone::Select { rids, note })
        });
        Continue(())
    }

    pub(super) fn select_equal_cell(&mut self) -> ControlFlow<()> {
        let s = self.sheets.active();
        let Some(column) = self.current_column_name() else {
            return Break(());
        };
        let Some(value) = s.cursor_value(&column).map(|v| v.into_static()) else {
            return Break(());
        };
        let sheet = s.clone();
        self.spawn_job("select", move || {
            let rids = sheet
                .rids_with_values(&column, std::slice::from_ref(&value))
                .map_err(|e| e.to_string())?;
            let text = crate::sheet::any_text(&value);
            let note = format!("selected {} rows where {column} = {text}", rids.len());
            Ok(JobDone::Select { rids, note })
        });
        Continue(())
    }

    pub(super) fn select_to_selected(&mut self, forward: bool) {
        let at = self.cursor_row();
        let sheet = self.sheets.active().clone();
        self.spawn_job("select", move || {
            let (_, rids) = sheet
                .rids_to_selected(at, forward)
                .map_err(|e| e.to_string())?
                .ok_or("no selected row")?;
            Ok(JobDone::SelectIn {
                sheet: sheet.id,
                select: true,
                rids,
            })
        });
    }

    pub(super) fn select_before(&mut self, action: Action) {
        let select = matches!(action, Action::SelectBefore);
        let n = self.cursor_row() as usize;
        let sheet = self.sheets.active().clone();
        self.spawn_job("select", move || {
            let rids = sheet.view_rids_at(0, n).map_err(|e| e.to_string())?;
            Ok(JobDone::SelectIn {
                sheet: sheet.id,
                select,
                rids,
            })
        });
    }
}
