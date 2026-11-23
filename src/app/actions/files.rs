//! Opening, saving and exporting: files, tails, joins, SQL, `:source`.

use super::*;
use crate::sheet::PlanStep;

/// An ex script's steps, and the missing files whose blocks were skipped.
fn source_steps(text: &str, exists: impl Fn(&str) -> bool) -> (Vec<Action>, Vec<String>) {
    let (mut steps, mut missing, mut skipping) = (Vec::new(), Vec::new(), false);
    for line in text.lines() {
        let line = line.trim();
        let cmd = line.strip_prefix(':').unwrap_or(line).trim();
        if cmd.is_empty() || cmd.starts_with('#') {
            continue;
        }
        if let Ok([Action::Open { path, .. } | Action::Tail { path, .. }]) =
            crate::exline::parse(cmd).as_deref()
        {
            skipping = !exists(path);
            if skipping {
                missing.push(path.clone());
            }
        }
        if !skipping {
            steps.push(Action::ExCommand(cmd.to_string()));
        }
    }
    (steps, missing)
}

impl App {
    /// Persist the active sheet to disk. Writer picked by extension; applies
    /// the sheet's `view_plan` (type-casts + filter + sort + visible projection,
    /// all rows — not just the materialized window) so the saved file matches
    /// what the user sees, not raw source data. Overwrites existing files.
    pub(super) fn save_active(&mut self, path: &str) {
        let s = self.sheets.active();
        let plan = s.save_plan();
        let edits = s.edits();
        let table = s.table_name();
        let path = path.to_string();
        self.spawn_job("save", move || {
            let mut df = plan.collect().map_err(|e| format!("collect: {e}"))?;
            let p = std::path::Path::new(&path);
            if crate::duckdb::is_db(p) {
                crate::duckdb::save(&mut df, p, &table)?;
            } else {
                crate::sheet::write_df_by_extension(&mut df, p).map_err(|e| e.to_string())?;
            }
            let note = format!("saved {} rows → {path}", df.height());
            let rows = df.height();
            Ok(JobDone::Saved {
                note,
                edits,
                path,
                rows,
            })
        });
    }

    /// `:open`: like the command line, a multi-part file opens on its list.
    pub(super) fn open_file(&mut self, path: &str, format: Option<&str>) {
        let Ok(opened) = self.open_with_format("open", path, format) else {
            return;
        };
        let id = self.sheets.alloc_id();
        let path = path.to_string();
        let kind = format.and_then(crate::plugins::builtin_format);
        let header = self.theme.header;
        self.spawn_job("open", move || {
            let loaded = match opened {
                Some(l) => l,
                None => crate::reader::open_with(path.as_ref(), kind, header)
                    .map_err(|e| e.to_string())?,
            };
            Ok(JobDone::PushSheet(Box::new(Sheet::opened(id, loaded))))
        });
    }

