# S-26 — Vim grammar and TUI conventions

Status: **drafted 2026-11-08**, not started.
Depends on: S-04 (keymap, counts, `.`), S-18 (register, edit keys, D1–D6), S-19 (macros on `m`),
S-20 (undo tree), S-23 B (splits). Constraint: direction §9.2 — the **default** keymap feels like
VisiData from keystroke 1; deviations need a reason. So this spec has two layers:

1. **Default keymap**: only additions on keys VisiData leaves free or that vdrs doesn't use, plus
   data-safety and TUI-convention fixes.
2. **`vd.option("keymap", "vim")`** (S-18 D6 options): an opt-in profile that rebinds the VisiData
   keys whose vim meaning differs. Built on `vd.bind` patches, so it is one Lua table shipped in the
   binary (`plugins::vim_profile_lua`), readable and copyable by users.

## 1. What exists (verified)
- Count prefix for motions (`3j`), `.` repeats resolved Actions, `:` ex line with history + Tab
  completion, `:N` go to row, which-key overlay after a pending prefix (1000 ms).
- Free in vdrs today: `<C-r> <C-o> <C-z> <C-w> v V ' \` <Space> ?`; `y p d x a e m` are claimed by S-18 /
  S-19 with VisiData meanings. `u` = unselect, `U`/`R` = undo/redo (VisiData).
- Missing TUI conventions: no `Ctrl+Z` suspend (raw mode swallows it), colors are 24-bit RGB only (no
  256/16-color fallback, no `NO_COLOR`), no help sheet, no mouse, no keyboard-enhancement protocol
  (`Tab` and `<C-i>` are the same key; `Esc` vs `Alt` ambiguity).

