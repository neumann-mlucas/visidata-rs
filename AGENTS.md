# visidata-rs (vdrs)

VisiData-compatible terminal data explorer: Polars + Ratatui + mlua.

## Commands

- `just check`: fmt check, clippy `-D warnings`, full nextest run. Done = exit 0.
- `just test <filter>` for a narrow run first.
- `prek run --files <changed>` before committing (`prek install` once). Test data under `tests/golden/`, `tests/vd_replay/` and `docs/fixtures/` is byte-exact, so the hooks skip it.
- `UPDATE_GOLDEN=1` rewrites `tests/golden/` cases. Review the diff before committing.
- `VDRS_PYTHON=<python with polars>` runs exported scripts in golden cases.
- `tests/vd_replay.rs` reads the VisiData checkout at `../visidata`.

## Rules

- Default keys are VisiData's. Check `../visidata/visidata/**/*.py`
  (`addCommand`/`bindkey`) before binding one. Vim/TUI meanings go only in
  the opt-in `vd.option("keymap", "vim")` profile.
- A feature starts as a spec in `docs/specs/S-NN-*.md`. Status is tracked in `docs/tasks.md`.
- UI changes get a `Harness` test (`src/app/scenario.rs`; tests by topic in
  `src/app/scenario/*.rs`): real key path, real background materialize. Unit
  tests alone don't cover the UI.
- Tests never touch the user's state or clipboard (`cfg!(test)` guards in
  `exline::history_path`, `paste_cmd_for`).
- A `vd_replay` count drop can stay green: read the `vd_replay:` line.
  Regenerate `tests/vd_replay/triage.txt` with `VDRS_REPLAY_TRIAGE=1`.
