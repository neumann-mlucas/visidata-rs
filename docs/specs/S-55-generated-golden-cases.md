# S-55 Generated golden cases

Status: shipped 2026-11-22.
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

- All four properties run in `just check`: §4.4 and §4.2 with 24 cases,
  §4.1 with 12. `PROPTEST_CASES` raises them. Before shipping, §4.4 and
  §4.2 passed 300 cases and §4.1 passed 100.
- §4.1 needs `VDRS_PYTHON` with polars and the DuckDB CLI.
  `VDRS_REQUIRE_TOOLS` makes a missing tool fail the test, as in CI.
- Proptest writes failing seeds to `proptest-regressions/`. Commit them, and
  exclude them from the prek hooks like other test data.

## 6. Layout

- `src/app/generated.rs`: strategies and the four properties.
- `src/app/golden.rs`: the session compare and the export compare move into
  functions that both suites call. No behavior change for the hand-written
  cases.
- New dev-dependency: `proptest`.

## 7. Engine divergences and findings

The generator keeps away from these divergences between Polars and DuckDB:

- Integers stay under 230 in size: DuckDB raises on an i64 overflow, and
  Polars wraps.
- The generator uses no unsigned column. `length()` is cast to `BIGINT`
  (finding 1).
- The compare reads DuckDB's `-0.0` as `0`, and sets floats under 1e-12 to 0.
- DuckDB reads `1.5` as a DECIMAL, which can overflow where Polars uses a
  float. The generator writes `CAST(1.5 AS DOUBLE)`.
- Floats stay far from 1e10: a stdev of values near 1e10 differs between
  the engines in the 7th digit.

Open findings:

1. Fixed in part: freq, pivot and describe counts were unsigned, so
   `count - 10` wrapped to a big number where VisiData gives a negative
   one. They are `Int64` now. Open: `length()` is still `u32`, so
   `length('NULL') - 12` gives 4294967288, where VisiData gives -8.
2. Fixed: vdrs cast a number to a date as days since 1970. VisiData reads
   it as seconds (`datetime.fromtimestamp`), and vdrs does too now, in UTC.
3. Open: a `:derive` over a column with every cell empty, after a
   `:type int` on it, exports SQL that DuckDB refuses
   (`-(BOOLEAN, BIGINT)`). Found at 200 cases; not triaged.

The relations of §4.3 run through the `Harness` (S-56): the real key path is
worth the time.

## 8. Bugs found

Each has a test or a golden case:

- An ill-typed `:filter-expr` was accepted. A derive or a cast could break a
  filter-expr that was there.
- A `:derive` that fails only on evaluation (`upper` of a Boolean) was
  accepted, and the sheet stopped loading.
- A session replay toggled off the keys of a derived sheet, and hid the
  describe columns that `unhide` had shown.
- The SQL date cast from a number or a Boolean gave null.
- Describe of no rows: the SQL sum was null and the Python script failed.
- The SQL export accepted columns differing only by case (from a derive),
  and a sheet with every column hidden.
- The freq dive on `0` missed `-0.0`.
- A quoted CSV header kept its `""`.

## 9. Done when

- The suite runs in `just check` and in CI.
- A deliberately injected bug in each of `export/python.rs`, `export/sql.rs`
  and `session.rs` fails the suite, and the shrunk case reads as a golden case.
- Each bug the suite finds becomes a fixed case under `tests/golden/`.
