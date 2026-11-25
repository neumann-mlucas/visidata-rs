# S-60 — Out-of-core pivot on the Polars streaming engine

Status: **blocked** on a Rust `polars` release with the 2.1/2.2 streaming engine. Drafted 2026-11-25.

## 1. Why

`direction.md` §8 names the biggest engine risk: a pivot is a group-by, and Polars could not spill a
group-by to disk. The fallback was a DuckDB backend, dropped in §9.3 until a user hits the wall.

Python Polars has since closed the gap:

- 2.0 (2026-10-06) makes the streaming engine the default and spills sort and window functions.
- 2.1 (2026-10-25) spills the streaming group-by (pola-rs/polars#29911).
- 2.2 (2026-11-24) spills joins (pola-rs/polars#30679, #30558).

So the fix is the Polars streaming engine, not DuckDB.

## 2. Blocker

vdrs uses the crates.io `polars` 0.55.2 (rs-0.55.2, 2026-08-06). Python Polars builds the workspace
crates from source at its tag, so a Python release does not publish a Rust crate. The `py-2.2.0` tag
is 1408 commits ahead of `rs-0.55.2`, and its crates still say version 0.55.1.

- Start this spec when crates.io has a `polars` release after `py-2.1.0` that contains #29911.
- A git dependency on a `py-*` tag is the fallback. Use it only if a user hits the wall first.
  It pins a tag, but it builds slower and blocks a crates.io release of vdrs.

## 3. Where vdrs pivots in memory today

- `Sheet::pivot_from` (`src/sheet/derive.rs`) collects the parent's three needed columns into a
  `DataFrame` before it pivots.
- `pivot_df` (`src/sheet/mod.rs`) finds the `on` values with an eager `unique_stable`, then runs
  `lazy().pivot(…).collect()` on the in-memory engine.
- The Lua aggregator path (`lua_pivot`, `src/app.rs`) and the `:pivot` live preview stay eager.
  The preview reads the first 10k rows only.

## 4. Plan

1. Bump `polars` in `Cargo.toml` and fix the API breaks. — verify: `just check` exits 0.
2. Find the `on` values on the lazy plan: `select(on).unique_stable().collect()` with the
   streaming engine. — verify: column order of existing pivot goldens does not change.
3. Run the pivot on the parent's lazy plan, not on a collected frame, and collect it with the
   streaming engine. Keep `when(len() > 0)` so a missing index × on pair stays null.
   — verify: pivot unit tests, `tests/golden/` pivot cases and `vd_replay` pivot files pass.
4. Test with a parquet file larger than the memory limit of the test process.
   — verify: the pivot finishes and spill files appear under the Polars spill directory.
5. Update `direction.md` §8 and §9.3, S-15 and `tasks.md`.

## 5. Out of scope

- Other derived sheets (`F` freq, `:join`) gain from the same engine. Give each its own item when
  a user hits that wall.
- The result sheet keeps its window in memory (`with_window(out)`). A pivot result is small next
  to its input.

## 6. Open questions

- Does the Rust API expose the out-of-core memory budget? The sources disagree on the variable
  name (`POLARS_OOC_MEMORY_BUDGET_FRACTION` or `…_MB`). Read the release's docs before step 4.
- Does the streaming engine keep first-seen row order for the pivot index? If not, set
  `maintain_order`, as VisiData order is a replay requirement.
