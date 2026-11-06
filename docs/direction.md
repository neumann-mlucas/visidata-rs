# VisiData → Rust Refactor: Direction Doc

Research synthesis for a Rust rewrite of VisiData using Ratatui + Polars + mlua, leaning harder into vim/neovim philosophy. Based on local codebase read, GitHub issue scan, and tool-landscape survey (Oct 2026).

Everything below is a proposal, not a commitment. Open questions at the bottom need your call before anything gets built.

---

## 1. Current VisiData — what to keep, replace, cut

Source: [saulpw/visidata](https://github.com/saulpw/visidata) (v3.4).

### Core abstractions (worth preserving in shape)

| Abstraction | File | Keep? | Why / how it changes |
|---|---|---|---|
| `BaseSheet` / `TableSheet` hierarchy | `visidata/sheets.py:149` (1379 LOC) | **Keep shape** | `trait Sheet` + concrete types. 48 subclasses today — most should be views, not types. |
| `Column` with getter/setter/type/width/colorizers | `visidata/column.py:44` (637 LOC) | **Keep concept** | Backed by Polars `Series` + a `derive_fn: Option<LuaFunction>` for computed columns. |
| `Command` (longname + execstr + helpstr) | `visidata/settings.py:123` | **Keep** | Replace Python `exec(execstr)` with a Lua function reference. The eval-the-string ergonomics come from Lua, not from re-implementing a mini-interpreter. |
| `LazyComputeRow` expression resolution w/ cycle detect | `visidata/sheets.py:76` | **Keep concept** | Column expressions reference other columns transparently. In Rust this is just a DAG + topo-sort. |
| Modal keybind model w/ prefix maps (`g`, `z`, `gz`, `Alt+`) | `visidata/keys.py` | **Keep + extend** | Users want *more* vim here (see §2). Leader key, text objects, `.` repeat. |
| `@asyncthread` for long ops | `visidata/vdobj.py:14` | **Replace** | Tokio tasks. No GIL. Progress channels instead of shared mutable progress lists. |
| Undo via closure-stack (`undofuncs`) | `visidata/undo.py` | **Replace** | Polars LazyFrame plan IS the undo log — the stack of transforms is the history. Snapshot the plan, not the data. |
| `.visidatarc` Python config | `visidata/plugins.py` | **Replace** | `init.lua`. Sandboxed. Hot-reload. |

### Hard replacements

- **curses → Ratatui + crossterm.** The curses layer is the #1 bug source (see §2). Ratatui's double-buffer diff gives us dirty-region updates for free; today VisiData does a full erase+redraw every keystroke with a 100ms debounce (`visidata/mainloop.py`).
- **In-memory Python row objects → Polars columnar.** Rows today are Python lists/dicts/namedtuples stored in `sheet.rows` with per-cell `LazyComputeRow.__getitem__` on every access. That's the perf ceiling, acknowledged by the maintainer in #1068.
- **Python eval'd `execstr` → Lua function.** Same ergonomics (interactive expression editing, macro recording), real sandbox, no AST we have to invent.
- **60 bespoke loaders → Polars-native where possible (csv/parquet/json/arrow/xlsx/sqlite) + Lua loader plugins for the long tail.** VisiData ships loaders for `grep`, `mailbox`, `conllu`, `pcap`, `ttf`, `vcf`, `shapefile`, `reddit`, `zulip`, `matrix`, `mbtiles`. These should not be in the core binary.

### Hard cuts

- **130-module import cascade** at startup (`visidata/__init__.py:29-156`). Rust binaries don't need auto-discovery of features at import time; plugins load on demand.
- **Draw cache layer** (`@drawcache = @lru_cache` cleared per frame). Ratatui's render model subsumes this.
- **Special metasheets as sheet types** (`CmdlogSheet`, `OptionsSheet`, `IndexSheet`, `HelpSheet`, `PluginsSheet`). These are views over structured data, not new type hierarchies.
- **Options context chain** (default→global→class→instance resolution — `visidata/settings.py`). Flat config with per-sheet overrides is fine; the four-level fallback is complexity nobody asked for.

---

## 2. Where the real pain lives (GitHub signal)

Repo: 9.3k stars, 115 contributors but effective bus factor ~2 (saulpw 5027 commits, anjakefala 2363, next 556). Release cadence: one minor every 6–12 months. **No existing Rust-rewrite discussion in the tracker** — lane is open.

Top-of-ticket thermal mass clusters into five themes:

1. **Engine ceiling** — the maintainer's own words in **#1068**: *"if we want to seriously address performance... we'd want to change the architecture and/or use something like pandas or DuckDB more proactively."* 150MB CSV sorts take minutes vs. seconds in pandas. See also #1050 (freq-table slow), #1964 (nav lag), #2440 (ODS loader OOMs on phantom cells), #2003 (parquet vlen strings), #184 (20k-row lag, open since v1.3), #2634 (replay slower in 3.1.1).
2. **Curses / terminal fragility** — the single biggest category of filed bugs: #192 (`get_wch` env hell, 38 comments), #1259 (menu glitches, 37 comments), #1454 (Windows resize crash), #2138 (sidebar crash on resize), #2756 (uv breaks terminfo), #2824 (macOS Sequoia colors), #3042 (Ghostty histograms wrong), #3206 (`COLOR_PAIRS-1` crash), #2416 (CJK wrap broken), #2685 (wcwidth is hand-rolled).
3. **Users want MORE vim, not less** — the UX wishes are modal-direction, not away from it: #1441 scrolloff, #1651 vertical splits, #2500 cell-edit needs readline/text-objects (13 comments), #2574 space-as-leader, #3008 adopt `y/p` mnemonics, #2986 enter-moves-down.
4. **Plugin brittleness** — monkeypatching a live Python runtime. #2142, #2084 (macros break on Python bumps), #2777 (invalid .visidatarc crashes on first use not parse), #2703 (no central plugin list), #2736 (vdrepl merged in-tree because external lifecycle broken).
5. **Python version tax** — #2179 (3.12 broke expr eval), #2773 (3.14 path tests), #2923 (Debian). A compiled binary erases this entire category.

### What a Rust+Polars+Ratatui+Lua rewrite plausibly fixes

Themes 1–5 above, structurally: engine swap (1), library-grade terminal handling (2), stable sandboxed scripting API (4), binary distribution (5), vim-ish by construction (3).

### What it won't fix (name these up front so we don't over-promise)

- **Discoverability** of hundreds of commands (#2313, #2202, #322) — this is a docs/UX problem; `which-key` style palette helps but Lua doesn't.
- **Keymap wars** (#3008, #2574). Pick an opinion, ship it. New engine, same arguments.
- **Live plots** (#2570), **LLM-on-cells** (#2947), **structured replay across locales** (#3195) — product decisions, not engine ones.
- **Bus factor.** Rust codebases don't produce maintainers; often *smaller* contributor pool than Python.
- **Distro packaging** (#2922, #2923) — a different kind of politics, same cost.

---

## 3. Landscape — what's taken, what's open

| Lane | Owner | Verdict |
|---|---|---|
| SQL REPL over files | duckdb-cli, clickhouse-local, datafusion-cli | **Don't compete.** SQL REPLs are not modal cursor explorers. |
| Pure CSV viewer | csvlens (Rust, Ratatui) | **Don't rebuild.** Scope-complete for what it is. |
| csvlens++ (CSV/Parquet + vim keys + basic agg) | datasight (Rust, Ratatui, ~12 stars) | **Half-taken, barely moving.** View-only. Open to a bigger swing. |
| Pretty-printer | tv (tidy-viewer) | Done. |
| Scripted transforms | qsv, csvtk, xsv | Pipeline tools, not interactive. Different product. |
| Structured shell | nushell | Overlapping surface but TUI table viewer is minimal. |
| Terminal spreadsheet w/ formulas | sc-im | Pre-dataframe era, crashy. |
| Desktop pivot GUI (DuckDB) | Tad | Not terminal. |
| Log viewer | lnav | Logs, not tables. |

Nobody has shipped "VisiData with pivot + Lua + Polars" in a terminal.

### Infra decisions already made for us

- **mlua** for Lua↔Rust. `rlua` was archived Sep 2025 and now redirects to mlua. No debate.
- **Ratatui + crossterm** for TUI. Elm-ish Model/Update/View is the documented pattern.
- **yazi is the reference architecture.** Rust + Ratatui + mlua + async plugins + separated plugin *shapes* (loader/preview/functional). Clone the architecture, don't invent.
- **Polars 2.0** (Oct 2026) made streaming default + spill-to-disk at ~80% RAM. Caveat: joins and group-by are *not yet* out-of-core — and pivots are group-bys. This is the single biggest engine risk.

---

## 4. Direction

### Engine: Polars-primary, DuckDB-optional
- LazyFrame IS the user's history/undo log. The transforms the user builds with keystrokes compile to a Polars plan.
- DuckDB behind an optional backend switch, for SQL-heavy or OOC-pivot workloads where Polars 2.0 streaming falls short.
- Don't ship both as first-class from day one. Polars first; DuckDB when a user actually hits the ceiling.

### TUI: Ratatui + crossterm, yazi-shaped
- Model/Update/View. Render on event, not on timer. Yazi idles at 0% CPU; VisiData doesn't.
- Async I/O for load/compute/preview via tokio. UI thread only touches state via a channel.

### Scripting: mlua, Neovim-style API discipline
- Separate plugin shapes (yazi pattern): **loader** (bytes→DataFrame), **aggregator** (DataFrame→DataFrame), **renderer** (cell→display), **keymap** (binding→action).
- `since` field per API fn, `vim.deprecate()`-style, SemVer on plugins, deprecations never silently removed. This is the one thing the Neovim ecosystem got genuinely right; copy it.
- Config: `~/.config/vdrs/init.lua`. Hot-reload on save.

---

## 5. Distinctive bets, ranked

Ranked by (differentiation × achievability). Everything above is table stakes; this is what makes it worth shipping.

1. **Modal pivot/melt with live preview over Polars LazyFrame.** VisiData has pivot/melt but they're clunky and freeze on big data. Nobody has them fast + previewable. This is the single most differentiating feature.
2. **Visible LazyFrame plan + "save as script".** A side-panel showing the Polars plan your keystrokes built. `o` to optimize, `s` to export as `.pl` or SQL. Turns the TUI into a query-building IDE. Nothing in any tool category does this.
3. **yazi-shaped mlua plugin system with Neovim-grade API versioning.** You become the extensible one in a field of monoliths. Proven architecture, not an invention.
4. **Ex-line expression language.** `:filter age>30 | sort name | pivot by state agg sum(revenue)` composes like a vim command, with history + completion + `.` repeat. VisiData only half-does this.
5. **Streaming / tailable sheets.** Point at a growing parquet dir, Kafka topic, or tail of a `.jsonl` file; get a live-updating cursor sheet. lnav × VisiData hybrid. Polars 2.0 streaming makes this feasible for the first time.
6. **Modal structured diff of two frames.** Schema diff + join-on-keys row diff. Small surface, large pain, nobody owns it.

---

## 6. Proposed phase order (each phase = a usable binary)

Lazy on purpose. Phase N ships before Phase N+1 is scoped.

- **Phase 0 — Viewer parity with csvlens.** CSV/TSV/Parquet/Arrow. Scroll, search, filter, freeze cols. No Lua yet. Beats csvlens on Parquet + Arrow and that's enough.
- **Phase 1 — Columns, types, sort, freq-table, ex-line.** Introduce the ex-line parser (bet #4) now so later phases live on top of it.
- **Phase 2 — Pivot/melt with live preview panel.** Bet #1 + bet #2 together. The differentiating release.
- **Phase 3 — mlua plugin system.** Loaders first (easiest surface, biggest user leverage), then keymaps, then aggregators.
- **Phase 4 — Tail-sheet / streaming source.** Bet #5.
- **Phase 5 — Modal frame diff.** Bet #6.

Stop at the end of any phase if adoption signal is weak; each phase stands alone.

---

## 7. Explicit non-goals

- **Loader-count parity with VisiData.** The 60-loader zoo is a liability, not an asset. Lua loader plugins cover the tail. Core binary ships ~10.
- **`.vdj` replay back-compat.** Nice if cheap, not if it drags. New plan format based on the LazyFrame.
- **Winning the SQL-REPL lane.** duckdb-cli owns it. We ship a `:sql` ex-command that pipes to DuckDB and reads the result as a sheet; we do not become a SQL REPL.
- **Cross-platform curses parity.** We're not emulating curses behavior; terminal issues become Ratatui/crossterm's problem, not ours.
- **Being a "faster VisiData".** That framing underwhelms (saulpw himself prioritizes responsiveness over speed). Lead with pivot preview, Lua, streaming — capabilities. Rust is incidental.

---

## 8. Risk register

- **Polars OOC gap for group-by/join.** Pivots ARE group-bys. If a user pivots a 50GB parquet, Polars 2.0 may not spill correctly. Mitigation: ship DuckDB backend by Phase 2, not Phase 5. Concrete, not speculative.
- **"Rewrite in Rust" fatigue (2026).** Don't frame the project as a rewrite; frame it as the modal pivot/plan/Lua tool. VisiData comparison is subhead, not headline.
- **Audience small anyway.** polars-gui exists and is barely alive. VisiData has 9k stars after a decade. This is a niche in a niche. OK — but means distribution, docs, and demo videos are 50% of the project, not 10%.
- **mlua version churn / Lua 5.1 vs 5.4 vs Luau split.** Pick one (5.4), freeze, document. Don't support three.
- **ponytail**: this doc assumes "yazi architecture" is the right lift. Pay down by actually reading yazi's source before Phase 1 locks in — don't commit to a pattern we've only read about in a landscape report.

---

## 9. Decisions (locked 2026-10-08)

1. **Scope**: new product with VisiData's **shape**. Not feature-parity, not a blank-slate "what if a spreadsheet in a terminal?" — the modal flow, g/z prefixes, and command vocabulary are the taste we keep.
2. **Audience**: primary user is a heavy VisiData user (the person commissioning this). Build-for-me first; broader terminal-data users are secondary. **Implication**: default keymap must feel like VisiData from keystroke 1 even without a compat layer — muscle memory is the UX contract, which is stricter than "generic vim-ish."
3. **Engine**: Polars only, all phases. DuckDB backend dropped. Revisit only if we hit a Polars OOC wall we can't engineer around.
4. **Config**: pure `init.lua`. No TOML/KDL layer.
5. **Compat**: zero. No `.vdj`, no `.visidatarc` import, no reading VisiData state files. Data files are shared; everything else starts fresh.
6. **Name**: codename `visidata-rs`; final name deferred. Decide before public release.

### Deltas these cause in earlier sections

- §1 keymap row: not just "modal, g/z prefixes" — the specific VisiData default bindings become the baseline that S-04 (keybindings) must respect. Deviations need an explicit justification, not the other way around.
- §5 bet #4 (ex-line) bumped in priority. A daily VisiData user doesn't leave the keyboard; ex-line + `.` repeat is leverage that compounds.
- §1 and §5 DuckDB-related rows drop. Simpler Phase 2.
- §8 Polars OOC risk no longer mitigated by "ship DuckDB" — mitigation now is "when a user hits it, add DuckDB then." Deferred, not pre-paid.
