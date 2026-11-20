# visidata-rs (vdrs)

VisiData-compatible terminal data explorer: Polars + Ratatui + mlua.

## Commands

- `just check`: fmt check, clippy `-D warnings`, full nextest run. Done = exit 0.
- `just test <filter>` for a narrow run first.
- Builds go to `../.visidata-rs-target`, shared by the checkout and its worktrees (`.cargo/config.toml`). Dev profile keeps line tables only. `cargo clean` there when it grows.
- `prek run --files <changed>` before committing (`prek install` once). Test data under `tests/golden/`, `tests/vd_replay/`, `tests/vd_local/` and `docs/fixtures/` is byte-exact, so the hooks skip it.
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
  `state_path` in `src/lib.rs`, `paste_cmd_for`).
- A `vd_replay` count drop can stay green: read the `vd_replay:` line.
  Regenerate `tests/vd_replay/triage.txt` with `VDRS_REPLAY_TRIAGE=1`.

## Docs

- `docs/index.html` is the user guide, pulled into tests by `include_str!`:
  `keymap.rs` `user_guide_documents_every_default_key` (a `data-keys` row per
  default chord), `action.rs` `user_guide_lists_every_action_name`
  (`<code>name</code>` per `Action::NAMED`), `exline.rs`
  `user_guide_commands_exist` (every `data-ex` parses) and
  `user_guide_examples_parse` (every concrete `<code>:cmd</code>` parses).
  `plugins/tests.rs` also loads its Lua blocks.
- Screens between `<!-- golden:docs-* -->` and `<!-- /golden -->` come from
  `tests/golden/docs-*` via `UPDATE_GOLDEN=1`. Never hand-edit them.
- A user-visible change updates the guide prose and S-15 in the same commit.

## Adding a command

1. Spec, plus a row in `docs/specs/README.md` and `docs/tasks.md`.
2. Check `../visidata` `addCommand`/`bindkey` for its key and longname.
3. `src/action.rs`: `Action` variant; a `NAMED` entry (no arguments) or a
   `to_ex` arm plus a case in `to_ex_round_trips_through_the_ex_parser`.
4. Handle it in `App::route` (`src/app/actions.rs`): a one-line arm, a
   longer body as a method in the topic file under `src/app/actions/`.
5. `Keymap::defaults` entry with its desc; `exline::parse` arm.
6. `Harness` test in `src/app/scenario/<topic>.rs`.
7. Guide: key list row, prose, `data-ex` row. S-15 row.
8. `tests/vd_replay.rs`: translate the longname, check
   `tests/vd_replay/quarantine.txt`, raise `MIN_PASS`.
9. `just check`.
