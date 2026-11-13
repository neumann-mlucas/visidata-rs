//! Vim grammar: operators, text objects, visual ranges, ex ranges, `:normal`, the jumplist.

use super::*;

/// `:normal`'s keys: `<…>` names one (`<CR>`, `<C-d>`), any other char is
/// itself, a space is `<Space>`.
fn normal_keys(s: &str) -> Result<Vec<Key>, String> {
    let mut out = Vec::new();
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        let (tok, len) = match c {
            '<' => match rest.find('>') {
                Some(i) => (rest[..=i].to_string(), i + 1),
                None => ("<".to_string(), 1),
            },
            ' ' => ("<Space>".to_string(), 1),
            c => (c.to_string(), c.len_utf8()),
        };
        out.push(tok.parse::<Key>()?);
        rest = &rest[len..];
    }
    match out.is_empty() {
        true => Err("no keys".into()),
        false => Ok(out),
    }
}

impl App {
    /// A key after `d` `y` `c` (S-26 D3): a count, the operator again (its
    /// rows: `dd`), `i` + an object, or a motion whose rows it acts on.
    pub(in crate::app) fn operator_key(&mut self, key: Key) {
        let Some(mut p) = self.operator else { return };
        let ch = match key.code {
            KeyCode::Char(c) if !key.ctrl && !key.alt => Some(c),
            _ => None,
        };
        if key.code == KeyCode::Esc {
            (self.operator, self.pending) = (None, Vec::new());
            return;
        }
        if p.object {
            self.operator = None;
            return match ch {
                Some(o @ ('r' | 'c' | 's' | 'v' | 'b')) => self.operator_object(p, o),
                _ => self.toast = Some(format!("no text object i{key} (ir ic is iv ib)")),
            };
        }
        if self.pending.is_empty()
            && let Some(d) = ch.and_then(|c| c.to_digit(10))
            && (d > 0 || p.motion_count.is_some())
        {
            p.motion_count = Some(p.motion_count.unwrap_or(0) * 10 + d as usize);
            self.operator = Some(p);
            return;
        }
        let n = p.count.unwrap_or(1) * p.motion_count.unwrap_or(1);
        if self.pending.is_empty() && ch == Some(p.op) {
            self.operator = None;
            let at = self.cursor_row();
            return self.operator_rows(p.op, at, at + n as u64 - 1);
        }
        if self.pending.is_empty() && ch == Some('i') {
            p.object = true;
            self.operator = Some(p);
            return;
        }
        let actions = match self.keymap.resolve(Layer::Sheet, &self.pending, key) {
            Dispatch::Pending(_) => {
                self.pending.push(key);
                return;
            }
            Dispatch::Fire(a) => a,
            Dispatch::Miss => Vec::new(),
        };
        (self.operator, self.pending) = (None, Vec::new());
        let [motion] = actions.as_slice() else {
            self.toast = Some(format!("{} needs a motion (j k G gg H L {{ }} …)", p.op));
            return;
        };
        let from = self.cursor_row();
        let to = match motion {
            Action::GotoTop => 0,
            Action::GotoBottom => match self.sheets.active().row_count() {
                Some(t) => t.saturating_sub(1),
                None => return self.toast = Some("the row count isn't known yet".into()),
            },
            Action::CursorDown { .. }
            | Action::CursorUp { .. }
            | Action::HalfPage { .. }
            | Action::PageDown
            | Action::PageUp
            | Action::ScreenRow { .. }
            | Action::ValueChange { .. } => {
                let m = motion.clone().with_count(Some(n));
                self.dispatch(m);
                let to = self.cursor_row();
                self.goto_row(from);
                to
            }
            _ => {
                self.toast = Some(format!("{} needs a motion (j k G gg H L {{ }} …)", p.op));
                return;
            }
        };
        self.operator_rows(p.op, from.min(to), from.max(to));
    }

    pub(super) fn cursor_row(&self) -> u64 {
        let s = self.sheets.active();
        s.view.window_offset + s.view.row_cursor as u64
    }

    /// `op` on view rows `lo..=hi` (`d` cuts, `y` copies, `c` sets the
    /// column); `.` repeats it from the cursor.
    pub(super) fn operator_rows(&mut self, op: char, lo: u64, hi: u64) {
        let cmd = match op {
            'd' => "cut-row",
            'y' => "y",
            _ => return self.change_rows(lo, hi),
        };
        self.goto_row(lo);
        let l = |r: u64| crate::exline::Addr::Line(r + 1);
        let again = format!(".,.+{} {cmd}", hi - lo);
        self.run_ranged_as(crate::exline::Rows::Span(l(lo), l(hi)), cmd.into(), again);
    }

