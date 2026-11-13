//! Edits: cells, rows added, pasted and deleted, `:setcol`, `:setexpr`, `:s`.

use super::*;

impl Sheet {
    /// Sheets read from a source take edits; derived ones (freq, pivot,
    /// join, `:sql`, selected rows, …) don't (S-18 D3).
    pub fn editable(&self) -> Result<(), String> {
        if matches!(self.origin, Origin::File | Origin::Tail { .. }) {
            return Ok(());
        }
        Err(match &self.state.lineage {
            Some(l) => format!("read-only: {} is derived from {}", self.name, l.parent.name),
            None => format!("read-only: {} is derived", self.name),
        })
    }

    /// `text` typed into `column`, as stored (None for empty = null), and the
    /// column's base type. It must fit the column as displayed (after
    /// `# % ~ @`) and its base type (S-18 §2.4).
    pub fn edit_value(
        &self,
        column: &str,
        text: &str,
    ) -> Result<(Option<String>, DataType), String> {
        if text.is_empty() {
            return Ok((None, self.base_type(column)?));
        }
        let base = self
            .edit_values(column, &[Some(text.to_string())])?
            .dtype()
            .clone();
        let text = match base {
            DataType::Boolean => text.to_ascii_lowercase(),
            _ => text.to_string(),
        };
        Ok((Some(text), base))
    }

    pub(super) fn base_type(&self, column: &str) -> Result<DataType, String> {
        self.plan
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?
            .get(column)
            .cloned()
            .ok_or_else(|| format!("no column: {column}"))
    }

    /// `edit_value` for many texts at once (`gzp`): the values in the column's
    /// base type, or the first text that doesn't fit. None = null.
    pub fn edit_values(&self, column: &str, texts: &[Option<String>]) -> Result<Series, String> {
        let base = self.base_type(column)?;
        let refuse = |i: usize, dtype: &dyn std::fmt::Display| {
            let t = texts[i].as_deref().unwrap_or_default();
            format!("{t:?} doesn't fit {column} ({dtype})")
        };
        if base == DataType::Boolean {
            // Polars doesn't cast text to bool
            let vals = texts.iter().enumerate().map(|(i, t)| {
                match t.as_deref().map(str::to_ascii_lowercase).as_deref() {
                    None => Ok(None),
                    Some("true") => Ok(Some(true)),
                    Some("false") => Ok(Some(false)),
                    Some(_) => Err(refuse(i, &"bool: true/false")),
                }
            });
            let vals: Vec<Option<bool>> = vals.collect::<Result<_, _>>()?;
            return Ok(Series::new(column.into(), vals));
        }
        let df = DataFrame::new_infer_height(vec![Column::new(column.into(), texts)])
            .map_err(|e| e.to_string())?;
        // The first text a (non-strict) cast turns into null.
        let first_bad = |e: Expr| -> Result<Option<usize>, String> {
            let out = df
                .clone()
                .lazy()
                .select([e.alias("v")])
                .collect()
                .map_err(|e| e.to_string())?;
            let v = out.column("v").map_err(|e| e.to_string())?;
            Ok((0..texts.len())
                .find(|&i| texts[i].is_some() && v.get(i).is_ok_and(|x| x.is_null())))
        };
        let shown = self
            .view
            .type_casts
            .iter()
            .find(|(n, _)| n == column)
            .map(|(_, d)| d);
        let shown_e = match shown {
            Some(DataType::Date) if base == DataType::String => Some(lenient_date(column)),
            Some(dt) => Some(col(column).cast(dt.clone())),
            None => None,
        };
        if let (Some(e), Some(dt)) = (shown_e, shown)
            && let Some(i) = first_bad(e)?
        {
            return Err(refuse(i, dt));
        }
        if let Some(i) = first_bad(col(column).cast(base.clone()))? {
            return Err(refuse(i, &base));
        }
        let out = df
            .lazy()
            .select([col(column).cast(base)])
            .collect()
            .map_err(|e| e.to_string())?;
        Ok(out
            .column(column)
            .map_err(|e| e.to_string())?
            .as_materialized_series()
            .clone())
    }

