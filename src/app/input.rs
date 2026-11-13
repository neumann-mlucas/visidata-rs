//! Keys and prompts: the keymap path, prompt editing, submitting, ex lines.

use super::*;

/// Tab in the `:` prompt: complete the command name under the cursor, vim
/// style. One match → the name and a space; several → their common prefix,
/// with the candidates (and their keys) as the returned toast. No name starts
/// with the text → names holding its letters in order (`srtd` → `sort-desc`).
/// Arguments aren't completed.
pub(super) fn complete_command(
    line: &mut LineEdit,
    lua: Vec<String>,
    key_of: impl Fn(&str) -> Option<String>,
) -> Option<String> {
    let (before, after) = line.split();
    if !after.is_empty() || before.contains(char::is_whitespace) {
        return None;
    }
    let mut all: Vec<String> = crate::exline::command_names()
        .into_iter()
        .chain(lua)
        .collect();
    all.sort();
    all.dedup();
    let prefixed: Vec<&String> = all.iter().filter(|n| n.starts_with(before)).collect();
    let fuzzy = prefixed.is_empty();
    let names: Vec<&String> = match fuzzy {
        false => prefixed,
        true => all.iter().filter(|n| subsequence(before, n)).collect(),
    };
    let shown = |n: &&String| match key_of(n) {
        Some(k) => format!("{n} ({k})"),
        None => n.to_string(),
    };
    match names.as_slice() {
        [] => Some(format!("no command matches {before:?}")),
        [one] => {
            line.set(&format!("{one} "));
            None
        }
        many if fuzzy => {
            const SHOWN: usize = 12;
            let more = if many.len() > SHOWN { " …" } else { "" };
            let list: Vec<String> = many.iter().take(SHOWN).map(shown).collect();
            Some(list.join(" ") + more)
        }
        many => {
            let first = &many[0];
            let common = many.iter().fold(first.len(), |n, s| {
                first
                    .bytes()
                    .zip(s.bytes())
                    .take(n)
                    .take_while(|(a, b)| a == b)
                    .count()
            });
            line.set(&first[..common]);
            const SHOWN: usize = 12;
            let more = if many.len() > SHOWN { " …" } else { "" };
            let list: Vec<String> = many.iter().take(SHOWN).map(shown).collect();
            Some(list.join(" ") + more)
        }
    }
}

