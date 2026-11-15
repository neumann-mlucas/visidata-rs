# S-40 — Lua config: event groups, project config, multi-sheet loaders, bars and sidebar pages

Status: **drafted 2026-11-15**, rough. K1–K7 decided 2026-11-15 (§9).
Depends on: S-09 (plugin shapes), S-10 (API versioning), S-29 (`vd.on`, `vd.exec`, `:source`),
S-39 A (a loader may return rows), S-37 P0c (`options.lua`), S-23 A (sidebar pages).

Scope: let `init.lua` do more, and let one project carry its own config. Each part ships alone,
bumps `vd.api_version` by one and adds a guide section.

Order: A, B, C, D, G, E, F.

## 0. Today

- One config: `$XDG_CONFIG_HOME/vdrs/init.lua`, then `macros.lua` next to it
  (`Plugins::load_from`, `src/plugins.rs`). Hot reload watches the `init.lua` mtime only.
- `vd.on(event, fn)`: 4 events (`open` `push` `save` `quit`; `tick` from S-29 is not built), a
  flat `Vec<(String, LuaFunction)>`. No way to remove a handler, filter by path, or run once.
  `:source` on a file with `vd.on` runs it again and **doubles** its handlers; hot reload is
  fine because it builds a fresh VM.
- `vd.register_loader { extensions, open }`: picked by extension, the **first** registered wins,
  `open` returns one path. `name` is accepted and unused. `--format` takes only built-in kinds.
- The title bar and status line are fixed Rust (`render_titlebar`, `render_statusbar` in
  `src/app/render.rs`). Lua picks `vd.set_theme("dark"|"light")`, nothing else.
- Sidebar pages are a closed enum (`SidebarPage`: plan, plan-opt, column, cell, help).

## A. Event groups (`vd.augroup`) and handler options (1–1.5 d) — shipped 2026-11-15

As built: `:source` runs ex scripts only and hot reload builds a fresh VM, so nothing re-runs
Lua in a live VM today and the "doubling" below can't happen yet; groups still clear on
`vd.augroup` for when something does (B's project file). `enter` fires from `follow_link` (once
per activation, the first sheet at start too), `loaded` right after `push` with `rows` empty
while the count is running, `reload` after hot reload with `paths` = the `init.lua` path.

Neovim's autocmd shape, cut to what vdrs's events need.

```lua
local g = vd.augroup("logs", { clear = true })   -- clear: drop the group's handlers first
vd.on("open", { group = g, pattern = { "*.log.csv", "*.log.parquet" } }, function(s)
  vd.exec(":type ts date"); vd.exec(":sort -ts")
end)
local id = vd.on("save", { once = true }, function(e) vd.toast("saved " .. e.rows) end)
vd.off(id)                 -- one handler
vd.clear_group("logs")     -- every handler in the group
```

- `vd.on(event, fn)` keeps working: the options table is an optional second argument.
- `vd.on` returns a handler id (integer). `vd.augroup(name, opts)` returns the name, so a string
  works wherever a group is taken. `clear` defaults to `true`, as in Neovim: re-sourcing a file
  replaces its handlers instead of adding copies. That fixes the `:source` doubling.
- `pattern`: a glob or a list of globs. `*` and `?` only (no `**`, no braces). A pattern without
  `/` matches the file name; with `/` it matches the full path. Translated to the `regex` crate,
  no new dependency. Events without a path (`quit`, `tick`) refuse a `pattern` at `vd.on` time.
- `once = true`: removed after it runs.
- `vd.handlers()` lists `{ id, event, group, pattern }` for debugging; `:handlers` shows it as a
  sheet.

New events, each with a real caller (more only when someone asks):

