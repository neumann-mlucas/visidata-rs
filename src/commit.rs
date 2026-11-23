//! `z Ctrl+S` (VisiData commit-sheet, S-46 B8): a DuckDB or SQLite table's
//! edits, added rows and deleted rows written back to the table, matched by
//! its primary key.

use std::path::PathBuf;

use polars::prelude::*;

use crate::reader::SourceKind;

/// A table in a database file: `part` as the file's part list names it
/// (`schema.table` in DuckDB).
#[derive(Debug, Clone)]
pub struct Table {
    pub db: PathBuf,
    pub kind: SourceKind,
    pub part: String,
}

/// What `Sheet::commit_plan` hands a job: the rows as read and as they are
/// now, both with their row ids, still lazy.
pub struct CommitPlan {
    pub table: Table,
    pub before: LazyFrame,
    pub now: LazyFrame,
    pub deleted: Vec<u64>,
    /// The table's columns the sheet shows: the only ones written.
    pub columns: Vec<String>,
}

/// Rows to change, each frame without row ids. `deleted` and `old` hold the
/// rows as read (every table column); `new` and `added`, [`CommitPlan::columns`].
/// `old` and `new` pair up by position.
pub struct Changes {
    pub deleted: DataFrame,
    pub old: DataFrame,
    pub new: DataFrame,
    pub added: DataFrame,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        [&self.deleted, &self.new, &self.added]
            .iter()
            .all(|d| d.height() == 0)
    }
}

/// Write `plan`'s changes to its table in one transaction. The note says what
/// changed. A table without a primary key takes only added rows.
// ponytail: VisiData matches SQLite rows by rowid; vdrs needs a primary key
// in SQLite too. Read the rowid with the rows if keyless tables matter
pub fn commit(plan: &CommitPlan) -> Result<String, String> {
    let c = plan.changes().map_err(|e| e.to_string())?;
    if c.is_empty() {
        return Err("no changes to commit".into());
    }
    let t = &plan.table;
    let keys = match t.kind {
        SourceKind::Duckdb => crate::duckdb::primary_key(&t.db, &t.part)?,
        _ => sqlite::primary_key(t)?,
    };
    if keys.is_empty() && (c.deleted.height() > 0 || c.new.height() > 0) {
        return Err("cannot modify rows in tables without primary key".into());
    }
    match t.kind {
        SourceKind::Duckdb => crate::duckdb::commit(&t.db, &t.part, &keys, &c)?,
        _ => sqlite::commit(t, &keys, &c)?,
    }
    Ok(format!(
        "committed {} changed, {} added, {} deleted rows to {}",
        c.new.height(),
        c.added.height(),
        c.deleted.height(),
        t.part
    ))
}

mod sqlite {
    use rusqlite::types::Value;

    use super::*;
    use crate::duckdb::ident;

    pub(super) fn primary_key(t: &Table) -> Result<Vec<String>, String> {
        let conn = rusqlite::Connection::open(&t.db).map_err(|e| e.to_string())?;
        let mut stmt = (conn.prepare(&format!("PRAGMA table_info({})", ident(&t.part))))
            .map_err(|e| e.to_string())?;
        let mut keys: Vec<(i64, String)> = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(5)?, r.get::<_, String>(1)?)))
            .and_then(Iterator::collect)
            .map_err(|e| e.to_string())?;
        keys.retain(|(pk, _)| *pk > 0);
        keys.sort();
        Ok(keys.into_iter().map(|(_, n)| n).collect())
    }

    /// Row `i` of `df`'s `columns` as SQLite values.
    fn values(df: &DataFrame, columns: &[String], i: usize) -> Result<Vec<Value>, String> {
        (columns.iter())
            .map(|c| {
                let v = df
                    .column(c)
                    .and_then(|s| s.get(i))
                    .map_err(|e| e.to_string())?;
                Ok(match v {
                    AnyValue::Null => Value::Null,
                    AnyValue::Boolean(b) => Value::Integer(b.into()),
                    v if v.dtype().is_integer() => {
                        v.extract::<i64>().map_or(Value::Null, Value::Integer)
                    }
                    v if v.dtype().is_float() => {
                        v.extract::<f64>().map_or(Value::Null, Value::Real)
                    }
                    v => Value::Text(crate::sheet::any_text(&v)),
                })
            })
            .collect()
    }

    pub(super) fn commit(t: &Table, keys: &[String], c: &Changes) -> Result<(), String> {
        let e = |e: rusqlite::Error| e.to_string();
        let mut conn = rusqlite::Connection::open(&t.db).map_err(e)?;
        let tx = conn.transaction().map_err(e)?;
        // `sql` once per row of the frames; its parameters: each frame's columns
        let run = |sql: &str, params: &[(&DataFrame, &[String])]| -> Result<(), String> {
            let rows = params.first().map_or(0, |(df, _)| df.height());
            if rows == 0 {
                return Ok(());
            }
            let mut stmt = tx.prepare(sql).map_err(e)?;
            for i in 0..rows {
                let mut v = Vec::new();
                for (df, cs) in params {
                    v.extend(values(df, cs, i)?);
                }
                stmt.execute(rusqlite::params_from_iter(v)).map_err(e)?;
            }
            Ok(())
        };
        let table = ident(&t.part);
        let names = |df: &DataFrame| -> Vec<String> {
            (df.get_column_names().iter())
                .map(|n| n.to_string())
                .collect()
        };
        let matched: Vec<String> = keys.iter().map(|k| format!("{} IS ?", ident(k))).collect();
        let matched = matched.join(" AND ");
        run(
            &format!("DELETE FROM {table} WHERE {matched}"),
            &[(&c.deleted, keys)],
        )?;
        let columns = names(&c.new);
        let set: Vec<String> = columns
            .iter()
            .map(|n| format!("{} = ?", ident(n)))
            .collect();
        let sql = format!("UPDATE {table} SET {} WHERE {matched}", set.join(", "));
        run(&sql, &[(&c.new, &columns), (&c.old, keys)])?;
        let added = names(&c.added);
        let quoted: Vec<String> = added.iter().map(|n| ident(n)).collect();
        let marks = vec!["?"; added.len()].join(", ");
        let sql = format!(
            "INSERT INTO {table} ({}) VALUES ({marks})",
            quoted.join(", ")
        );
        run(&sql, &[(&c.added, &added)])?;
        tx.commit().map_err(e)
    }
}
