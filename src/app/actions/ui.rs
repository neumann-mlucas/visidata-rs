//! The app around the sheets: quit, macros, help, prompts, sidebar, handlers.

use super::*;

impl App {
    pub(super) fn quit(&mut self) {
        let n: usize =
            self.sheets.iter().map(Sheet::unsaved_edits).sum::<usize>() + self.macro_edits.len();
        if n == 0 {
            self.quit = true;
        } else {
            self.ask(Confirm::Quit, format!("quit and discard {}?", edits(n)));
        }
    }

    pub(super) fn run_macro(&mut self, steps: Vec<Action>) {
        // a macro a replayed step calls runs next; a key pressed while
        // a replay still waits on a job queues behind it
        if self.replaying {
            for a in steps.into_iter().rev() {
                self.macro_queue.push_front((a, false));
            }
        } else {
            self.macro_queue
                .extend(steps.into_iter().map(|a| (a, false)));
        }
        self.pump_macro();
    }

    pub(super) fn help_commands(&mut self) {
        use crate::keymap::{Keymap, Layer, menu_path};
        let defaults = Keymap::defaults();
        let defaults = defaults.chords(Layer::Sheet);
        let rows: Vec<_> = self
            .keymap
            .chords(Layer::Sheet)
            .iter()
            .map(|c| {
                let keys: Vec<String> = c.on.iter().map(|k| k.to_string()).collect();
                let keys = keys.join(" ");
                let run: Vec<String> = c.run.iter().filter_map(Action::to_ex).collect();
                // VisiData's menu, while the key still does what VisiData's does
                let stock = defaults.iter().any(|d| d.on == c.on && d.run == c.run);
                let menu = menu_path(&c.on).filter(|_| stock).unwrap_or_default();
                vec![keys, run.join(" ; "), c.desc.to_string(), menu.to_string()]
            })
            .collect();
        let id = self.sheets.alloc_id();
        let names = ["key", "action", "description", "menu"];
        self.sheets.push(Sheet::listing(id, "help", &names, &rows));
        self.spawn_materialize();
    }

    pub(super) fn ex_prompt(&mut self, cmd: &'static str) {
        let mut text = format!("{cmd} ");
        if cmd == "join"
            && let Some(below) = self.sheets.iter().rev().nth(1)
        {
            text += &crate::action::quote_arg(&below.name);
            text.push(' ');
        }
        self.start_input(Prompt::Ex);
        if let Some((_, line)) = &mut self.input {
            line.set(&text);
        }
    }

    pub(super) fn cycle_sidebar(&mut self) {
        let lua = self
            .plugins
            .as_ref()
            .map_or_else(Vec::new, |p| p.page_names());
        let page = match &self.sidebar {
            Some(p) => p.next(&lua),
            None => self.sidebar_page.clone(),
        };
        (self.sidebar, self.sidebar_page) = (Some(page.clone()), page);
    }

    pub(super) fn show_sidebar(&mut self, page: SidebarPage) {
        let lua = self
            .plugins
            .as_ref()
            .map_or_else(Vec::new, |p| p.page_names());
        match &page {
            SidebarPage::Lua(n) if !lua.contains(n) => {
                let names = ["column", "cell", "help", "plan", "plan-opt"]
                    .map(String::from)
                    .into_iter()
                    .chain(lua);
                self.toast = Some(format!(
                    "no sidebar page {n} ({})",
                    names.collect::<Vec<_>>().join(", ")
                ));
            }
            _ => (self.sidebar, self.sidebar_page) = (Some(page.clone()), page),
        }
    }

    pub(super) fn handlers(&mut self) {
        let rows = self
            .plugins
            .as_ref()
            .map(|p| p.handler_rows())
            .unwrap_or_default();
        let id = self.sheets.alloc_id();
        let names = ["id", "event", "group", "pattern", "once"];
        self.sheets
            .push(Sheet::listing(id, "handlers", &names, &rows));
        self.spawn_materialize();
    }
}
