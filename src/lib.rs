//! visidata-rs library crate. Modules re-exported so `tests/*.rs` integration tests
//! (e.g. `tests/vd_replay.rs` — the S-14 golden corpus harness) can drive internals
//! directly. The `vdrs` binary consumes this crate.

pub mod action;
pub mod app;
pub mod clipboard;
pub mod duckdb;
pub mod exline;
pub mod export;
pub mod key;
pub mod keymap;
pub mod lineedit;
pub mod notes;
pub mod plugins;
pub mod reader;
pub mod session;
pub mod sheet;
pub mod theme;
pub mod widget;

/// `$XDG_STATE_HOME/vdrs/<name>` (else `~/.local/state`, else the temp dir).
/// Under `cfg!(test)` a per-process temp dir, so tests never touch user state.
pub fn state_path(name: &str) -> std::path::PathBuf {
    if cfg!(test) {
        return std::env::temp_dir()
            .join(format!("vdrs-test-{}", std::process::id()))
            .join(name);
    }
    std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|h| std::path::PathBuf::from(h).join(".local").join("state"))
        })
        .unwrap_or_else(std::env::temp_dir)
        .join("vdrs")
        .join(name)
}