    /// `:source`: each line of an ex script becomes a macro step (blank lines
    /// and `#` comments skipped). A block from `:open`/`:tail` of a missing
    /// file to the next one is skipped whole: its steps were for that file.
    /// `skip_open`: the first `:open` is the sheet already open (`vdrs -S`).
    pub(crate) fn source(&mut self, path: &str, skip_open: bool) {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                self.toast = Some(format!("source {path}: {e}"));
                return;
            }
        };
        let (mut steps, missing) = source_steps(&text, |p| std::path::Path::new(p).exists());
        if skip_open
            && let Some(i) = steps.iter().position(
                |a| matches!(a, Action::ExCommand(c) if crate::session::root(c).is_some()),
            )
            // `-S` opened the first sheet's file: only an `open` (first_root)
            && matches!(&steps[i], Action::ExCommand(c)
                if crate::session::root(c).is_some_and(|(_, tail)| !tail)
            )
        {
            steps.remove(i);
        }
        for a in steps.into_iter().rev() {
            self.macro_queue.push_front((a, false));
        }
        if !missing.is_empty() {
            self.toast = Some(format!("source: skipped (missing): {}", missing.join(", ")));
        }
        self.pump_macro();
    }

    pub(super) fn open_tail(&mut self, path: &str, follow: bool) {
        use std::ffi::OsString;
        // A loader converts once; refreshes would re-scan that stale snapshot.
        if let Some(p) = &self.plugins
            && p.resolve_loader(std::path::Path::new(path)).is_some()
        {
            self.toast = Some(format!(
                "tail: {path} needs a plugin loader; can't follow it"
            ));
            return;
        }
        let id = self.sheets.alloc_id();
        let path = path.to_string();
        self.spawn_job("tail", move || {
            let loaded =
                crate::reader::open(&OsString::from(path), None).map_err(|e| e.to_string())?;
            let s = Sheet::tail_from(id, loaded, follow).map_err(|e| e.to_string())?;
            Ok(JobDone::PushSheet(Box::new(s)))
        });
    }

    /// Hot reload: `init.lua` edited/created/deleted → fresh VM, keymap, theme.
    pub(in crate::app) fn reload_plugins_if_changed(&mut self) {
        let Some((fresh, warning)) = self.plugins.as_ref().and_then(|p| p.reload_if_changed())
        else {
            return;
        };
        self.install_plugins(fresh, warning.unwrap_or_else(|| "init.lua reloaded".into()));
    }

    /// `:trust [view|off]` (S-40 B2).
    pub(in crate::app) fn trust(&mut self, arg: Option<&str>) {
        let Some((path, trusted)) = self.plugins.as_ref().and_then(|p| p.project_file()) else {
            return self.fail("trust: no .vdrs.lua from here up to $HOME".into());
        };
        let path = path.to_path_buf();
        if arg == Some("view") {
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| e.to_string());
            let rows: Vec<Vec<String>> = text
                .lines()
                .enumerate()
                .map(|(i, l)| vec![(i + 1).to_string(), l.to_string()])
                .collect();
            let id = self.sheets.alloc_id();
            let name = path.display().to_string();
            self.sheets
                .push(Sheet::listing(id, &name, &["line", "text"], &rows));
            return self.spawn_materialize();
        }
        let on = arg != Some("off");
        if let Err(e) = crate::plugins::set_trust(&path, on) {
            return self.fail(format!("trust: {}: {e}", path.display()));
        }
        if on == trusted {
            let state = if on { "trusted" } else { "not trusted" };
            self.toast = Some(format!("{}: {state}", path.display()));
            return; // nothing to run or stop
        }
        let Some((fresh, warning)) = self.plugins.as_ref().map(|p| p.reload()) else {
            return;
        };
        let note = match on {
            true => format!("{} trusted and run", path.display()),
            false => format!("{} no longer trusted", path.display()),
        };
        self.install_plugins(fresh, warning.filter(|_| on).unwrap_or(note));
    }

    /// A freshly loaded Lua host: its keys, theme, options in effect.
    fn install_plugins(&mut self, fresh: crate::plugins::Plugins, toast: String) {
        self.plugins = Some(fresh);
        (self.keymap, self.theme) = keymap_and_theme(self.plugins.as_ref());
        self.clipboard = super::clipboard_for(self.plugins.as_ref());
        self.paste_cmd = super::paste_cmd_for(self.plugins.as_ref());
        crate::duckdb::set_cmd(self.plugins.as_ref().and_then(|p| p.options().duckdb_cmd));
        self.sheets.active_mut().state.widths_key = None; // renderers may have changed
        self.toast = Some(toast);
        let paths = self.plugins.as_ref().and_then(|p| p.config_file());
        let paths = paths.map(|p| p.display().to_string());
        self.fire_event("reload", &[("paths", paths.unwrap_or_default())]);
    }

    pub(in crate::app) fn on_tail_tick(&mut self) {
        // Only refresh the active sheet's tail if any. Dormant tail sheets are reactivated
        // via sheet-switcher; refreshing all of them per tick would be wasteful.
        let active = self.sheets.active_mut();
        if !matches!(active.origin, Origin::Tail { .. }) {
            return;
        }
        // One reopen at a time: a change meanwhile is seen on a later tick.
        let reopening = self.busy.iter().any(|j| j.label == "tail reopen");
        let active = self.sheets.active_mut();
        if !reopening && let Some(path) = active.tail_needs_reopen() {
            let kind = active.kind;
            self.spawn_job("tail reopen", move || {
                let loaded = crate::reader::open(path.as_os_str(), Some(kind));
                let loaded = loaded.map_err(|e| e.to_string())?;
                Ok(JobDone::TailSource(Box::new(loaded)))
            });
            return;
        }
        // Reads only appended bytes, so the UI thread can afford it; rotation
        // rereads the whole file.
        // ponytail: move to a job if a rotated giant log ever stalls a tick.
        self.after_tail_change(Sheet::refresh_tail);
    }

    /// Run `change` on the active tail; keep following the bottom if the cursor was there.
    pub(super) fn after_tail_change(
        &mut self,
        change: impl FnOnce(&mut Sheet) -> Result<bool, crate::reader::OpenError>,
    ) {
        let active = self.sheets.active_mut();
        let follow = matches!(active.origin, Origin::Tail { follow: true, .. });
        let v = &active.view;
        let abs = v.window_offset + v.row_cursor as u64;
        let was_at_bottom = match (active.row_count(), active.window()) {
            (Some(total), _) => abs + 1 >= total,
            (None, Some(df)) => v.row_cursor + 1 >= df.height(),
            (None, None) => true,
        };
        match change(active) {
            Ok(true) => {}
            Ok(false) => return, // nothing new
            Err(e) => {
                self.toast = Some(format!("tail refresh: {e}"));
                return;
            }
        }
        // Settles across the window + count results the refresh triggers.
        active.view.jump_bottom |= follow && was_at_bottom;
        self.spawn_materialize();
    }

    pub(in crate::app) fn apply_tail_source(
        &mut self,
        sheet_id: SheetId,
        loaded: crate::reader::LoadedSource,
    ) {
        if self.sheets.active().id == sheet_id {
            self.after_tail_change(|s| {
                s.set_tail_source(loaded)?;
                Ok(true)
            });
        } else if let Some(s) = self.sheets.find_by_id_mut(sheet_id) {
            // Shown next time it's raised and something re-materializes.
            let _ = s.set_tail_source(loaded);
        }
    }

    /// Whether a Lua loader claims `path`: those run here, on the VM's thread.
    pub(super) fn lua_loader_for(&self, path: &str) -> bool {
        let p = self.plugins.as_ref();
        p.is_some_and(|p| p.loader_for(path, None).is_some())
    }

    /// Open `path` now through a Lua loader, or return None for `job` to read it.
    pub(super) fn open_with_loader(
        &mut self,
        label: &str,
        path: &str,
    ) -> Result<Option<crate::reader::LoadedSource>, ()> {
        self.open_with_format(label, path, None)
    }

    /// [`Self::open_with_loader`]; `format` a loader's name runs it (an
    /// unknown one is an error), a built-in kind is for the job's reader.
    pub(super) fn open_with_format(
        &mut self,
        label: &str,
        path: &str,
        format: Option<&str>,
    ) -> Result<Option<crate::reader::LoadedSource>, ()> {
        let lua = match format {
            Some(f) => crate::plugins::builtin_format(f).is_none(),
            None => self.lua_loader_for(path),
        };
        if !lua {
            return Ok(None);
        }
        crate::plugins::open(self.plugins.as_ref(), path.as_ref(), format)
            .map(Some)
            .map_err(|e| self.fail(format!("{label}: {e}")))
    }

    pub(super) fn open_diff(&mut self, path: &str, keys: &[String]) {
        let Ok(opened) = self.open_with_loader("diff", path) else {
            return;
        };
        let id = self.sheets.alloc_id();
        let parent = self.sheets.active().clone();
        let (path, keys) = (path.to_string(), keys.to_vec());
        self.spawn_job("diff", move || {
            let loaded = match opened {
                Some(l) => l,
                None => crate::reader::open(path.as_ref(), None).map_err(|e| e.to_string())?,
            };
            let mut s = if keys.is_empty() {
                Sheet::schema_diff_from(&parent, id, &loaded)?
            } else {
                Sheet::row_diff_from(&parent, id, &Sheet::from_source(0, loaded), &keys)?
            };
            // A full join has no fixed row order: collect once, like `:join`.
            if !keys.is_empty() {
                s.plan = s.plan.clone().collect().map_err(|e| e.to_string())?.lazy();
            }
            Ok(JobDone::PushSheet(Box::new(s)))
        });
    }
}

