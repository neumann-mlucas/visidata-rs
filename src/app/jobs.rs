//! Background work: jobs, their results, and materializing windows.

use super::*;

impl App {
    /// Run `work` on the blocking pool; its result lands in `on_job_done`.
    /// A selection or jump for a sheet that's gone is dropped, while a
    /// finished `PushSheet` still opens on top of whatever is active.
    /// `cancel_jobs` drops the result (Polars collects can't be interrupted, so
    /// the work runs on in the pool and its result is discarded).
    pub(super) fn spawn_job(
        &mut self,
        label: &'static str,
        work: impl FnOnce() -> Result<JobDone, String> + Send + 'static,
    ) {
        let sheet_id = self.sheets.active().id;
        let tx = self.materialize_tx.clone();
        self.next_job += 1;
        let id = self.next_job;
        let handle = tokio::spawn(async move {
            let result = tokio::task::spawn_blocking(work)
                .await
                .unwrap_or_else(|e| Err(format!("job crashed: {e}")));
            let _ = tx
                .send(MaterializeResult::Job {
                    id,
                    sheet_id,
                    label,
                    result,
                })
                .await;
        });
        self.busy.push(Job {
            id,
            sheet_id,
            label,
            abort: handle.abort_handle(),
        });
    }

    /// `<C-c>` / `g<C-c>`: drop the active sheet's (or every) background job.
    pub(super) fn cancel_jobs(&mut self, all: bool) {
        let active = self.sheets.active().id;
        let before = self.busy.len();
        self.busy.retain(|j| {
            let drop = all || j.sheet_id == active;
            if drop {
                j.abort.abort();
            }
            !drop
        });
        let n = before - self.busy.len();
        self.toast = Some(match n {
            0 => "no background jobs".into(),
            1 => "cancelled 1 job".into(),
            n => format!("cancelled {n} jobs"),
        });
    }

