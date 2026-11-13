//! Meta sheets: the sheet list, notes, search hits, a file's parts.

use super::*;

impl Sheet {
    /// Build a tiny in-memory sheet listing every sheet in the stack.
    #[expect(clippy::expect_used, reason = "columns built here have one length")]
    pub fn sheet_switcher(id: SheetId, summaries: Vec<(SheetId, String, SourceKind)>) -> Self {
        let ids: Vec<i64> = summaries.iter().map(|(i, _, _)| *i as i64).collect();
        let names: Vec<String> = summaries.iter().map(|(_, n, _)| n.clone()).collect();
        let kinds: Vec<String> = summaries.iter().map(|(_, _, k)| format!("{k:?}")).collect();
        let df = DataFrame::new_infer_height(vec![
            Column::new("id".into(), ids),
            Column::new("name".into(), names),
            Column::new("kind".into(), kinds),
        ])
        .expect("sheet-switcher dataframe");
        Self::meta(id, "<sheets>".into(), df, Origin::SheetSwitcher)
    }

    /// An in-memory meta-sheet (sheet list, columns, undo tree) showing `df`.
    /// Its row ids are positions (the view numbers its rows), known now, so
    /// row keys work before the first materialize lands.
    pub(super) fn meta(id: SheetId, name: String, df: DataFrame, origin: Origin) -> Self {
        let window_rids = (0..df.height() as u64).collect();
        Sheet {
            id,
            name,
            kind: SourceKind::Csv, // placeholder; this is a meta-sheet
            source: Arc::new(LoadedSource::empty()),
            plan: df.clone().lazy(),
            view: View::default(),
            window: Some(df),
            revision: 0,
            state: SheetState {
                window_rids,
                ..Default::default()
            },
            origin,
        }
    }

    /// A sheet of text columns `names`, one `rows` entry per row.
    pub fn listing(id: SheetId, name: &str, names: &[&str], rows: &[Vec<String>]) -> Self {
        let cols = names.iter().enumerate().map(|(i, n)| {
            let v: Vec<String> = rows
                .iter()
                .map(|r| r.get(i).cloned().unwrap_or_default())
                .collect();
            Column::new((*n).into(), v)
        });
        let df = DataFrame::new_infer_height(cols.collect()).unwrap_or_default();
        Self::meta(id, name.into(), df, Origin::Listing)
    }

    /// `:notes` (S-30 B): a sheet of the notes on `parent`'s cells, `row` = its
    /// view position now (empty: filtered out or gone); `Enter` jumps, as `:hits`.
    pub fn notes_from(
        parent: &Sheet,
        id: SheetId,
        notes: &crate::notes::Notes,
    ) -> Result<Self, String> {
        use crate::notes::Row;
        let e = |e: PolarsError| e.to_string();
        let view = parent.view_plan_rid(true).collect().map_err(e)?;
        let rids = rid_values(&view).map_err(e)?;
        let keys = &parent.view.keys;
        let rows: Vec<Vec<String>> = match keys.is_empty() {
            true => Vec::new(),
            false => (0..view.height())
                .map(|r| key_text(&view, keys, r))
                .collect(),
        };
        let at = |row: &Row| {
            match row {
                Row::Id(r) => rids.iter().position(|x| x == r),
                Row::Keys(k) => rows.iter().position(|v| v == k),
            }
            .map(|p| p as u64 + 1)
        };
        let ns = &notes.notes;
        let df = DataFrame::new_infer_height(vec![
            Column::new(
                "row".into(),
                ns.iter().map(|n| at(&n.row)).collect::<Vec<_>>(),
            ),
            Column::new(
                "key".into(),
                ns.iter()
                    .map(|n| match &n.row {
                        Row::Id(r) => format!("id {r}"),
                        Row::Keys(k) => k.join(" · "),
                    })
                    .collect::<Vec<_>>(),
            ),
            Column::new(
                "column".into(),
                ns.iter().map(|n| n.column.clone()).collect::<Vec<_>>(),
            ),
            Column::new(
                "note".into(),
                ns.iter().map(|n| n.text.clone()).collect::<Vec<_>>(),
            ),
            Column::new(
                "stale?".into(),
                ns.iter().map(|n| notes.stale(n)).collect::<Vec<_>>(),
            ),
            Column::new(
                "created".into(),
                ns.iter()
                    .map(|n| n.created as i64 * 1000)
                    .collect::<Vec<_>>(),
            )
            .cast(&DataType::Datetime(TimeUnit::Milliseconds, None))
            .map_err(e)?,
        ])
        .map_err(e)?;
        let name = format!("notes: {}", parent.name);
        Ok(Self::meta(id, name, df, Origin::Hits { parent: parent.id }))
    }