/// Another sheet a command reads: open already, or a file to read in its job.
pub(in crate::app) enum Other {
    Open(Box<Sheet>),
    File(String, Option<Box<crate::reader::LoadedSource>>),
}

impl Other {
    /// The sheet; a file is read now (call it in the job).
    pub(in crate::app) fn sheet(self) -> Result<Sheet, String> {
        Ok(match self {
            Other::Open(s) => *s,
            Other::File(_, Some(l)) => Sheet::from_source(0, *l),
            Other::File(path, None) => {
                let l = crate::reader::open(path.as_ref(), None);
                Sheet::from_source(0, l.map_err(|e| e.to_string())?)
            }
        })
    }
}

impl App {
    /// `:join` / `:lookup` source: `#<id>` or the exact name of an open
    /// sheet, else a file (through a Lua loader when one claims it). `None`:
    /// the loader failed, with a toast.
    pub(in crate::app) fn other_sheet(&mut self, label: &str, source: &str) -> Option<Other> {
        let active = self.sheets.active().id;
        let open = self.sheets.iter().find(|s| {
            s.id != active
                && (s.name == source || source.strip_prefix('#') == Some(&s.id.to_string()))
        });
        if let Some(s) = open {
            return Some(Other::Open(Box::new(s.clone())));
        }
        let opened = self.open_with_loader(label, source).ok()?;
        Some(Other::File(source.to_string(), opened.map(Box::new)))
    }

