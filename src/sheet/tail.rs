use super::*;

impl Sheet {
    /// Open a tail-mode sheet; `refresh_tail` picks up what the file gains.
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
        let plan = loaded.lazy.clone();
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
            sheet.state.tail_stamp = tail_stamp(path);
        }
        if sheet.tails_incrementally() {
            sheet.reload_tail_cache()?;
            sheet.rebuild_plan()?;
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

    /// Bytes `[from..]` of the tail file: `(lines, text after the last newline)`.
    pub(super) fn tail_bytes(&self, from: u64) -> std::io::Result<(Vec<u8>, Vec<u8>)> {
        use std::io::{Read, Seek, SeekFrom};
        let Origin::Tail { path, .. } = &self.origin else {
            return Ok(Default::default());
        };
        let mut f = std::fs::File::open(path)?;
        f.seek(SeekFrom::Start(from))?;
        let mut buf = Vec::new();
        f.read_to_end(&mut buf)?;
        let cut = buf.iter().rposition(|b| *b == b'\n').map_or(0, |i| i + 1);
        let rest = buf.split_off(cut);
        Ok((buf, rest))
    }

    /// `partial` bytes as a row of `df`'s shape; None when they don't parse yet.
    pub(super) fn parse_partial(&self, mut partial: Vec<u8>, df: &DataFrame) -> Option<DataFrame> {
        if partial.is_empty() {
            return None;
        }
        partial.push(b'\n');
        crate::reader::parse_chunk(self.kind, partial, Some(df.schema().clone()), false).ok()
    }

    /// Read the whole tail file into the cache (first load, or after truncation).
    pub(super) fn reload_tail_cache(&mut self) -> Result<(), crate::reader::OpenError> {
        let (mut lines, mut rest) = self.tail_bytes(0)?;
        // `consumed` 0 = a lone header without its newline: reread it all later.
        let consumed = lines.len() as u64;
        if lines.is_empty() {
            lines = std::mem::take(&mut rest);
            lines.push(b'\n');
        }
        let df = crate::reader::parse_chunk(self.kind, lines, None, true)?;
        let partial = self.parse_partial(rest, &df);
        self.state.tail = Some(TailCache {
            consumed,
            df,
            partial,
        });
        Ok(())
    }

    /// The table `state.steps` apply to: rows read so far, or the reopened source.
    pub(super) fn tail_base(&self) -> PolarsResult<LazyFrame> {
        match &self.state.tail {
            Some(c) => {
                let mut df = c.df.clone();
                if let Some(p) = &c.partial {
                    df.vstack_mut(p)?;
                }
                Ok(df.lazy())
            }
            None => Ok(self
                .state
                .memory_base
                .clone()
                .unwrap_or_else(|| self.source.lazy.clone())),
        }
    }

    /// Plan = the base (tail rows, in-memory copy, or source) + this sheet's edits.
    pub(super) fn rebuild_plan(&mut self) -> PolarsResult<()> {
        let mut plan = self.tail_base()?;
        for step in &self.state.steps {
            plan = step.apply(plan)?;
        }
        self.plan = plan;
        Ok(())
    }

    /// A tail that can't append (Parquet dir, gzip) whose file changed: the
    /// path to reopen off the UI thread, then hand to `set_tail_source`.
    pub fn tail_needs_reopen(&mut self) -> Option<std::path::PathBuf> {
        let Origin::Tail { path, .. } = &self.origin else {
            return None;
        };
        let path = path.clone();
        let stamp = tail_stamp(&path);
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
        self.view.count_key = None;
        Ok(())
    }

    /// Pick up rows appended to a CSV/TSV/JSONL tail. Returns whether anything
    /// changed (an unchanged file isn't re-materialized or recounted).
    pub fn refresh_tail(&mut self) -> Result<bool, crate::reader::OpenError> {
        let Origin::Tail { path, .. } = &self.origin else {
            return Ok(false);
        };
        let Some(cache) = &self.state.tail else {
            return Ok(false);
        };
        let stamp = tail_stamp(path);
        if stamp == self.state.tail_stamp {
            return Ok(false);
        }
        self.state.tail_stamp = stamp;
        let header_only = cache.consumed == 0 || cache.df.height() == 0;
        if std::fs::metadata(path)?.len() < cache.consumed || header_only {
            self.reload_tail_cache()?; // truncated, rotated, or types still unknown
        } else {
            let (new, rest) = self.tail_bytes(cache.consumed)?;
            let added = new.len() as u64;
            let schema = cache.df.schema().clone();
            let more = (!new.is_empty())
                .then(|| crate::reader::parse_chunk(self.kind, new, Some(schema), false))
                .transpose();
            match more {
                Ok(more) => {
                    let cache = self.state.tail.as_mut().expect("checked above");
                    if let Some(more) = more {
                        cache.df.vstack_mut(&more)?;
                        cache.consumed += added;
                    }
                    let df = cache.df.clone();
                    let partial = self.parse_partial(rest, &df);
                    self.state.tail.as_mut().expect("checked above").partial = partial;
                }
                Err(_) => self.reload_tail_cache()?, // new rows don't fit the schema
            }
        }
        self.rebuild_plan()?;
        self.view.count_key = None;
        Ok(true)
    }
}
