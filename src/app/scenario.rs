//! End-to-end: real keymap + dispatch + background Polars tasks + render.
use super::*;
use std::io::Write;
use unicode_width::UnicodeWidthStr;

mod edit;
mod jobs;
mod lua;
mod sheets;
mod tail;
mod view;
mod vim;

pub struct Harness {
    pub app: App,
    pub term: Terminal<ratatui::backend::TestBackend>,
    _tmp: Option<tempfile::NamedTempFile>,
}

impl Harness {
    /// `rows`-row CSV: id,name,grp,amt with id = 1..=rows.
    pub async fn new(rows: usize) -> Self {
        let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
        writeln!(tmp, "id,name,grp,amt").unwrap();
        for i in 1..=rows {
            writeln!(tmp, "{i},name_{i},{},{}", i % 7, i as f64 * 1.5).unwrap();
        }
        tmp.flush().unwrap();
        let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
        Self::with_source(loaded, tmp).await
    }

    pub async fn with_source(
        loaded: crate::reader::LoadedSource,
        tmp: tempfile::NamedTempFile,
    ) -> Self {
        let mut app = App::new(Sheet::from_source(0, loaded), None, None);
        app.spawn_materialize();
        let mut h = Self::from_app(app).await;
        h._tmp = Some(tmp);
        h
    }

    pub async fn from_app(app: App) -> Self {
        let term = Terminal::new(ratatui::backend::TestBackend::new(100, 12)).unwrap();
        let mut h = Self {
            app,
            term,
            _tmp: None,
        };
        h.pump().await;
        h
    }

    /// Apply background results until the app is idle, then draw.
    pub async fn pump(&mut self) {
        // Generous timeout: parallel tests share CPUs; settled() ends it early.
        loop {
            self.app.fire_push_hooks();
            self.app.ask_sel_stats();
            self.app.follow_link();
            self.app.feed_other_pane();
            self.app.pump_macro();
            if self.app.settled() {
                break;
            }
            let next = self.app.materialize_rx.recv();
            match tokio::time::timeout(Duration::from_secs(20), next).await {
                Ok(Some(res)) => self.app.on_materialize(res),
                _ => break,
            }
        }
        // stale results (superseded counts) may still be queued; apply them too
        while let Ok(res) = self.app.materialize_rx.try_recv() {
            self.app.on_materialize(res);
        }
        self.app.draw(&mut self.term).unwrap();
    }

    /// The terminal pastes `text` (bracketed paste).
    pub async fn paste(&mut self, text: &str) {
        self.app.on_paste(text.into());
        self.pump().await;
    }

    /// Space-separated key tokens, e.g. "g g", "<C-d>", or literal text via `type_`.
    pub async fn keys(&mut self, ks: &str) {
        for k in ks.split(' ').filter(|k| !k.is_empty()) {
            self.app.on_key(k.parse().unwrap());
            self.app.draw(&mut self.term).unwrap();
        }
        self.pump().await;
    }

    pub async fn type_(&mut self, text: &str) {
        self.type_no_enter(text).await;
        self.keys("<CR>").await;
    }

    /// Type into the open prompt without submitting; waits for jobs.
    pub async fn type_no_enter(&mut self, text: &str) {
        for c in text.chars() {
            self.app
                .on_key(Key::from(crossterm::event::KeyEvent::from(KeyCode::Char(
                    c,
                ))));
        }
        self.pump().await;
        self.app.draw(&mut self.term).unwrap();
    }

    /// Name of the column under the cursor.
    pub fn current_column(&self) -> String {
        self.app.current_column_name().unwrap_or_default()
    }

    pub fn screen(&self) -> String {
        let buf = self.term.backend().buffer();
        (0..buf.area.height)
            .map(|y| {
                // Like a terminal: a wide symbol covers the next cell too.
                let mut line = String::new();
                let mut x = 0;
                while x < buf.area.width {
                    let sym = buf[(x, y)].symbol();
                    line.push_str(sym);
                    x += sym.width().max(1) as u16;
                }
                line.trim_end().to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Harness over a CSV holding exactly `bytes`.
async fn csv(bytes: &[u8]) -> Harness {
    let mut tmp = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    tmp.write_all(bytes).unwrap();
    tmp.flush().unwrap();
    let loaded = crate::reader::open(tmp.path().as_os_str(), None).unwrap();
    Harness::with_source(loaded, tmp).await
}

/// Every row of `sheet`'s view as delimited text.
pub fn csv_text(sheet: &Sheet, sep: u8) -> String {
    let mut df = sheet.view_plan().collect().unwrap();
    let mut out = Vec::new();
    CsvWriter::new(&mut out)
        .with_separator(sep)
        .finish(&mut df)
        .unwrap();
    String::from_utf8(out).unwrap()
}

fn title(h: &Harness) -> String {
    // Thousands separators aside, tests read plain numbers.
    h.screen()
        .lines()
        .next()
        .unwrap_or_default()
        .replace(',', "")
}

/// First data row (below the header), first cell.
fn first_cell(h: &Harness) -> String {
    let screen = h.screen();
    let line = screen.lines().nth(2).unwrap_or_default();
    line.split_whitespace()
        .next()
        .unwrap_or_default()
        .to_string()
}

fn status(h: &Harness) -> String {
    h.screen().lines().last().unwrap_or_default().to_string()
}

async fn tail_sheet(h: &mut Harness, path: &std::path::Path) {
    h.keys(":").await;
    h.type_(&format!("tail {}", path.display())).await;
}

fn view_csv(h: &Harness) -> String {
    csv_text(h.app.sheets.active(), b',')
}

/// Harness with `lua` loaded before the first sheet's `open` event.
async fn with_lua(rows: usize, lua: &str) -> Harness {
    let mut h = Harness::new(rows).await;
    h.app.plugins = Some(crate::plugins::Plugins::load_from_source(lua).unwrap());
    h.app.keymap = keymap_and_theme(h.app.plugins.as_ref()).0;
    h.app.hooked = None;
    h.pump().await;
    h
}

/// `S`, then `<CR>` on the first sheet whose name starts with `name`.
async fn raise(h: &mut Harness, name: &str) {
    let row = h
        .app
        .sheets
        .iter()
        .position(|s| s.name.starts_with(name))
        .unwrap();
    h.keys("S").await;
    h.keys(&format!("{}<CR>", "j ".repeat(row))).await;
    assert!(title(h).contains(name), "{}", title(h));
}

/// Foreground of the first screen cell of `text` on the line that holds it.
fn fg_of(h: &Harness, text: &str) -> ratatui::style::Color {
    cell_at(h, text).fg
}

/// The first screen cell of word `text`, on the first data line that holds it.
fn cell_at(h: &Harness, text: &str) -> ratatui::buffer::Cell {
    let screen = h.screen();
    let (y, line) = screen
        .lines()
        .enumerate()
        .skip(1)
        .find(|(_, l)| l.split_whitespace().any(|w| w == text))
        .unwrap_or_else(|| panic!("{text} not on screen:\n{screen}"));
    let x = line
        .match_indices(text)
        .map(|(i, _)| i)
        .find(|&i| {
            (i == 0 || line[..i].ends_with(' '))
                && line[i + text.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| c == ' ')
        })
        .unwrap_or_default();
    h.term.backend().buffer()[(x as u16, y as u16)].clone()
}