    /// Selected row ids in view order (sort applied); selected rows the
    /// filter hides follow, in row order.
    pub fn selected_in_view_order(&self) -> PolarsResult<Vec<u64>> {
        let sel: Vec<u64> = self.view.selected.iter().copied().collect();
        let df = self
            .sorted_plan(true)
            .filter(rid_in(&sel))
            .select([col(ROW_ID)])
            .collect()?;
        let mut out = rid_values(&df)?;
        let shown: std::collections::HashSet<u64> = out.iter().copied().collect();
        out.extend(sel.into_iter().filter(|r| !shown.contains(r)));
        Ok(out)
    }

    /// The plan carries [`ROW_ID`] (source sheets, see [`with_rids`]).
    // ponytail: a schema lookup per view plan; Polars caches it, a flag would
    // go stale whenever a caller swaps `plan`.
    pub(super) fn has_rids(&self) -> bool {
        self.plan
            .clone()
            .collect_schema()
            .is_ok_and(|s| s.contains(ROW_ID))
    }

    /// The plan's rows and columns as the user has them (hidden ones too, no
    /// view casts/filter/sort): deleted rows out, no [`ROW_ID`].
    pub fn data_plan(&self) -> LazyFrame {
        if !self.has_rids() {
            return self.plan.clone(); // derived sheets take no deletes
        }
        let deleted = self.deleted_rids();
        let mut lf = self.plan.clone();
        if !deleted.is_empty() {
            lf = lf.filter(rid_in(&deleted).not());
        }
        lf.drop(cols([ROW_ID]))
    }

    /// Added rows' (id, position) pairs, from every `AddRows` step.
    pub(super) fn added_positions(&self) -> Vec<(u64, f64)> {
        let mut out = Vec::new();
        for s in &self.state.steps {
            if let PlanStep::AddRows { rows } = s
                && let (Ok(ids), Ok(pos)) = (rid_values(rows), rows.column(POS))
                && let Ok(pos) = pos.f64()
            {
                out.extend(ids.into_iter().zip(pos.into_no_null_iter()));
            }
        }
        out
    }

    /// `k` positions strictly between `anchor`'s row and the next one in base
    /// order (`after`), or the previous one. No anchor: after the last added
    /// row, or at the top of an empty sheet. Midpoints, so nothing renumbers;
    /// a source row's position is its id, so `:tail` rows still follow.
    /// `None`: the gap is too narrow for `k` distinct positions.
    pub(super) fn insert_positions(
        &self,
        anchor: Option<u64>,
        after: bool,
        k: usize,
    ) -> Option<Vec<f64>> {
        let added = self.added_positions();
        let pos_of = |rid: u64| {
            if rid < ADDED_RID {
                rid as f64
            } else {
                added
                    .iter()
                    .find(|(r, _)| *r == rid)
                    .map_or(0.0, |(_, p)| *p)
            }
        };
        let (lo, hi) = match (anchor.map(pos_of), after) {
            (Some(p), true) => {
                let next = added.iter().map(|(_, q)| *q).filter(|q| *q > p);
                (p, next.fold(p.floor() + 1.0, f64::min))
            }
            (Some(p), false) => {
                let prev = added.iter().map(|(_, q)| *q).filter(|q| *q < p);
                (prev.fold(p.ceil() - 1.0, f64::max), p)
            }
            (None, _) => {
                let last = added.iter().map(|(_, q)| *q).fold(-1.0, f64::max);
                (last, last.floor() + 1.0)
            }
        };
        let out: Vec<f64> = (1..=k)
            .map(|i| lo + (hi - lo) * i as f64 / (k + 1) as f64)
            .collect();
        let fits = std::iter::once(lo)
            .chain(out.iter().copied())
            .chain([hi])
            .collect::<Vec<_>>()
            .windows(2)
            .all(|w| w[0] < w[1]);
        fits.then_some(out)
    }