/// `needle`'s chars appear in `hay` in order.
pub(super) fn subsequence(needle: &str, hay: &str) -> bool {
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

/// `=` input: `name = expr` names the column when `name` is a new column;
/// otherwise the whole text is the expression and names it (VisiData), so
/// `region = 'east'` on a sheet with `region` is a boolean column.
pub(super) fn derive_input(text: &str, exists: impl Fn(&str) -> bool) -> (String, String) {
    let text = text.trim();
    if let Some((name, expr)) = text.split_once('=') {
        let name = name.trim();
        let plain = !name.is_empty()
            && name.chars().all(|c| c.is_alphanumeric() || c == '_')
            && !expr.starts_with('=');
        if plain && !exists(name) && !expr.trim().is_empty() {
            return (name.to_string(), expr.trim().to_string());
        }
    }
    (text.to_string(), text.to_string())
}

pub(super) fn inject_count(actions: Vec<Action>, count: Option<usize>) -> Vec<Action> {
    if count.is_none() {
        return actions;
    }
    actions
        .into_iter()
        .enumerate()
        .map(|(i, a)| if i == 0 { a.with_count(count) } else { a })
        .collect()
}

impl App {
    pub(super) fn on_key(&mut self, key: Key) {
        // Toast lives until the next key — "clear on next action" without a timer.
        self.toast = None;
        if let Some((c, _)) = self.confirm.take() {
            if key.code == KeyCode::Char('y') {
                match c {
                    Confirm::Pop => self.pop_sheet(),
                    Confirm::Quit => self.quit = true,
                }
            } else {
                self.toast = Some("kept the edits".into());
            }
            return;
        }
        if self.input.is_some() {
            self.on_input_key(key);
            self.refresh_pivot_preview();
            return;
        }
        if self.operator.is_some() {
            self.operator_key(key);
            return;
        }
        if let Some(what) = self.awaiting.take() {
            self.awaited_key(what, key);
            return;
        }
        if self.visual.is_some() && key.code == KeyCode::Esc && self.pending.is_empty() {
            self.visual = None;
            return;
        }
        // Count prefix: digits accumulate when no chord is pending and the digit
        // isn't bound. `0` is bound to goto_leftmost, so it only extends a count.
        if self.pending.is_empty()
            && let KeyCode::Char(c) = key.code
            && let Some(d) = c.to_digit(10)
        {
            let d = d as usize;
            // A bound digit fires (`0`, `vd.bind`) unless a count is underway.
            let unbound = matches!(self.keymap.resolve(Layer::Sheet, &[], key), Dispatch::Miss);
            let accumulate = unbound || self.pending_count.is_some();
            if accumulate {
                let cur = self.pending_count.unwrap_or(0);
                self.pending_count = Some(cur.saturating_mul(10).saturating_add(d));
                return;
            }
        }
        match self.keymap.resolve(Layer::Sheet, &self.pending, key) {
            Dispatch::Fire(actions) => {
                self.pending.clear();
                self.pending_candidates.clear();
                let count = self.pending_count.take();
                let actions = inject_count(actions, count);
                self.dispatch_sequence(actions);
            }
            Dispatch::Pending(cands) => {
                self.pending.push(key);
                self.pending_candidates = cands;
            }
            Dispatch::Miss => {
                self.pending.clear();
                self.pending_candidates.clear();
                self.pending_count = None;
                self.toast = Some(format!("no binding: {key}"));
            }
        }
    }

    pub(super) fn on_input_key(&mut self, key: Key) {
        let Some((prompt, line)) = self.input.as_mut() else {
            return;
        };
        // Ex-mode history navigation: Up/Down walk history, not bound in keymap to keep layer clean.
        if *prompt == Prompt::Ex {
            let recalled = match key.code {
                KeyCode::Up => Some(self.ex_history.prev()),
                KeyCode::Down => Some(self.ex_history.next()),
                _ => None,
            };
            if let Some(r) = recalled {
                if let Some(text) = r {
                    line.set(text);
                }
                return;
            }
        }
        self.toast = None;
        if line.is_normal() {
            match key.code {
                KeyCode::Esc => self.cancel_input(),
                KeyCode::Enter => self.submit_input(),
                _ => line.normal_key(key),
            }
            return;
        }
        if *prompt == Prompt::Ex && key.code == KeyCode::Tab {
            let lua = self.plugins.as_ref().map(|p| p.command_names());
            let chords = self.keymap.chords(Layer::Sheet);
            let key_of = |name: &str| {
                let a = Action::from_name(name)?;
                let c = chords.iter().find(|c| c.run == [a.clone()])?;
                Some(
                    c.on.iter()
                        .map(|k| k.to_string())
                        .collect::<Vec<_>>()
                        .join(" "),
                )
            };
            self.toast = complete_command(line, lua.unwrap_or_default(), key_of);
            return;
        }
        if line.edit(key) {
            return;
        }
        match self.keymap.resolve(Layer::Input, &[], key) {
            Dispatch::Fire(actions) => {
                for a in actions {
                    match a {
                        Action::SubmitInput => self.submit_input(),
                        Action::CancelInput => self.cancel_input(),
                        Action::InputBackspace => {
                            if let Some((_, l)) = self.input.as_mut() {
                                l.backspace();
                            }
                        }
                        Action::InputNormal => {
                            if let Some((_, l)) = self.input.as_mut() {
                                l.enter_normal();
                            }
                        }
                        other @ (Action::Lua(_) | Action::ExCommand(_)) => self.dispatch(other),
                        _ => {}
                    }
                }
            }
            _ => {
                if let (KeyCode::Char(c), false) = (key.code, key.ctrl) {
                    line.insert(c);
                }
            }
        }
    }

    /// Keep the `:pivot` preview in step with the ex-line text.
    pub(super) fn refresh_pivot_preview(&mut self) {
        let text = match &self.input {
            Some((Prompt::Ex, line)) => line.text().to_string(),
            _ => {
                self.pivot_preview = None;
                return;
            }
        };
        let rest = match text.trim_start().strip_prefix("pivot") {
            Some(r) if r.is_empty() || r.starts_with(' ') => r.to_string(),
            _ => {
                self.pivot_preview = None;
                return;
            }
        };
        if self.pivot_preview.as_ref().is_some_and(|p| p.text == text) {
            return;
        }
        let s = self.sheets.active();
        // the window is gone while a new one loads: the schema still knows
        let columns: Vec<String> = match s.window() {
            Some(df) => df
                .get_column_names()
                .iter()
                .map(|n| n.to_string())
                .collect(),
            None => s.view_plan().collect_schema().map_or_else(
                |_| Vec::new(),
                |sc| sc.iter_names().map(|n| n.to_string()).collect(),
            ),
        };
        let args = crate::exline::split_args(&rest, PIVOT_USAGE).unwrap_or_default();
        let hint = |next: &str| {
            format!(
                "{PIVOT_USAGE}\n\nnext: {next}\n\ncolumns: {}\naggs: {}",
                columns.join(", "),
                PIVOT_AGGS
            )
        };
        // A half-typed column name (prefix of one, no space yet) isn't an error.
        let typing = !rest.ends_with(' ')
            && args.len() <= 3
            && args.last().is_some_and(|a| {
                !columns.contains(a) && columns.iter().any(|c| c.starts_with(a.as_str()))
            });
        let done = args.len() - usize::from(typing);
        let unknown = args.iter().take(done.min(3)).find(|a| !columns.contains(a));
        let body = match (done, unknown) {
            (_, Some(c)) => Err(format!("unknown column: {c}\n\n{}", hint("a column name"))),
            (0, _) => Err(hint("<rows>: column whose values label the rows")),
            (1, _) => Err(hint("<columns>: column whose values become new columns")),
            (2, _) => Err(hint("<values>: column to aggregate in each cell")),
            _ => {
                let (index, on, values) = (args[0].clone(), args[1].clone(), args[2].clone());
                let agg = args.get(3).cloned().unwrap_or_else(|| "sum".into());
                let mut sample = s.clone();
                sample.plan = s.view_plan().slice(0, PIVOT_PREVIEW_ROWS);
                sample.state.steps.clear(); // baked into the view, deletes too
                sample.view = Default::default();
                let lua = self.plugins.as_ref().and_then(|p| p.aggregator(&agg));
                let previous = self.pivot_preview.take().map(|p| p.body);
                let job_text = text.clone();
                self.spawn_job("preview", move || {
                    let result = match &lua {
                        Some(a) => lua_pivot(a, &sample, &index, &on, &values)
                            .map(|df| Box::new(preview_sheet(&sample, df))),
                        None => {
                            Sheet::pivot_from(&sample, 0, &index, &on, &values, &agg).map(Box::new)
                        }
                    };
                    Ok(JobDone::PivotPreview {
                        text: job_text,
                        result,
                    })
                });
                // Keep showing the last preview until this one lands.
                previous.unwrap_or_else(|| Err("computing…".into()))
            }
        };
        self.pivot_preview = Some(PivotPreview { text, body });
    }

    pub(super) fn start_input(&mut self, prompt: Prompt) {
        self.input = Some((prompt, LineEdit::default()));
    }

    pub(super) fn submit_input(&mut self) {
        let Some((prompt, line)) = self.input.take() else {
            return;
        };
        let text = line.text().to_string();
        // empty = null for edits
        if text.is_empty()
            && !matches!(
                prompt,
                Prompt::Ex | Prompt::Edit | Prompt::SetCol | Prompt::MacroKey | Prompt::Note
            )
        {
            return;
        }
        let search = matches!(prompt, Prompt::Search { .. } | Prompt::SearchExpr { .. });
        if let Some(rec) = self.recording.as_mut().filter(|_| search) {
            rec.refused.push("search".into());
            self.toast = Some("not recorded (no ex form): search".into());
        }
        match prompt {
            Prompt::Search {
                all_columns,
                backward,
            } => {
                if let Err(e) = regex::Regex::new(&text) {
                    self.toast = Some(format!("bad regex: {e}"));
                    return;
                }
                self.start_search(SearchState {
                    pattern: text,
                    all_columns,
                    backward,
                    expr: false,
                });
            }
            Prompt::SearchExpr { backward } => self.start_search(SearchState {
                pattern: text,
                all_columns: false,
                backward,
                expr: true,
            }),
            Prompt::Derive => {
                let (name, expr) = derive_input(&text, |n| self.sheets.active().has_column(n));
                self.dispatch_sequence(vec![Action::Derive { name, expr }]);
            }
            Prompt::SelectExpr { unselect } => self.dispatch_sequence(vec![Action::SelectExpr {
                expr: text,
                unselect,
            }]),
            Prompt::Filter => {
                if let Some(column) = self.current_column_name() {
                    self.dispatch_sequence(vec![Action::SetFilter {
                        column,
                        regex: text,
                    }]);
                } else {
                    self.toast = Some("no column to filter".into());
                }
            }
            Prompt::Ex => {
                self.ex_history.push(&text);
                self.run_ex(&text);
            }
            Prompt::MacroKey => self.finish_macro(&text),
            Prompt::Note => self.dispatch_sequence(vec![Action::SetNote { text }]),
            Prompt::AddCol => self.dispatch_sequence(vec![Action::AddCol { name: text }]),
            // VisiData's `regex_flags` default `I`, as the `*` prompt
            Prompt::Capture => self.dispatch_sequence(vec![Action::Capture {
                pattern: format!("(?i){text}"),
            }]),
            Prompt::Subst { mode } => {
                // the first `/` not escaped splits, as VisiData's one-liner
                let mut split = None;
                let mut esc = false;
                for (i, c) in text.char_indices() {
                    match c {
                        '\\' if !esc => esc = true,
                        '/' if !esc => {
                            split = Some(i);
                            break;
                        }
                        _ => esc = false,
                    }
                }
                let (pattern, replacement) = match split {
                    Some(i) => (&text[..i], &text[i + 1..]),
                    None => (text.as_str(), ""),
                };
                // VisiData's `regex_flags` default `I`: case-insensitive
                self.dispatch_sequence(vec![Action::SubstPrompted {
                    pattern: format!("(?i){}", pattern.replace("\\/", "/")),
                    replacement: crate::exline::vim_backrefs(replacement, true),
                    mode,
                }]);
            }
            Prompt::Select => self.dispatch_sequence(vec![Action::SelectByRegex { regex: text }]),
            Prompt::Rename => self.dispatch_sequence(vec![Action::RenameCol { new_name: text }]),
            Prompt::Diff => {
                let keys = self.sheets.active().view.keys.clone();
                self.dispatch_sequence(vec![Action::Diff { path: text, keys }]);
            }
            Prompt::Edit => self.dispatch_sequence(vec![Action::EditCell { value: text }]),
            Prompt::SetCol => self.dispatch_sequence(vec![Action::SetCol { value: text }]),
            Prompt::SetExpr => self.dispatch_sequence(vec![Action::SetExpr { expr: text }]),
            Prompt::AddRows => match text.trim().parse() {
                Ok(count) => self.dispatch_sequence(vec![Action::AddRows { count }]),
                Err(_) => self.toast = Some(format!("add rows: {text:?} isn't a number")),
            },
            Prompt::ExpandDepth { all } => match text.trim().parse() {
                Ok(depth) if all => self.dispatch_sequence(vec![Action::ExpandCols { depth }]),
                Ok(depth) => self.dispatch_sequence(vec![Action::ExpandCol { depth }]),
                Err(_) => self.toast = Some(format!("expand depth: {text:?} isn't a number")),
            },
            Prompt::Aggregate { memo, selected } => {
                let names = text;
                self.dispatch_sequence(vec![match (memo, selected) {
                    (true, _) => Action::MemoAggregate { names },
                    (_, true) => Action::AggregateCols { names },
                    _ => Action::AggregateCol { names },
                }]);
            }
        }
    }

    pub(super) fn cancel_input(&mut self) {
        if let Some((Prompt::MacroKey, _)) = self.input {
            self.toast = Some("still recording; m to stop".into());
        }
        self.input = None;
        self.ex_history.reset_cursor();
    }

    /// Run an ex command: a built-in or named action, else a `vd.command`.
    pub(super) fn run_ex(&mut self, text: &str) {
        if let Some(r) = crate::exline::ranged(text) {
            match r {
                Ok((rows, cmd)) => self.run_ranged(rows, cmd),
                Err(e) => self.fail(format!("{e}")),
            }
            return;
        }
        match crate::exline::parse(text) {
            Ok(actions) => self.dispatch_sequence(actions),
            Err(crate::exline::ExError::Unknown(name)) => {
                match self.plugins.as_ref().and_then(|p| p.command(&name)) {
                    Some(id) => {
                        let line = text.trim().trim_start_matches(':').trim_start();
                        let args = line[name.len()..].trim().to_string();
                        if !self.replaying
                            && let Some(rec) = self.recording.as_mut()
                        {
                            rec.steps.push(format!(":{line}"));
                        }
                        self.dispatch(Action::LuaCommand { id, args });
                    }
                    None => self.fail(format!("unknown command: :{name}")),
                }
            }
            Err(e) => self.fail(format!("{e}")),
        }
    }
}