    /// `c`: `:'<,'> setcol ` on the rows, waiting for the value.
    pub(super) fn change_rows(&mut self, lo: u64, hi: u64) {
        let s = self.sheets.active();
        let col = s.view.col_cursor;
        self.last_visual = Some(Visual {
            sheet: s.id,
            row: lo,
            col,
            end: (hi, col),
            block: false,
        });
        self.start_input(Prompt::Ex);
        if let Some((_, line)) = self.input.as_mut() {
            line.set("'<,'> setcol ");
        }
    }

    /// `ir` rows, `is` the selection, `ic` the column's cells, `iv` the run of
    /// equal values around the cursor, `ib` the block of non-empty cells.
    pub(super) fn operator_object(&mut self, p: crate::app::Pending, o: char) {
        let at = self.cursor_row();
        let n = p.count.unwrap_or(1) as u64;
        let cells = match p.op {
            'd' => "cut-cells",
            'y' => "copy-cells",
            _ => "",
        };
        match o {
            'r' => self.operator_rows(p.op, at, at + n - 1),
            's' => match p.op {
                'd' => self.dispatch(Action::CutSelected),
                'y' => self.dispatch(Action::CopySelected),
                _ => {
                    self.start_input(Prompt::Ex);
                    if let Some((_, line)) = self.input.as_mut() {
                        line.set("setcol ");
                    }
                }
            },
            'c' if p.op == 'c' => {
                self.start_input(Prompt::Ex);
                if let Some((_, line)) = self.input.as_mut() {
                    line.set("% setcol ");
                }
            }
            'c' => self.run_ranged(crate::exline::Rows::All, cells.into()),
            _ => {
                // ponytail: the loaded rows only, like `{` `}`
                let s = self.sheets.active();
                let Some(col) = s
                    .window()
                    .and_then(|w| w.columns().get(s.view.col_cursor).cloned())
                else {
                    return;
                };
                let r = s.view.row_cursor;
                let get = |i: usize| col.get(i).map(|v| v.into_static()).ok();
                let here = get(r);
                let keep = |i: usize| match o {
                    'v' => get(i) == here,
                    _ => get(i).is_some_and(|v| !v.is_null()),
                };
                if !keep(r) {
                    return self.toast = Some("the cursor cell is empty".into());
                }
                let top = (0..r).rev().take_while(|i| keep(*i)).last().unwrap_or(r);
                let bottom = (r + 1..col.len())
                    .take_while(|i| keep(*i))
                    .last()
                    .unwrap_or(r);
                let off = s.view.window_offset;
                let (lo, hi) = (off + top as u64, off + bottom as u64);
                if p.op == 'c' {
                    return self.change_rows(lo, hi);
                }
                self.goto_row(lo);
                let l = |r: u64| crate::exline::Addr::Line(r + 1);
                self.run_ranged(crate::exline::Rows::Span(l(lo), l(hi)), cells.into());
            }
        }
    }

    /// The visual range with its moving corner at the cursor; `None` when
    /// the active sheet isn't the one it was started on.
    pub(in crate::app) fn live_visual(&self) -> Option<Visual> {
        let mut v = self.visual?;
        let s = self.sheets.active();
        if v.sheet != s.id {
            return None;
        }
        v.end = (
            s.view.window_offset + s.view.row_cursor as u64,
            s.view.col_cursor,
        );
        Some(v)
    }

    /// The live range in `sheet`'s window rows, for the widget.
    pub(in crate::app) fn visual_on(&self, sheet: &Sheet) -> Option<crate::widget::VisualSpan> {
        let v = self.live_visual().filter(|v| v.sheet == sheet.id)?;
        let (a, b) = v.rows();
        let off = sheet.view.window_offset;
        let clip = |r: u64| r.saturating_sub(off) as usize;
        (b >= off).then(|| ((clip(a), clip(b)), v.cols()))
    }

