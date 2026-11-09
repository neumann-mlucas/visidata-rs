//! Nested data (S-22): `(` expands a struct column into a column per field,
//! `)` folds them back.

use std::collections::HashSet;

use super::*;

impl Sheet {
    /// `(` / `z(`: a column per field of struct `column`, `depth` levels down,
    /// right after it, named `column.field`; `column` is hidden. One undo step.
    /// Already expanded: its columns show again.
    pub fn expand(&mut self, column: &str, depth: usize) -> Result<(), String> {
        let snap = self.snapshot();
        self.expand_one(column, depth)?;
        self.push_undo(snap);
        Ok(())
    }

    /// `g(`: every visible struct column, one level. The number expanded.
    pub fn expand_all(&mut self) -> Result<usize, String> {
        let schema = self
            .view_plan()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        let structs: Vec<String> = schema
            .iter()
            .filter(|(_, d)| matches!(d, DataType::Struct(_)))
            .map(|(n, _)| n.to_string())
            .collect();
        let snap = self.snapshot();
        for c in &structs {
            if let Err(e) = self.expand_one(c, 1) {
                self.restore(snap);
                return Err(e);
            }
        }
        if !structs.is_empty() {
            self.push_undo(snap);
        }
        Ok(structs.len())
    }

    fn expand_one(&mut self, column: &str, depth: usize) -> Result<(), String> {
        if let Some(kids) = self.expanded(column) {
            let v = &mut self.view;
            v.hidden.insert(column.into());
            for k in kids {
                v.hidden.remove(&k);
            }
            return Ok(());
        }
        let schema = self
            .plan
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?;
        let dt = schema
            .get(column)
            .ok_or_else(|| format!("no column: {column}"))?;
        if !matches!(dt, DataType::Struct(f) if !f.is_empty()) {
            return Err(format!("{column} is not a struct"));
        }
        let mut taken: HashSet<String> = schema.iter_names().map(|n| n.to_string()).collect();
        let mut fields = Vec::new();
        field_columns(
            dt,
            column,
            &mut Vec::new(),
            depth.max(1),
            &mut taken,
            &mut fields,
        );
        let after = schema
            .iter_names()
            .skip_while(|n| n.as_str() != column)
            .skip(1)
            .filter(|n| n.as_str() != ROW_ID)
            .map(|n| n.to_string())
            .collect();
        let step = PlanStep::Expand {
            column: column.into(),
            fields,
            after,
        };
        self.plan = step.apply(self.plan.clone()).map_err(|e| e.to_string())?;
        self.state.steps.push(step);
        self.view.hidden.insert(column.into());
        self.invalidate(Reason::Rows);
        Ok(())
    }

    /// `)`: on `column` or a column expanded from the same struct, hide
    /// those columns and show the struct again. Its name, for the cursor.
    pub fn contract(&mut self, column: &str) -> Result<String, String> {
        // a column made by an expansion folds into its struct; else a struct
        // shown again (`gv`) folds its own columns
        let made_from = |own: bool| {
            self.state.steps.iter().rev().find_map(|s| match s {
                PlanStep::Expand {
                    column: c, fields, ..
                } if (own && c == column) || (!own && fields.iter().any(|(_, n)| n == column)) => {
                    Some(c.clone())
                }
                _ => None,
            })
        };
        let parent = made_from(false)
            .or_else(|| made_from(true))
            .ok_or_else(|| format!("{column} is not an expanded column"))?;
        let kids = self.expanded(&parent).unwrap_or_default();
        self.checkpoint();
        let v = &mut self.view;
        v.hidden.remove(&parent);
        v.hidden.extend(kids);
        Ok(parent)
    }

    /// The columns `column` was last expanded into.
    fn expanded(&self, column: &str) -> Option<Vec<String>> {
        self.state.steps.iter().rev().find_map(|s| match s {
            PlanStep::Expand {
                column: c, fields, ..
            } if c == column => Some(fields.iter().map(|(_, n)| n.clone()).collect()),
            _ => None,
        })
    }

