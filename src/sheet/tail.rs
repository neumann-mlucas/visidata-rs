//! Live sheets: `:tail` of a growing file (new lines only for CSV/TSV/JSONL;
//! others reopen) and Lua sources (S-39 B), polled on the tail tick.

use super::*;

impl Sheet {
    /// Open a tail-mode sheet; `poll_live` picks up what the file gains.
    pub fn tail_from(
        id: SheetId,
        loaded: LoadedSource,
        follow: bool,
    ) -> Result<Self, crate::reader::OpenError> {
        let Some(path) = loaded.path.clone() else {
            return Err(crate::reader::OpenError::UnknownFormat {
                path: std::path::PathBuf::from(&loaded.display_name),
            });
        };
        let kind = loaded.kind;
        let plan = with_rids(loaded.lazy.clone());
        let display = loaded.display_name.clone();
        let mut sheet = Sheet {
            id,
            name: format!("tail: {display}"),
            kind,
            source: Arc::new(loaded),
            plan,
            view: View::default(),
            window: None,
            revision: 0,
            state: Default::default(),
            origin: Origin::Tail { path, follow },
        };
        if let Origin::Tail { path, .. } = &sheet.origin {
            sheet.state.tail_stamp = crate::source::file_stamp(path);
            if sheet.tails_incrementally() {
                let tail = crate::source::FileTail::open(path, kind)?;
                sheet.state.tail = Some(crate::source::Live::new(tail));
                sheet.rebuild_plan()?;
            }
        }
        Ok(sheet)
    }

    /// Plain (not gzipped) CSV/TSV/JSONL files grow by appending lines, so a
    /// refresh can parse just the new bytes. Others (Parquet dirs, …) re-scan.
    pub(super) fn tails_incrementally(&self) -> bool {
        let Origin::Tail { path, .. } = &self.origin else {
            return false;
        };
        matches!(
            self.kind,
            SourceKind::Csv | SourceKind::Tsv | SourceKind::Jsonl
        ) && path.is_file()
            && !path
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("gz"))
    }

    /// The table `state.steps` apply to: rows read so far, or the reopened source.
    pub(super) fn tail_base(&self) -> PolarsResult<LazyFrame> {
        match &self.state.tail {
            Some(live) => Ok(live.lock().rows()?.lazy()),
            None => Ok(self
                .state
                .memory_base
                .clone()
                .unwrap_or_else(|| self.source.lazy.clone())),
        }
    }

    /// Plan = the base (tail rows, in-memory copy, or source) + this sheet's edits.
    pub(super) fn rebuild_plan(&mut self) -> PolarsResult<()> {
        self.plan = super::step::apply_steps(self.steps_base()?, &self.state.steps)?;
        Ok(())
    }

    /// The schema each of `state.steps` reads, in step order: `:export`
    /// writes SQL for the types a step sees.
    pub(crate) fn step_schemas(&self) -> PolarsResult<Vec<SchemaRef>> {
        let mut plan = match &self.state.lineage {
            Some(l) if self.state.memory_base.is_none() => l.rows.clone(),
            _ => self.steps_base()?,
        };
        let mut out = Vec::with_capacity(self.state.steps.len());
        for step in &self.state.steps {
            out.push(plan.collect_schema()?);
            plan = step.apply(plan)?;
        }
        Ok(out)
    }

    /// The plan the steps apply to: the base plus its added rows.
    fn steps_base(&self) -> PolarsResult<LazyFrame> {
        let mut plan = with_rids(self.tail_base()?);
        let added: Vec<LazyFrame> = self
            .state
            .steps
            .iter()
            .filter_map(|s| match s {
                PlanStep::AddRows { rows } => Some((**rows).clone().lazy()),
                _ => None,
            })
            .collect();
        if !added.is_empty() {
            // ponytail: a full sort on every rebuild of a sheet with added
            // rows; fine in memory, slow on huge scans (§2.3: side table).
            let base = plan.with_column(col(ROW_ID).cast(DataType::Float64).alias(POS));
            let args = UnionArgs {
                diagonal: true,
                ..Default::default()
            };
            plan = concat(std::iter::once(base).chain(added).collect::<Vec<_>>(), args)?
                .sort(
                    [POS],
                    SortMultipleOptions::default().with_maintain_order(true),
                )
                .drop(cols([POS]));
        }
        Ok(plan)
    }

    /// A tail that can't append (Parquet dir, gzip) whose file changed: the
    /// path to reopen off the UI thread, then hand to `set_tail_source`.
    pub fn tail_needs_reopen(&mut self) -> Option<std::path::PathBuf> {
        let Origin::Tail { path, .. } = &self.origin else {
            return None;
        };
        let path = path.clone();
        let stamp = crate::source::file_stamp(&path);
        if self.state.tail.is_some() || stamp == self.state.tail_stamp {
            return None;
        }
        self.state.tail_stamp = stamp;
        Some(path)
    }

    /// The reopened source of a tail that can't append. Holding it keeps a
    /// decompressed tempfile alive.
    pub fn set_tail_source(&mut self, loaded: LoadedSource) -> PolarsResult<()> {
        self.source = Arc::new(loaded);
        self.rebuild_plan()?;
        self.invalidate(Reason::Rows);
        Ok(())
    }

    /// Pick up a live sheet's new rows (S-39 B): a CSV/TSV/JSONL tail's
    /// appended lines, a Lua source's `poll`. Returns whether anything changed
    /// (unchanged rows aren't re-materialized or recounted).
    pub fn poll_live(&mut self) -> Result<bool, String> {
        let Some(live) = self.state.tail.clone() else {
            return Ok(false);
        };
        if !live.lock().poll()? {
            return Ok(false);
        }
        self.rebuild_plan().map_err(|e| e.to_string())?;
        self.invalidate(Reason::Rows);
        Ok(true)
    }

    /// What the live source has to say since the last poll (rows it dropped).
    pub fn live_note(&self) -> Option<String> {
        self.state.tail.as_ref()?.lock().take_note()
    }
}