    pub(super) fn open_join(&mut self, source: &str, keys: &[String], how: &str) {
        let Some(right) = self.other_sheet("join", source) else {
            return;
        };
        let id = self.sheets.alloc_id();
        let left = self.sheets.active().clone();
        let (keys, how) = (keys.to_vec(), how.to_string());
        self.spawn_job("join", move || {
            let right = right.sheet()?;
            let mut s = Sheet::join_from(&left, id, &right, &keys, &how)?;
            s.plan = s.plan.clone().collect().map_err(|e| e.to_string())?.lazy();
            Ok(JobDone::PushSheet(Box::new(s)))
        });
    }

    /// `g&` / `:join-all` (VisiData join-sheets-all): the active sheet joined
    /// with each sheet below it, nearest first, on its key columns.
    pub(super) fn open_join_all(&mut self, how: &str) {
        let left = self.sheets.active().clone();
        let others: Vec<Sheet> = self.sheets.iter().rev().skip(1).cloned().collect();
        if others.is_empty() {
            return self.fail("join-all: no other sheet".into());
        }
        let keyless = matches!(how, "append" | "concat");
        if left.view.keys.is_empty() && !keyless {
            return self.fail("join-all: no key columns".into());
        }
        let id = self.sheets.alloc_id();
        let (keys, how) = (left.view.keys.clone(), how.to_string());
        self.spawn_job("join", move || {
            let names: Vec<&str> = std::iter::once(&left)
                .chain(&others)
                .map(|s| s.name.as_str())
                .collect();
            let name = format!("{} ({how})", names.join("&"));
            let mut acc = (others.iter()).try_fold(left, |acc, right| {
                Sheet::join_from(&acc, id, right, &keys, &how)
            })?;
            acc.name = name;
            acc.plan = acc
                .plan
                .clone()
                .collect()
                .map_err(|e| e.to_string())?
                .lazy();
            Ok(JobDone::PushSheet(Box::new(acc)))
        });
    }

    /// `:lookup` (S-36 A): `cols` (their column, new name) of `source` by
    /// `key` = their `their_key`, after the cursor column. An open sheet stays
    /// linked (A4); a file is a snapshot.
    pub(super) fn lookup(
        &mut self,
        source: &str,
        key: &str,
        their_key: &str,
        cols: &[(String, String)],
    ) {
        let me = self.sheets.active();
        if let Err(e) = me.editable() {
            return self.fail(format!("lookup: {e}"));
        }
        if !me.has_column(key) {
            return self.fail(format!("lookup: no column {key}"));
        }
        let Some(other) = self.other_sheet("lookup", source) else {
            return;
        };
        if let Other::Open(s) = &other
            && self.lookup_reaches(s.id, self.sheets.active().id)
        {
            return self.fail(format!("lookup: {} looks up this sheet", s.name));
        }
        let at = self.current_column_name();
        let (key, their_key, cols) = (key.to_string(), their_key.to_string(), cols.to_vec());
        self.spawn_job("lookup", move || {
            let live = matches!(other, Other::Open(_));
            let their = other.sheet()?;
            let stamp = their.lookup_stamp();
            let mut steps = Vec::new();
            let mut dups = 0;
            for (column, name) in cols {
                let (table, d) = their
                    .lookup_table(&their_key, &column)
                    .map_err(|e| format!("lookup: {e}"))?;
                dups = d;
                steps.push(PlanStep::Lookup {
                    name,
                    key: key.clone(),
                    table: Arc::new(table),
                    link: live.then(|| crate::sheet::LookupLink {
                        sheet: their.id,
                        their_key: their_key.clone(),
                        column,
                        stamp: stamp.clone(),
                    }),
                });
            }
            let note = (dups > 0)
                .then(|| format!("lookup: {dups} rows repeat a {their_key}; first match kept"));
            Ok(JobDone::Lookup { steps, at, note })
        });
    }