    pub(super) fn on_job_done(
        &mut self,
        id: u64,
        sheet_id: SheetId,
        label: &'static str,
        result: Result<JobDone, String>,
    ) {
        // Not in `busy`: cancelled (its result may have been in the channel already).
        match self.busy.iter().position(|j| j.id == id) {
            Some(i) => {
                self.busy.remove(i);
            }
            None => return, // cancelled
        }
        match result {
            Err(e) => self.fail(format!("{label}: {e}")),
            Ok(JobDone::Toast(t)) => self.toast = Some(t),
            Ok(JobDone::SidebarStats { key, stats }) => {
                if self.sidebar_stats.as_ref().is_some_and(|(k, _)| *k == key) {
                    self.sidebar_stats = Some((key, stats));
                }
            }
            Ok(JobDone::PivotPreview { text, result }) => {
                if let Some(p) = self.pivot_preview.as_mut().filter(|p| p.text == text) {
                    p.body = result.map_err(|e| format!("{e}\n\n{}", PIVOT_USAGE));
                }
            }
            Ok(JobDone::Loaded(df)) => {
                if let Some(s) = self.sheets.find_by_id_mut(sheet_id) {
                    s.set_memory_base(df);
                }
            }
            Ok(JobDone::Saved {
                note,
                edits,
                path,
                rows,
            }) => {
                if let Some(s) = self.sheets.find_by_id_mut(sheet_id) {
                    s.state.saved_edits = edits;
                }
                self.toast = Some(note);
                self.fire_event("save", &[("path", path), ("rows", rows.to_string())]);
            }
            Ok(JobDone::TailSource(loaded)) => self.apply_tail_source(sheet_id, *loaded),
            Ok(JobDone::ExecSql { db, index, rows }) => {
                let ids: Vec<SheetId> = self
                    .sheets
                    .iter()
                    .filter(|s| matches!(&s.origin, Origin::Parts { path, kind: crate::reader::SourceKind::Duckdb } if *path == db))
                    .map(|s| s.id)
                    .collect();
                for id in ids {
                    let Some(s) = self.sheets.find_by_id_mut(id) else {
                        continue;
                    };
                    s.plan = index.clone().lazy();
                    s.invalidate(crate::sheet::Reason::Plan);
                }
                self.toast = Some(match &rows {
                    Some(_) => "exec-sql: done".into(),
                    None => format!("exec-sql: done; {} reloaded", db.display()),
                });
                if let Some(sheet) = rows {
                    self.sheets.push(*sheet);
                }
                self.spawn_materialize();
            }
            Ok(JobDone::PushSheet(sheet)) => {
                self.sheets.push(*sheet);
                self.spawn_materialize();
            }
            // jobs finish out of order: keep only the latest ask's answer
            Ok(JobDone::SelStats { key, stats }) => {
                if self.sel_stats_asked.as_ref() == Some(&key) {
                    self.sel_stats = Some((key, stats));
                }
            }
            Ok(JobDone::Rebinned(fresh, reset)) => {
                if let Some(s) = self.sheets.find_by_id_mut(fresh.id) {
                    s.plan = fresh.plan.clone();
                    if reset {
                        s.state.plot = fresh.state.plot.clone();
                    }
                    s.invalidate(crate::sheet::Reason::Plan);
                    self.spawn_materialize_for(fresh.id);
                }
            }
            Ok(JobDone::SelectIn {
                sheet,
                select,
                rids,
            }) => {
                if let Some(s) = self.sheets.find_by_id_mut(sheet) {
                    s.checkpoint();
                    let sel = std::sync::Arc::make_mut(&mut s.view.selected);
                    let n = rids.len();
                    for r in rids {
                        if select {
                            sel.insert(r);
                        } else {
                            sel.remove(&r);
                        }
                    }
                    let verb = if select { "selected" } else { "unselected" };
                    self.toast = Some(format!("{verb} {} rows in {}", grouped(n as u64), s.name));
                }
            }
            Ok(JobDone::Refreshed(r)) => {
                // ponytail: whole sheets replace the stack's, so a sort made while
                // the job ran is lost; adopt on landing if refreshes get slow.
                let active = self.sheets.active().id;
                for new in r.sheets {
                    if let Some(s) = self.sheets.find_by_id_mut(new.id) {
                        *s = new;
                        s.state.stale = s.id != active;
                    }
                }
                if r.snapshot {
                    self.toast = Some("refresh: parent closed, rebuilt from its copy".into());
                }
                self.spawn_materialize();
            }
            Ok(JobDone::PushSheetToast(sheet, toast)) => {
                self.sheets.push(*sheet);
                self.spawn_materialize();
                self.toast = Some(toast);
            }
            Ok(JobDone::Jump { row, col }) => {
                if self.sheets.active().id == sheet_id {
                    self.goto_row(row);
                    self.sheets.active_mut().view.col_cursor = col;
                }
            }
            Ok(JobDone::Ranged { rids, cmd }) => {
                if self.sheets.active().id == sheet_id {
                    self.run_on_rows(rids, &cmd);
                }
            }
            Ok(JobDone::Scale(rule)) => {
                if let Some(s) = self.sheets.find_by_id_mut(sheet_id) {
                    s.checkpoint();
                    s.view.scales.push(rule);
                }
            }
            Ok(JobDone::Hide(cols)) => {
                if let Some(s) = self.sheets.find_by_id_mut(sheet_id) {
                    self.toast = Some(format!("hid {} uniform columns", cols.len()));
                    s.view.hidden.extend(cols);
                    if s.id == self.sheets.active().id {
                        self.spawn_materialize();
                    }
                }
            }
            Ok(JobDone::Select { rids, note }) => {
                if let Some(s) = self.sheets.find_by_id_mut(sheet_id) {
                    std::sync::Arc::make_mut(&mut s.view.selected).extend(rids);
                    self.toast = Some(note);
                }
            }
            Ok(JobDone::Copy(r, to)) => {
                let (n, what) = match r.kind {
                    RegisterKind::Rows => (r.df.height(), "row"),
                    RegisterKind::Cells => (r.df.height() * r.df.width(), "cell"),
                };
                self.toast = Some(match n {
                    0 => "nothing to copy".into(),
                    1 => format!("copied 1 {what}"),
                    n => format!("copied {} {what}s", grouped(n as u64)),
                });
                if n > 0 {
                    self.store_register(*r, to);
                }
            }
            Ok(JobDone::SysPaste { text, selected }) => {
                if sheet_id == self.sheets.active().id {
                    self.paste_text(&text, selected);
                }
            }
            Ok(JobDone::SysCopy { note, osc52 }) => {
                if let Some(seq) = osc52 {
                    use std::io::Write;
                    let mut out = std::io::stdout();
                    let _ = out.write_all(seq.as_bytes()).and_then(|()| out.flush());
                }
                self.toast = Some(note);
            }
            Ok(JobDone::Unselect { rids, note }) => {
                if let Some(s) = self.sheets.find_by_id_mut(sheet_id) {
                    let sel = std::sync::Arc::make_mut(&mut s.view.selected);
                    for r in rids {
                        sel.remove(&r);
                    }
                    self.toast = Some(note);
                }
            }
        }
    }