    /// Spread the added rows evenly over each gap between source rows,
    /// keeping their order, so midpoints have room again (inserting after
    /// the row just added halves the gap every time).
    pub(super) fn respace_added(&mut self) -> PolarsResult<()> {
        let mut all: Vec<(f64, usize, usize)> = Vec::new(); // (pos, step, row)
        for (i, s) in self.state.steps.iter().enumerate() {
            if let PlanStep::AddRows { rows } = s {
                let pos = rows.column(POS)?.f64()?;
                all.extend(pos.into_no_null_iter().enumerate().map(|(j, p)| (p, i, j)));
            }
        }
        // view order: position, then step and row order (stable sort)
        all.sort_by(|a, b| a.0.total_cmp(&b.0).then((a.1, a.2).cmp(&(b.1, b.2))));
        let mut new = std::collections::HashMap::new();
        for gap in all.chunk_by(|a, b| a.0.floor() == b.0.floor()) {
            let g = gap[0].0.floor();
            for (r, &(_, i, j)) in gap.iter().enumerate() {
                new.insert((i, j), g + (r + 1) as f64 / (gap.len() + 1) as f64);
            }
        }
        for (i, s) in self.state.steps.iter_mut().enumerate() {
            if let PlanStep::AddRows { rows } = s {
                let pos: Vec<f64> = (0..rows.height()).map(|j| new[&(i, j)]).collect();
                let mut df = (**rows).clone();
                df.with_column(Column::new(POS.into(), pos))?;
                *rows = Arc::new(df);
            }
        }
        Ok(())
    }

    /// `a` `ga` `p` `P`: `n` blank rows, or `values` (copied rows, matched by
    /// column name; missing columns stay null, a value that doesn't convert
    /// refuses all; columns the sheet doesn't have are added, as VisiData
    /// does, #1377), after or before `anchor` in base order. Returns the new
    /// rows' ids.
    pub fn add_rows(
        &mut self,
        anchor: Option<u64>,
        after: bool,
        values: Option<&DataFrame>,
        n: usize,
    ) -> Result<Vec<u64>, String> {
        let before = self.snapshot();
        let ids = self.insert_rows(anchor, after, values, n)?;
        self.push_undo(before);
        Ok(ids)
    }

    /// [`Self::add_rows`] without the undo step.
    pub(super) fn insert_rows(
        &mut self,
        anchor: Option<u64>,
        after: bool,
        values: Option<&DataFrame>,
        n: usize,
    ) -> Result<Vec<u64>, String> {
        self.editable()?;
        let n = values.map_or(n, DataFrame::height);
        if n == 0 {
            return Err("nothing to add".into());
        }
        let next = self
            .added_positions()
            .iter()
            .map(|(r, _)| r + 1)
            .max()
            .unwrap_or(ADDED_RID);
        let ids: Vec<u64> = (next..next + n as u64).collect();
        let pos = match self.insert_positions(anchor, after, n) {
            Some(p) => p,
            None => {
                self.respace_added().map_err(|e| e.to_string())?;
                self.insert_positions(anchor, after, n)
                    .ok_or("too many rows to add here")?
            }
        };
        let mut cols = vec![
            Column::new(ROW_ID.into(), &ids),
            Column::new(POS.into(), pos),
        ];
        if let Some(values) = values {
            let shown = self
                .plan
                .clone()
                .collect_schema()
                .map_err(|e| e.to_string())?;
            for c in values.columns() {
                if !shown.contains(c.name()) && c.name() != ROW_ID {
                    cols.push(c.clone()); // a new column
                }
            }
            let mut base = (*self
                .tail_base()
                .and_then(|mut b| b.collect_schema())
                .map_err(|e| e.to_string())?)
            .clone();
            // columns an earlier paste added are base columns too
            for s in &self.state.steps {
                if let PlanStep::AddRows { rows } = s {
                    for (n, d) in rows.schema().iter() {
                        if n != ROW_ID && n != POS && !base.contains(n) {
                            base.insert(n.clone(), d.clone());
                        }
                    }
                }
            }
            for (name, dtype) in base.iter() {
                let mut now = name.to_string(); // its name after this sheet's renames
                for s in &self.state.steps {
                    if let PlanStep::Rename { old, new } = s
                        && *old == now
                    {
                        now = new.clone();
                    }
                }
                if let Ok(c) = values.column(&now) {
                    let refuse = |_| format!("{now}: {} values don't fit {dtype}", c.dtype());
                    let json = self.state.steps.iter().find_map(|s| match s {
                        PlanStep::Json { column, dtype } if *column == now => Some(dtype),
                        _ => None,
                    });
                    if let (Some(dt), Ok(text)) = (json, c.str()) {
                        // `:type json` decodes every row: refuse what it can't
                        let _decoded = text
                            .json_decode(Some(dt.clone()), None)
                            .map_err(|e| format!("{now}: {e}"))?;
                    }
                    let c = if c.dtype() == &DataType::String {
                        // text goes in as `e` takes it: bool words, the shown type
                        let texts = column_texts(c).map_err(|e| e.to_string())?;
                        let typed = self.edit_values(&now, &texts)?;
                        match typed.dtype() == dtype {
                            true => typed.into_column(),
                            false => c.strict_cast(dtype).map_err(refuse)?,
                        }
                    } else {
                        c.strict_cast(dtype).map_err(refuse)?
                    };
                    cols.push(c.with_name(name.clone()));
                }
            }
        }
        let rows = DataFrame::new_infer_height(cols).map_err(|e| e.to_string())?;
        let at = self
            .state
            .steps
            .iter()
            .take_while(|s| matches!(s, PlanStep::AddRows { .. }))
            .count();
        self.state.steps.insert(
            at,
            PlanStep::AddRows {
                rows: Arc::new(rows),
            },
        );
        if let Err(e) = self.rebuild_plan() {
            self.state.steps.remove(at);
            return Err(e.to_string());
        }
        self.invalidate(Reason::Rows);
        Ok(ids)
    }