| Event | Fires | Arg | Caller |
|---|---|---|---|
| `tick` | the 2 s tick (S-29 planned it) | — | a live plugin polls a file or a command |
| `enter` | a sheet becomes active (push, pop, `S`, pane focus) | `vd.sheet()` table | a status line that differs by sheet; per-sheet keys |
| `loaded` | the first window of a sheet is materialized | same + `rows` (if known) | a cast or sort that needs the schema read |
| `reload` | after hot reload or `:source` | `paths` | re-applying state a plugin keeps outside the VM |

`pattern` on `enter` and `loaded` matches the sheet's source path; derived sheets have none and
never match a pattern.

Tests: plugin unit tests (ids, `once`, `clear`, `vd.off`, glob on name vs path, `pattern` refused
on `quit`); scenario: `:source` the same file twice, one handler fires; `enter` fires on `q` back
to the parent sheet.

## B. Project config `.vdrs.lua` (1.5–2 d) — shipped 2026-11-15

As built: load order is today's `init.lua`, `options.lua`, `macros.lua`, then `.vdrs.lua` (B3
listed macros before options; the code ran options first since S-37 P0c and that stays). The
project file is found once at start (`Plugins::load`, never under `cfg!(test)`); `:trust`
reloads with the same files. Hot reload watches `init.lua` and `.vdrs.lua`, not `macros.lua` /
`options.lua` (vdrs writes those itself). Renderers keep first-match order: they have no name.

A project carries its own loaders, options and keys, merged over the global config.

### B1. Where

