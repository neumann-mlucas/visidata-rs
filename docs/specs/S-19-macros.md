# S-19 — Macros

Status: **done 2026-11-10** (searches stay unrecorded: they aren't actions yet). Do after S-18 phase C (macros that edit are the useful kind)
and S-20 if undo grouping is wanted.
Depends on: S-04 (`.` repeat capture), S-09/S-10 (`vd.bind`), S-18 C (edits as steps).

## 1. What exists

- `.` repeats the last resolved actions (`Action::repeatable`).
- `vd.bind(layer, keys, action)` takes an action name, a `":ex command"` or a Lua function;
  `Action::NAMED` / `Action::from_name` cover every action without a payload.

## 2. Design

- `m` (VisiData `macro-record`) starts recording; the status line shows `● rec`. `m` again stops and
  asks for a key; empty = discard.
- Recorded: the resolved `Action`s, as `.` captures them (prompts already resolved: a filter is
  `Filter { column, pattern }`, not keystrokes). Not recorded: `m`, `U`/`R`, quit, `.`.
- Each action is written as text: an action name when it has no payload, else an ex command
  (`:filter region ^n`). Needs `Action::to_ex() -> Option<String>`; an action with a payload and no ex
  form makes recording refuse it with a message (lists which). Most already have ex forms.
- Saved to `$XDG_CONFIG_HOME/vdrs/macros.lua`, loaded after `init.lua`:
  `vd.bind("sheet", "<F5>", { "goto_rightmost", ":filter region ^n", "open_freq_table" })`.
  `vd.bind` accepts a list (next Lua API version; S-18 B's `vd.option` may take v5 first). The file is plain Lua: edit, share, delete by hand.
- Playback = dispatch the list in order, waiting for background jobs between steps (as the golden
  harness does), so `F` then `<CR>` works.
- Undo: each step its own undo entry in v1; one-step undo of a macro needs S-20 grouping.

## 3. Decisions

- **M1** column-relative vs absolute: `l` recorded as `cursor_right` (relative, VisiData) — recommend.
- **M2** recursion: a macro key pressed while recording is recorded as its actions (inlined), not the
  key.

## 4. Test plan

- Unit: `Action::to_ex` round trip — for every action with a payload, `exline::parse(to_ex(a)) == a`
  (one test over a table of examples; new payload actions must add an entry).
- Unit: macros.lua writer → `Plugins::load` → same binding (round trip through the real Lua loader).
- Scenario: record `l l F`, bind, replay on another sheet → same freq sheet; a macro with `:derive` +
  edit (S-18) → same TSV as typing it.
- Scenario: playback waits for jobs (`F` then `<CR>` in one macro lands on the dived sheet).
- Scenario: refused action message; empty key discards; `m` while recording a prompt cancels cleanly.
- Golden: `macro-replay` case (script records, binds, replays) + export round trip.
- pty: record + replay on the real binary; `macros.lua` written under the test's `XDG_CONFIG_HOME`
  only.
- Corpus: VisiData macro tests stay out (their `.vd`/`.vdj` macro files are VisiData's format,
  direction §9.5).

Effort: 2–3 days.