    fn restore(&mut self, snap: Snapshot) {
        self.plan = snap.plan;
        self.view = snap.view;
        self.state.steps = snap.steps;
        self.invalidate(Reason::Rows);
    }
}

/// `name`'s columns: one per field, structs `depth` levels down; a name
/// already taken gets `_2`, `_3`, … (VisiData names, S-22 §2).
fn field_columns(
    dt: &DataType,
    name: &str,
    path: &mut Vec<String>,
    depth: usize,
    taken: &mut HashSet<String>,
    out: &mut Vec<(Vec<String>, String)>,
) {
    match dt {
        DataType::Struct(fs) if depth > 0 && !fs.is_empty() => {
            for f in fs {
                path.push(f.name.to_string());
                let n = format!("{name}.{}", f.name);
                field_columns(f.dtype(), &n, path, depth - 1, taken, out);
                path.pop();
            }
        }
        _ => {
            let mut n = name.to_string();
            let mut i = 2;
            while !taken.insert(n.clone()) {
                n = format!("{name}_{i}");
                i += 1;
            }
            out.push((path.clone(), n));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nested() -> (Sheet, tempfile::NamedTempFile) {
        let t = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        let rows = [
            r#"{"id":1,"user":{"name":"ann","geo":{"lat":1.5,"lon":2}},"user.name":"x","n":7}"#,
            r#"{"id":2,"user":{"name":"bob","geo":{"lat":3.0,"lon":4}},"user.name":"y","n":8}"#,
            r#"{"id":3,"user":null,"user.name":"z","n":9}"#,
        ];
        std::fs::write(t.path(), rows.join("\n")).unwrap();
        let loaded = crate::reader::open(t.path().as_os_str(), None).unwrap();
        (Sheet::from_source(0, loaded), t)
    }

    fn names(s: &Sheet) -> Vec<String> {
        let sc = s.view_plan().collect_schema().unwrap();
        sc.iter_names().map(|n| n.to_string()).collect()
    }

    #[test]
    fn expand_names_fields_after_the_struct_and_dodges_clashes() {
        let (mut s, _t) = nested();
        s.expand("user", 1).unwrap();
        assert_eq!(
            names(&s),
            ["id", "user.name_2", "user.geo", "user.name", "n"]
        );
        s.expand("user.geo", 1).unwrap();
        assert_eq!(
            names(&s),
            [
                "id",
                "user.name_2",
                "user.geo.lat",
                "user.geo.lon",
                "user.name",
                "n"
            ]
        );
        let df = s.view_plan().collect().unwrap();
        let lat: Vec<Option<f64>> = df
            .column("user.geo.lat")
            .unwrap()
            .f64()
            .unwrap()
            .iter()
            .collect();
        assert_eq!(lat, [Some(1.5), Some(3.0), None]); // null struct: null fields
        // `)` folds back the innermost, then the outer struct
        assert_eq!(s.contract("user.geo.lon").unwrap(), "user.geo");
        assert_eq!(
            names(&s),
            ["id", "user.name_2", "user.geo", "user.name", "n"]
        );
        assert_eq!(s.contract("user.geo").unwrap(), "user");
        assert_eq!(names(&s), ["id", "user", "user.name", "n"]);
        // `(` again shows the same columns: no `_2` copies
        s.expand("user", 1).unwrap();
        assert_eq!(
            names(&s),
            ["id", "user.name_2", "user.geo", "user.name", "n"]
        );
        assert!(s.contract("n").is_err());
        assert!(s.expand("n", 1).unwrap_err().contains("not a struct"));
    }

    #[test]
    fn expand_depth_goes_down_and_undoes_in_one_step() {
        let (mut s, _t) = nested();
        let before = names(&s);
        s.expand("user", 2).unwrap();
        assert_eq!(
            names(&s),
            [
                "id",
                "user.name_2",
                "user.geo.lat",
                "user.geo.lon",
                "user.name",
                "n"
            ]
        );
        assert!(s.time_travel(true));
        assert_eq!(names(&s), before);
        assert_eq!(s.expand_all().unwrap(), 1);
        assert_eq!(
            names(&s),
            ["id", "user.name_2", "user.geo", "user.name", "n"]
        );
    }
}