    pub(super) fn spawn_materialize(&mut self) {
        self.spawn_materialize_for(self.sheets.active().id);
        self.spawn_count_if_stale();
    }

    /// Materialize sheet `id`'s window (the active one, or the other pane's).
    pub(super) fn spawn_materialize_for(&mut self, id: SheetId) {
        let Some(s) = self.sheets.find_by_id_mut(id) else {
            return;
        };
        let rev = s.begin_window();
        let sheet_id = s.id;
        let plan = s.window_plan_with_rids();
        if let Some(h) = self.materialize_handles.remove(&sheet_id) {
            h.abort();
        }
        let tx = self.materialize_tx.clone();
        self.materialize_handles.insert(
            sheet_id,
            tokio::spawn(async move {
                let result = tokio::task::spawn_blocking(move || plan.collect()).await;
                let msg = match result {
                    Ok(Ok(df)) => MaterializeResult::Ok {
                        sheet_id,
                        revision: rev,
                        df,
                    },
                    Ok(Err(e)) => MaterializeResult::Err {
                        sheet_id,
                        revision: rev,
                        err: e.to_string(),
                    },
                    Err(join_err) => MaterializeResult::Err {
                        sheet_id,
                        revision: rev,
                        err: format!("materialize task crashed: {join_err}"),
                    },
                };
                let _ = tx.send(msg).await;
            }),
        );
    }

    /// The other pane's sheet gets a window when it has none or is stale.
    pub(super) fn feed_other_pane(&mut self) {
        let Some(id) = self.split.as_ref().map(|sp| sp.other) else {
            return;
        };
        let busy = self
            .materialize_handles
            .get(&id)
            .is_some_and(|h| !h.is_finished());
        let Some(s) = self.sheets.find_by_id_mut(id) else {
            return;
        };
        if std::mem::take(&mut s.state.stale) || (!s.is_loaded() && !busy) {
            self.spawn_materialize_for(id);
        }
    }

    /// `Z` / `:vsplit`, `gZ`, `Tab`, `gTab` (S-23 B).
    pub(super) fn pane(&mut self, a: &Action) {
        let active = self.sheets.active().id;
        match (a, self.split.as_mut()) {
            (Action::SplitPane { vertical }, _) => {
                // the sheet under this one, VisiData's choice; alone: itself
                let n = self.sheets.len();
                let under = self
                    .sheets
                    .iter()
                    .nth(n.saturating_sub(2))
                    .map_or(active, |s| s.id);
                let other = self.split.as_ref().map_or(under, |sp| sp.other);
                self.split = Some(Split {
                    other,
                    vertical: *vertical,
                    focus_first: true,
                });
            }
            (Action::ClosePane, _) => self.split = None,
            (Action::FocusPane, Some(sp)) => {
                let other = std::mem::replace(&mut sp.other, active);
                sp.focus_first = !sp.focus_first;
                self.sheets.switch_to(other);
                let s = self.sheets.active_mut();
                if std::mem::take(&mut s.state.stale) {
                    self.spawn_materialize();
                }
            }
            (Action::SwapPanes, Some(sp)) => sp.focus_first = !sp.focus_first,
            _ => self.toast = Some("no split: Z splits".into()),
        }
    }

