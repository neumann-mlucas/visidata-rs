//! System clipboard (S-18 B, G), as VisiData's `clipboard.py`: pipe text to
//! a copy command, picked from the platform unless `init.lua` sets
//! `vd.option("clipboard_copy_cmd", …)`; over SSH the default is OSC 52.
//! Text comes back from a paste command (`clipboard_paste_cmd`) or the
//! terminal's own paste.

use std::io::Write;
use std::process::{Command, Stdio};

use polars::prelude::*;

use crate::sheet::any_text;

#[derive(Debug, Clone)]
pub enum Clipboard {
    /// argv of a command that reads the text on stdin.
    Command(Vec<String>),
    /// `ESC ] 52 ; c ; <base64> BEL` written to the terminal.
    Osc52,
    /// Tests: copies land here, never on the real clipboard.
    #[cfg(test)]
    Memory(std::sync::Arc<std::sync::Mutex<Vec<String>>>),
}

impl Clipboard {
    /// `option` is `clipboard_copy_cmd` from init.lua; `env` reads a variable.
    pub fn pick(option: Option<&str>, env: impl Fn(&str) -> Option<String>) -> Self {
        // ponytail: whitespace split, no shell quoting; use a wrapper script for odd argv.
        let cmd = |s: &str| Clipboard::Command(s.split_whitespace().map(String::from).collect());
        match option {
            Some("osc52") => return Clipboard::Osc52,
            Some(s) => return cmd(s),
            None => {}
        }
        if env("SSH_TTY").is_some() {
            Clipboard::Osc52
        } else if cfg!(windows) || env("WSL_DISTRO_NAME").is_some() {
            cmd("clip.exe")
        } else if cfg!(target_os = "macos") {
            cmd("pbcopy w")
        } else if env("WAYLAND_DISPLAY").is_some() {
            cmd("wl-copy")
        } else {
            cmd("xclip -selection clipboard -filter")
        }
    }

    /// Send `text`. `Osc52` comes back as `Some(escape)` for the UI thread to
    /// write, so it can't interleave with a frame being drawn.
    pub fn send(&self, text: &str) -> Result<Option<String>, String> {
        match self {
            Clipboard::Osc52 => Ok(Some(osc52(text))),
            #[cfg(test)]
            Clipboard::Memory(m) => {
                m.lock().unwrap().push(text.to_string());
                Ok(None)
            }
            Clipboard::Command(argv) => {
                let Some((prog, args)) = argv.split_first() else {
                    return Err("clipboard_copy_cmd is empty".into());
                };
                let hint = "set vd.option(\"clipboard_copy_cmd\", …) in init.lua";
                let mut child = Command::new(prog)
                    .args(args)
                    .stdin(Stdio::piped())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .map_err(|e| match e.kind() {
                        std::io::ErrorKind::NotFound => format!("{prog} not found; {hint}"),
                        _ => format!("{prog}: {e}"),
                    })?;
                let wrote = match child.stdin.take() {
                    Some(mut stdin) => stdin.write_all(text.as_bytes()),
                    None => Err(std::io::ErrorKind::BrokenPipe.into()), // piped above
                };
                let status = child.wait().map_err(|e| format!("{prog}: {e}"))?;
                wrote.map_err(|e| format!("{prog}: {e}"))?;
                if !status.success() {
                    return Err(format!("{prog} failed ({status}); {hint}"));
                }
                Ok(None)
            }
        }
    }
}

/// argv of the command `zP` `gzP` read the clipboard from: `option`
/// (`clipboard_paste_cmd`), else the platform's. `None` over SSH: the remote
/// clipboard is out of reach, only the terminal's paste gets text in.
pub fn paste_cmd(
    option: Option<&str>,
    env: impl Fn(&str) -> Option<String>,
) -> Option<Vec<String>> {
    let argv = |s: &str| Some(s.split_whitespace().map(String::from).collect());
    if let Some(s) = option {
        return argv(s);
    }
    if env("SSH_TTY").is_some() {
        None
    } else if cfg!(windows) || env("WSL_DISTRO_NAME").is_some() {
        argv("powershell.exe -noprofile -command Get-Clipboard")
    } else if cfg!(target_os = "macos") {
        argv("pbpaste")
    } else if env("WAYLAND_DISPLAY").is_some() {
        argv("wl-paste")
    } else {
        argv("xclip -selection clipboard -o")
    }
}

