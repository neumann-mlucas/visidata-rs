//! visidata-rs library crate. Modules re-exported so `tests/*.rs` integration tests
//! (e.g. `tests/vd_replay.rs` — the S-14 golden corpus harness) can drive internals
//! directly. The `vdrs` binary consumes this crate.

pub mod action;
pub mod app;
pub mod exline;
pub mod key;
pub mod keymap;
pub mod plugins;
pub mod reader;
pub mod sheet;
pub mod theme;
pub mod widget;