    /// Recount the active sheet's rows when its `count_key` changed. Not aborted on
    /// supersede (blocking work can't be); stale results are dropped by key.
    pub(super) fn spawn_count_if_stale(&mut self) {
        let s = self.sheets.active_mut();
        let Some(key) = s.start_count() else {
            return;
        };
        let (sheet_id, plan) = (s.id, s.count_plan());
        let tx = self.materialize_tx.clone();
        tokio::spawn(async move {
            let total = match tokio::task::spawn_blocking(move || plan.collect()).await {
                Ok(Ok(df)) => df.columns()[0]
                    .get(0)
                    .and_then(|v| v.try_extract::<u64>())
                    .map_err(|e| e.to_string()),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(format!("count task crashed: {e}")),
            };
            let _ = tx
                .send(MaterializeResult::Count {
                    sheet_id,
                    key,
                    total,
                })
                .await;
        });
    }

    /// Drive a pending `G` as far as the current window/count allow: slide the
    /// window to the last page once the total is known, then park the cursor on
    /// the last row. Without a total the cursor parks provisionally on the
    /// window's last row and moves again when the count lands.
    pub(super) fn settle_jump_bottom(&mut self) {
        let s = self.sheets.active_mut();
        if !s.view.jump_bottom {
            return;
        }
        match s.row_count() {
            // In-window → done; otherwise the slide's window lands and settles again.
            Some(total) => {
                if total == 0 || self.goto_row(total - 1) {
                    self.sheets.active_mut().view.jump_bottom = false;
                }
            }
            None => {
                if let Some(h) = s.window().map(|df| df.height()).filter(|h| *h > 0) {
                    s.view.row_cursor = h - 1;
                }
            }
        }
    }

    /// First window is up: read the source into RAM behind the user's back.
    /// Undo snapshots keep their scan plans; they stay correct, just slower.
    pub(super) fn load_into_memory(&mut self) {
        let s = self.sheets.active_mut();
        if std::mem::replace(&mut s.state.load_tried, true) {
            return;
        }
        if let Some(plan) = s.memory_plan() {
            self.spawn_job("load into memory", move || {
                let df = plan.collect().map_err(|e| e.to_string())?;
                Ok(JobDone::Loaded(df))
            });
        }
    }

    pub(super) fn on_materialize(&mut self, res: MaterializeResult) {
        match res {
            MaterializeResult::Ok {
                sheet_id,
                revision,
                df,
            } => {
                let Some(s) = self.sheets.find_by_id_mut(sheet_id) else {
                    return;
                };
                if !s.land_window(revision, df) {
                    return;
                }
                if sheet_id == self.sheets.active().id {
                    self.settle_jump_bottom();
                    self.load_into_memory();
                }
            }
            MaterializeResult::Job {
                id,
                sheet_id,
                label,
                result,
            } => self.on_job_done(id, sheet_id, label, result),
            MaterializeResult::Count {
                sheet_id,
                key,
                total,
            } => {
                let Some(s) = self.sheets.find_by_id_mut(sheet_id) else {
                    return;
                };
                if !s.land_count(&key, total.as_ref().ok().copied()) {
                    return;
                }
                match total {
                    Ok(_) => {}
                    Err(e) => {
                        s.view.jump_bottom = false; // nothing to settle on
                        self.toast = Some(format!("row count: {e}"));
                        return;
                    }
                }
                if sheet_id == self.sheets.active().id {
                    self.settle_jump_bottom();
                }
            }
            MaterializeResult::Err {
                sheet_id,
                revision,
                err,
            } => {
                let Some(s) = self.sheets.find_by_id_mut(sheet_id) else {
                    return;
                };
                if !s.land_error(revision) {
                    return;
                }
                self.toast = Some(format!("materialize: {err}"));
            }
        }
    }
}
