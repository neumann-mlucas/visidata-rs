# VisiData parity report

Written 2026-11-16 against VisiData `develop` @ `68d8ac31` (v3.5 + 5 commits)
and vdrs `master` @ `732dc4d`. Sources:

- `../visidata/CHANGELOG.md` v3.0–v3.5 (2023-12 → 2026-10), read in full.
- GitHub `saulpw/visidata`: all 1,728 issues and 1,053 PRs (titles, labels,
  state), and the bodies of the issues cited below.
- vdrs: `Keymap::defaults`, `src/options.rs`, S-15, and the replay corpora.
- 10 probe scripts: VisiData ran each one to make a golden, then vdrs replayed it
  (branch `test/vd-probes`, section 5).

"Verified" means a probe or a test showed it. Anything else is marked
*not verified*.

## 1. VisiData at a glance

- **Issues:** 1,728 in all, only 4 open. The maintainers close almost everything.
  - 1,656 completed, 66 not planned.
  - Labels: `bug` 769, `wishlist` 452 (189 granted), `By Design` 36,
    `Limitation` 8, `wart` 8, `needs design` 14.
- **Bug reports per year:** 2019: 54, 2020: 134, 2021: 115, 2022: 117, 2023: 133,
  2024: 98, 2025: 46, 2026: 71.
  - The core is still changing: 2026 rewrote concat, describe, dedupe, cmdlog and
    the hidden-column model.
- **PRs:** 883 merged, 166 closed, 4 open.
  - Top authors: midichef 305, anjakefala 210, saulbert 67.
  - midichef writes most of the edge-case fixes, the ones vdrs should mirror.
- **Open items now:**
  - #3276 / PR #3277: `-o` exits 0 when the save fails.
  - #3278: filetype with a remote URL.
  - #3279: vimcompat `Ctrl+D` clash.
  - PR #3273: decimal `$(12.34)` parsed as negative.
  - vdrs has no batch `-o` and no http reader, so none of these apply yet.

## 2. VisiData features vdrs doesn't have

### 2.1 Default keys vdrs leaves unbound

The comparison: every `addCommand` key on `BaseSheet`, `Sheet`, `TableSheet`,
`IndexSheet`, `ColumnsSheet` and `FreqTableSheet`, against `Keymap::defaults`
(176 chords). Several of these features exist in vdrs as ex commands only.

| Gap | VisiData keys | vdrs today |
|---|---|---|
| Core file and sheet keys | `g Ctrl+S` save-all, `` ` `` open-source, `Q` quit-sheet-free, `gU` undo-last-quit | `o` `Ctrl+S` `&` `g&` `M` `T` bound (S-50, S-51) |
| Movement | `zr` go-row-number, `Ctrl+Left/Right` page left/right | `{` `}` `<` `>` `z<` `z>` `c` bound (S-50) |
| Selection | `g,` `z,` `gz,` select-equal-row / exact-cell / exact-row, `gzs` `gzu` select/unselect after, `gt` `zt` `gzt` toggles | `s` `u` `gs` `gu` `,` `\|` `\` `g\` `z\` exist |
| Sorting | `gz[` `gz]` add key sorts, `z[` `z]` change one level | `[` `]` `g[` `g]` |
| Columns | `w` addcol-window, `z;` addcol-shell, `gza` addcol-bulk, `zi` `gzi` incr-step, `z=` setcell-expr, `gz=` setcol-iter, `z#` type-len, `z@` custom date, `z%` floatsi, `z~` type-any (and the `g`/`gz` selected forms on ColumnsSheet) | `=`, `g=`, `i`, `gi` |
| Freq and dive | `zF` freq-summary, `gzEnter` dive-selected-cells, `zEnter`/`gzEnter` dive-except on freq | `gF`, `zEnter` open-cell (S-51); dive-except in S-46 B |
| Layout | `z-` `z_` `gz_` `g_` resize, `zv` `gzv` row height, `v` multiline, `zj zk zh zl` (with `gz`) scroll cells, `zZ` split input | — |
| Meta sheets | `gS` sheets-all, `zO` sheet options, `gO` open-config, `Ctrl+E` `gCtrl+E` `zCtrl+E` errors, `Ctrl+P` statuses, `Ctrl+T` threads, `gI` describe-all | `O`, `I` |
| Python-only | `Ctrl+X` pyobj-expr, `Ctrl+Y` pyobj-row, `gCtrl+X` import-python, menus `Alt+…` | out of scope |

S-50 bound `o`, `Ctrl+S`, `&`, `M`, `{` `}`, `<` `>`, `z<` `z>`, `c`, `g[` `g]`.
S-51 added `T`, `gF`, `zEnter` and `g&`.

