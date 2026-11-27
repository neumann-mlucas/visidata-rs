//! `:lookup` (S-36 A): columns of another sheet matched by a key, as
//! [`PlanStep::Lookup`] steps.

use super::*;

/// A lookup key as both sides compare it: text, a whole number without its
/// `.0`, so an int `1` and a float `1.0` match (Excel compares numbers by
/// value). The exports repeat [`LOOKUP_KEY_RE`].
pub(crate) fn lookup_key(key: Expr) -> Expr {
    key.cast(DataType::String)
        .str()
        .replace(lit(LOOKUP_KEY_RE), lit("${1}"), false)
}

/// A whole number written as a float: `1.0`, `-3.00`.
pub(crate) const LOOKUP_KEY_RE: &str = r"^(-?\d+)\.0+$";

impl Sheet {
    /// `column` of this sheet's view by `key`, as a lookup table: `__k` (the
    /// key as text, nulls dropped, first match kept) and `__v`. Also the
    /// number of rows dropped as later matches of a key.
    pub fn lookup_table(&self, key: &str, column: &str) -> Result<(DataFrame, usize), String> {
        let rows = self
            .view_plan()
            .select([lookup_key(col(key)).alias("__k"), col(column).alias("__v")])
            .filter(col("__k").is_not_null())
            .collect()
            .map_err(|e| e.to_string())?;
        let table = rows
            .unique_stable(Some(&["__k".into()]), UniqueKeepStrategy::First, None)
            .map_err(|e| e.to_string())?;
        let dups = rows.height() - table.height();
        Ok((table, dups))
    }

    /// What a live lookup of this sheet compares to see a change: its view
    /// and its undo state (a cell edit moves it).
    pub fn lookup_stamp(&self) -> String {
        format!("{}#{}", self.plan_key(), self.state.undo.current())
    }

    /// The sheets this one's live lookups read.
    pub fn lookup_sources(&self) -> impl Iterator<Item = SheetId> + '_ {
        self.state.steps.iter().filter_map(|s| match s {
            PlanStep::Lookup { link: Some(l), .. } => Some(l.sheet),
            _ => None,
        })
    }

    /// Add `steps` (all [`PlanStep::Lookup`]) as one undo state. A step
    /// replaces an earlier lookup of the same name; another column of that
    /// name is refused. The names that are new columns.
    pub fn add_lookups(&mut self, steps: Vec<PlanStep>) -> Result<Vec<String>, String> {
        self.make_editable()?;
        let mut new = Vec::new();
        for s in &steps {
            let Some(name) = s.writes() else { continue };
            if name == ROW_ID {
                return Err(format!("{ROW_ID} is reserved"));
            }
            if self.lookup_at(name).is_none() {
                if self.has_column(name) || new.iter().any(|n| n == name) {
                    return Err(format!("{name} is already a column (use `as` to rename)"));
                }
                new.push(name.to_string());
            }
        }
        let before = self.state.steps.clone();
        self.checkpoint();
        self.put_lookups(steps);
        if let Err(e) = self.rebuild_plan() {
            self.state.steps = before;
            let _ = self.rebuild_plan();
            return Err(e.to_string());
        }
        self.invalidate(Reason::Rows);
        Ok(new)
    }

    /// Fresh tables for live lookups (A4): not an undo state.
    pub fn refresh_lookups(&mut self, steps: Vec<PlanStep>) -> PolarsResult<()> {
        let steps: Vec<PlanStep> = steps
            .into_iter()
            .filter(|s| s.writes().is_some_and(|n| self.lookup_at(n).is_some()))
            .collect();
        if steps.is_empty() {
            return Ok(());
        }
        self.put_lookups(steps);
        self.rebuild_plan()?;
        self.invalidate(Reason::Rows);
        Ok(())
    }

    /// Lookups whose source sheet is gone keep their last table; their names.
    pub fn freeze_lookups(&mut self, gone: &[SheetId]) -> Vec<String> {
        let mut names = Vec::new();
        for s in &mut self.state.steps {
            if let PlanStep::Lookup { name, link, .. } = s
                && link.as_ref().is_some_and(|l| gone.contains(&l.sheet))
            {
                *link = None;
                names.push(name.clone());
            }
        }
        names
    }

    fn lookup_at(&self, name: &str) -> Option<usize> {
        self.state
            .steps
            .iter()
            .position(|s| matches!(s, PlanStep::Lookup { name: n, .. } if n == name))
    }

    fn put_lookups(&mut self, steps: Vec<PlanStep>) {
        for s in steps {
            match s.writes().and_then(|n| self.lookup_at(n)) {
                Some(i) => self.state.steps[i] = s,
                None => self.state.steps.push(s),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(df: DataFrame) -> Sheet {
        let src = LoadedSource::from_df(SourceKind::Csv, "t".into(), None, df);
        Sheet::from_source(0, src)
    }

    /// First match wins, no match and a null key give null, the value keeps
    /// their type, a re-run replaces the column, a taken name is refused.
    #[test]
    fn lookup_keeps_first_match_and_their_type() {
        let theirs = sheet(
            df!("id" => [Some(1i64), Some(2), Some(1), None], "v" => [10.5, 20.0, 30.0, 40.0])
                .unwrap(),
        );
        let (table, dups) = theirs.lookup_table("id", "v").unwrap();
        assert_eq!(dups, 1);
        let step = |name: &str| PlanStep::Lookup {
            name: name.into(),
            key: "k".into(),
            table: Arc::new(table.clone()),
            link: None,
        };
        let mut mine = sheet(df!("k" => [Some("2"), Some("1"), Some("3"), None]).unwrap());
        assert_eq!(mine.add_lookups(vec![step("v")]).unwrap(), ["v"]);
        let out = mine.plan.clone().collect().unwrap();
        let v: Vec<Option<f64>> = out.column("v").unwrap().f64().unwrap().iter().collect();
        assert_eq!(v, [Some(20.0), Some(10.5), None, None]);
        assert!(mine.add_lookups(vec![step("v")]).unwrap().is_empty());
        assert!(mine.add_lookups(vec![step("k")]).is_err());
    }
}
