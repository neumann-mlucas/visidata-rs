# S-58 Undo model and random-walk property

Status: shipped 2026-11-22.
Depends on: S-20 (undo tree), S-55 (table and step generators),
`src/app/scenario/probe.rs`.

## 1. Problem

Unit tests check `U` and `R` after fixed edits. No test checks undo after
a long mix of commands, so a command that changes the sheet without an undo
state, or a `U` that restores part of the view, can go unseen.

The panic probes in `scenario/probe.rs` press random chords, but they are
ignored (slow), use a fixed LCG and four fixed files, and report a walk of
25 chords that nobody shrinks.

## 2. Undo model

Test in `src/app/generated/undo.rs`.

- The generator makes a walk of up to 12 operations. An operation is an
  S-55 step that stays on the sheet, a row or cell edit (`setcol`, `edit`,
  `d`, `y` `p`, `addcol`), `U` or `R`.
- The model holds each undo state (`Sheet::undo_mark`), what the sheet
  showed there, the parent of each state and the state `R` goes to.
  "Shown" is the rows as TSV, the selection and every view setting a
  command can change. The cursor and the window are not part of it.
- After each operation:
  1. If a command made a new state, the id must be new. The model records
     it as the child of the state before.
  2. If the operation is `U`, the sheet must be at the parent state, and
     show what it showed there. With no parent, nothing changes.
  3. If the operation is `R`, the sheet must be at the redo child, and show
     what it showed there.
  4. If a command made no state, the sheet must show what it showed.
- A command that opens or closes a sheet ends the walk: undo is per sheet.
- A mutation check: `U` pressed twice for one `Op::Undo` fails the test.

## 3. Random walks

Test in `src/app/generated/walk.rs`.

- A walk is 1 to 25 presses. A press is a default chord that does not save,
  write or quit (`unsafe_chord`), then an answer: nothing, Enter, `j`
  Enter, Esc or `a` Enter.
- The sheet is an S-55 table (3 in 4) or one of the probe's edge-case files.
  The terminal is 100×12 or 12×3.
- After the walk and Esc, the app must not panic and must pass the S-55
  health check (§4.4).
- The ignored probes stay: `probe_every_chord` covers every chord once,
  which a random walk does not promise.

## 4. Running

- Both run in `just check` (24 and 16 cases). `PROPTEST_CASES` raises them.
- Before shipping, the walk passed two runs of 1000 cases and the model
  300 cases.

## 5. Bugs found

- `M` read a hidden key column that the view leaves out. The melt now
  leaves the hidden key out, as VisiData: its `keyCols` are the shown key
  columns only.
- Enter on a hidden column in the columns sheet, and `gF` with a hidden
  key, opened a freq sheet that could not load. Enter now opens the freq
  of the hidden column, as VisiData. `gF` groups by the shown keys only.
- `M` on a melted sheet failed: Polars can't make a `Variable` column while
  the melted `Variable` column is still there. The melt uses temporary
  names, and a key named `Variable` or `Value` is refused.

## 6. Decisions

1. A derived sheet reads a hidden column that it names one by one: the
   freq column, the dive column and the `:pivot` columns. A hidden key is
   no key, as VisiData's `keyCols`. The app and both exports use the same
   rule (`Derivation::reads`).
