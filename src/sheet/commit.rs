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
        // same type: as is (a list or struct has no text cast); else as text
        let (was, is) = (
            before.clone().collect_schema()?,
            now.clone().collect_schema()?,
        );
        let changed = self.columns.iter().map(|c| {
            let (a, b) = (col(c.as_str()), col(fresh(c)));
            match was.get(c) == is.get(&fresh(c)) {
                true => a.neq_missing(b),
                false => (a.cast(DataType::String)).neq_missing(b.cast(DataType::String)),
            }
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(before: DataFrame, now: DataFrame) -> CommitPlan {
        let columns = (before.get_column_names().iter())
            .map(|n| n.to_string())
            .collect();
        CommitPlan {
            table: Table {
                db: "t.duckdb".into(),
                kind: SourceKind::Duckdb,
                part: "t".into(),
            },
            before: with_rids(before.lazy()),
            now: with_rids(now.lazy()),
            deleted: Vec::new(),
            columns,
        }
    }

    /// A list or struct column beside an edited one doesn't stop the diff.
    #[test]
    fn changes_compare_nested_columns() {
        let before = df!("id" => [1i64, 2], "note" => ["a", "b"]).unwrap();
        let tags = Series::new(
            "tags".into(),
            [
                Series::new("".into(), [1i64]),
                Series::new("".into(), [2i64, 3]),
            ],
        );
        let before = before.hstack(&[tags.into_column()]).unwrap();
        let before = before
            .lazy()
            .with_column(as_struct(vec![col("id"), col("note")]).alias("s"))
            .collect()
            .unwrap();
        let now = before
            .clone()
            .lazy()
            .with_column(
                when(col("id").eq(lit(2)))
                    .then(lit("z"))
                    .otherwise(col("note"))
                    .alias("note"),
            )
            .collect()
            .unwrap();
        let c = plan(before, now).changes().unwrap();
        assert_eq!(
            c.new.column("note").unwrap().str().unwrap().get(0),
            Some("z")
        );
        assert_eq!(c.new.height(), 1);
    }
}
