# S-22 — Nested data: expand structs, unfurl lists

Status: **drafted 2026-11-08**, not started.
Depends on: S-03 (readers), S-15. Corrected S-15 §10 (it said "JSON records load with nested fields as JSON
strings"; fixed 2026-11-08): verified 2026-11-08 that Polars' JSON/JSONL readers give native `struct[...]` and
`list[...]` columns (`vdrs --headless` on nested JSONL), and Parquet/Arrow carry them as-is. So the
data is already typed; only the operations are missing.

## 1. VisiData surface (reference)
- `(` `expand-col`: struct/dict → one column per field (`user.name`, `user.geo`), one level.
- `g(` `expand-cols` (selected/all), `z(` `expand-col-depth N`, `gz(` `expand-cols-depth`.
- `)` `contract-col`: undo the expansion (hide children, show parent).
- `zM` `unfurl-col`: list → one row per element (dicts → one row per key/value), pushed as a **new
  sheet** `<name>_unfurled`; no revert command, `q` it (`visidata/features/unfurl.py`).
- Corpus: `unfurl-col` 4 files, `expand-col-depth` 1, `expand-cols-depth` 1 (UNTESTABLE today, most
  chain into Python exprs — measure after phase A).

## 2. Design
New plan steps, applied in `PlanStep::apply`, so undo / `:export` / `:tail` / plan panel get them:

| Step | Key | Polars | DuckDB export |
|---|---|---|---|
| `Expand { column, depth }` | `(` (depth 1), `z(` N, `g(` all struct cols | `struct.field(f).alias("col.f")` per field, recursive to `depth`; parent column hidden (kept in plan, `View.hidden`) | `col.f` via `struct_extract` / `col.f` dot syntax |
| contract | `)` | no step: un-hide the parent, hide its `col.*` children (view only) | — |

Unfurl is a **derived sheet**, as in VisiData: `zM` pushes `Origin::Unfurl { parent, column }` built
from the parent's `view_plan()`: `with_row_index("column_index")` per list (`list.eval` index) then
`explode(column)`; structs unfurl to `column_key`, `column_value` rows. Export: a `Derivation::Unfurl`
in the lineage (`explode` / DuckDB `UNNEST` + `generate_subscripts`). Being derived, it is read-only
under S-18 D3, so the row-count change never meets row ids or edits.

Rules:
- Naming `parent.field` (VisiData). Name clash with an existing column → suffix `_2`.
- Empty list → one row with null element (VisiData keeps the row); null list → same.
- Map-like structs (JSON objects with varying keys) arrive as a struct with the union of keys and nulls,
  Polars' default — fine; no special case.
- Cells: struct shows `{name: a, geo: {…}}` (keys, depth 1), list shows `[x, y] (2)`; via the existing
  cell-text path (`any_text`), truncated by width. Type label already `list` (`src/widget.rs`), add `struct`.
- JSON strings (a CSV column holding JSON): `:type json` → `str.json_decode` into a struct/list, then
  `(`/`zM` work. Optional phase C.

## 3. Phases

| Phase | Ships | Effort |
|---|---|---|
| A | `Expand` (`(`, `z(`, `g(`), `)` contract, struct/list cell text, export | 1–1.5 d |
| B | unfurl sheet (`zM`, `Origin::Unfurl`) + `column_index`, export | 1 d |
| C | `:type json` (decode JSON text) | 0.5 d |

## 4. Test plan
- Unit: field naming + clash suffix; depth 2 expansion order matches VisiData (parent position, fields
  in schema order).
- Unit: unfurl of `[]`, null, `[x]`, nested list → expected rows.
- Scenario: open nested JSONL, `(` on `user` → `user.name`, `user.geo`; `)` restores; `U` undoes.
- Scenario: `zM` on `tags` pushes `n_unfurled`; `F` on `tags` there → counts per tag; `q` back to the
  parent, unchanged; edit keys on the unfurled sheet → read-only message (S-18 D3).
- Golden + export round trip: `expand-struct`, `unfurl-list` (DuckDB + Polars rebuild the same TSV).
- Corpus: translate `expand-col*`, `unfurl-col`; record which files pass.
- S-15: move nested data from §10.1 (planned) to §3 when phase A ships.
