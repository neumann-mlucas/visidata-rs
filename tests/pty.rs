//! The real binary on a pseudo-terminal must draw what the in-process golden
//! harness draws: each `tests/golden/docs-*` script is typed into `vdrs
//! sales.csv` (100x8) and the vt100-parsed screen must reach
//! `expected.screen`, the colors included at the cursor cell.

use std::io::{Read, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, PtySize, native_pty_system};

const ROWS: u16 = 8;
const COLS: u16 = 100;

struct Vdrs {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    input: Box<dyn Write + Send>,
    screen: Arc<Mutex<vt100::Parser>>,
    _home: tempfile::TempDir,
}

impl Vdrs {
    fn open(file: &str) -> Self {
        let pty = native_pty_system()
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .unwrap();
        let home = tempfile::tempdir().unwrap();
        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_vdrs"));
        cmd.arg(file);
        cmd.cwd(Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/fixtures"));
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        cmd.env("HOME", home.path());
        cmd.env("XDG_CONFIG_HOME", home.path().join("config"));
        cmd.env("XDG_STATE_HOME", home.path().join("state"));
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
            _home: home,
        };
        // Keys before the columns are known have nothing to move over.
        let start = Instant::now();
        while !vd.text().contains("row 1/300") {
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

/// `l l ]` → `ll]`: the docs scripts use single-key tokens only.
fn keys(script: &str) -> Vec<u8> {
    script
        .split_whitespace()
        .inspect(|k| assert_eq!(k.len(), 1, "pty test types plain keys only: {k}"))
        .collect::<String>()
        .into_bytes()
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
        let want = std::fs::read_to_string(dir.join("expected.screen")).unwrap();
        let want = want.trim_end_matches('\n');
        let mut vd = Vdrs::open("sales.csv");
        let script = std::fs::read_to_string(dir.join("script")).unwrap();
        vd.send(&keys(&script));
        if let Err(got) = vd.wait_for(want) {
            panic!("{}\n--- want\n{want}\n--- got\n{got}", dir.display());
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