    /// A verb while `V` / `<C-v>` is open (S-26 C) runs on the range, then
    /// the range closes; motions and other keys pass through and move it.
    pub(super) fn visual_dispatch(&mut self, a: &Action) -> bool {
        let Some(v) = self.live_visual() else {
            self.visual = None; // the sheet changed under it
            return false;
        };
        let (r0, r1) = v.rows();
        // from the cursor, so a macro (which records the motions) and `.`
        // replay it where they run, as vim does
        let cur = self.cursor_row() as i64;
        let span = || {
            let l = |r: u64| crate::exline::Addr::Offset(r as i64 - cur);
            crate::exline::Rows::Span(l(r0), l(r1))
        };
        let ex = |me: &mut Self, prefill: &str| {
            me.start_input(Prompt::Ex);
            if let Some((_, line)) = me.input.as_mut() {
                line.set(prefill);
            }
        };
        match a {
            Action::Visual { block } if *block == v.block => {}
            Action::Visual { .. } => {
                self.visual = Some(Visual {
                    block: !v.block,
                    ..v
                });
                return true;
            }
            Action::ToggleSelectRow | Action::SelectRow => self.run_ranged(span(), String::new()),
            Action::UnselectRow => self.run_ranged(span(), "unselect-all".into()),
            Action::CopyRow => self.run_ranged(span(), "y".into()),
            Action::DeleteRow => self.run_ranged(span(), "d".into()),
            Action::CutRow => self.run_ranged(span(), "cut-row".into()),
            // vim profile: an operator on a visual range takes the range
            Action::Operator { op: 'd', .. } => self.run_ranged(span(), "cut-row".into()),
            Action::Operator { op: 'y', .. } => self.run_ranged(span(), "y".into()),
            Action::Operator { op: 'c', .. } => ex(self, "'<,'> setcol "),
            Action::DupSelected => self.run_ranged(span(), "dup-selected".into()),
            Action::StartEditInput => ex(self, "'<,'> setcol "),
            Action::StartExInput => ex(self, "'<,'> "),
            Action::HideCol if v.block => {
                let (c0, c1) = v.cols().unwrap_or_default();
                let names: Vec<String> = self.sheets.active().window().map_or(Vec::new(), |df| {
                    let names = df.get_column_names();
                    names[c0.min(names.len())..(c1 + 1).min(names.len())]
                        .iter()
                        .map(|n| n.to_string())
                        .collect()
                });
                self.sheets.active_mut().checkpoint();
                self.sheets.active_mut().view.hidden.extend(names);
                self.spawn_materialize();
            }
            _ => return false,
        }
        self.last_visual = Some(v);
        self.visual = None;
        true
    }

    /// `:5,20 cmd`, `:% cmd`, `:g/re/cmd` … (S-26 B): the rows' ids come from
    /// a job, then `cmd` runs as if they were selected (`run_on_rows`).
    pub(in crate::app) fn run_ranged(&mut self, rows: crate::exline::Rows, cmd: String) {
        let ex = crate::exline::ranged_text(&rows, &cmd);
        self.run_ranged_as(rows, cmd, ex);
    }

    /// [`run_ranged`](Self::run_ranged); a macro and `.` replay `:{ex}`.
    pub(in crate::app) fn run_ranged_as(
        &mut self,
        rows: crate::exline::Rows,
        cmd: String,
        ex: String,
    ) {
        use crate::exline::{Addr, Rows};
        if !self.replaying {
            if let Some(rec) = self.recording.as_mut() {
                rec.steps.push(format!(":{ex}"));
            }
            self.last_action = Some(vec![Action::ExCommand(ex)]);
        }
        let s = self.sheets.active();
        let cursor = s.view.window_offset + s.view.row_cursor as u64;
        let total = s.row_count();
        let marks = self
            .last_visual
            .filter(|v| v.sheet == s.id)
            .map(|v| v.rows());
        let line = |a: &Addr| match a {
            Addr::Line(n) => Ok(n.saturating_sub(1)),
            Addr::Cursor => Ok(cursor),
            Addr::Offset(d) => Ok(cursor.saturating_add_signed(*d)),
            Addr::VisualStart => marks
                .map(|m| m.0)
                .ok_or("'<: no visual range on this sheet"),
            Addr::VisualEnd => marks
                .map(|m| m.1)
                .ok_or("'>: no visual range on this sheet"),
            Addr::Last => total
                .map(|t| t.saturating_sub(1))
                .ok_or("$: the row count isn't known yet"),
        };
        let span = match &rows {
            Rows::Span(a, b) => match (line(a), line(b)) {
                (Ok(a), Ok(b)) => Some((a.min(b), a.max(b))),
                (Err(e), _) | (_, Err(e)) => {
                    self.toast = Some(e.into());
                    return;
                }
            },
            Rows::All => total.map(|t| (0, t.saturating_sub(1))),
            _ => None,
        };
        if let Some(keys) = cmd.strip_prefix("normal ") {
            match span {
                Some((a, b)) => self.normal_on_rows(a, b, keys),
                None => self.toast = Some(":normal takes a row range (:5,20 normal …)".into()),
            }
            return;
        }
        let column = self.current_column_name();
        let sheet = s.clone();
        self.spawn_job("range", move || {
            let e = |e: PolarsError| e.to_string();
            let rids = match rows {
                Rows::Span(..) | Rows::All => match span {
                    Some((a, b)) => sheet.rids_in_rows(a, b - a + 1).map_err(e)?,
                    None => sheet.rids_in_rows(0, u64::MAX >> 1).map_err(e)?,
                },
                Rows::Match { pattern, invert } => {
                    // `col=re` names the column when `col` is one
                    let (column, pattern) = match pattern.split_once('=') {
                        Some((c, p)) if sheet.has_column(c) => (c.to_string(), p.to_string()),
                        _ => (column.ok_or("no column")?, pattern),
                    };
                    sheet.rids_matching(&column, &pattern, invert).map_err(e)?
                }
                Rows::Where(pred) => sheet.rids_where(&pred)?,
            };
            Ok(JobDone::Ranged { rids, cmd })
        });
    }

