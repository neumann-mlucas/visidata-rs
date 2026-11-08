# S-26 — Vim grammar and TUI conventions

Status: **drafted 2026-11-08**; phase 0 shipped 2026-11-08 (see §0.1). Parts A–E not started.
Depends on: S-04 (keymap, counts, `.`), S-18 (register, edit keys, D1–D6), S-19 (macros on `m`),
S-20 (undo tree), S-23 B (splits). Constraint: direction §9.2 — the **default** keymap feels like
VisiData from keystroke 1; deviations need a reason. So this spec has two layers:

1. **Default keymap**: VisiData's, plus additions on keys VisiData leaves free, plus data-safety and
   TUI-convention fixes.
2. **`vd.option("keymap", "vim")`** (S-18 D6 options): an opt-in profile that rebinds the VisiData
   keys whose vim meaning differs. Built on `vd.bind` patches, so it is one Lua table shipped in the
   binary (`plugins::vim_profile_lua`), readable and copyable by users. Precedent: VisiData ships the
   same idea as `visidata/experimental/vimcompat.py` (`Ctrl+D`/`Ctrl+U` half page, `go` home).

## 0. Keymap rule (confirmed 2026-11-08, applies to every spec)
- **R-a** A key VisiData binds to a command vdrs has → the VisiData meaning, in the default keymap.
- **R-b** A key VisiData leaves free → vdrs may bind a new feature to it in the default keymap
  (additive, breaks no muscle memory): `V` / `<C-v>` visual, `gz|` filter, `zn` notes.
- **R-c** A vdrs meaning that differs from VisiData's → vim profile only.
- **R-d** Transition: a VisiData key whose command vdrs doesn't have *yet* keeps its current vdrs
  meaning until that command ships; the shipping commit switches the key and moves the old meaning to
  the vim profile. No key goes dead in between.
- **R-e** A VisiData key whose command vdrs cuts on purpose (direction §9.5: cmdlog, `.vdj`) → treated
  as free (R-b), listed in the table below with the cut it rests on: `D` diff (VisiData
  `cmdlog-sheet`), `<C-d>` page (VisiData `save-cmdlog`).
- Every VisiData command is also reachable as `:<name>` in both profiles.

Shipped vdrs keys this changes (compared with VisiData's core `addCommand`/`bindkey` table at
`5f67ff8a`, 2026-11-08):

| Key | vdrs today | VisiData | Default | Vim profile | When |
|---|---|---|---|---|---|
| `Space` | — | `exec-longname` | opens the `:` command line | same | phase 0 |
| `:` | command line | `addcol-split` | command line until split-column ships (R-d) | command line | when `addcol-split` ships |
| `.` | repeat | `plot-column` | repeat until plots ship (R-d) | repeat | S-23 C1 |
| `$` | last column | `type-currency` | currency cast; last/first column = `gl` / `gh` (`0` stays: VisiData alias) | `$` / `0` | phase 0 |
| `g-` | unhide all | `hide-uniform-cols` | `:uniform` | — | phase 0 |
| `gv` | — | `unhide-cols` | unhide all | reselect last visual range | phase 0 |
| `<C-c>` | quit | `cancel-sheet` | cancel the sheet's background jobs (`gCtrl+C` all); quit all = `gq`, hard quit = `<C-q>` | `<C-c>` cancels too; `:qa` | phase 0 |
| `D` | diff prompt | `cmdlog-sheet` (cut, §9.5) | diff prompt (R-e) | same | — (kept) |
| `<C-d>` / `<C-u>` | page down / up | `save-cmdlog` (cut, §9.5) / — | page, plus `<C-f>` / `<C-b>` (VisiData) / PgDn / PgUp; `<C-d>` kept under R-e, `<C-u>` is free | half page (as vimcompat) | phase 0 |
| `<C-r>` | — | `reload-sheet` | `:refresh` (S-30 A; asks first if the sheet has unsaved edits) | redo | S-30 A |
| `<C-o>` / `<C-i>` | — | `sysopen-*` (external program) | — | jumplist back / forward | phase A |
| `zs` / `zc` | freeze / unfreeze cols | `select-before` / `go-col-number` | freeze/unfreeze until those ship (R-d), then `:freeze` / `:unfreeze` | — | when they ship |
| `zo` | clear sort | `open-cell-file` | clear sort until that ships (R-d), then `:unsort` | — | when it ships |
| `z\|` | regex filter | `select-expr` | select-expr; filter = `gz\|` + `:filter` | same | S-21 A |
| `P` | plan panel | `paste-before` | paste-before; `:plan` | same | S-18 F (D1) |
| `/` / `g/` | search all columns / — | `search-col` (current column) / `search-cols` (all) | `/` current column, `g/` all columns; `?` / `g?` backwards (`searchr-col`); case-insensitive regex, as VisiData's default `regex_flags` | `/` all columns (vim searches the whole buffer) | phase 0 |

