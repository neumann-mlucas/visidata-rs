#![expect(
    clippy::unwrap_used,
    reason = "test helpers fail the test by panicking"
)]
//! The real binary on a pseudo-terminal must draw what the in-process golden
//! harness draws: each `tests/golden/docs-*` script is typed into `vdrs
//! <fixture>` (100x10) and the vt100-parsed screen must reach
//! `expected.screen`, the colors included at the cursor cell.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const ROWS: u16 = 10;
const COLS: u16 = 100;

struct Vdrs {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    input: Box<dyn Write + Send>,
    screen: Arc<Mutex<vt100::Parser>>,
    _home: Option<tempfile::TempDir>,
}

impl Vdrs {
    fn open(file: &str) -> Self {
        let home = tempfile::tempdir().unwrap();
        let mut vd = Self::spawn(&[file], home.path());
        vd._home = Some(home);
        vd
    }

    /// `vdrs <args>` in docs/fixtures with config and state under `home`.
    fn spawn(args: &[&str], home: &Path) -> Self {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_vdrs"));
        cmd.args(args);
        cmd.cwd(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/fixtures"));
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("HOME", home);
        cmd.env("XDG_CONFIG_HOME", home.join("config"));
        cmd.env("XDG_STATE_HOME", home.join("state"));
        let child = pty.slave.spawn_command(cmd).unwrap();
        let mut reader = pty.master.try_clone_reader().unwrap();
        let screen = Arc::new(Mutex::new(vt100::Parser::new(
            ROWS.try_into().unwrap(),
            COLS.try_into().unwrap(),
            0,
        )));
        let sink = Arc::clone(&screen);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n @ 1..) = reader.read(&mut buf) {
                sink.lock().unwrap().process(&buf[..n]);
            }
        });
        let input = pty.master.take_writer().unwrap();
        std::mem::forget(pty.master); // keep the pty open while the child runs
        let vd = Self {
            child,
            input,
            screen,
            _home: None,
        };
        // Keys before the columns are known have nothing to move over.
        let start = Instant::now();
        // counted: "row <n>/<digits>"
        let counted = |t: &str| {
            t.split("row ").skip(1).any(|r| {
                r.split_once('/')
                    .is_some_and(|(_, c)| c.starts_with(|c: char| c.is_ascii_digit()))
            })
        };
        while !counted(&vd.text()) {
            assert!(start.elapsed() < Duration::from_secs(20), "never loaded");
            std::thread::sleep(Duration::from_millis(50));
        }
        vd
    }

    fn text(&self) -> String {
        let p = self.screen.lock().unwrap();
        p.screen()
            .rows(0, COLS)
            .map(|l| l.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Poll until the screen is `want`; the last screen on timeout.
    fn wait_for(&self, want: &str) -> Result<(), String> {
        let end = Instant::now() + Duration::from_secs(20);
        loop {
            let got = self.text();
            if got == want {
                return Ok(());
            }
            if Instant::now() > end {
                return Err(got);
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.input.write_all(bytes).unwrap();
        self.input.flush().unwrap();
    }
}

impl Drop for Vdrs {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// A golden script as typed bytes, one chunk per recorded frame (the whole
/// script when it has no `# caption`): `>> text` as is, `> text` plus Enter,
/// other lines space-separated keys (one character, `<CR>` or `<Esc>`).
fn steps(script: &str) -> Vec<Vec<u8>> {
    let mut steps = vec![String::new()];
    let mut captions = 0;
    for line in script.lines() {
        let line = line.replace("{fixtures}/", "");
        if line.starts_with("# ") {
            captions += 1;
            if captions > 1 {
                steps.push(String::new()); // lines before the first: setup
            }
            continue;
        }
        let out = steps.last_mut().unwrap();
        if let Some(text) = line.strip_prefix(">> ") {
            out.push_str(text);
        } else if let Some(text) = line.strip_prefix("> ") {
            out.push_str(text);
            out.push('\r');
        } else {
            for k in line.split_whitespace() {
                match k {
                    "<CR>" => out.push('\r'),
                    "<Esc>" => out.push('\x1b'),
                    k if k.chars().count() == 1 => out.push_str(k),
                    k => panic!("pty test can't type {k}"),
                }
            }
        }
    }
    steps.into_iter().map(String::into_bytes).collect()
}

#[test]
fn real_binary_draws_the_golden_screens() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden");
    let mut cases: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("docs-")
        })
        .collect();
    cases.sort();
    assert!(!cases.is_empty());
    for dir in cases {
        let screens = std::fs::read_to_string(dir.join("expected.screen")).unwrap();
        let frames: Vec<&str> = screens
            .trim_end_matches('\n')
            .split("\n--- frame\n")
            .collect();
        let open = std::fs::read_to_string(dir.join("open")).unwrap_or("sales.csv".into());
        let mut vd = Vdrs::open(open.trim());
        let script = std::fs::read_to_string(dir.join("script")).unwrap();
        let steps = steps(&script);
        assert_eq!(steps.len(), frames.len(), "{}", dir.display());
        for (i, (keys, want)) in steps.iter().zip(&frames).enumerate() {
            vd.send(keys);
            if let Err(got) = vd.wait_for(want) {
                panic!(
                    "{} frame {}\n--- want\n{want}\n--- got\n{got}",
                    dir.display(),
                    i + 1
                );
            }
        }
        // the header row's cursor cell carries the theme's cursor colors
        let p = vd.screen.lock().unwrap();
        let hc = (0..COLS)
            .filter_map(|x| p.screen().cell(1, x))
            .find(|c| c.bgcolor() == vt100::Color::Rgb(224, 175, 104));
        assert!(hc.is_some(), "{}: no amber header cursor", dir.display());
    }
}

#[test]
fn q_quits_and_restores_the_terminal() {
    let mut vd = Vdrs::open("sales.csv");
    vd.send(b"q");
    let start = Instant::now();
    loop {
        if let Some(status) = vd.child.try_wait().unwrap() {
            assert!(status.success(), "{status:?}");
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(10), "q didn't quit");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(100));
    let p = vd.screen.lock().unwrap();
    assert!(
        !p.screen().alternate_screen(),
        "left the alternate screen on"
    );
}

/// S-18 G: the terminal's paste (bracketed) reaches the register, not the
/// keymap; quitting turns bracketed paste off again.
#[test]
fn bracketed_paste_fills_the_register() {
    let mut vd = Vdrs::open("sales.csv");
    assert!(vd.screen.lock().unwrap().screen().bracketed_paste());
    vd.send(b"\x1b[200~1\tq\n2\tr\n\x1b[201~");
    let start = Instant::now();
    while !vd.text().contains("clipboard: 2 rows × 2 cols") {
        assert!(start.elapsed() < Duration::from_secs(20), "{}", vd.text());
        std::thread::sleep(Duration::from_millis(50));
    }
    vd.send(b"q");
    let start = Instant::now();
    while vd.child.try_wait().unwrap().is_none() {
        assert!(start.elapsed() < Duration::from_secs(10), "q didn't quit");
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(!vd.screen.lock().unwrap().screen().bracketed_paste());
}

/// Quitting saves the session; `vdrs -S` on it brings the screen back, and
/// so does a `:mksession` file (S-29 D).
#[test]
fn sessions_survive_a_restart() {
    let home = tempfile::tempdir().unwrap();
    let mut vd = Vdrs::spawn(&["sales.csv"], home.path());
    vd.send(b"ll]j");
    std::thread::sleep(Duration::from_millis(500));
    let named = home.path().join("named.vdrs");
    vd.send(format!(":mksession {}\r", named.display()).as_bytes());
    let end = Instant::now() + Duration::from_secs(20);
    while !vd.text().contains("session saved") {
        assert!(Instant::now() < end, "{}", vd.text());
        std::thread::sleep(Duration::from_millis(50));
    }
    vd.send(b"F");
    let end = Instant::now() + Duration::from_secs(20);
    while !vd.text().contains("freq(") {
        assert!(Instant::now() < end, "{}", vd.text());
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(300));
    let freq = vd.text();
    vd.send(b"gq");
    let _ = vd.child.wait();
    let saved = home.path().join("state/vdrs/session.vdrs");
    let again = Vdrs::spawn(&["-S", saved.to_str().unwrap()], home.path());
    if let Err(got) = again.wait_for(&freq) {
        panic!("--- want\n{freq}\n--- got\n{got}");
    }
    drop(again);
    let named = Vdrs::spawn(&["-S", named.to_str().unwrap()], home.path());
    let end = Instant::now() + Duration::from_secs(20);
    while !(named.text().contains("sort") && named.text().contains("row 2/")) {
        assert!(Instant::now() < end, "{}", named.text());
        std::thread::sleep(Duration::from_millis(50));
    }
}