The keys a VisiData user presses from muscle memory (`o`, `Ctrl+S`, `&`, `M`,
`{`/`}`, `<`/`>`) are bound now; what is left in the table is rarer.

### 2.2 v3.x commands with no vdrs counterpart

These are named in the changelog. I found no vdrs action, ex command or key for
them by name; vdrs may cover some under another name, so this is *not verified*
case by case.

- **Columns and types:**
  - `type-decimal` (3.5, opt-in, accounting negatives);
  - `type-datetime`, `type-url` / `open-url`, `type-ipaddr` / `type-ipnet`;
  - `addcol-histogram`, `addcol-sparkline`, `addcol-source`;
  - `normalize-col-names` / `clean-names`, `setcol-fake`;
  - `resize-col-max-all` (3.5).
- **Rows:** `select-equal-selected`, `select-first` (freq), `sort-selected-asc/desc`,
  `reopen-last-closed`, `reload-rows`.
- **Aggregation:** `addcol-aggregate`, `addcol-rank-sheet`, `memo-aggregate` (S-46 B).
- **Conditional format:** `color-cell-from-col`, `color-row-from-col` (3.5).
- **Undo and history:** `zU` undo-redo-input and `redo-input` (3.5); input
  history palette (3.4).
- **Search:** hlsearch (`highlight_search`), so all matches of `/` show (3.4).
- **Prompts:** `name=expr` in addcol-expr (3.4); Tab completion in open/save
  prompts (3.5).
- **Options:** `max_rows`, `rename_cascade`, `confirm`, `overwrite`,
  `json_ensure_ascii`, `regex_skip`, `save_encoding`.
  - vdrs has 15 options; VisiData has a few hundred.
  - `regex_skip` is the most data-affecting of these.
- **cmdlog:** `set-envvar` (3.5).
- **CLI:** `-o` and `-of` (batch save), `-O` output-cell, `-f` per path.
- **Readers:**
  - vdrs leaves out on purpose (S-15): stata/sas/xpt, hdf5, npy, png, http. xml and msgpack read since S-53.
  - Also missing: `.psv`, grep output, `.rec`, `.vcf`, postgres, s3, `.zstd`.

## 3. VisiData bug fixes, checked against vdrs

Only fixes in areas vdrs implements are listed. Curses, Python and
packaging fixes are left out.

| VisiData fix | vdrs | How known |
|---|---|---|
| #3085 CSV blank lines skipped (3.4) | done (S-49) | `vd_local` `csv-blank-lines` PASS |
| #2944 dict/list cells save as JSON in text formats (3.5) | done (S-49) | `vd_local` `save-nested-csv` PASS |
| #3178 append keeps same-named columns distinct (3.5) | **diverges**: Polars renames `a, a` to `a, a_duplicated_0` at read | probe `append-dup-names` |
| #3219 dedupe groups as freq does: `1` ≠ `1.0` (3.5) | **diverges**: a JSONL column of `1`, `1.0` is one f64 column, so freq and dedupe both see `1.0` twice | probes `dedupe-int-float`, `freq-int-float` |
| #3223 all null forms are one distinct value; describe fixes (3.5) | median selection matches already (S-44). Null count differs: VisiData counts CSV `""` as a value (0 nulls), vdrs as null (2) | probe `describe-nulls` |
| #3208 concat matches by position (3.5) | done (`:join … concat`) | S-47 `join-concat` PASS |
| #3142 sort-add twice on one column (3.4) | matches | probe `sort-add-twice` PASS |
| #2475 JSON: no type inference (3.1) | numbers compare equal | probe `json-numbers` PASS |
| #3001 nested freq + dive-selected (3.4) | probe couldn't run: replay's freq-col on a freq sheet with an empty `col` field. Harness gap, app *not verified* | probe `nested-freq-dive` |
| #2476 derived nulls saved as `str(None)` (3.1) | *not verified*: replay can't run `addcol-expr` (Python) | probe UNTESTABLE |
| #3003 footer totals refresh after edits (3.5) | *not verified* | — |
| #3122 hidden-column edits not committed (3.5) | *not verified* (`:save` writes the view) | — |
| `[save]` NaN saves as null (3.4) | *not verified* | — |
| #2868 mean/median keep the numeric type (3.4) | numerically equal (`2` vs `2.0`) | eq_loose |
| `[csv]` skipinitialspace (default since 2.x) | done (S-48) | S-47 `cast-*` PASS |

## 4. VisiData's own limitations and by-design choices

Sources: the `By Design`, `Limitation`, `wart` and `needs design` labels.
For each: whether vdrs avoids it or should copy it.

**Where vdrs is already better, by design:**

