//! How the view looks: sort, filter, colors, precision.

use super::*;

impl App {
    pub(super) fn set_sort(&mut self, column: String, descending: bool) {
        let s = self.sheets.active_mut();
        let order = if descending {
            SortOrder::Desc
        } else {
            SortOrder::Asc
        };
        if let Some(k) = s.view.sort.iter_mut().find(|k| k.column == column) {
            k.order = order;
        } else {
            s.view.sort.push(SortKey { column, order });
        }
        s.view.window_offset = 0;
        s.view.row_cursor = 0;
        s.view.row_offset = 0;
    }

    pub(super) fn sort_keys(&mut self, descending: bool) -> ControlFlow<()> {
        let s = self.sheets.active_mut();
        if s.view.keys.is_empty() {
            self.toast = Some("no key columns to sort by".into());
            return Break(());
        }
        let order = if descending {
            SortOrder::Desc
        } else {
            SortOrder::Asc
        };
        s.view.sort = (s.view.keys.iter())
            .map(|k| SortKey {
                column: k.clone(),
                order,
            })
            .collect();
        s.view.window_offset = 0;
        s.view.row_cursor = 0;
        s.view.row_offset = 0;
        Continue(())
    }

    pub(super) fn precision(&mut self, action: Action) -> ControlFlow<()> {
        let Some(column) = self.current_column_name() else {
            return Break(());
        };
        let (set, delta) = match action {
            Action::PrecisionMore => (None, 1),
            Action::PrecisionLess => (None, -1),
            Action::SetPrecision { digits } => (digits, 0),
            _ => return Break(()),
        };
        let s = self.sheets.active_mut();
        let dated = s
            .view_dtype(&column)
            .is_some_and(|d| matches!(d, DataType::Date | DataType::Datetime(..)));
        if dated && set.is_none() && delta != 0 {
            let t = s.format_change(|s| s.cycle_date_fmt(&column, delta));
            self.toast = Some(t.unwrap_or_else(|e| e));
            return Continue(());
        }
        // VisiData starts from its default `{:.2f}`
        let start = self.theme.float_digits.unwrap_or(2);
        let s = self.sheets.active_mut();
        self.toast = Some(
            match s.format_change(|s| s.set_precision(&column, set, delta, start)) {
                Ok(Some(1)) => format!("{column}: 1 decimal"),
                Ok(Some(d)) => format!("{column}: {d} decimals"),
                Ok(None) => format!("{column}: default precision"),
                Err(e) => e,
            },
        );
        Continue(())
    }

    pub(super) fn color(&mut self, column: Option<String>, pred: String, style: String) {
        self.sheets
            .active_mut()
            .view
            .formats
            .push(crate::sheet::FormatRule {
                column,
                pred,
                style,
            })
    }

    pub(super) fn no_color(&mut self, column: Option<String>) {
        let s = self.sheets.active_mut();
        let v = &mut s.view;
        let before = v.formats.len() + v.scales.len();
        v.formats.retain(|r| match column.as_deref() {
            None => false,
            Some("*") => r.column.is_some(),
            Some(c) => r.column.as_deref() != Some(c),
        });
        v.scales
            .retain(|r| column.as_deref().is_some_and(|c| r.column != c));
        let n = before - v.formats.len() - v.scales.len();
        self.toast = Some(format!(
            "{n} color rule{} cleared",
            if n == 1 { "" } else { "s" }
        ));
    }

    pub(super) fn set_filter_expr(&mut self, expr: Option<String>) {
        let s = self.sheets.active_mut();
        match s.set_filter_expr(expr.as_deref()) {
            // `affects_data`: dispatch re-materializes.
            Ok(()) => (s.view.window_offset, s.view.row_cursor, s.view.row_offset) = (0, 0, 0),
            Err(e) => self.toast = Some(format!("filter-expr: {e}")),
        }
    }
}
