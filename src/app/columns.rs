//! Columns sheet write-back (S-25 P3): keys and edits on a `C` sheet change
//! the sheet(s) its rows describe; the columns sheet is then rebuilt.

use super::*;

impl App {
    /// `C` (`all = false`) / `gC`.
    pub(super) fn open_columns(&mut self, all: bool) {
        let id = self.sheets.alloc_id();
        let sheet = if all {
            let data: Vec<&Sheet> = self
                .sheets
                .iter()
                .filter(|s| !matches!(s.origin, Origin::Columns { .. }))
                .collect();
            Sheet::columns_all(&data, id)
        } else {
            Sheet::columns_of(self.sheets.active(), id)
        };
        self.sheets.push(sheet);
        self.spawn_materialize();
    }

    /// On a columns sheet: `true` when `action` was one of its own.
    pub(super) fn columns_dispatch(&mut self, action: &Action) -> bool {
        let selected = match action {
            Action::DeleteRow => false, // d: hide, as VisiData drops it
            Action::HideUniform | Action::DeleteSelected => true, // g- gd
            Action::KeySelected { .. } | Action::TypeSelected { .. } => true,
            Action::StartAggregateInput { memo, selected } => {
                self.start_input(Prompt::Aggregate {
                    memo: *memo,
                    selected: *selected,
                });
                return true;
            }
            Action::AggregateCols { names } => {
                let names = names.clone();
                let mut targets = self.columns_targets(true);
                if targets.is_empty() {
                    targets = self.columns_visible_nonkey();
                }
                self.columns_write(targets, None, |s, c| s.add_aggregators(c, &names));
                return true;
            }
            Action::StartEditInput => {
                self.columns_start_edit();
                return true;
            }
            Action::EditCell { value } => {
                let value = value.clone();
                self.columns_edit(&value, false);
                return true;
            }
            Action::StartSetColInput => {
                if self.columns_editable_field().is_some() {
                    self.start_input(Prompt::SetCol);
                }
                return true;
            }
            Action::SetCol { value } => {
                let value = value.clone();
                self.columns_edit(&value, true);
                return true;
            }
            Action::SubmitRow => {
                self.columns_freq();
                return true;
            }
            Action::Undo | Action::Redo => {
                self.columns_undo(matches!(action, Action::Undo));
                return true;
            }
            _ => return false,
        };
        let action = action.clone();
        let targets = self.columns_targets(selected);
        self.columns_write(targets, None, move |s, c| {
            match &action {
                Action::KeySelected { on: true } if !s.view.keys.iter().any(|k| k == c) => {
                    s.view.keys.push(c.into())
                }
                Action::KeySelected { on: true } => {}
                Action::KeySelected { on: false } => s.view.keys.retain(|k| k != c),
                Action::TypeSelected { name } => return s.cast_column(c, name),
                _ => {
                    s.view.hidden.insert(c.into());
                }
            }
            Ok(())
        });
        true
    }

    /// The sheets and columns the cursor row (or the selected rows) stand for.
    fn columns_targets(&self, selected: bool) -> Vec<(SheetId, String)> {
        let s = self.sheets.active();
        let Origin::Columns { cols } = &s.origin else {
            return Vec::new();
        };
        let rids: Vec<u64> = match selected {
            true => s.view.selected.iter().copied().collect(),
            false => self.rid_at_cursor().into_iter().collect(),
        };
        rids.iter()
            .filter_map(|r| cols.get(*r as usize).cloned())
            .collect()
    }

    /// The listed columns their sheet shows and doesn't key (`g+` with no
    /// rows selected, VisiData's `nonKeyVisibleCols`).
    fn columns_visible_nonkey(&self) -> Vec<(SheetId, String)> {
        let Origin::Columns { cols } = &self.sheets.active().origin else {
            return Vec::new();
        };
        cols.iter()
            .filter(|(id, c)| {
                self.sheets
                    .iter()
                    .any(|s| s.id == *id && !s.view.hidden.contains(c) && !s.view.keys.contains(c))
            })
            .cloned()
            .collect()
    }

    /// `f` on each target column's sheet, one undo step per sheet; on an
    /// error that sheet is left as it was. Then the columns sheet is rebuilt.
    /// `renamed`: the cursor column's new name, when `f` renames it.
    fn columns_write(
        &mut self,
        targets: Vec<(SheetId, String)>,
        renamed: Option<&str>,
        mut f: impl FnMut(&mut Sheet, &str) -> Result<(), String>,
    ) {
        if targets.is_empty() {
            self.toast = Some("no rows selected".into());
            return;
        }
        let mut ids: Vec<SheetId> = targets.iter().map(|(id, _)| *id).collect();
        ids.dedup();
        for id in ids {
            let Some(s) = self.sheets.find_by_id_mut(id) else {
                self.toast = Some("that sheet was closed".into());
                continue;
            };
            let snap = s.snapshot();
            let res = targets
                .iter()
                .filter(|(i, _)| *i == id)
                .try_for_each(|(_, c)| f(s, c));
            match res {
                Ok(()) => s.push_undo(snap),
                Err(e) => {
                    s.restore(snap);
                    self.toast = Some(e);
                }
            }
            s.invalidate(crate::sheet::Reason::Rows);
            s.state.stale = true;
        }
        self.rebuild_columns(renamed);
    }

