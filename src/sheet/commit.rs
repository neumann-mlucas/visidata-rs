//! `z Ctrl+S` on a database table (VisiData commit-sheet, S-46 B8): what
//! changed since the table was read, by row id.

use super::*;
use crate::commit::{Changes, CommitPlan, Table};

impl Sheet {
    /// The table this sheet shows, if it is a DuckDB or SQLite table opened
    /// as a file's part; with what [`CommitPlan::changes`] needs to diff it.
    /// Hidden columns and columns the table doesn't have aren't written.
    pub fn commit_plan(&self) -> Result<CommitPlan, String> {
        let kind = self.source.kind;
        let (Some(db), Origin::File) = (self.source.path.clone(), &self.origin) else {
            return Err("nothing to commit on this sheet".into());
        };
        if !matches!(kind, SourceKind::Duckdb | SourceKind::Sqlite) {
            return Err("nothing to commit on this sheet".into());
        }
        let shown = format!("{}:", db.display());
        let part = match self.source.display_name.strip_prefix(&shown) {
            Some(p) => p.to_string(),
            // a SQLite file of one table opens it directly
            None => crate::reader::sqlite_tables(&db)
                .map_err(|e| e.to_string())?
                .into_iter()
                .next()
                .ok_or("no table to commit to")?,
        };
        let base = (self.tail_base()).map_err(|e| e.to_string())?;
        let table_cols: Vec<String> = (base.clone().collect_schema())
            .map_err(|e| e.to_string())?
            .iter_names()
            .map(|n| n.to_string())
            .collect();
        // an unedited sheet's plan is its base, numbered the same way
        let now = match self.has_rids() {
            true => self.plan.clone(),
            false => with_rids(self.plan.clone()),
        };
        let schema = now.clone().collect_schema().map_err(|e| e.to_string())?;
        let columns = (table_cols.iter())
            .filter(|c| schema.contains(c) && !self.view.hidden.contains(*c))
            .cloned()
            .collect();
        Ok(CommitPlan {
            table: Table { db, kind, part },
            before: with_rids(base),
            now,
            deleted: self.deleted_rids(),
            columns,
        })
    }
}

impl CommitPlan {
    /// The rows deleted, changed (in a shown table column; compared as text)
    /// and added since the table was read.
    pub fn changes(&self) -> PolarsResult<Changes> {
        let gone = rid_in(&self.deleted);
        let added = col(ROW_ID).gt_eq(lit(ADDED_RID));
        let before = self.before.clone();
        let deleted = (before.clone().filter(gone.clone()).drop(cols([ROW_ID]))).collect()?;
        let now = self.now.clone().filter(gone.not());
        let shown = || self.columns.iter().map(|c| col(c.as_str()));
        let new = now
            .clone()
            .filter(added.clone())
            .select(shown().collect::<Vec<_>>())
            .collect()?;
        // each kept source row beside its values now, `__new_`-prefixed
        let fresh = |c: &str| format!("__new_{c}");
        let now = now.filter(added.not()).select(
            std::iter::once(col(ROW_ID))
                .chain(self.columns.iter().map(|c| col(c.as_str()).alias(fresh(c))))
                .collect::<Vec<_>>(),
        );
        let text = |e: Expr| e.cast(DataType::String);
        let changed = self
            .columns
            .iter()
            .map(|c| text(col(c.as_str())).neq_missing(text(col(fresh(c)))));
        let both = before
            .join(
                now,
                [col(ROW_ID)],
                [col(ROW_ID)],
                JoinArgs::new(JoinType::Inner),
            )
            .filter(any_horizontal(changed.collect::<Vec<_>>())?)
            .collect()?;
        let table: Vec<String> = (self.before.clone().collect_schema()?.iter_names())
            .map(|n| n.to_string())
            .filter(|n| n != ROW_ID)
            .collect();
        let old = both.select(table)?;
        let fresh_cols: Vec<Expr> = (self.columns.iter())
            .map(|c| col(fresh(c)).alias(c.as_str()))
            .collect();
        let changed_new = both.lazy().select(fresh_cols).collect()?;
        Ok(Changes {
            deleted,
            old,
            new: changed_new,
            added: new,
        })
    }
}
