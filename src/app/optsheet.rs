//! `O` options sheet (S-37 P0c): every option with its value, default and
//! help. `e` on `value` sets it now (`:set`); `z<C-s>` writes the ones set
//! here to `options.lua`, as VisiData's options sheet commits to its config.

use super::*;

impl App {
    /// `O` / `:options-global`: the options sheet, cursor kept on a rebuild.
    pub(super) fn open_options(&mut self) {
        let rows: Vec<Vec<String>> = crate::options::OPTIONS
            .iter()
            .map(|o| {
                let help = match o.live {
                    true => o.help.to_string(),
                    false => format!("{} (from the next start)", o.help),
                };
                let from = self
                    .plugins
                    .as_ref()
                    .and_then(|p| p.options().from.get(o.name).cloned());
                vec![
                    o.name.into(),
                    self.option_value(o.name),
                    from.unwrap_or_else(|| "default".into()),
                    o.default.into(),
                    help,
                ]
            })
            .collect();
        let refresh = matches!(self.sheets.active().origin, Origin::Options);
        let cursor = self.sheets.active().view.row_cursor;
        if refresh {
            self.sheets.pop();
        }
        let id = self.sheets.alloc_id();
        let mut sheet = Sheet::listing(
            id,
            "options",
            &["option", "value", "from", "default", "description"],
            &rows,
        );
        sheet.origin = Origin::Options;
        if refresh {
            sheet.view.row_cursor = cursor;
            sheet.view.col_cursor = 1;
        }
        self.sheets.push(sheet);
        self.spawn_materialize();
    }

    /// On the options sheet: `true` when `action` was one of its own.
    pub(super) fn options_dispatch(&mut self, action: &Action) -> bool {
        let name = || {
            let s = self.sheets.active();
            s.cursor_value("option")
                .and_then(|v| v.get_str().map(String::from))
        };
        match action {
            Action::StartEditInput => {
                if self.current_column_name().as_deref() != Some("value") {
                    self.toast = Some("options: e edits value".into());
                    return true;
                }
                let Some(n) = name() else { return true };
                let text = self.option_value(&n);
                self.start_input(Prompt::Edit);
                if let Some((_, line)) = self.input.as_mut() {
                    line.set(&text);
                }
            }
            Action::EditCell { value } => {
                let Some(n) = name() else { return true };
                let msg = self.set_option(&n, Some(value));
                if !msg.starts_with(":set:") {
                    let v = self.option_value(&n);
                    self.option_edits.retain(|(o, _)| *o != n);
                    self.option_edits.push((n, v));
                    self.open_options();
                }
                self.toast = Some(msg);
            }
            Action::CommitSheet => {
                let edits = std::mem::take(&mut self.option_edits);
                let saved = match (edits.is_empty(), crate::plugins::options_path()) {
                    (true, _) => Err("nothing to commit".to_string()),
                    (false, None) => Err("no config dir".into()),
                    (false, Some(p)) => crate::plugins::save_options(&p, &edits)
                        .map(|()| p)
                        .map_err(|e| e.to_string()),
                };
                self.toast = Some(match saved {
                    Ok(p) => format!("options: {} saved to {}", edits.len(), p.display()),
                    Err(e) => {
                        self.option_edits = edits;
                        format!("options: {e}")
                    }
                });
            }
            _ => return false,
        }
        true
    }
}
