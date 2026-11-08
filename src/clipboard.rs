//! System clipboard out (S-18 B), as VisiData's `clipboard.py`: pipe text to
//! a copy command, picked from the platform unless `init.lua` sets
//! `vd.option("clipboard_copy_cmd", …)`. Over SSH the default is OSC 52.

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
                let wrote = child.stdin.take().unwrap().write_all(text.as_bytes());
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
        let line: Vec<String> = cols.iter().map(|c| cell(&c.get(i).unwrap())).collect();
        out.push_str(&line.join("\t"));
        out.push('\n');
    }
    out
}

/// One value per line, no header (`gzY`); a single value without newline (`zY`).
pub fn lines(c: &Column) -> String {
    if c.len() == 1 {
        return cell(&c.get(0).unwrap());
    }
    (0..c.len())
        .map(|i| cell(&c.get(i).unwrap()) + "\n")
        .collect()
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