    /// `:hits needle` (S-28 B): `row` (from 1), `column`, `value` and the key
    /// columns' values of each hit of `needle` in `parent`'s view, at most
    /// `cap`. Also whether the cap cut it short.
    pub fn hits_from(
        parent: &Sheet,
        id: SheetId,
        needle: &str,
        cap: usize,
    ) -> Result<(Self, bool), String> {
        let e = |e: PolarsError| e.to_string();
        let mut hits = parent.search_hits(needle, None).map_err(e)?;
        let capped = hits.len() > cap;
        hits.truncate(cap);
        // only the hit rows, in view order: `pos` maps a view row to its line
        let mut rows: Vec<u64> = hits.iter().map(|(r, _)| *r).collect();
        rows.sort_unstable();
        rows.dedup();
        let pos: std::collections::HashMap<u64, usize> =
            rows.iter().enumerate().map(|(i, r)| (*r, i)).collect();
        let at_rows = lit(Series::new("rows".into(), &rows)).implode(false);
        let view = parent
            .view_plan()
            .with_row_index("__vdrs_hitrow", None)
            .filter(
                col("__vdrs_hitrow")
                    .cast(DataType::UInt64)
                    .is_in(at_rows, false),
            )
            .drop(cols(["__vdrs_hitrow"]))
            .collect()
            .map_err(e)?;
        let names: Vec<String> = view
            .get_column_names()
            .iter()
            .map(|n| n.to_string())
            .collect();
        let text = |name: &str| -> Result<Series, String> {
            Ok(view
                .column(name)
                .map_err(e)?
                .cast(&DataType::String)
                .map_err(e)?
                .as_materialized_series()
                .clone())
        };
        let texts: Vec<Series> = names.iter().map(|n| text(n)).collect::<Result<_, _>>()?;
        let at = |c: usize, r: u64| {
            texts[c]
                .str()
                .ok()
                .and_then(|s| s.get(pos[&r]))
                .map(String::from)
        };
        let mut cols = vec![
            Column::new(
                "row".into(),
                hits.iter().map(|(r, _)| r + 1).collect::<Vec<u64>>(),
            ),
            Column::new(
                "column".into(),
                hits.iter()
                    .map(|(_, c)| names[*c].clone())
                    .collect::<Vec<_>>(),
            ),
            Column::new(
                "value".into(),
                hits.iter().map(|(r, c)| at(*c, *r)).collect::<Vec<_>>(),
            ),
        ];
        for k in &parent.view.keys {
            let Some(i) = names.iter().position(|n| n == k) else {
                continue;
            };
            if matches!(k.as_str(), "row" | "column" | "value") {
                continue;
            }
            cols.push(Column::new(
                k.as_str().into(),
                hits.iter().map(|(r, _)| at(i, *r)).collect::<Vec<_>>(),
            ));
        }
        let df = DataFrame::new_infer_height(cols).map_err(e)?;
        let name = format!("hits({needle}): {}", parent.name);
        Ok((
            Self::meta(id, name, df, Origin::Hits { parent: parent.id }),
            capped,
        ))
    }

    /// Table this sheet saves as in a database file, as VisiData names it:
    /// `data3.tsv` → `data3`, a table opened from `app.duckdb` → its name.
    pub fn table_name(&self) -> String {
        let part = self.source.path.as_ref().and_then(|p| {
            let shown = format!("{}:", p.display());
            self.source
                .display_name
                .strip_prefix(&shown)
                .map(String::from)
        });
        match part {
            Some(part) if matches!(self.origin, Origin::File) => {
                part.rsplit('.').next().unwrap_or(&part).to_string()
            }
            _ => std::path::Path::new(&self.name)
                .file_stem()
                .map_or_else(|| self.name.clone(), |s| s.to_string_lossy().into_owned()),
        }
    }

    /// List sheet for a multi-part xlsx/sqlite file, or a duckdb file's index
    /// (its source already is that list; `parts` is unused).
    pub fn parts_list(id: SheetId, loaded: LoadedSource, parts: Vec<String>) -> Self {
        let path = loaded.path.clone().unwrap_or_default();
        let kind = loaded.kind;
        let what = if kind == SourceKind::Xlsx {
            "sheet"
        } else {
            "table"
        };
        let plan = if kind == SourceKind::Duckdb {
            loaded.lazy.clone()
        } else {
            Column::new(what.into(), parts).into_frame().lazy()
        };
        Sheet {
            id,
            name: format!("{}: {what}s", loaded.display_name),
            kind,
            source: Arc::new(loaded),
            plan,
            view: View::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin: Origin::Parts { path, kind },
        }
    }
}
