//! `gm` macros sheet (S-36 E): one row per macro binding. `d` marks a macro
//! for deletion and `e` on `keys` rebinds it; nothing changes until
//! `z<C-s>` commits, as VisiData's `commit-sheet` (K5).

use super::*;

/// A pending change to the macro on `keys`: `None` deletes it, else its new keys.
pub(super) type MacroEdit = (Vec<Key>, Option<Vec<Key>>);

/// A macro step as `m` records it: an action name, or `:ex`.
pub(super) fn step_text(a: &Action) -> String {
    match a {
        Action::ExCommand(c) => format!(":{c}"),
        _ => match a.to_ex() {
            Some(ex) if Action::from_name(&ex).is_some() => ex,
            Some(ex) => format!(":{ex}"),
            None => format!("{a:?}"),
        },
    }
}

fn keys_text(keys: &[Key]) -> String {
    keys.iter()
        .map(Key::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

impl App {
    /// Macro bindings in the sheet layer, the binding that wins for its keys
    /// only: (keys, steps).
    fn macros(&self) -> Vec<(Vec<Key>, Vec<Action>)> {
        let chords = self.keymap.chords(Layer::Sheet);
        let mut out: Vec<(Vec<Key>, Vec<Action>)> = Vec::new();
        for (i, c) in chords.iter().enumerate() {
            let [Action::Macro(steps)] = c.run.as_slice() else {
                continue;
            };
            if chords[i + 1..].iter().any(|later| later.on == c.on) {
                continue; // shadowed
            }
            out.push((c.on.clone(), steps.clone()));
        }
        out
    }

    /// `gm` / `:macros`: the macros sheet, with pending changes in `change`.
    pub(super) fn open_macros(&mut self) {
        let saved = crate::plugins::macros_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let rows: Vec<Vec<String>> = self
            .macros()
            .into_iter()
            .map(|(keys, steps)| {
                let texts: Vec<String> = steps.iter().map(step_text).collect();
                let prefix = crate::plugins::macro_prefix(&keys);
                let source = match saved.lines().any(|l| l.starts_with(&prefix)) {
                    true => "macros.lua",
                    false => "init.lua",
                };
                let change = match self.macro_edits.iter().find(|(k, _)| *k == keys) {
                    Some((_, None)) => "delete".to_string(),
                    Some((_, Some(to))) => format!("bind {}", keys_text(to)),
                    None => String::new(),
                };
                vec![
                    keys_text(&keys),
                    steps.len().to_string(),
                    texts.first().cloned().unwrap_or_default(),
                    source.into(),
                    change,
                ]
            })
            .collect();
        let cursor = self.sheets.active().view.row_cursor;
        let refresh = matches!(self.sheets.active().origin, Origin::Macros);
        if refresh {
            self.sheets.pop();
        }
        let id = self.sheets.alloc_id();
        let mut sheet = Sheet::listing(
            id,
            "macros",
            &["keys", "steps", "first", "source", "change"],
            &rows,
        );
        sheet.origin = Origin::Macros;
        if refresh {
            sheet.view.row_cursor = cursor.min(rows.len().saturating_sub(1));
        }
        self.sheets.push(sheet);
        self.spawn_materialize();
    }

    /// On the macros sheet: `true` when `action` was one of its own.
    pub(super) fn macros_dispatch(&mut self, action: &Action) -> bool {
        match action {
            Action::DeleteRow => self.mark_macro(None),
            Action::StartEditInput => {
                if self.current_column_name().as_deref() != Some("keys") {
                    self.toast =
                        Some("macros: e edits keys (record again with m to change steps)".into());
                    return true;
                }
                let text = self
                    .cursor_macro()
                    .map(|(k, _)| keys_text(&k))
                    .unwrap_or_default();
                self.start_input(Prompt::Edit);
                if let Some((_, line)) = self.input.as_mut() {
                    line.set(&text);
                }
            }
            Action::EditCell { value } => {
                let keys: Result<Vec<Key>, _> = value.split_whitespace().map(str::parse).collect();
                match keys {
                    Ok(k) if !k.is_empty() => self.mark_macro(Some(k)),
                    Ok(_) => self.toast = Some("macros: no keys; d deletes".into()),
                    Err(e) => self.toast = Some(format!("bad key: {e}")),
                }
            }
            Action::SubmitRow => {
                let Some((keys, steps)) = self.cursor_macro() else {
                    return true;
                };
                let rows: Vec<Vec<String>> = steps.iter().map(|a| vec![step_text(a)]).collect();
                let id = self.sheets.alloc_id();
                let name = format!("macro {}", keys_text(&keys));
                self.sheets
                    .push(Sheet::listing(id, &name, &["step"], &rows));
                self.spawn_materialize();
            }
            Action::CommitSheet => self.commit_macros(),
            _ => return false,
        }
        true
    }

    /// The macro on the cursor row.
    fn cursor_macro(&self) -> Option<(Vec<Key>, Vec<Action>)> {
        let keys = self.sheets.active().cursor_value("keys")?;
        let keys = keys.get_str()?.to_string();
        self.macros()
            .into_iter()
            .find(|(k, _)| keys_text(k) == keys)
    }

    /// `d` (`to` None, toggles) or `e` on the cursor macro: a pending change.
    fn mark_macro(&mut self, to: Option<Vec<Key>>) {
        let Some((keys, _)) = self.cursor_macro() else {
            return;
        };
        let source = self.sheets.active().cursor_value("source");
        if source.as_ref().and_then(|v| v.get_str()) != Some("macros.lua") {
            self.toast = Some("macros: defined in init.lua; change it there".into());
            return;
        }
        if let Some(to) = &to
            && self.keymap.chords(Layer::Sheet).iter().any(|c| c.on == *to)
        {
            self.toast = Some(format!("macros: {} is bound already", keys_text(to)));
            return;
        }
        let old = self.macro_edits.iter().position(|(k, _)| *k == keys);
        match (old, to) {
            (Some(i), None) if self.macro_edits[i].1.is_none() => {
                self.macro_edits.remove(i); // d again: unmarked
            }
            (Some(i), to) => self.macro_edits[i].1 = to,
            (None, to) => self.macro_edits.push((keys, to)),
        }
        self.open_macros();
    }

    /// `z<C-s>`: apply the pending changes to the keymap and `macros.lua`.
    fn commit_macros(&mut self) {
        let edits = std::mem::take(&mut self.macro_edits);
        if edits.is_empty() {
            self.toast = Some("macros: nothing to commit".into());
            return;
        }
        let Some(path) = crate::plugins::macros_path() else {
            self.toast = Some("macros: no config dir".into());
            return;
        };
        let mut err = None;
        for (keys, to) in &edits {
            let Some(chord) = self.keymap.remove_last(Layer::Sheet, keys) else {
                continue;
            };
            if let Err(e) = crate::plugins::remove_macro(&path, keys) {
                err = Some(e.to_string());
            }
            if let Some(to) = to {
                let [Action::Macro(steps)] = chord.run.as_slice() else {
                    continue;
                };
                let texts: Vec<String> = steps.iter().map(step_text).collect();
                if let Err(e) = crate::plugins::save_macro(&path, to, &texts) {
                    err = Some(e.to_string());
                }
                self.keymap.append(
                    Layer::Sheet,
                    Chord {
                        on: to.clone(),
                        ..chord
                    },
                );
            }
        }
        self.toast = Some(match err {
            None => format!("macros: {} committed", edits.len()),
            Some(e) => format!("macros: committed; not saved: {e}"),
        });
        self.open_macros();
    }
}