The first `.vdrs.lua` found walking up from the current directory, stopping at `$HOME` (`$HOME`
itself is searched only when vdrs starts there). Not the data file's directory: `vdrs
~/data/x.csv` from a project uses the project's config. (K1.)

### B2. Trust

A `.vdrs.lua` is code from whoever wrote the folder: a cloned repo or an unpacked archive runs
`os.execute` the moment vdrs starts. Neovim's `exrc` asks first; so does vdrs.

- An untrusted file isn't run. The TUI starts with the global config and a toast:
  `.vdrs.lua not trusted: :trust to run it, :trust view to read it`.
- `:trust` stores a copy of the file in `state_path("trust/<path, escaped>")` and reloads.
  Next start, the file runs if its bytes equal the stored copy. Any edit makes it untrusted
  again (comparing bytes needs no hash crate). `:trust off` deletes the copy.
- Editing the file while vdrs runs and trusted: hot reload asks again (the bytes changed).
  Your own edits then need one `:trust`. (K2: or keep trusting files you edited in this session.)
- `--headless`: an untrusted file is skipped with one stderr line.
- Tests run with `state_path`'s `cfg!(test)` guard; a test never trusts a real file.

### B3. Merge order

Each file runs in one VM, in this order:

1. `init.lua` (global)
2. `macros.lua` (global)
3. `options.lua` (global, S-37 P0c)
4. `.vdrs.lua` (project)

What "merge" means per kind:

- `vd.option`: last write wins, per name. The project overrides only what it sets.
- `vd.bind` / `vd.unbind`: later patches win, as today.
- `vd.on`: handlers add up. A project that wants the global group gone calls
  `vd.clear_group("name")`.
- `vd.register_loader`: changes to **last registered wins**, so a project loader for `.csv` beats
  a global one. Today the first wins; nobody relies on it (no shipped loader shares an extension).
- Aggregators, renderers, `vd.command`: same name → last wins.

To **replace** the global config instead of merging: `vd.reset()` as the first line of
`.vdrs.lua`. It clears everything registered so far back to the built-in defaults (keys, options,
handlers, loaders, theme). It's allowed only at the top level of a config file, not inside a
handler. (K3: or a `--config FILE` / `--clean` CLI flag instead, or as well.)

### B4. Reload, sessions, `options.lua`

- Hot reload watches every file it ran (a `Vec<(PathBuf, mtime)>` instead of one path).
- `:mksession` stays config-free: a session is ex commands over sources, not keys.
- S-37's `O` sheet shows each option's value and where it came from (`default`, `init.lua`,
  `.vdrs.lua`). `z<C-s>` keeps writing the global `options.lua`; writing to the project file is
  out of scope until asked.
- `vd.config_dirs()` returns `{ global = "...", project = "..." | nil }` so a config can load
  helpers next to itself. `package.path` gets `<global>/lua/?.lua` and, when trusted,
  `<project>/.vdrs/lua/?.lua`, so `require("loaders.pdb")` works. (K4.)

Tests: unit (walk-up stops at `$HOME`, merge order per kind, `vd.reset()` refused in a handler,
last loader wins); scenario: untrusted file → toast, `:trust` → the project key works, edit the
file → untrusted again.

## C. Loaders: rows, several sheets, choosing a loader (2–2.5 d, after S-39 A)

S-39 A lets `open(path)` return `{ columns, rows }` (one sheet). This part adds:

### C1. Several sheets from one file

`open` may return a list of named parts. Each part is either a path or a rows table:

```lua
-- .cvsv: CSV blocks, each after a line "## <name>"
vd.register_loader { name = "cvsv", extensions = { "cvsv" }, open = function(path)
  local parts, cur = {}, nil
  for line in io.lines(path) do
    local name = line:match("^## (.+)$")
    if name then
      cur = { name = name, lines = {} }; table.insert(parts, cur)
    elseif cur and line ~= "" then
      table.insert(cur.lines, line)
    end
  end
  local out = {}
  for _, p in ipairs(parts) do
    local f = os.tmpname() .. ".csv"
    local h = assert(io.open(f, "w")); h:write(table.concat(p.lines, "\n"), "\n"); h:close()
    table.insert(out, { name = p.name, path = f })
  end
  return { parts = out }
end }
```

- More than one part → the same table list xlsx and sqlite show (`list_parts`), `<CR>` opens one,
  `file.cvsv:block` opens one directly, sessions reopen by that name. One part → it opens straight
  away, as for a one-sheet xlsx.
- Parts as paths keep everything a file gives (background scans, `:export`, `:tail`); rows tables
  follow S-39 A's rules (in memory; export placeholder).
- The loader runs once per open; the part list is kept with the source. Opening a part after that
  doesn't call `open` again.

### C2. Which loader opens a file

VisiData has the same three ways: `-f`/`--filetype`, the extension, and `guess_*` sniffers that
read the first lines (`visidata/_open.py` `guessFiletype`). vdrs picks the first that matches:

1. **Named:** `vdrs --format cvsv blocks.txt` (stdin: `cat x | vdrs --format cvsv -`),
   `:open blocks.txt cvsv` (a new optional second argument). `name` stops being unused.
   `--format` becomes a string: a built-in kind first, then a loader name, else the error lists
   both; `--help` lists the built-ins and says "or a Lua loader name". `-f` as a short alias,
   as VisiData.
2. **By file name:** `extensions = {...}` as today, or `patterns = { "export_*.txt", "*.pdb.gz" }`
   (globs as in A). Last registered wins (B3), so a project `.vdrs.lua` can claim
   `export_*.txt` in that project only. This is the everyday way: no flag to remember.
3. **By content** (later, K5): a `detect(path, head)` sniffer, as VisiData's `guess_*`, only if
   named + patterns leave a real gap (files with no extension that recur).

From Lua, both are the same API: `patterns` on `vd.register_loader`, and the named form through
`vd.exec(":open blocks.txt cvsv")` in a binding or handler.

- `vd.loaders()` lists `{ name, extensions, patterns }`; the column sidebar
  page names the loader that opened the sheet, so "why did it open like that?" has an answer.

Tests: plugin unit tests (parts list, mixed path/rows parts, empty parts list is an error naming
the loader); scenario: open a `.cvsv` fixture, the table list shows three blocks, `<CR>` opens one,
`F` on a column; CLI test for `--format cvsv`; unit tests for the order (named beats
pattern beats extension's built-in reader, project pattern beats global).

## D. Title bar and status line from Lua (1.5–2 d)

Two shapes were weighed:

- **D1. A format string** (vim's `statusline`, `%f %l/%L`): no Lua per frame, but a mini-language
  to spec, test and document, and no logic.
- **D2. A Lua function returning segments**: plain Lua, any logic, styles from the same table
  renderers use (`fg`, `bg`, `bold`, …). Costs one Lua call per redraw.

**Chosen: D2.** Redraws happen on a key, a finished job or the 2 s tick, not per frame, so one
call is cheap; the result is cached and the function is called again only when its input table
changed.

```lua
vd.statusline(function(ctx)
  return {
    left  = { { text = ctx.mode, bold = true }, " ", ctx.sheet.name },
    right = { ctx.selected > 0 and ("%d sel "):format(ctx.selected) or "",
              ("%d/%d"):format(ctx.row, ctx.rows or 0) },
  }
end)
vd.titlebar(function(ctx) return { left = { ctx.sheet.name }, right = { ctx.column } } end)
```

- `ctx` (read-only): `sheet` (the `vd.sheet()` table), `mode` (`normal`, `input`, `visual`, …),
  `row`, `rows` (nil until counted), `column`, `selected`, `jobs` (running job names), `pane`
  (1 or 2), `message` (the toast, if any), `keys` (the pending chord).
- A segment is a string or `{ text, fg, bg, bold, italic, underline }`.
- The built-in bars stay as `vd.bar.statusline(ctx)` / `vd.bar.titlebar(ctx)`, which return
  segments too, so a config can wrap them instead of rebuilding: `left = { "★ ", table.unpack(
  vd.bar.statusline(ctx).left) }`.
- Toasts and prompts (`:`, `/`) still own the status line while they are up; the function draws
  only the idle line.
- An error, or a call over 5 ms, puts the built-in bar back for the session and toasts once
  (`statusline: …`). `vd.statusline(nil)` restores it on purpose.
- Recolouring the built-in bars without a function: part G.

Tests: unit (segments → spans, cache hit when `ctx` is unchanged, error falls back); scenario with
the `TestBackend` screen: the custom line shows the selection count after `s`, a prompt still
replaces it.

## G. Theme colours from Lua (0.5 d)

`vd.set_theme` also takes a table: `{ base = "dark", statusbar_bg = "#24283b", header_fg = "blue",
… }`. Keys are the `Theme` fields; `base` picks the theme the rest patch. An unknown key or a bad
colour is an error at `vd.set_theme` time, naming the key. Independent of D (K6): most users want
other colours, not other content.

Tests: unit (patch over `dark` and `light`, unknown key refused); scenario: the status line's
background on the `TestBackend` screen.

## E. Sidebar pages from Lua (1 d)

```lua
vd.register_page { name = "atoms", render = function(ctx)
  return { "chain " .. (ctx.cell or ""), { text = "warn", fg = "red" } }   -- lines
end }
```

- `<C-g>` cycles built-in pages, then Lua pages in registration order; `:sidebar atoms` opens one.
- `render(ctx)` gets the same `ctx` as D plus `cell` (the cursor cell as text); it returns lines,
  each a string or a list of segments. Called again when the cursor or sheet changes, cached
  otherwise.
- `SidebarPage` gains `Lua(String)`.

Tests: scenario: register a page, `<C-g>` reaches it, it shows the cursor cell.

## F. Guide recipes (0.5–1 d, with each part)

A "Recipes" section in `docs/index.html`. `plugins.rs` already loads every Lua block in the guide,
so each recipe is checked to parse and register.

Each format recipe ships a small fixture in `docs/fixtures/` (`crambin.pdb` cut to ~50 atoms,
`sample.fasta`, `access.log`; all open formats, no proprietary samples) and a plugin test that runs the guide's loader on it and checks the
columns and row count, so a recipe that stops working fails `just check`.

1. **Multi-block CSV** (`.cvsv`, C1 above).
2. **PDB (Protein Data Bank)**: ATOM/HETATM records are fixed columns (record 1–6, serial 7–11,
   atom name 13–16, residue 18–20, chain 22, residue number 23–26, x/y/z 31–54 in ångströms,
   occupancy 55–60, B-factor 61–66, element 77–78). A loader reads them with `string.sub`,
   returns rows (S-39 A) as one part per record type (`atoms`, `hetatm`), or one part per chain.
   Then:
   - distance to a point: `= sqrt(power(x - 12.1, 2) + power(y - 3.4, 2) + power(z - 7.0, 2))`
     (Polars SQL);
   - atoms within 5 Å of a ligand: that derive, then `z|` `dist < 5`;
   - per-residue counts: `F` on `residue`; per-chain B-factor mean: `W` with `+` on `bfactor`.
   - Pairwise distances (atom × atom) need a cross join; `:join` has no cross type. Out of scope
     here; a loader can emit a contacts part instead.
   - Files are small (≤ ~100k atoms), so Lua rows are fast enough. mmCIF is the same idea on a
     different parser.
3. **FASTA** (sequences, `.fa` `.fasta` `.faa`): a `>` header line, then sequence lines until the
   next `>`. The loader returns rows `id`, `description`, `length`, `gc` (G+C fraction, DNA only),
   `sequence`. Shows a block format → one row per record, and computed columns in the loader.
   Then: `]` on `length`, `z|` `gc > 0.6`, `F` on a description word via `=`. Long sequences:
   the `sequence` column is wide, so the recipe hides it (`-`) and keeps it for `:save`.
4. **Web server access log** (Combined Log Format, the open format Apache and nginx write):
   `host ident user [10/Oct/2026:13:55:36 -0700] "GET /a HTTP/1.1" 200 2326 "ref" "agent"`. The
   loader splits each line with a Lua pattern into `host`, `time`, `method`, `path`, `status`,
   `bytes`, `referer`, `agent`; lines that don't match go to a second part `unparsed`, never
   dropped. Shows `patterns = { "access.log*" }` (no useful extension), `.gz` rotation, and the
   time convention: the offset in the line is kept, times become UTC. Then: `F` on `status`,
   `W` bytes per path, `z|` `status >= 500`.
5. **Log files**: `vd.augroup` + `pattern = "*.log.csv"` → cast and sort on open.
6. **Status line** showing the selection count and running jobs (D).
7. **Project config**: a `.vdrs.lua` with a loader, one `vd.option` and one key (B).

## 9. Decisions to make

- **K1** (decided) Walk up from cwd, stop at `$HOME`.
- **K2** (decided) Any change to the file, yours included, needs `:trust` again.
- **K3** (decided) Replacing the global config: `vd.reset()` only, no new flag; the guide's
  project-config section documents it. A `--clean` (no config at all, like `nvim --clean`) waits
  for the first bug report that needs "is it my config?".
- **K4** (decided) `require` paths in B: `package.path` gets `<global>/lua/?.lua` and the trusted
  `<project>/.vdrs/lua/?.lua`, so `init.lua` can `require("loaders.pdb")`.
- **K5** (decided) Loader choice: named (`--format` / `-f` / `:open … name`, and `vd.exec` from
  Lua), then file name (`extensions`, `patterns`). Content sniffing (`detect`) deferred until a
  recurring extension-less file needs it.
- **K6** (decided) Theme colours are their own part, G.
- **K7** (decided) Events beyond `tick` `enter` `loaded` `reload` (`edit`, `sort`, `filter`,
  `column`) only with a caller.

## 10. Out of scope

- Per-sheet option scopes (VisiData `zO`, S-37 P0c "later").
- Lua access to sheet data beyond the cursor (`vd.frame`, S-16).
- Plugin packages or a plugin manager: a config `require`s files, that's all.
- Running Lua off the UI thread (the VM isn't `Send`-safe to share; S-39 §4).