    /// Absolute view row of row `rid`, if the view shows it.
    pub fn view_index_of(&self, rid: u64) -> PolarsResult<Option<u64>> {
        let df = self.sorted_plan(true).select([col(ROW_ID)]).collect()?;
        Ok(rid_values(&df)?
            .iter()
            .position(|r| *r == rid)
            .map(|i| i as u64))
    }

    /// `JobDone::Loaded`: the source, read into memory, becomes the base.
    pub fn set_memory_base(&mut self, df: DataFrame) {
        let base = df.lazy();
        if self.state.steps.is_empty() {
            self.plan = with_rids(base.clone());
        }
        self.state.memory_base = Some(base);
    }

    /// Row ids of deleted rows (`DeleteRows` steps), ascending.
    pub fn deleted_rids(&self) -> Vec<u64> {
        let mut out: Vec<u64> = self
            .state
            .steps
            .iter()
            .filter_map(|s| match s {
                PlanStep::DeleteRows { rids } => Some(rids.iter().copied()),
                _ => None,
            })
            .flatten()
            .collect();
        out.sort_unstable();
        out
    }

    /// `d` / `gd`: delete rows `rids`; they leave the selection too. Returns
    /// how many were deleted.
    pub fn delete_rows(&mut self, rids: Vec<u64>) -> Result<usize, String> {
        self.editable()?;
        if rids.is_empty() {
            return Err("no rows selected".into());
        }
        self.checkpoint();
        let sel = Arc::make_mut(&mut self.view.selected);
        for r in &rids {
            sel.remove(r);
        }
        let n = rids.len();
        self.state
            .steps
            .push(PlanStep::DeleteRows { rids: rids.into() });
        self.invalidate(Reason::Rows);
        Ok(n)
    }

    /// `gzp` (VisiData setcol-clipboard): copied `values` into `column` on the
    /// selected rows in view order, repeating them to fill. All of them must
    /// fit, or nothing changes.
    pub fn paste_cells(&mut self, column: &str, values: &Column) -> Result<usize, String> {
        self.editable()?;
        if values.is_empty() {
            return Err("nothing to paste".into());
        }
        let rids = self.selected_in_view_order().map_err(|e| e.to_string())?;
        if rids.is_empty() {
            return Err("no rows selected".into());
        }
        let texts = column_texts(values).map_err(|e| e.to_string())?;
        let typed = self.edit_values(column, &texts)?;
        if let [one] = &texts[..] {
            return self.set_col(column, one.as_deref().unwrap_or_default());
        }
        let n = rids.len();
        let idx = IdxCa::from_vec(
            "i".into(),
            (0..n).map(|i| (i % texts.len()) as IdxSize).collect(),
        );
        let vals = typed.take(&idx).map_err(|e| e.to_string())?;
        let patch = DataFrame::new_infer_height(vec![
            Column::new("__vdrs_rid".into(), rids),
            vals.with_name("__vdrs_v".into()).into_column(),
        ])
        .map_err(|e| e.to_string())?;
        let step = PlanStep::SetCells {
            column: column.to_string(),
            patch: Arc::new(patch),
        };
        let before = self.snapshot();
        self.insert_edit(column, step).map_err(|e| e.to_string())?;
        self.push_undo(before);
        Ok(n)
    }