    /// Some chain of live lookups from sheet `from` reads sheet `to`.
    fn lookup_reaches(&self, from: SheetId, to: SheetId) -> bool {
        let mut todo = vec![from];
        let mut seen = Vec::new();
        while let Some(id) = todo.pop() {
            if id == to {
                return true;
            }
            if seen.contains(&id) {
                continue;
            }
            seen.push(id);
            if let Some(s) = self.sheets.iter().find(|s| s.id == id) {
                todo.extend(s.lookup_sources());
            }
        }
        false
    }

    /// The active sheet's live lookups catch up with their sheets (A4): a
    /// changed one is collected again in a job, a closed one keeps its values.
    pub(in crate::app) fn follow_lookups(&mut self) {
        let s = self.sheets.active();
        let ids: Vec<SheetId> = s.lookup_sources().collect();
        if ids.is_empty() {
            return;
        }
        let gone: Vec<SheetId> = ids
            .iter()
            .copied()
            .filter(|id| !self.sheets.iter().any(|o| o.id == *id))
            .collect();
        if !gone.is_empty() {
            let names = self.sheets.active_mut().freeze_lookups(&gone);
            self.toast = Some(format!(
                "lookup: source sheet closed; {} keep their values",
                names.join(", ")
            ));
        }
        let mut stale = Vec::new();
        for step in &self.sheets.active().state.steps {
            let PlanStep::Lookup { link: Some(l), .. } = step else {
                continue;
            };
            let Some(src) = self.sheets.iter().find(|o| o.id == l.sheet) else {
                continue;
            };
            if src.lookup_stamp() != l.stamp {
                stale.push((step.clone(), src.clone()));
            }
        }
        if stale.is_empty() {
            return;
        }
        self.spawn_job("lookup", move || {
            let mut fresh = Vec::new();
            for (step, src) in stale {
                let PlanStep::Lookup {
                    name,
                    key,
                    link: Some(mut l),
                    ..
                } = step
                else {
                    continue;
                };
                let (table, _) = src.lookup_table(&l.their_key, &l.column)?;
                l.stamp = src.lookup_stamp();
                fresh.push(PlanStep::Lookup {
                    name,
                    key,
                    table: Arc::new(table),
                    link: Some(l),
                });
            }
            Ok(JobDone::LookupRefresh(fresh))
        });
    }

    pub(super) fn open_unpivot(&mut self, on: &[String], index: &[String]) {
        // the melt makes these two; Polars holds one column per name
        if let Some(c) = (index.iter()).find(|c| ["Variable", "Value"].contains(&c.as_str())) {
            self.toast = Some(format!(
                "unpivot: key {c} clashes with the melt's own column"
            ));
            return;
        }
        let id = self.sheets.alloc_id();
        let child = {
            let parent = self.sheets.active();
            Sheet::unpivot_from(parent, id, on, index)
        };
        self.sheets.push(child);
        self.spawn_materialize();
    }