    /// `cmd` with `rids` as the selection; the user's selection comes back
    /// after, less the rows `cmd` unselected (a delete) plus those it
    /// selected. No `cmd`: select them.
    pub(in crate::app) fn run_on_rows(&mut self, rids: Vec<u64>, cmd: &str) {
        let s = self.sheets.active_mut();
        if cmd.is_empty() {
            let n = rids.len();
            Arc::make_mut(&mut s.view.selected).extend(rids);
            self.toast = Some(format!("selected {n} rows"));
            return;
        }
        let saved = std::mem::replace(&mut s.view.selected, Arc::new(rids.into_iter().collect()));
        let ranged = s.view.selected.clone();
        let actions = match crate::exline::parse(cmd) {
            Ok(a) => a,
            Err(e) => {
                s.view.selected = saved;
                self.toast = Some(format!("{e}"));
                return;
            }
        };
        let (id, mark) = (s.id, s.undo_mark());
        // a row command on a range: its selected-rows form
        let actions = actions.into_iter().map(|a| match a {
            Action::DeleteRow => Action::DeleteSelected,
            Action::CopyRow => Action::CopySelected,
            Action::CutRow => Action::CutSelected,
            Action::SysCopyRow => Action::SysCopySelected,
            a => a,
        });
        // a macro and `.` hold the ranged command, not its selected-rows form
        let replaying = std::mem::replace(&mut self.replaying, true);
        self.dispatch_sequence(actions.collect());
        self.replaying = replaying;
        if let Some(s) = self.sheets.find_by_id_mut(id) {
            let now = &s.view.selected;
            let mut back: std::collections::BTreeSet<u64> = saved
                .iter()
                .filter(|r| !ranged.contains(r) || now.contains(r))
                .copied()
                .collect();
            back.extend(now.difference(&ranged));
            s.view.selected = Arc::new(back);
            // `U` goes back to the user's selection, not the range
            s.undo_selection_since(mark, &saved);
        }
    }

    /// `:5,20 normal <keys>`: the keys, typed on each row in turn. Only rows
    /// already loaded (the window): moving there must not wait for a load.
    // ponytail: keys run back to back; a key whose effect lands in a job
    // (a freq, a copy) sees each row's state when it started.
    pub(super) fn normal_on_rows(&mut self, from: u64, to: u64, keys: &str) {
        const MAX: u64 = 10_000;
        let keys = match normal_keys(keys) {
            Ok(k) => k,
            Err(e) => {
                self.toast = Some(format!("normal: {e}"));
                return;
            }
        };
        let s = self.sheets.active();
        let (lo, n) = (
            s.view.window_offset,
            s.window().map_or(0, |w| w.height() as u64),
        );
        if to - from + 1 > MAX || from < lo || to >= lo + n {
            self.toast = Some(format!(
                "normal: rows {}–{} aren't all loaded (rows {}–{} are, up to {MAX})",
                from + 1,
                to + 1,
                lo + 1,
                lo + n
            ));
            return;
        }
        let id = s.id;
        for row in from..=to {
            if self.sheets.active().id != id {
                break; // a key left the sheet
            }
            self.goto_row(row);
            for k in &keys {
                self.on_key(*k);
            }
        }
    }

    pub(in crate::app) fn here(&self) -> Jump {
        let s = self.sheets.active();
        Jump {
            sheet: s.id,
            row: s.view.window_offset + s.view.row_cursor as u64,
            column: self.current_column_name(),
        }
    }

    /// Before a big jump (S-26 A7): the position joins the jumplist, and
    /// what was ahead of a `jump_back` is dropped (vim).
    pub(in crate::app) fn record_jump(&mut self) {
        if let Some(i) = self.jump_at.take() {
            self.jumps.truncate(i);
        }
        let here = self.here();
        if self.jumps.last() != Some(&here) {
            self.jumps.push(here);
        }
        if self.jumps.len() > 100 {
            self.jumps.remove(0);
        }
    }

    pub(super) fn go_jump(&mut self, to: usize) {
        self.jump_at = Some(to);
        let j = self.jumps[to].clone();
        self.go_to(j);
    }

    /// The sheet, row and column of `j`.
    pub(in crate::app) fn go_to(&mut self, j: Jump) {
        if self.sheets.active().id != j.sheet {
            if !self.sheets.switch_to(j.sheet) {
                self.toast = Some("that sheet is closed".into());
                return;
            }
            self.spawn_materialize();
        }
        self.goto_row(j.row);
        if let Some(c) = j.column.filter(|c| self.sheets.active().has_column(c)) {
            self.goto_column(&c);
        }
    }
}
