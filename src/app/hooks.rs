//! Lua calls and events, and macro recording.

use super::*;

impl App {
    /// `vd.sheet()` of the active sheet, for a Lua callback.
    pub(super) fn with_ctx<R>(
        &self,
        f: impl FnOnce(&crate::plugins::Plugins, &crate::plugins::SheetCtx) -> R,
    ) -> Option<R> {
        let p = self.plugins.as_ref()?;
        let s = self.sheets.active();
        let v = &s.view;
        let df = s.window();
        let ctx = crate::plugins::SheetCtx {
            name: &s.name,
            row: v.window_offset + v.row_cursor as u64 + 1,
            col: v.col_cursor + 1,
            column_names: df.map_or_else(Vec::new, |df| {
                df.get_column_names()
                    .iter()
                    .map(|n| n.to_string())
                    .collect()
            }),
            value: df
                .and_then(|df| df.columns().get(v.col_cursor)?.get(v.row_cursor).ok())
                .unwrap_or(AnyValue::Null),
        };
        Some(f(p, &ctx))
    }

    /// Run `event`'s `vd.on` handlers on the active sheet; their `vd.exec`
    /// commands queue as macro steps, their messages go to the status line.
    pub(super) fn fire_event(&mut self, event: &str, extra: &[(&str, String)]) {
        if !self.plugins.as_ref().is_some_and(|p| p.handles(event)) {
            return;
        }
        let Some((out, errors)) = self.with_ctx(|p, ctx| p.fire(event, ctx, extra)) else {
            return;
        };
        let mut msgs = errors;
        for cmd in out.execs {
            let name = cmd.split_whitespace().next().unwrap_or_default();
            if event == "open" && matches!(name, "open" | "o" | "tail" | "source") {
                msgs.push(format!("vd.on open: :{name} would open again; skipped"));
                continue;
            }
            self.macro_queue.push_back((Action::ExCommand(cmd), true));
        }
        msgs.extend(out.toasts);
        if !out.sheets.is_empty() {
            msgs.push("vd.push_sheet works in vd.bind / vd.command functions".into());
        }
        if !msgs.is_empty() {
            self.toast = Some(msgs.join(" · "));
        }
    }

    /// `open` / `push` for a new sheet on top, once its first window is in
    /// (so `vd.sheet()` has columns). Sheets hook commands made fire nothing.
    pub(super) fn fire_push_hooks(&mut self) -> bool {
        let s = self.sheets.active();
        if self.hooked.is_some_and(|h| s.id <= h) || !s.is_loaded() {
            return false;
        }
        self.hooked = Some(s.id);
        if self.hook_born.iter().any(|r| r.contains(&s.id)) {
            return false;
        }
        let origin = s.origin.name();
        let path = s
            .source
            .path
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let format = format!("{:?}", s.kind).to_lowercase();
        if matches!(
            s.origin,
            Origin::File | Origin::Tail { .. } | Origin::Parts { .. }
        ) {
            self.fire_event(
                "open",
                &[("path", path.clone()), ("format", format.clone())],
            );
        }
        self.fire_event(
            "push",
            &[
                ("path", path),
                ("format", format),
                ("origin", origin.into()),
            ],
        );
        true
    }

    /// Record what a key or prompt resolved to (S-19); `None` = recorded,
    /// else the refused action, named.
    pub(super) fn record(&mut self, a: &Action) -> Option<String> {
        self.recording.as_ref()?;
        if let Action::Macro(steps) = a {
            // M2: a macro played while recording is recorded as its steps.
            let mut refused = None;
            for s in steps.clone() {
                refused = self.record(&s).or(refused);
            }
            return refused;
        }
        if !a.repeatable() {
            return None;
        }
        let rec = self.recording.as_mut()?;
        match a.to_ex() {
            Some(ex) if Action::from_name(&ex).is_some() => rec.steps.push(ex),
            Some(ex) => rec.steps.push(format!(":{ex}")),
            None => {
                let name = format!("{a:?}");
                let end = name.find([' ', '(', '{']).unwrap_or(name.len());
                rec.refused.push(name[..end].to_string());
                return Some(name[..end].to_string());
            }
        }
        None
    }