    /// `U` / `R` on a columns sheet: on the sheet it lists.
    fn columns_undo(&mut self, undo: bool) {
        let mut moved = false;
        for id in self.columns_sheet_ids() {
            if let Some(s) = self.sheets.find_by_id_mut(id)
                && s.time_travel(undo)
            {
                s.state.stale = true;
                moved = true;
            }
        }
        if !moved {
            self.toast = Some(format!("nothing to {}", if undo { "undo" } else { "redo" }));
        }
        self.rebuild_columns(None);
    }

    fn columns_sheet_ids(&self) -> Vec<SheetId> {
        let Origin::Columns { cols } = &self.sheets.active().origin else {
            return Vec::new();
        };
        let mut ids: Vec<SheetId> = cols.iter().map(|(id, _)| *id).collect();
        ids.dedup();
        ids
    }

    /// The columns sheet again from its sheets; cursor and selection stay on
    /// the same columns (keying moves rows), `renamed` = the cursor column's
    /// new name.
    // ponytail: follows by base position, so a sorted columns sheet's cursor
    // lands by position; map through the view order if that bites.
    fn rebuild_columns(&mut self, renamed: Option<&str>) {
        let ids = self.columns_sheet_ids();
        let mut cur = self.columns_targets(false);
        let sel = self.columns_targets(true);
        if let (Some(c), Some(n)) = (cur.first_mut(), renamed) {
            c.1 = n.to_string();
        }
        let s = self.sheets.active();
        let (id, mut view, all) = (s.id, s.view.clone(), s.name == "all_columns");
        let data: Vec<&Sheet> = self.sheets.iter().filter(|s| ids.contains(&s.id)).collect();
        let mut new = match (all, data.first()) {
            (true, _) => Sheet::columns_all(&data, id),
            (false, Some(one)) => Sheet::columns_of(one, id),
            (false, None) => return,
        };
        if let Origin::Columns { cols } = &new.origin {
            let at = |t: &(SheetId, String)| cols.iter().position(|c| c == t);
            if let Some(i) = cur.first().and_then(at) {
                view.row_cursor = i;
            }
            view.selected =
                std::sync::Arc::new(sel.iter().filter_map(at).map(|i| i as u64).collect());
        }
        new.view = view;
        *self.sheets.active_mut() = new;
        self.spawn_materialize();
    }

    /// `e` on a columns sheet: the cell, for `name width type value aggregators`.
    fn columns_start_edit(&mut self) {
        let Some(field) = self.columns_editable_field() else {
            return;
        };
        let s = self.sheets.active();
        let text = s
            .window()
            .and_then(|df| df.column(&field).ok()?.get(s.view.row_cursor).ok())
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    crate::sheet::any_text(&v)
                }
            })
            .unwrap_or_default();
        self.start_input(Prompt::Edit);
        if let Some((_, line)) = self.input.as_mut() {
            line.set(&text);
        }
    }

    /// The current column, when `e` / `ge` may write it: toasts otherwise.
    fn columns_editable_field(&mut self) -> Option<String> {
        let field = self.current_column_name()?;
        if !matches!(
            field.as_str(),
            "name" | "width" | "type" | "value" | "aggregators"
        ) {
            self.toast = Some(format!("{field}: not editable here"));
            return None;
        }
        Some(field)
    }

    /// `e` (the cursor row) / `ge` (`selected` rows): the typed `text` for
    /// the current field, written to each row's sheet.
    fn columns_edit(&mut self, text: &str, selected: bool) {
        let Some(field) = self.columns_editable_field() else {
            return;
        };
        let text = text.trim().to_string();
        let renamed = (field == "name" && !selected).then_some(text.as_str());
        let targets = self.columns_targets(selected);
        self.columns_write(targets, renamed, |s, c| match field.as_str() {
            "name" => s.rename_col(c, &text),
            "width" => match text.parse::<u32>() {
                Ok(0) => {
                    s.view.hidden.insert(c.into());
                    Ok(())
                }
                Ok(_) => {
                    s.view.hidden.remove(c);
                    Ok(())
                }
                Err(_) => Err(format!("width: {text:?} isn't a number")),
            },
            "type" => s.cast_column(c, &text),
            "value" => {
                let rid = s.window_rids().get(s.view.row_cursor).copied();
                let rid = rid.ok_or("value: no row under the sheet's cursor")?;
                s.editable()?;
                s.edit_cell(c, rid, &text)
            }
            "aggregators" => s.set_aggregators(c, &text),
            other => Err(format!("{other}: not editable here")),
        });
    }

    /// `Enter` on a columns sheet: the frequency table of that column.
    fn columns_freq(&mut self) {
        let Some((sid, c)) = self.columns_targets(false).pop() else {
            return;
        };
        let id = self.sheets.alloc_id();
        let Some(parent) = self.sheets.iter().find(|s| s.id == sid) else {
            self.toast = Some("that sheet was closed".into());
            return;
        };
        let freq = Sheet::freq_from(parent, id, &c);
        self.sheets.push(freq);
        self.spawn_materialize();
    }
}
