#![allow(
    clippy::unwrap_used,
    reason = "test helpers fail the test by panicking; allow, not expect: clippy 1.100 treats tests/ as test code and no longer fires here"
)]
//! The `vdrs` binary from the outside: arguments, stdin, exit codes.

use std::io::Write;
use std::process::{Command, Output, Stdio};

fn vdrs(args: &[&str], stdin: &[u8]) -> Output {
    let dir = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vdrs"))
        .args(args)
        .env("XDG_CONFIG_HOME", dir.path())
        .env("XDG_STATE_HOME", dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // A process that exits without reading closes the pipe; that's its business.
    let _ = child.stdin.take().unwrap().write_all(stdin);
    child.wait_with_output().unwrap()
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

#[test]
fn headless_prints_schema_and_rows() {
    let f = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    std::fs::write(f.path(), "a,b\n1,x\n2,y\n").unwrap();
    let o = vdrs(
        &[f.path().to_str().unwrap(), "--headless", "--head", "1"],
        b"",
    );
    assert!(o.status.success(), "{}", text(&o.stderr));
    let out = text(&o.stdout);
    assert!(out.contains("schema : 2 columns"), "{out}");
    assert!(out.contains("shape: (1, 2)"), "{out}");
}

#[test]
fn stdin_is_sniffed_and_format_overrides() {
    let o = vdrs(&["-", "--headless"], b"a\tb\n1\t2\n");
    assert!(text(&o.stdout).contains("Tsv"), "{}", text(&o.stdout));
    let o = vdrs(&["-", "--headless", "--format", "jsonl"], b"{\"a\":1}\n");
    assert!(text(&o.stdout).contains("Jsonl"), "{}", text(&o.stderr));
    let o = vdrs(&["-", "--headless", "--format", "json"], b"[{\"a\":1}]");
    assert!(text(&o.stdout).contains("Json"), "{}", text(&o.stderr));
}

#[test]
fn failures_exit_nonzero_and_say_why() {
    let o = vdrs(&["/no/such/file.csv", "--headless"], b"");
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("/no/such/file.csv"),
        "{}",
        text(&o.stderr)
    );
    let o = vdrs(&["-", "--headless"], b"");
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("empty stdin"),
        "{}",
        text(&o.stderr)
    );
    let f = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    std::fs::write(f.path(), "a\n1\n").unwrap();
    let o = vdrs(&[f.path().to_str().unwrap()], b""); // no terminal
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("--headless"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn a_closed_stdout_is_not_an_error() {
    let f = tempfile::Builder::new().suffix(".csv").tempfile().unwrap();
    std::fs::write(f.path(), "a\n1\n").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_vdrs"))
        .args([f.path().to_str().unwrap(), "--headless"])
        .env("XDG_CONFIG_HOME", f.path().with_extension("cfg"))
        .env("XDG_STATE_HOME", f.path().with_extension("state"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take()); // the reader left, like `| head`
    assert!(child.wait().unwrap().success());
}

#[test]
fn session_flag_needs_a_session_with_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let s = dir.path().join("s.vdrs");
    std::fs::write(&s, "# nothing\n").unwrap();
    let out = vdrs(&["-S", s.to_str().unwrap()], b"");
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("no :open line"),
        "{}",
        text(&out.stderr)
    );
    std::fs::write(&s, ":tail /tmp/a.log\n:open /tmp/b.csv\n").unwrap();
    let out = vdrs(&["-S", s.to_str().unwrap()], b"");
    assert!(!out.status.success());
    assert!(
        text(&out.stderr).contains("the first sheet is a tail"),
        "{}",
        text(&out.stderr)
    );
    let out = vdrs(&["-S", "/no/such.vdrs"], b"");
    assert!(!out.status.success());
    let out = vdrs(&[], b"");
    assert!(!out.status.success(), "a path or -S is required");
}