    /// `m` stopped: bind the recording to `keys` and save it to `macros.lua`.
    pub(super) fn finish_macro(&mut self, text: &str) {
        let Some(Recording { steps, refused, .. }) = self.recording.take() else {
            return;
        };
        if text.trim().is_empty() || steps.is_empty() {
            self.toast = Some("macro discarded".into());
            return;
        }
        let keys: Result<Vec<Key>, _> = text.split_whitespace().map(str::parse::<Key>).collect();
        let keys = match keys {
            Ok(k) => k,
            Err(e) => {
                self.toast = Some(format!("bad key: {e}; m to try again"));
                self.recording = Some(Recording {
                    steps,
                    refused,
                    into: None,
                });
                return;
            }
        };
        let run = macro_actions(&steps);
        self.keymap.append(
            Layer::Sheet,
            Chord {
                on: keys.clone(),
                run: vec![Action::Macro(run)],
                desc: "macro".into(),
            },
        );
        let saved = crate::plugins::macros_path()
            .ok_or_else(|| "no config dir".to_string())
            .and_then(|p| crate::plugins::save_macro(&p, &keys, &steps).map_err(|e| e.to_string()));
        let mut msg = format!("macro on {}: {} steps", text.trim(), steps.len());
        if !refused.is_empty() {
            msg += &format!(", not recorded: {}", refused.join(", "));
        }
        msg += &match saved {
            Ok(()) => "; saved".to_string(),
            Err(e) => format!("; not saved: {e}"),
        };
        self.toast = Some(msg);
    }

    /// The key after `"` `m` `'` `q` `@` (S-26 D2/D4).
    pub(super) fn awaited_key(&mut self, what: Await, key: Key) {
        let c = match key.code {
            KeyCode::Char(c) if !key.ctrl => c,
            _ => {
                self.toast = Some(format!("not a register or mark: {key}"));
                return;
            }
        };
        match (what, c) {
            (Await::Register, 'a'..='z' | '0'..='9' | '"' | '+') => {
                self.register_prefix = Some(c);
                self.toast = Some(format!("register \"{c}"));
            }
            (Await::SetMark, 'a'..='z') => {
                self.marks.insert(c, self.here());
                self.toast = Some(format!("mark {c}"));
            }
            (Await::GoMark, 'a'..='z') => match self.marks.get(&c).cloned() {
                Some(j) => {
                    self.record_jump();
                    self.go_to(j);
                }
                None => self.toast = Some(format!("no mark {c}")),
            },
            (Await::Record, 'a'..='z') => {
                self.recording = Some(Recording {
                    into: Some(c),
                    ..Default::default()
                });
                self.toast = Some(format!("recording @{c}; q to stop"));
            }
            (Await::Play, 'a'..='z' | '@') => {
                let Some(c) = (if c == '@' { self.last_macro } else { Some(c) }) else {
                    self.toast = Some("no macro played yet".into());
                    return;
                };
                match self.macros.get(&c) {
                    Some(steps) => {
                        self.last_macro = Some(c);
                        let run = macro_actions(steps);
                        self.dispatch(Action::Macro(run));
                    }
                    None => self.toast = Some(format!("register {c} holds no macro")),
                }
            }
            _ => self.toast = Some(format!("not a register or mark: {key}")),
        }
    }

    /// A Lua callback (`vd.bind`, or `vd.command` with `args`): `vd.sheet()`
    /// describes the active sheet's cursor.
    pub(super) fn run_lua(&mut self, id: usize, args: Option<&str>) {
        let out = match self.with_ctx(|p, ctx| p.call(id, ctx, args)) {
            None => return,
            Some(Ok(out)) => out,
            Some(Err(e)) => {
                self.toast = Some(format!("{e}"));
                return;
            }
        };
        let parent = self.sheets.active().id;
        for (name, df) in out.sheets {
            let id = self.sheets.alloc_id();
            let origin = Origin::Lua { parent };
            let child = Sheet::child(self.sheets.active(), id, name, df.lazy(), origin);
            self.sheets.push(child);
            self.spawn_materialize();
        }
        for cmd in out.execs {
            self.macro_queue.push_back((Action::ExCommand(cmd), false));
        }
        if !out.toasts.is_empty() {
            self.toast = Some(out.toasts.join(" · "));
        }
    }
}
