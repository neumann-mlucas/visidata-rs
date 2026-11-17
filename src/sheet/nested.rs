//! Nested data (S-22): `(` expands a struct column into a column per field,
//! `)` folds them back.

use std::collections::HashSet;

use super::*;

impl Sheet {
    /// `(` / `z(`: a column per field of struct `column`, `depth` levels down
    /// (0: all the way),
    /// right after it, named `column.field`; `column` is hidden. One undo step.
    /// Already expanded: its columns show again.
    pub fn expand(&mut self, column: &str, depth: usize) -> Result<(), String> {
        let snap = self.snapshot();
        self.expand_one(column, depth)?;
        self.push_undo(snap);
        Ok(())
    }

    /// `g(` `gz(`: every visible struct column. The number expanded.
    pub fn expand_all(&mut self, depth: usize) -> Result<usize, String> {
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
            if let Err(e) = self.expand_one(c, depth) {
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
            if depth == 0 { usize::MAX } else { depth },
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
        let kids: Vec<String> = fields.iter().map(|(_, n)| n.clone()).collect();
        let step = PlanStep::Expand {
            column: column.into(),
            fields,
            after,
        };
        self.plan = step.apply(self.plan.clone()).map_err(|e| e.to_string())?;
        self.state.steps.push(step);
        self.view.hidden.insert(column.into());
        self.invalidate(Reason::Rows);
        // VisiData puts them right after the struct
        self.place_cols(&kids, Some(column))
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

    /// `:type json`: decode `column`'s JSON text into lists / structs, typed
    /// by every value. One undo step; unchanged on error.
    pub fn json_decode(&mut self, column: &str) -> Result<(), String> {
        let dtype = self.json_dtype(column)?;
        self.json_decode_as(column, dtype)
    }

    /// [`Self::json_decode`]'s read (S-33): the type every value fits.
    pub fn json_dtype(&self, column: &str) -> Result<DataType, String> {
        let err = |e: PolarsError| format!("json: {e}");
        let text = self
            .plan
            .clone()
            .select([col(column)])
            .collect()
            .map_err(err)?;
        let text = text.column(column).map_err(err)?;
        let ca = text
            .str()
            .map_err(|_| format!("json: {column} is not text"))?;
        Ok(ca.json_decode(None, None).map_err(err)?.dtype().clone())
    }

    /// [`Self::json_decode`]'s write: `column` decoded as `dtype`.
    pub fn json_decode_as(&mut self, column: &str, dtype: DataType) -> Result<(), String> {
        let err = |e: PolarsError| format!("json: {e}");
        let step = PlanStep::Json {
            column: column.into(),
            dtype,
        };
        let plan = step.apply(self.plan.clone()).map_err(err)?;
        self.checkpoint();
        self.plan = plan;
        self.state.steps.push(step);
        self.invalidate(Reason::Rows);
        Ok(())
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

/// How `zM` takes a column apart: lists by position, structs by field
/// (values in one column, so text unless the fields share a type), anything
/// else as a one-item list — VisiData's `unfurl-col`.
pub enum Unfurl {
    List,
    /// `text`: the fields' types differ, so values are cast to text.
    Struct {
        fields: Vec<String>,
        text: bool,
    },
    Scalar,
}

/// Row number of the unfurled row's source row, while unfurling.
pub const UNFURL_ROW: &str = "__vdrs_u";
/// The source row's list was empty (`unfurl_empty`).
const UNFURL_EMPTY: &str = "__vdrs_uempty";

pub fn unfurl_kind(dt: &DataType) -> Result<Unfurl, String> {
    Ok(match dt {
        DataType::List(_) => Unfurl::List,
        DataType::Struct(fs) if !fs.is_empty() => {
            let text = fs.iter().any(|f| f.dtype() != fs[0].dtype());
            if text && fs.iter().any(|f| f.dtype().is_nested()) {
                return Err("unfurl: struct fields of different nested types".into());
            }
            Unfurl::Struct {
                fields: fs.iter().map(|f| f.name.to_string()).collect(),
                text,
            }
        }
        _ => Unfurl::Scalar,
    })
}

impl Sheet {
    /// `zM`: `parent`'s rows, one per item of `column`, which becomes
    /// `column_key` (position or field) and `column_value`. Empty lists drop
    /// their row, or with `empty` (`unfurl_empty`) keep it with both empty; a
    /// null keeps it (key 0), as VisiData.
    pub fn unfurl_from(
        parent: &Sheet,
        id: SheetId,
        column: &str,
        empty: bool,
    ) -> Result<Self, String> {
        let lf = parent.view_plan();
        let schema = lf.clone().collect_schema().map_err(|e| e.to_string())?;
        let dt = schema
            .get(column)
            .ok_or_else(|| format!("no column: {column}"))?;
        let (key, val) = (format!("{column}_key"), format!("{column}_value"));
        let c = || col(column);
        let kind = unfurl_kind(dt)?;
        let is_struct = matches!(kind, Unfurl::Struct { .. });
        let is_list = matches!(kind, Unfurl::List);
        let items = match kind {
            Unfurl::List => c(),
            Unfurl::Struct { fields, text } => concat_list(
                fields
                    .iter()
                    .map(|f| {
                        let v = c().struct_().field_by_name(f);
                        let v = if text { v.cast(DataType::String) } else { v };
                        as_struct(vec![lit(f.as_str()).alias("k"), v.alias("v")])
                    })
                    .collect::<Vec<_>>(),
            )
            .map_err(|e| e.to_string())?,
            Unfurl::Scalar => concat_list([c()]).map_err(|e| e.to_string())?,
        };
        let (k, v) = if is_struct {
            (
                c().struct_().field_by_name("k"),
                c().struct_().field_by_name("v"),
            )
        } else {
            let i = int_range(lit(0), len(), 1, DataType::Int64)
                .over([col(UNFURL_ROW)])
                .map_err(|e| e.to_string())?;
            let i = match empty {
                true => when(col(UNFURL_EMPTY)).then(lit(NULL)).otherwise(i),
                false => i,
            };
            (i, c())
        };
        let names: Vec<Expr> = schema
            .iter_names()
            .flat_map(|n| match n.as_str() == column {
                true => vec![k.clone().alias(key.as_str()), v.clone().alias(val.as_str())],
                false => vec![col(n.as_str())],
            })
            .collect();
        let lf = lf.with_row_index(UNFURL_ROW, None);
        let lf = match (empty, is_list) {
            (true, true) => lf.with_column(c().list().len().eq(lit(0)).alias(UNFURL_EMPTY)),
            _ => lf.with_column(lit(false).alias(UNFURL_EMPTY)),
        };
        let plan = lf
            .with_column(items.alias(column))
            .explode(
                cols([column]),
                ExplodeOptions {
                    empty_as_null: empty,
                    keep_nulls: true,
                },
            )
            .select(names);
        Ok(Sheet::child(
            parent,
            id,
            format!("unfurl({column}): {}", parent.name),
            plan,
            Origin::Unfurl { parent: parent.id },
        )
        .derived(
            parent,
            Derivation::Unfurl {
                column: column.into(),
                empty,
            },
        ))
    }
}

impl Sheet {
    /// `zEnter` (VisiData open-cell): cell `v` (a list, a struct, or text
    /// holding a JSON array or object) as a sheet named `name`. A list of
    /// objects is a table; a list of other values one column named `name`; an
    /// object `key`/`value` rows keyed on `key`. Values of mixed types become
    /// text (JSON for the nested ones), which `zEnter` reads again.
    pub fn open_cell(id: SheetId, name: String, v: &AnyValue) -> Result<Sheet, String> {
        Self::open_json(id, name, cell_json(v))
    }

    /// `<CR>` on a `zEnter` sheet (VisiData open-row there): the cursor row's
    /// visible cells as `key`/`value` rows, named `name[rowkey]`.
    pub fn open_cell_row(&self, id: SheetId, row: u64) -> Result<Sheet, String> {
        let df = (self.view_plan().slice(row as i64, 1).collect()).map_err(|e| e.to_string())?;
        let mut obj = serde_json::Map::new();
        for c in df.columns() {
            let v = c.get(0).map_err(|e| e.to_string())?;
            let j = match &v {
                AnyValue::Null => serde_json::Value::Null,
                v if v.get_str().is_some() => serde_json::Value::String(any_text(v)),
                v => cell_json(v).unwrap_or_else(|| serde_json::Value::String(any_text(v))),
            };
            obj.insert(c.name().to_string(), j);
        }
        Self::open_json(
            id,
            format!("{}[{}]", self.name, self.row_key(&df, 0, row)),
            Some(serde_json::Value::Object(obj)),
        )
    }

    /// VisiData's rowkey for view row `row`, which is row `i` of `df`: its
    /// key cells joined with `,`, or `row` without keys.
    pub fn row_key(&self, df: &DataFrame, i: usize, row: u64) -> String {
        if self.view.keys.is_empty() {
            return row.to_string();
        }
        let cell = |k: &String| df.column(k).and_then(|c| c.get(i)).ok();
        (self.view.keys.iter())
            .map(|k| cell(k).map_or(String::new(), |v| any_text(&v)))
            .collect::<Vec<_>>()
            .join(",")
    }

    fn open_json(
        id: SheetId,
        name: String,
        parsed: Option<serde_json::Value>,
    ) -> Result<Sheet, String> {
        use serde_json::Value as J;
        let fail = || "cannot dive deeper on this object".to_string();
        let as_text = |v: J| match v {
            J::String(t) => J::String(t),
            J::Null => J::Null,
            v => J::String(v.to_string()),
        };
        let rows = |pairs: Vec<serde_json::Map<String, J>>| -> Result<DataFrame, String> {
            let objs = pairs.iter().cloned().map(J::Object).collect();
            match crate::reader::records(objs) {
                Ok(df) => Ok(df),
                Err(_) => {
                    let text = pairs
                        .into_iter()
                        .map(|m| J::Object(m.into_iter().map(|(k, v)| (k, as_text(v))).collect()));
                    crate::reader::records(text.collect()).map_err(|e| e.to_string())
                }
            }
        };
        let (df, keys) = match parsed.ok_or_else(fail)? {
            J::Array(items) => {
                // a list of objects is rows; anything else, one column
                let objects = !items.is_empty() && items.iter().all(J::is_object);
                let row = |v: J| match v {
                    J::Object(m) if objects => m,
                    v => [(name.clone(), v)].into_iter().collect(),
                };
                (rows(items.into_iter().map(row).collect())?, vec![])
            }
            J::Object(m) => {
                let kv = m.into_iter().map(|(k, v)| {
                    [("key".to_string(), J::String(k)), ("value".to_string(), v)]
                        .into_iter()
                        .collect()
                });
                (rows(kv.collect())?, vec!["key".to_string()])
            }
            _ => return Err(fail()),
        };
        let src = crate::reader::LoadedSource::from_df(SourceKind::Json, name.clone(), None, df);
        let mut sheet = Sheet::from_source(id, src);
        sheet.name = name;
        sheet.view.keys = keys;
        sheet.origin = Origin::Cell;
        Ok(sheet)
    }
}

/// A list or struct cell as JSON, or text holding a JSON array or object;
/// `None` for anything else. Dates and the like become JSON text, so a row of
/// them reads back as text.
fn cell_json(v: &AnyValue) -> Option<serde_json::Value> {
    match v.get_str() {
        Some(t) => serde_json::from_str(t).ok(),
        None => Some(any_json(v)),
    }
}

/// `v` as JSON; a value JSON can't hold (NaN, inf, a date) as its text.
fn any_json(v: &AnyValue) -> serde_json::Value {
    use serde_json::Value as J;
    let obj = |fields: &[Field], vals: &[AnyValue]| {
        J::Object(
            (fields.iter().zip(vals))
                .map(|(f, v)| (f.name.to_string(), any_json(v)))
                .collect(),
        )
    };
    match v {
        AnyValue::List(s) | AnyValue::Array(s, _) => {
            J::Array(s.iter().map(|v| any_json(&v)).collect())
        }
        AnyValue::Struct(_, _, fields) => obj(fields, &v._iter_struct_av().collect::<Vec<_>>()),
        AnyValue::StructOwned(p) => obj(&p.1, &p.0),
        v => {
            let text = crate::sheet::write::py_json(v);
            serde_json::from_str(&text).unwrap_or(J::String(text))
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

    /// A float list holding NaN or inf still opens: JSON can't hold them, so
    /// they read as text.
    #[test]
    fn open_cell_takes_non_finite_floats() {
        let v = AnyValue::List(Series::new("".into(), [1.5, f64::NAN, f64::INFINITY]));
        let s = Sheet::open_cell(1, "t".into(), &v).unwrap();
        let df = s.plan.clone().collect().unwrap();
        let t: Vec<_> = df.column("t").unwrap().str().unwrap().iter().collect();
        assert_eq!(t, [Some("1.5"), Some("NaN"), Some("Infinity")]);
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
        // an expanded column takes edits (they go after the expansion)
        s.edit_cell("user.name_2", 0, "zed").unwrap();
        let df = s.view_plan().collect().unwrap();
        let name = df.column("user.name_2").unwrap().get(0).unwrap();
        assert_eq!(name, AnyValue::String("zed"));
        assert!(s.contract("n").is_err());
        assert!(s.expand("n", 1).unwrap_err().contains("not a struct"));
    }

    fn rows(s: &Sheet) -> Vec<String> {
        let df = s.view_plan().collect().unwrap();
        (0..df.height())
            .map(|i| {
                let r: Vec<String> = df
                    .columns()
                    .iter()
                    .map(|c| any_text(&c.get(i).unwrap()))
                    .collect();
                r.join("|")
            })
            .collect()
    }

    #[test]
    fn unfurl_lists_structs_and_scalars_as_visidata() {
        let t = tempfile::Builder::new()
            .suffix(".jsonl")
            .tempfile()
            .unwrap();
        let lines = [
            r#"{"id":1,"tags":["a","b"],"m":{"x":1,"y":"q"},"deep":[[1,2],[3]]}"#,
            r#"{"id":2,"tags":[],"m":{"x":2,"y":"r"},"deep":[]}"#,
            r#"{"id":3,"tags":null,"m":null,"deep":null}"#,
            r#"{"id":4,"tags":["c"],"m":{"x":4,"y":"s"},"deep":[[4]]}"#,
        ];
        std::fs::write(t.path(), lines.join("\n")).unwrap();
        let loaded = crate::reader::open(t.path().as_os_str(), None).unwrap();
        let mut s = Sheet::from_source(0, loaded);
        s.view.hidden.extend(["m".to_string(), "deep".to_string()]);
        let u = Sheet::unfurl_from(&s, 1, "tags", false).unwrap();
        assert_eq!(u.name, format!("unfurl(tags): {}", s.name));
        assert_eq!(names(&u), ["id", "tags_key", "tags_value"]);
        // `[]` drops its row, null keeps it as item 0
        assert_eq!(rows(&u), ["1|0|a", "1|1|b", "3|0|null", "4|0|c"]);
        assert!(u.editable().is_ok());
        s.view.hidden.clear();
        s.view
            .hidden
            .extend(["tags".to_string(), "deep".to_string()]);
        let u = Sheet::unfurl_from(&s, 1, "m", false).unwrap();
        // fields of different types: values as text
        assert_eq!(rows(&u)[..2], ["1|x|1".to_string(), "1|y|q".to_string()]);
        s.view.hidden.clear();
        s.view.hidden.extend(["tags".to_string(), "m".to_string()]);
        let u = Sheet::unfurl_from(&s, 1, "deep", false).unwrap();
        assert_eq!(
            rows(&u)[..2],
            ["1|0|[1, 2]".to_string(), "1|1|[3]".to_string()]
        );
        let u = Sheet::unfurl_from(&s, 1, "id", false).unwrap(); // a scalar is one item
        assert_eq!(rows(&u)[0], "0|1|[[1, 2], [3]]");
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
        assert_eq!(s.expand_all(0).unwrap(), 1);
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
        assert_eq!(s.expand_all(1).unwrap(), 1);
        assert_eq!(
            names(&s),
            ["id", "user.name_2", "user.geo", "user.name", "n"]
        );
    }
}