## 2. Part A — Default keymap: safety + conventions (2–3 d)
| Item | Behavior | Why |
|---|---|---|
| A1 quit guard | `q` / `:q` / `<C-c>` on a sheet with unsaved edits (S-18) asks `discard 3 edits to sales.csv? y/N`; `:q!` / `:qa!` skip it | VisiData's `quitguard` defaults off, so `q` silently drops edits; vim refuses `:q` with changes |
| A2 `<C-r>` = redo | alias of `R` | vdrs has no reload, so no VisiData meaning lost; vim users get redo instead of nothing |
| A3 `<C-z>` suspend | leave raw mode + alt screen, `SIGTSTP` self, restore on `SIGCONT` (unix only) | every TUI does this; VisiData does |
| A4 colors | detect `COLORTERM=truecolor|24bit`; else map theme RGB to 256 colors; `NO_COLOR` → no color, selection/cursor by reverse/bold | 24-bit-only breaks on older terminals, tmux without `Tc`, Linux console |
| A5 help | `:help` (and `<F1>`) opens a sheet of all bindings of the current layer (key, action, desc) — the which-key table, searchable with `/` | VisiData `z^H` help-commands; nothing in vdrs today |
| A6 keyboard protocol | crossterm `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)` when supported, so `<C-i>` ≠ `Tab` and `Esc` is immediate | needed for A7's jumplist and S-23 `Tab` |
| A7 jumplist | `<C-o>` / `<C-i>` go back/forward through big jumps: `gg G :N n N /` hits, sheet switches, freq dives | vim; VisiData only has `Ctrl+^` prev sheet (also add `<C-^>`) |
| A8 Space | command palette: fuzzy list of every `Action::NAMED` + ex command with its key (VisiData `Space` = exec-longname) | discoverability (VisiData #2313) |
| A9 mouse (opt-in) | `vd.option("mouse", true)`: wheel scrolls, click moves cursor, click header = column | off by default so terminal text selection keeps working |

## 3. Part B — Ex-line ranges and global commands (2 d, default keymap)
No key conflicts — all on `:`.
- Ranges: `:5,20 <cmd>`, `:.,$ <cmd>`, `:'<,'> <cmd>` (visual, Part C), `:% <cmd>` = all rows.
  A range resolves to a row-id set, then the command runs as if those rows were selected:
  `:5,20 setcol 0`, `:'<,'>d` (S-18 E), `:% y`.
- `:g/<regex>/<cmd>` = select rows where the current column matches, run `<cmd>` on them;
  `:v/<regex>/<cmd>` inverse; `:g/<col>=<regex>/` names a column. With S-21: `:g{<sql pred>}<cmd>`.
  Without `<cmd>`: just select (VisiData `|`).
- `:normal <keys>` on a range = run keys per row (bounded: refuses over 10k rows; `ponytail:` limit).
- Tests: parser unit tests per range form; scenario `:g/^n/d` = `|^n` + `gd`.

## 4. Part C — Visual mode (2–3 d, default keymap)
`v`/`V` are free in vdrs (VisiData `v` is a multiline-display toggle — move that to `zv`?, decision V1).
- `V` starts a **row range** at the cursor; motions (`j k G gg :N /`, counts) extend it; the range is
  highlighted; `Esc` cancels.
- `<C-v>` starts a **cell block** (rows × columns): `h l` extend columns too.
- On the range, the VisiData verbs apply as their `g` (selected) form: `s`/`u` select/unselect, `y`
  copy rows (S-18 A), `d` delete (S-18 E), `e` setcol (S-18 C), `-` hide block columns, `F` freq of the
  block's columns, `"` new sheet. `:` pre-fills `:'<,'>`.
- `gv` reselects the last range — but `gv` is VisiData unhide-cols (vdrs `g-`), decision V2.
- Visual ranges are row ids (survive sort, like selection).
- Tests: scenario per verb on a range; range survives `[` sort; monkey gets `V`, `<C-v>`.

## 5. Part D — `keymap = "vim"` profile (3–4 d)
Rebinds only where VisiData and vim disagree; everything else stays VisiData.

| Key | VisiData / vdrs default | vim profile |
|---|---|---|
| `u` | unselect row | undo |
| `<C-r>` | redo (A2) | redo |
| `U` | undo | unselect row (VisiData `u` moves here) |
| `R` | redo | — (unbound; free for future replace mode) |
| `q{a-z}` / `@{a-z}` / `@@` | `q` pop sheet; `m` macro (S-19) | record / play macro into a register (S-19 machinery) |
| `q` alone (no register key in 1000 ms) / `:q` | pop sheet | `:q` / `ZZ` pop; `q` waits for a register |
| `d{motion}` `y{motion}` `c{motion}` | `d`/`y` act on the row at once (S-18) | operators: `dd` `yy` = row, `dj` `d3j` `dG` `dgg`, `yip`-style text objects (below) |
| `"{a-z}` | `"` = new sheet of selected rows | register prefix for the next `y`/`d`/`p`; `"` sheet moves to `g"`-like `:dupsel` (decision V3) |
| `x` | cut row (S-18) | cut cell (vim `x` = one char) |
| `H` `M` `L` | slide column / melt / slide | screen top / middle / bottom row |
| `zt` `zz` `zb` | — | scroll cursor row to top / middle / bottom |
| `w` `b` `e` | — / — / edit | next / prev non-null cell in the row (`e` edit moves to `i` / `<CR>` on a cell, decision V4) |
| `{` `}` | — | prev / next change of value in the current column (VisiData `<`/`>` go-prev/next-value) |
| `*` `#` | — / type-int | search the cursor cell's value forward / back (`#` int cast → `:type int` + `z#`) |
| `<C-w>s/v/w/o/q` | — | splits (S-23 B) |
| `m{a-z}` / `'{a-z}` | macro (S-19) | marks (row id + sheet); macros on `q` above |

Text objects for operators and visual mode (table-shaped, no words): `ir` row, `ic` column (cells of
the current column on the view), `is` the selection, `iv` the run of equal values around the cursor in
the current column, `ib` block of non-null cells around the cursor.

Registers: S-18 §2.6's single `App.register` becomes `App.registers: HashMap<char, Register>` with
`"` (unnamed, default), `0` (last yank), `1`–`9` (delete ring), `a`–`z`, `+` (system clipboard, S-18
B/G). Default keymap uses only `"`; `:registers` lists them in both profiles.

Grammar: one pending-operator state in the keymap resolver (S-04's `Dispatch::Pending` already holds a
key prefix): `Operator { op, count, register }` waits for a motion or text object, then resolves to a
row-id / cell set and dispatches the operator's `g` form on it. Counts multiply (`2d3j` = 6 rows).
`.` repeats the whole operator + motion (already captures resolved Actions).

## 6. Part E — Line editor normal mode (2 d)
Prompts and cell edits (S-18 C) get vim normal mode on `Esc` when the vim profile is on (default
profile: `Esc` cancels, as today): `h l w b e 0 $ x dw cw D C i a A I u`, `Esc` again cancels,
`Enter` submits from either mode. `src/lineedit.rs` already has the cursor model. VisiData #2500.

## 7. Phases
| Phase | Ships | Effort |
|---|---|---|
| A | safety + TUI conventions A1–A9 (A1 lands with S-18 C) | 2–3 d |
| B | ex ranges, `:g` / `:v`, `:normal` | 2 d |
| C | visual mode `V` / `<C-v>` | 2–3 d |
| D | vim profile: rebinds, operators + text objects, registers, marks, macros on `q` | 3–4 d |
| E | line editor normal mode | 2 d |

## 8. Test plan
- Unit: operator resolver — `dd`, `d3j`, `2d3j`, `dG`, `yic`, `"ayy`, `diw`-like invalid object →
  refused, timeout on `q` without register; range parser.
- Unit: profile table — every key it binds resolves to an existing action / ex command (as the
  `init.lua` doc test does); applying the profile twice = once.
- Scenario: default profile behaves exactly as before (whole existing suite passes with the profile
  off); vim profile: `u` undoes, `U` unselects, `qa…q @a` replays; `"ayy` then `"ap` pastes.
- Scenario: A1 quit guard with edits / with `:q!`; A3 suspend on a pty (`SIGTSTP` sent, terminal
  restored — pty test); A4 color mapping with `COLORTERM` unset and `NO_COLOR` set (render snapshot
  has no RGB).
- Docs: guide "vim users" page lists the profile table; doc-sync test covers both profiles.

## 9. Decisions (open)
- **V1** `v`: visual (vim) vs VisiData multiline toggle — vdrs has no multiline yet; recommend `v`
  stays free for a charwise/cell visual later, `V` + `<C-v>` now.
- **V2** `gv`: reselect (vim) vs unhide-cols (VisiData `gv`; vdrs uses `g-`) — recommend reselect.
- **V3** `"` in the vim profile: register prefix — recommend yes, profile only.
- **V4** `e` in the vim profile: word motion vs edit — recommend motion, edit on `i` / `<CR>`.
- **V5** should A2 (`<C-r>` redo) and A7 (jumplist) be default? Recommend yes (no VisiData meaning
  lost in vdrs).