/// Run the paste command; its stdout.
pub fn read(argv: &[String]) -> Result<String, String> {
    let Some((prog, args)) = argv.split_first() else {
        return Err("clipboard_paste_cmd is empty".into());
    };
    let hint = "set vd.option(\"clipboard_paste_cmd\", …) in init.lua, or paste with the terminal";
    let out = Command::new(prog)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => format!("{prog} not found; {hint}"),
            _ => format!("{prog}: {e}"),
        })?;
    if !out.status.success() {
        return Err(format!("{prog} failed ({}); {hint}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Pasted text as lines of tab-separated fields, as VisiData's
/// `pasteFromClipboard`: `\r\n` endings and the final newline don't count.
pub fn fields(text: &str) -> Vec<Vec<String>> {
    let text = text.trim_end_matches(['\n', '\r']);
    if text.is_empty() {
        return Vec::new();
    }
    text.split('\n')
        .map(|l| {
            l.strip_suffix('\r')
                .unwrap_or(l)
                .split('\t')
                .map(String::from)
                .collect()
        })
        .collect()
}

/// A terminal paste: TSV, or CSV (quotes honoured) when no line has a tab.
pub fn table(text: &str) -> Vec<Vec<String>> {
    if text.contains('\t') || !text.contains(',') {
        return fields(text);
    }
    let mut out = Vec::new();
    let mut rdr = text.trim_end_matches(['\n', '\r']).chars().peekable();
    let (mut row, mut cell, mut quoted) = (Vec::new(), String::new(), false);
    while let Some(c) = rdr.next() {
        match (c, quoted) {
            ('"', true) if rdr.peek() == Some(&'"') => {
                rdr.next();
                cell.push('"');
            }
            ('"', _) => quoted = !quoted,
            (',', false) => row.push(std::mem::take(&mut cell)),
            ('\r', false) if rdr.peek() == Some(&'\n') => {}
            ('\n', false) => {
                row.push(std::mem::take(&mut cell));
                out.push(std::mem::take(&mut row));
            }
            _ => cell.push(c),
        }
    }
    row.push(cell);
    out.push(row);
    out
}

/// A cell as VisiData's TSV saver writes it: tab → U+001F, newline → U+001E, null empty.
fn cell(v: &AnyValue) -> String {
    if v.is_null() {
        return String::new();
    }
    any_text(v).replace('\t', "\u{1f}").replace('\n', "\u{1e}")
}

/// TSV with a header line, one line per row (`Y`, `gY`).
pub fn tsv(df: &DataFrame) -> String {
    let mut out = df
        .get_column_names()
        .iter()
        .map(|n| n.replace('\t', "\u{1f}"))
        .collect::<Vec<_>>()
        .join("\t");
    out.push('\n');
    let cols = df.columns();
    for i in 0..df.height() {
        let line: Vec<String> = cols.iter().map(|c| cell_at(c, i)).collect();
        out.push_str(&line.join("\t"));
        out.push('\n');
    }
    out
}

/// One value per line, no header (`gzY`); a single value without newline (`zY`).
pub fn lines(c: &Column) -> String {
    if c.len() == 1 {
        return cell_at(c, 0);
    }
    (0..c.len()).map(|i| cell_at(c, i) + "\n").collect()
}

/// Row `i` of `c` as [`cell`] writes it (`i` is in range).
fn cell_at(c: &Column, i: usize) -> String {
    c.get(i).map(|v| cell(&v)).unwrap_or_default()
}

fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", base64(text.as_bytes()))
}

fn base64(bytes: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for ch in bytes.chunks(3) {
        let n = ch
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &b)| n | (b as u32) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= ch.len() {
                T[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tsv_escapes_like_visidata_nulls_empty() {
        let df = df!(
            "a\tb" => [Some("x\ty"), None],
            "n" => [Some(1i64), Some(2)],
            "s" => [Some("l1\nl2"), Some("z")],
        )
        .unwrap();
        assert_eq!(
            tsv(&df),
            "a\u{1f}b\tn\ts\nx\u{1f}y\t1\tl1\u{1e}l2\n\t2\tz\n"
        );
        assert_eq!(lines(df.column("n").unwrap()), "1\n2\n");
        assert_eq!(lines(&df.column("s").unwrap().slice(1, 1)), "z");
    }

    #[test]
    fn backend_choice() {
        let env =
            |vars: &'static [&'static str]| move |k: &str| vars.contains(&k).then(String::new);
        let argv = |c: Clipboard| match c {
            Clipboard::Command(a) => a.join(" "),
            other => format!("{other:?}"),
        };
        assert_eq!(
            argv(Clipboard::pick(None, env(&["SSH_TTY", "WAYLAND_DISPLAY"]))),
            "Osc52"
        );
        assert_eq!(argv(Clipboard::pick(Some("osc52"), env(&[]))), "Osc52");
        assert_eq!(
            argv(Clipboard::pick(Some("my-copy  -x"), env(&["SSH_TTY"]))),
            "my-copy -x"
        );
        if cfg!(target_os = "linux") {
            assert_eq!(
                argv(Clipboard::pick(None, env(&["WAYLAND_DISPLAY"]))),
                "wl-copy"
            );
            assert_eq!(
                argv(Clipboard::pick(None, env(&["WSL_DISTRO_NAME"]))),
                "clip.exe"
            );
            assert_eq!(
                argv(Clipboard::pick(None, env(&[]))),
                "xclip -selection clipboard -filter"
            );
        }
    }

    #[test]
    fn pasted_text_splits_into_fields() {
        assert_eq!(fields(""), Vec::<Vec<String>>::new());
        assert_eq!(fields("one"), [["one"]]);
        assert_eq!(fields("a\tb\r\n\tc\n\n"), [vec!["a", "b"], vec!["", "c"]]);
        assert_eq!(fields("a\tb\nc"), [vec!["a", "b"], vec!["c"]]); // ragged
        // CSV only without tabs; quotes keep commas and newlines
        assert_eq!(
            table("x,\"a, \"\"b\"\"\"\r\n\"l1\nl2\",\n"),
            [vec!["x", "a, \"b\""], vec!["l1\nl2", ""]]
        );
        assert_eq!(table("a,b\tc"), [vec!["a,b", "c"]]);
        assert_eq!(table("Smith"), [["Smith"]]);
    }

    #[test]
    fn paste_command_choice() {
        let env =
            |vars: &'static [&'static str]| move |k: &str| vars.contains(&k).then(String::new);
        assert_eq!(paste_cmd(None, env(&["SSH_TTY"])), None);
        assert_eq!(
            paste_cmd(Some("my-paste  -o"), env(&["SSH_TTY"])),
            Some(vec!["my-paste".to_string(), "-o".into()])
        );
        if cfg!(target_os = "linux") {
            assert_eq!(
                paste_cmd(None, env(&["WAYLAND_DISPLAY"])),
                Some(vec!["wl-paste".to_string()])
            );
        }
        let e = read(&["vdrs-no-such-paste".into()]).unwrap_err();
        assert!(e.starts_with("vdrs-no-such-paste not found"), "{e}");
    }

    #[test]
    fn osc52_bytes() {
        assert_eq!(osc52("hi\n"), "\x1b]52;c;aGkK\x07");
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn missing_command_is_a_message() {
        let c = Clipboard::Command(vec!["vdrs-no-such-clip-cmd".into()]);
        let e = c.send("x").unwrap_err();
        assert!(e.starts_with("vdrs-no-such-clip-cmd not found;"), "{e}");
    }
}