    /// `zP` `gzP` (VisiData `pasteFromClipboard`): line `i` of `lines` into
    /// row `rids[i]`, its fields into `cols` left to right; a short line
    /// leaves the cells past it alone, extra fields are dropped. Lines past
    /// the last row go into new rows after the view's last row. An empty
    /// field is null, except in text columns. All of it fits, or nothing
    /// changes; one `U` takes it back. Returns cells written.
    pub fn paste_region(
        &mut self,
        cols: &[String],
        mut rids: Vec<u64>,
        lines: &[Vec<String>],
    ) -> Result<usize, String> {
        self.editable()?;
        if lines.is_empty() || cols.is_empty() {
            return Err("nothing to paste".into());
        }
        rids.truncate(lines.len());
        let before = self.snapshot();
        match self.paste_region_steps(cols, &mut rids, lines) {
            Ok(n) => {
                self.push_undo(before);
                Ok(n)
            }
            Err(e) => {
                self.restore(before);
                Err(e)
            }
        }
    }

    pub(super) fn paste_region_steps(
        &mut self,
        cols: &[String],
        rids: &mut Vec<u64>,
        lines: &[Vec<String>],
    ) -> Result<usize, String> {
        if lines.len() > rids.len() {
            let last = self
                .sorted_plan(true)
                .select([col(ROW_ID)])
                .tail(1)
                .collect();
            let last = rid_values(&last.map_err(|e| e.to_string())?)
                .map_err(|e| e.to_string())?
                .first()
                .copied();
            rids.extend(self.insert_rows(last, true, None, lines.len() - rids.len())?);
        }
        let mut n = 0;
        for (i, column) in cols.iter().enumerate() {
            let text = self.base_type(column)? == DataType::String;
            let (ids, texts): (Vec<u64>, Vec<Option<String>>) = rids
                .iter()
                .zip(lines)
                .filter_map(|(r, l)| {
                    let v = l.get(i)?;
                    Some((*r, (text || !v.is_empty()).then(|| v.clone())))
                })
                .unzip();
            if ids.is_empty() {
                continue;
            }
            let vals = self.edit_values(column, &texts)?;
            n += ids.len();
            let patch = DataFrame::new_infer_height(vec![
                Column::new(ROW_ID.into(), ids),
                vals.with_name("__vdrs_v".into()).into_column(),
            ])
            .map_err(|e| e.to_string())?;
            let step = PlanStep::SetCells {
                column: column.clone(),
                patch: Arc::new(patch),
            };
            self.insert_edit(column, step).map_err(|e| e.to_string())?;
        }
        Ok(n)
    }

    /// Row ids of view rows `from..from + n` (fewer at the end).
    pub fn view_rids_at(&self, from: u64, n: usize) -> PolarsResult<Vec<u64>> {
        let df = self
            .sorted_plan(true)
            .select([col(ROW_ID)])
            .slice(from as i64, n as IdxSize)
            .collect()?;
        rid_values(&df)
    }

    /// `e` / `:edit`: set one cell (row id `rid`) of `column`.
    pub fn edit_cell(&mut self, column: &str, rid: u64, text: &str) -> Result<(), String> {
        self.set_rows(column, Arc::from([rid]), text)
    }

    /// `:setcol` / `ge`: write `text` into `column` on the selected rows.
    pub fn set_col(&mut self, column: &str, text: &str) -> Result<usize, String> {
        let rids: Arc<[u64]> = self.view.selected.iter().copied().collect();
        if rids.is_empty() {
            self.editable()?;
            return Err("no rows selected".into());
        }
        let n = rids.len();
        self.set_rows(column, rids, text)?;
        Ok(n)
    }