    /// `:exec-sql`: run statements in the `.duckdb` file the active sheet came
    /// from (its index or one of its tables), as VisiData.
    pub(super) fn exec_sql(&mut self, sql: String) {
        use crate::reader::{LoadedSource, SourceKind};
        let s = self.sheets.active();
        let db = match (&s.origin, &s.source.path) {
            (
                Origin::Parts {
                    path,
                    kind: SourceKind::Duckdb,
                },
                _,
            ) => path.clone(),
            (_, Some(p)) if s.kind == SourceKind::Duckdb => p.clone(),
            _ => {
                self.toast = Some("exec-sql: not a .duckdb sheet".into());
                return;
            }
        };
        let id = self.sheets.alloc_id();
        self.spawn_job("exec-sql", move || {
            let rows = crate::duckdb::exec(&db, &sql)?;
            let index = crate::duckdb::index(&db)?;
            let rows = rows.map(|df| {
                let src = LoadedSource::from_df(
                    SourceKind::Duckdb,
                    "exec-sql".into(),
                    Some(db.clone()),
                    df,
                );
                Box::new(Sheet::from_source(id, src))
            });
            Ok(JobDone::ExecSql { db, index, rows })
        });
    }

    /// `:sql`: `self` = this view; every open sheet is also `s<id>` (see `S`).
    pub(super) fn open_sql(&mut self, query: &str) {
        let active = self.sheets.active().id;
        let ids: Vec<SheetId> = self.sheets.summaries().map(|(id, _, _)| id).collect();
        let others: Vec<Sheet> = ids
            .into_iter()
            .filter(|id| *id != active)
            .filter_map(|id| Some(self.sheets.find_by_id_mut(id)?.clone()))
            .collect();
        let id = self.sheets.alloc_id();
        match Sheet::sql_from(self.sheets.active(), id, query, others) {
            Ok(s) => self.push_collected("sql", s),
            Err(e) => self.toast = Some(format!("sql: {e}")),
        }
    }

    /// Push `sheet` with its plan collected once, in the background. For
    /// results whose row order Polars doesn't fix (joins, GROUP BY): row ids
    /// are positions, so a re-collect in another order would move selections.
    // ponytail: holds the whole result in memory; fine for joins/aggregates.
    // Sink to a parquet tempfile and scan it if a result outgrows memory.
    pub(super) fn push_collected(&mut self, label: &'static str, mut sheet: Sheet) {
        self.spawn_job(label, move || {
            sheet.plan = sheet
                .plan
                .clone()
                .collect()
                .map_err(|e| e.to_string())?
                .lazy();
            Ok(JobDone::PushSheet(Box::new(sheet)))
        });
    }

    pub(super) fn export(&mut self, path: &str) {
        let p = std::path::Path::new(path);
        let result = crate::export::script_for(self.sheets.active(), p)
            .and_then(|script| std::fs::write(p, script).map_err(|e| format!("export: {e}")));
        self.toast = Some(match result {
            Ok(()) => format!("exported → {path}"),
            Err(e) => e,
        });
    }
}

impl App {
    pub(super) fn patch(&mut self, column: Option<String>, path: String) {
        let read =
            polars::prelude::LazyFrame::scan_parquet(path.as_str().into(), Default::default())
                .and_then(|lf| lf.collect());
        let done = read.map_err(|e| e.to_string()).and_then(|df| {
            self.sheets
                .active_mut()
                .restore_patch(column.as_deref(), df)
        });
        match done {
            Ok(()) => self.spawn_materialize(),
            Err(e) => self.toast = Some(format!("patch {path}: {e}")),
        }
    }

    pub(super) fn open_cell_file(&mut self) {
        let s = self.sheets.active();
        let path = self
            .current_column_name()
            .and_then(|c| {
                let col = s.window()?.column(&c).ok()?;
                crate::widget::cell_display(&s.view, &self.theme, col, s.view.row_cursor)
            })
            .unwrap_or_default();
        if path.is_empty() {
            self.toast = Some("no path given".into());
        } else if !path.contains("://") && !std::path::Path::new(&path).exists() {
            self.toast = Some(format!("file {path} does not exist"));
        } else {
            self.dispatch(Action::Open { path, format: None });
        }
    }
}

#[cfg(test)]
mod source_tests {
    use super::*;

    #[test]
    fn missing_file_skips_only_its_block() {
        let text = "# session\n:open a.csv\n:sort -x\n\n:open gone.csv\n:sort -y\nopen_freq_table\n:tail b.csv\n:cursor 2 x\n";
        let (steps, missing) = source_steps(text, |p| p != "gone.csv");
        let cmds: Vec<String> = steps
            .into_iter()
            .map(|a| match a {
                Action::ExCommand(c) => c,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(cmds, ["open a.csv", "sort -x", "tail b.csv", "cursor 2 x"]);
        assert_eq!(missing, ["gone.csv"]);
    }
}