- Async corruption from commands on half-loaded sheets (#759). Sheets not
  joinable until viewed (#656). vdrs materializes in the background, and joins
  read the plan.
- `=` expressions re-evaluated on every draw (#2371); slow describe on dates
  (#2271). Polars evaluates once, vectorized.
- Saved text formats get displayed values, not raw ones: `1,370` (#2014).
  - vdrs saves the frame, so likely raw (*not verified with a fmtstr*). Keep it.
- Scientific notation on string columns (#2404): not applicable. A transposed sheet
  losing types (#2575): vdrs's `T` (S-51) makes every cell text too, as VisiData.

**Where VisiData's choice is the reference, and vdrs differs or must decide:**

- Sort uses the typed value, not the formatted one (#1013). vdrs does the same.
- `z[` doesn't keep the secondary sort at group edges (#3032).
- Copy/paste of a row drops values from added columns (#1727).
- Ctrl+Q can't be unbound (#3128). vdrs: check `vd.bind` (*not verified*).
- Saving sheets that have error cells (#198/#199, still `needs design`).
  - vdrs settled it for JSONL error rows only (S-44).

**Open design asks with no VisiData answer** (vdrs could lead):

- An aggregator's value inside expressions (#2862).
- Undo for selection (#362).
- Choosing the save format (#2286).
- Atomic save (#1009). vdrs already saves through a tempfile and rename.
- Rows added to a join (#2024).
- Expressions that read the previous row (#2033).

## 5. Where to extend our replay corpus

### Bug density by area

These are keyword buckets over issue titles, so read them as rough. All bugs
(n=769), with since-2024 (n=215) in brackets:

- columns/layout 133 (40)
- loaders other than csv/json 93 (27)
- types, dates and numbers 85 (25)
- editing and input 64 (17)
- save 60 (12)
- cmdlog, replay and macros 59 (17)
- nulls and errors 54 (10)
- expressions 46 (16)
- sql and db 39
- undo and commit 35 (9)
- select and filter 28 (13)
- csv/tsv 27
- aggregators 22 (9)
- json 22
- pivot and freq 17
- join 16 (9)
- sort 13

### What the probes show

Every difference found came from one of three places:

1. Reader edges: blank lines, duplicate names, mixed int/float, `""` versus null.
2. Save of non-flat data: nested cells.
3. Value identity: freq and dedupe grouping.

Join, sort and JSON numbers passed. Our corpus has covered joins since S-47; it
had none of the other three.

### Recommendations, in order

1. **Reader edge cases.** Highest yield: 4 of the 6 probe differences, and 93+27
   bugs in VisiData. Each probe file goes into the corpus, plus:
   - BOM;
   - CRLF;
   - ragged rows (short and long);
   - quoted newlines;
   - a header-only file and a 0-byte file;
   - duplicate and empty column names;
   - unicode names;
   - leading-zero ids;
   - `regex_skip`-style `#` comment lines.
   Run the same rows as CSV, TSV and JSONL.
2. **Save round trips of hard values.** Nested cells (#2944), NaN and inf,
   null versus `""`, bools, dates with time, very long text. Save to each text
   format, then open again. Today only flat sheets are covered.
3. **Value identity.** `1` / `1.0` / `"1"`, `True` / `1`, null / `""` / NaN
   through freq, dedupe, select-equal, pivot and join keys. VisiData's fixes
   #3219, #3223, #3112 and #2015 all landed here.
4. **Edit, undo and commit chains.** Hidden-column edits (#3122), type change and
   undo (#3234), undo across sorts and filters, edits on sorted or filtered views
   then save. 35 bugs, and S-45 just added this surface.
5. **Selection-driven commands.** Nested freq with dive-selected (#3001),
   select-after/before, `gs`/`gu` on filtered views. 13 bugs since 2024.
   First fix the harness gap the `nested-freq-dive` probe hit.
6. **Aggregators under change.** Footer after edits (#3003), aggregators on
   derived sheets, empty groups, error cells.
7. **Lower priority:** pivot and freq (17 bugs, mostly covered by S-47), and sort.

**Not worth adding:** loaders vdrs leaves out on purpose, curses and terminal
bugs, and Python expressions (`addcol-expr` is UNTESTABLE in replay).

### Decisions these raise

Each is a design choice, not a test to write:

- Duplicate column names (Polars can't hold them):
  - keep `_duplicated_0`, or
  - use VisiData's display names over unique internal ones.
- A blank CSV line: skip it as VisiData does (likely yes).
- Nested cells in a CSV save: write JSON text (VisiData 3.5).
- Mixed int/float in JSON: one f64 column (Polars), or keep the source text for
  grouping.
- CSV `""`: null (vdrs) or empty text (VisiData). This one touches nulls,
  describe and freq everywhere.
