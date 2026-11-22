# S-55 Generated golden cases

Status: drafted 2026-11-22.
Depends on: S-08 (`:export`), S-29 C (`:mksession`), `src/app/golden.rs`.

## 1. Problem

The golden harness has three strong oracles: the exported Polars script, the
exported DuckDB script and the session restore. Each must rebuild the view.
These oracles only see 119 hand-written cases. A hand-written case tests what
its author thought of. Generated inputs test what nobody thought of: odd column
names, nulls, ties, type inference edges and unusual command orders.

The oracles are independent of the code under test. The view comes from the
in-process plan. The scripts come from `export/python.rs` and `export/sql.rs`.
The session comes from `session.rs` and the ex parser. If two of them disagree,
one of them has a bug. The test needs no expected file.

## 2. Goal

A proptest suite generates a small table and a short ex script. It runs the
script through the real `Harness`. Then it checks the properties in §4. When a
property fails, proptest shrinks the case to a minimal table and script. The
failure message prints them as a golden case directory, so a developer can
commit it under `tests/golden/` as a fixed regression.

## 3. Generators

### 3.1 Table

- Rows: 0 to 30. Columns: 1 to 6, plus one unique `id` column (§4.1).
- Column kinds and value pools:
  - integer: 0, -1, `i64::MIN`, `i64::MAX`, small values, null.
  - float: 0.0, -0.0, 1e-300, 1e300, values with many digits, null.
  - text: empty string, spaces at the ends, quotes, commas, tabs,
    wide Unicode, combining marks, strings that look like numbers or dates.
  - boolean: `true`, `false`, `True`, null.
  - date: ISO dates, `7/3/2018`, null.
- Null rate per column: 0, 10 % or 50 %.
- Column names come from a pool: plain names, names with spaces, quotes,
  Unicode, SQL keywords (`select`, `order`), names that start with a digit,
  and near-duplicates (`a`, `A`, `a `).
- The generator writes the CSV with its own quoting. It does not use
  vdrs or Polars to write it.

Type inference is part of the test. vdrs, `pl.scan_csv` and DuckDB
`read_csv_auto` must infer the same types (`py_csv_args`, `sql_csv_args`).

### 3.2 Script

One to six steps, in the golden `script` format: ex commands, plus keys
where a feature has no ex command. Each step takes its arguments from the current
table schema, so most commands are valid.

- `filter <col> <regex>`, `filter-expr <pred>`, `unfilter`
- `sort <cols>` with `-` for descending, `unsort`
- `derive <name> = <expr>`, `setcol <value>`, `rename <old> <new>`
- `type <kind>`, `hide <col>`, `unhide`
- `pivot …`, `describe`, `dupsel`
- the key `F` (freq) on the current column; freq has no ex command
- `undo`, `redo`

`<expr>` and `<pred>` come from a typed grammar over Polars SQL (S-21):
arithmetic on numeric columns, comparison, `AND`/`OR`/`NOT`, `IS NULL`,
`upper`/`lower`/`length` on text, and integer and float literals. Each node
has a type, so the generator builds mostly well-typed expressions. One case
in ten allows an ill-typed node to test the error path.

The exact command list follows what `exline::parse` accepts. Before a
command enters the generator, check its `:export` support in `export.rs`.

## 4. Properties

Each property names its oracle. No property compares output with a stored
snapshot of earlier output.

### 4.1 Export equals view (differential)

The rows of the final view equal the rows of the Polars script and the
DuckDB script.

- The test compares rows as a multiset.
- If the script sorts, the test also compares the order of the sort-key
  tuples. Rows with equal keys may come in any order, because DuckDB does
  not sort stably.
- The `id` column makes each row unique, so a multiset compare also finds
  duplicated or lost rows.
- If export refuses the sheet (`export: can't rebuild …`), the case passes
  only when the refusal is one of the documented limits in `export.rs`.

### 4.2 Session restore equals view (round trip)

`session::save` then `App::source` on a fresh `Harness` gives the same view,
the same sheet count and the same current column. This check is the one in
`golden_cases_round_trip_through_sessions`, moved into a shared function.

### 4.3 Metamorphic relations (in process)

These relations need no external tool, so they run in the default suite:

1. A filter and its negation partition the rows: `filter p` rows plus
   `filter NOT p` rows plus rows where `p` is null equal all rows.
2. A sort is a permutation: the multiset of rows does not change.
3. Freq counts sum to the row count of the view below.
4. `hide` and column order change no row values.
5. `undo` after any command gives the view from before that command.

### 4.4 The app stays healthy

After each command, the sheet loads, the cursor stays in bounds and the app
does not panic. A rejected command shows a toast and leaves the view as it was.

## 5. Running

- Default suite (`just check`): §4.2, §4.3 and §4.4, with a low case count
  (`PROPTEST_CASES`, about 32). Target: under 20 s.
- Ignored test, run in CI and on request: §4.1 with about 256 cases. It
  needs `VDRS_PYTHON` with polars and the DuckDB CLI. `VDRS_REQUIRE_ROUNDTRIP`
  makes a missing tool fail the test (local/CI parity, see §7).
- Proptest writes failing seeds to `proptest-regressions/`. Commit them, and
  exclude them from the prek hooks like other test data.

## 6. Layout

- `src/app/generated.rs`: strategies and the four properties.
- `src/app/golden.rs`: the session compare and the export compare move into
  functions that both suites call. No behavior change for the hand-written
  cases.
- New dev-dependency: `proptest`.

## 7. Open questions

1. How should the suite treat a known divergence between Polars SQL and
   DuckDB SQL, such as integer division? Options: keep the generator away
   from it, or list it in a quarantine file with a reason, as
   `tests/vd_replay/quarantine.txt` does.
2. Should the metamorphic relations in §4.3 run on `Sheet` directly instead of
   through the `Harness`? `Sheet` is faster, but the `Harness` uses the real
   key path.
3. How many cases fit in the CI time budget? Today CI takes 2 to 3 minutes.

## 8. Done when

- The suite runs in `just check` and in CI.
- A deliberately injected bug in each of `export/python.rs`, `export/sql.rs`
  and `session.rs` fails the suite, and the shrunk case reads as a golden case.
- Each bug the suite finds becomes a fixed case under `tests/golden/`.