    /// `:setexpr` / `g=`: `column` := SQL `expr` on the selected rows. The
    /// expression reads the base columns (as `:derive`), not the view's casts;
    /// its type must fit the column's.
    pub fn set_expr(&mut self, column: &str, expr: &str) -> Result<usize, String> {
        self.editable()?;
        let rids: Arc<[u64]> = self.view.selected.iter().copied().collect();
        if rids.is_empty() {
            return Err("no rows selected".into());
        }
        let e = polars::sql::sql_expr(expr).map_err(|e| format!("expr: {e}"))?;
        // Steps after the insert point write neither `column` nor what `expr`
        // reads, so the current plan has their types there.
        let schema = self
            .plan
            .clone()
            .select([col(column).alias("__t"), e.alias("__v")])
            .collect_schema()
            .map_err(|e| format!("expr: {e}"))?;
        let (Some(want), Some(got)) = (schema.get("__t"), schema.get("__v")) else {
            return Err("expr: no result".into());
        };
        let fits = got == want
            || got == &DataType::Null
            || (want.is_numeric() && got.is_numeric())
            || (want.is_temporal()
                && got.is_temporal()
                && !matches!(want, DataType::Duration(_))
                && !matches!(got, DataType::Duration(_)))
            || (want == &DataType::String && !got.is_nested());
        if !fits {
            return Err(format!("expr: {got} doesn't fit {column} ({want})"));
        }
        let n = rids.len();
        let step = PlanStep::SetExpr {
            column: column.to_string(),
            expr: expr.to_string(),
            dtype: want.clone(),
            rids,
        };
        let before = self.snapshot();
        self.insert_edit(column, step).map_err(|e| e.to_string())?;
        self.push_undo(before);
        Ok(n)
    }

    /// `:s` / `g*` (S-28): replace `pattern` in text `column` on `rids` (`None`:
    /// every row). `undo`: push an undo step (`*` made its own with the derive).
    pub fn replace(
        &mut self,
        column: &str,
        pattern: &str,
        replacement: &str,
        all: bool,
        rids: Option<Arc<[u64]>>,
        undo: bool,
    ) -> Result<(), String> {
        self.editable()?;
        regex::Regex::new(pattern).map_err(|e| format!("bad regex: {e}"))?;
        let dtype = self
            .plan
            .clone()
            .collect_schema()
            .map_err(|e| e.to_string())?
            .get(column)
            .cloned()
            .ok_or_else(|| format!("no column: {column}"))?;
        if dtype != DataType::String {
            return Err(format!(
                "{column} is {dtype} in the data: replace works on text (* makes a text copy)"
            ));
        }
        let step = PlanStep::Replace {
            column: column.to_string(),
            pattern: pattern.to_string(),
            replacement: replacement.to_string(),
            all,
            rids,
        };
        let before = self.snapshot();
        self.insert_edit(column, step).map_err(|e| e.to_string())?;
        if undo {
            self.push_undo(before);
        }
        Ok(())
    }

    pub(super) fn set_rows(
        &mut self,
        column: &str,
        rids: Arc<[u64]>,
        text: &str,
    ) -> Result<(), String> {
        self.editable()?;
        let (value, dtype) = self.edit_value(column, text)?;
        let step = PlanStep::SetCol {
            column: column.to_string(),
            value,
            dtype,
            rids,
        };
        let before = self.snapshot();
        self.insert_edit(column, step).map_err(|e| e.to_string())?;
        self.push_undo(before);
        Ok(())
    }

    /// The sheet's edit steps, in plan order.
    pub fn edits(&self) -> Vec<PlanStep> {
        self.state
            .steps
            .iter()
            .filter(|s| s.is_edit())
            .cloned()
            .collect()
    }

    /// Edits not in the last `:save` (or open), plus saved ones undone since.
    pub fn unsaved_edits(&self) -> usize {
        let (now, saved) = (self.edits(), &self.state.saved_edits);
        now.iter().filter(|e| !saved.contains(e)).count()
            + saved.iter().filter(|e| !now.contains(e)).count()
    }
}