### 0.1 Phase 0 as shipped (2026-11-08)
- Default keymap: `Space` → `:`; `$` currency, `gh`/`gl` first/last column (`0` kept); `g-` `:uniform`,
  `gv` unhide-cols; `<C-c>` / `g<C-c>` cancel jobs (the result is dropped; Polars work can't be
  interrupted, it finishes in the pool), `gq` / `<C-q>` quit; `<C-f>` / `<C-b>` page; `/` `?` `g/` `g?`.
  With S-21 A: `=`, `z/` `z?`, `z|` `z\`, `:select` / `:unselect`; regex filter on `gz|`.
- Keymap resolver: a later binding that a key prefixes beats an earlier exact one, so
  `vd.bind("sheet", "<Space> f", …)` makes `<Space>` a leader again.
- Old vdrs meanings waiting for the vim profile (part D): `$` last column, `/` all columns, `<C-c>`
  quit is not kept (`:qa`). Until part D they are one `vd.bind` away (`vd.bind("sheet", "$",
  "goto_rightmost")`).
- Shipped in one commit, with the guide, recordings, doc-sync tests and S-15.

Phase 0 is one commit per row group, each updating the guide, its recordings, the doc-sync test,
S-15 §3–4 and the `init.lua` template. Tests: scenario per moved key (old key does the VisiData thing,
new key does the vdrs thing); `<C-c>` during a running job cancels it and the app stays open.

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
| A2 reload guard | `<C-r>` (`:refresh`, §0) on a sheet with unsaved edits asks first | VisiData's `Ctrl+R` reloads and drops edits silently (`quitguard` off) |
| A3 `<C-z>` suspend | leave raw mode + alt screen, `SIGTSTP` self, restore on `SIGCONT` (unix only) | every TUI does this; VisiData does |
| A4 colors | detect `COLORTERM=truecolor|24bit`; else map theme RGB to 256 colors; `NO_COLOR` → no color, selection/cursor by reverse/bold | 24-bit-only breaks on older terminals, tmux without `Tc`, Linux console |
| A5 help | `:help` (and `<F1>`) opens a sheet of all bindings of the current layer (key, action, desc) — the which-key table, searchable with `/` | VisiData `z^H` help-commands; nothing in vdrs today |
| A6 keyboard protocol | crossterm `PushKeyboardEnhancementFlags(DISAMBIGUATE_ESCAPE_CODES)` when supported, so `<C-i>` ≠ `Tab` and `Esc` is immediate | needed for A7's jumplist and S-23 `Tab` |
| A7 jumplist | the list of big jumps (`gg G :N n N /` hits, sheet switches, freq dives) with `:jumps`; keys `<C-o>` / `<C-i>` in the vim profile only (§0); default gets `<C-^>` prev sheet (VisiData `jump-prev`) | vim; VisiData only has `Ctrl+^` |
| A8 Space | opens the `:` line (§0); Tab completion lists every `Action::NAMED` + ex command with its key, fuzzy-matched | VisiData `Space` = exec-longname; discoverability (VisiData #2313) |
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
- Reselect the last range: `gv` in the vim profile only (default `gv` = unhide-cols, §0); `:reselect`
  in both.
- Visual ranges are row ids (survive sort, like selection).
- Tests: scenario per verb on a range; range survives `[` sort; monkey gets `V`, `<C-v>`.

## 5. Part D — `keymap = "vim"` profile (3–4 d)
Rebinds only where VisiData and vim disagree; everything else stays VisiData.

| Key | VisiData / vdrs default | vim profile |
|---|---|---|
| §0 rows | VisiData meanings | the "Vim profile" column of the §0 table (`.` repeat, `$`/`0`, `<C-d>`/`<C-u>`, `<C-r>` redo, `<C-o>`/`<C-i>`, `gv` reselect) |
| `u` | unselect row | undo |
| `<C-r>` | `:refresh` (§0) | redo |
| `U` | undo | unselect row (VisiData `u` moves here) |
| `R` | redo | — (unbound; free for future replace mode) |
| `q{a-z}` / `@{a-z}` / `@@` | `q` pop sheet; `m` macro (S-19) | record / play macro into a register (S-19 machinery) |
| `q` alone (no register key in 1000 ms) / `:q` | pop sheet | `:q` / `ZZ` pop; `q` waits for a register |
| `d{motion}` `y{motion}` `c{motion}` | `d`/`y` act on the row at once (S-18) | operators: `dd` `yy` = row, `dj` `d3j` `dG` `dgg`, `yip`-style text objects (below) |
| `"{a-z}` | `"` = new sheet of selected rows | register prefix for the next `y`/`d`/`p`; `"` sheet moves to `g"`-like `:dupsel` (decision V3) |
| `x` | cut row (S-18) | cut cell (vim `x` = one char) |
| `H` `M` `L` | slide column / melt / slide | screen top / middle / bottom row |
| `zt` `zz` `zb` | — | scroll cursor row to top / middle / bottom |
| `w` `b` `e` | — / sidebar (S-23 A) / edit | next / prev non-null cell in the row (`e` edit moves to `i` / `<CR>` on a cell, decision V4; sidebar toggle moves to `:sidebar` / `<C-g>`) |
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
| 0 | §0 key alignment, rows marked "phase 0" (`Space`, `$`/`gl`/`gh`, `g-`/`gv`, `<C-c>`/`gq`/`<C-q>` + job cancel, `<C-f>`/`<C-b>`, `/`/`g/`/`?`); the R-d rows move with their features | 2–2.5 d |
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

## 9. Decisions (confirmed 2026-11-08)
- **Rule**: §0 (R-a … R-e) for every spec. Direction §9.7: default = VisiData, opt-in vim profile for new or more standard meanings.
- **V1** `v` unbound for now; `V` / `<C-v>` visual in the default keymap (R-b). When multi-line cells
  ship, default `v` = VisiData's toggle; a charwise visual on `v` would be vim-profile only.
- **V2** `gv` = unhide-cols by default (VisiData); reselect in the vim profile.
- **V3** `"` = register prefix in the vim profile only; the selected-rows sheet there is `:dupsel`.
- **V4** `e` = cell motion in the vim profile; edit on `i` / `<CR>` there. Default `e` = edit.
- **V5** `<C-r>` redo and the `<C-o>`/`<C-i>` jumplist are vim profile only; default `<C-r>` = `:refresh`.
- **V6** `Space` opens the `:` line (VisiData exec-longname) rather than a separate palette.
