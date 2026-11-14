# S-39 — Sources that aren't files: Lua tables, then `trait Source`

Status: **drafted 2026-11-14**. A is small; B waits for a plugin that needs it.
Depends on: S-10 (Lua loaders), S-18 B (`vd.push_sheet`'s `sheet_from_lua`), S-29 (sessions),
Phase 4 (`:tail`).

## 1. Today

Every sheet starts as a `LoadedSource { kind: SourceKind, path, lazy: LazyFrame }`. `SourceKind`
(`src/reader.rs`) is a closed enum: CSV, TSV, Parquet, Arrow, JSON, JSONL, xlsx, sqlite, duckdb,
stdin. A Lua loader (`vd.register_loader { extensions, open }`) can only return a path to a file in
one of those formats: `plugins::open` calls it, then `reader::open` scans what it wrote.

A file underneath is what these rely on:

| Feature | How it uses the file |
|---|---|
| Background jobs (sort, freq, materialize, S-33 edits) | re-run the `LazyFrame` scan on other threads |
| `:export` | the Python/SQL script reads the same path |
| Sessions | reopen by path (`:open`, `-S`) |
| `:tail`, `^R` | re-read the file, or only its new bytes |

`register_loader_fn`'s comment says a loader returning rows is "intentionally not supported"
because of Lua → Polars marshalling. Since then `vd.push_sheet` added `sheet_from_lua`
(`{ columns, rows }` → `DataFrame`, typed per column), so that reason no longer holds.

## 2. Callers that want something else

- A plugin that fetches rows from an API, a command (`kubectl get pods -o json`), or a database
  driver Polars doesn't have. Today it writes a temp CSV: types get lost (everything re-inferred
  from text), and the plugin owns cleaning up the temp file.
- A live source that keeps producing rows (a socket, a message queue, `journalctl -f`). `:tail`
  covers files only.

## 3. Phase A: a loader may return rows

`open(path)` returns either a path (as today) or a table, the same shape `vd.push_sheet` takes:

```lua
vd.register_loader { name = "pods", extensions = {"pods"}, open = function(path)
  return { columns = {"name", "ready"}, rows = { {"api", true}, {"db", false} } }
end }
```

- `plugins::open` turns a table into a `DataFrame` with `sheet_from_lua` and returns
  `LoadedSource::from_df(kind, display_name, path, df)` (already used for `:exec-sql`). The rows
  are in memory, so background jobs scan the in-memory frame; no Lua runs off the Lua thread.
- `:export`: the frame has no file to read. The script starts with a comment and a placeholder
  `pl.DataFrame` / `VALUES` of the rows when there are under 10 000 of them, else a
  `# not exported: rows from Lua loader <name>` line. (To decide: or always refuse.)
- Sessions: reopen by path, as today. The loader runs again, so a session shows the rows the
  source has now, not the saved ones. That's what reopening a file does too.
- `:tail`, `^R`: `^R` runs the loader again; `:tail` refuses, as it does for loaders now.
- Size: about 0.5 d (the `open` result match, the export placeholder, tests, guide, Lua API
  version bump).

## 4. Phase B: `trait Source` (when a live source needs it)

Two shapes:

- **B1. Trait objects.**

  ```rust
  trait Source: Send + Sync {
      fn name(&self) -> String;
      fn scan(&self) -> PolarsResult<LazyFrame>;        // the rows so far
      fn poll(&self) -> PolarsResult<Option<DataFrame>> { Ok(None) }  // new rows (tail)
      fn reopen(&self) -> Option<String>;                // how a session reopens it
      fn export(&self, lang: Lang) -> Option<String>;    // how a script reads it
  }
  ```

  `SourceKind` stays the parser choice for files; `FileSource { kind, path }` implements the
  trait, and so does `LuaSource`. `:tail` becomes "call `poll` on the 2 s tick" for every source,
  instead of file-only code.
- **B2. Keep the enum, add `SourceKind::Lua { loader }`.** Each `match` on `SourceKind` gets an
  arm. Less indirection, but every new kind touches export, sessions and tail again.

**Chosen when B starts: B1.** The four behaviours in §1 are what differs between sources, and a
trait puts each one next to its source instead of in four `match`es.

The hard parts, whatever the shape:

- The Lua VM isn't `Send`. `poll` for a Lua source runs on the UI thread on the tick, turns the
  new rows into a `DataFrame`, and appends them to an in-memory buffer that jobs scan.
- Lua → Arrow goes cell by cell through mlua: fine for thousands of rows per tick, slow for
  millions. Big sources belong in Rust (`vdrs-ext-*`, S-09).
- Memory: a live source grows without bound. Keep a cap (`vd.option("source_max_rows", …)`)
  and drop the oldest rows, saying so in the status line.
- Export and sessions can't rebuild a live source's past rows. Sessions reopen it fresh;
  exports refuse with a reason (or snapshot, as in A).

Size: about 2–3 d. Start only when a plugin wants rows that keep coming.

## 5. Test plan

- A: a plugin test where `open` returns a table: types kept (bool, int, null), the sheet sorts and
  freqs in a job, `^R` re-runs the loader, a session restores it by path. Harness test: open the
  file through the key path (`o`), `F` on a column.
- B: a fake `Source` whose `poll` returns a batch per tick: rows appear without blocking the UI,
  the cap drops old rows, `<C-c>` stops nothing (polling isn't a job).

## 6. Out of scope

- Writing back to a source (`:save` goes to a file, as today).
- Lua sources in other threads (would need a Lua VM per thread).
