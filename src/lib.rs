//! visidata-rs library crate. Modules re-exported so `tests/*.rs` integration tests
//! (e.g. `tests/vd_replay.rs` — the S-14 golden corpus harness) can drive internals
//! directly. The `vdrs` binary consumes this crate.
//!
//! Data flow, one key press:
//!
//! 1. `app/input.rs`: [`keymap::Keymap::resolve`] maps keys to [`action::Action`]s
//!    (an ex line goes through [`exline::parse`] instead).
//! 2. `App::dispatch` (`app/actions.rs`) applies each one to the active [`sheet::Sheet`]:
//!    view state ([`sheet::View`]: cursor, sort, filter, hidden columns) or a
//!    [`sheet::PlanStep`] baked into its plan (`sheet/step.rs`, kept in `state.steps`).
//! 3. [`sheet::Sheet::window_plan`] is the view as a Polars query, sliced to the window.
//! 4. `app/jobs.rs` collects it off the UI thread (background materialize).
//! 5. `app/render.rs` draws the window that landed.
//!
//! [`sheet::Origin`] says what made a sheet; a derived one keeps its parent snapshot and
//! [`sheet::Derivation`] in `state.lineage` (`sheet/mod.rs`; built in `sheet/derive.rs`).
//! Undo is a tree of (plan, view, steps, lineage) states in `sheet/undo.rs`. [`export`]
//! walks lineage then steps to write the script; [`session`] replays sheets as ex commands.

pub mod action;
pub mod app;
pub mod clipboard;
pub mod commit;
pub mod duckdb;
pub mod exline;
pub mod export;
pub mod key;
pub mod keymap;
pub mod lineedit;
pub mod notes;
pub mod numfmt;
pub mod options;
pub mod plugins;
pub mod reader;
pub mod session;
pub mod sheet;
pub mod source;
pub mod sqlexpr;
pub mod style;
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
