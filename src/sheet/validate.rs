//! `:validate` (S-36 D): a SQL predicate each edit of a column must keep.

use super::*;

/// `:validate <column> <pred>`: edits of `column` must leave `pred` true
/// (or null) on the rows they write.
#[derive(Debug, Clone, PartialEq)]
pub struct Validation {
    pub column: String,
    pub pred: String,
}

impl Sheet {
    /// `:validate column pred` sets `column`'s rule (one per column); no
    /// `pred` clears it. An undo state. What to tell the user.
    pub fn set_validation(&mut self, column: &str, pred: Option<&str>) -> Result<String, String> {
        if !self.has_column(column) {
            return Err(format!("no column: {column}"));
        }
        if let Some(p) = pred {
            self.predicate(&self.plan, p).map(drop)?;
        }
        self.checkpoint();
        self.view.validations.retain(|v| v.column != column);
        let Some(pred) = pred else {
            return Ok(format!("{column}: no validation"));
        };
        self.view.validations.push(Validation {
            column: column.to_string(),
            pred: pred.to_string(),
        });
        Ok(format!(
            "{column}: edits must keep {pred}; :select NOT ({pred}) finds rows that fail now"
        ))
    }

    /// The `:validate` rules, as `:validations` lists them.
    pub fn validations_text(&self) -> String {
        match self.view.validations.as_slice() {
            [] => "no validations".into(),
            v => v
                .iter()
                .map(|v| format!("{}: {}", v.column, v.pred))
                .collect::<Vec<_>>()
                .join("; "),
        }
    }

    /// After an edit of `columns` on rows `rids` (`None`: every row): refuse
    /// it (Err) when a written cell fails its column's rule, or in warn mode
    /// keep it and leave a note. A rule that no longer compiles checks nothing.
    // ponytail: runs on the UI thread; `:s` on every row scans the column
    // there. Move it into the S-33 edit job if that shows up.
    pub(super) fn check_edit(
        &mut self,
        columns: &[&str],
        rids: Option<&[u64]>,
    ) -> Result<(), String> {
        for v in &self.view.validations {
            if !columns.contains(&v.column.as_str()) {
                continue;
            }
            let Ok(fails) = self.predicate(&self.plan, &format!("NOT ({})", v.pred)) else {
                continue;
            };
            let mut lf = self.plan.clone();
            if let Some(r) = rids {
                lf = lf.filter(rid_in(r));
            }
            let bad = lf
                .filter(fails)
                .select([col(ROW_ID)])
                .collect()
                .map_err(|e| e.to_string())?;
            let Some(&first) = rid_values(&bad).map_err(|e| e.to_string())?.first() else {
                continue;
            };
            let at = match self.view_index_of(first) {
                Ok(Some(i)) => format!("row {}", i + 1),
                _ => "a hidden row".into(),
            };
            if self.state.validation_warn {
                let n = bad.height();
                self.state.validation_note =
                    Some(format!("validate: {n} {} cells fail {}", v.column, v.pred));
                continue;
            }
            return Err(format!(
                "validate: {} fails {} on {at}; edit refused",
                v.column, v.pred
            ));
        }
        Ok(())
    }

    /// Rows failing `:validate` rules show in the invalid style (window only).
    pub(super) fn validation_marks(&self, base: &LazyFrame) -> Vec<Expr> {
        self.view
            .validations
            .iter()
            .map(|v| {
                self.predicate(base, &format!("NOT ({})", v.pred))
                    .unwrap_or(lit(false))
            })
            .collect()
    }
}
