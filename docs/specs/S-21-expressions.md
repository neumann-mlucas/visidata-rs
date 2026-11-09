# S-21 — Expression columns and expression selection (`=`, `:select`)

Status: **drafted 2026-11-08**; phase A shipped 2026-11-08 with S-26 phase 0 (`=`, `z/` `z?`, `z|` `z\`, `:select` / `:unselect`, filter → `gz|`); phase B shipped 2026-11-08 (`g=` / `:setexpr` = `PlanStep::SetExpr`, `:filter-expr` = `View.filter_expr`; `:setexpr` reads base columns like `:derive`, `:filter-expr` the typed view like `:select`). The `=`/`z/`/`z|` prompts don't share the ex history yet. `=` with `name = expr` names the column only when `name` doesn't exist (else `qty = 5` would replace `qty`); otherwise the whole text is the expression.
Depends on: S-15 (`:derive`, `:sql`), S-04 (keymap); phase B on S-18 C (`Arc` rid set, D3 read-only derived sheets). Touches S-14 (UNTESTABLE list).
Direction: §9.5 stays — no Python runtime. This spec gives VisiData's *keys* and *workflow* an
expression language vdrs already has (Polars SQL), and leaves a Python-flavoured subset as an
optional phase gated on evidence.

## 1. What exists (verified)
- `:derive name = <SQL expr>` → `Sheet::derive` (`src/sheet.rs`): `PlanStep::Derive`, schema-checked
  before it lands, undoable, exported to `.py`/`.sql`.
- `:sql <query>` over the view and other sheets.
- `|` / `,` select by regex / equal value → `rows_matching` / `rids_with_values` → row ids, background job.
- Free keys: `=` (VisiData `addcol-expr`), `g=` (`setcol-expr`), `z/` (`search-expr`), `z\`
  (`unselect-expr`). Taken: `z|` is VisiData's `select-expr` but vdrs binds it to the regex column
  filter (S-15 §3.3) — see E3.

## 2. Design

| Key / command | VisiData | vdrs | Implementation |
|---|---|---|---|
| `=` | `addcol-expr` | prompt `= ` → new column, name = the expression text (as VisiData), `name = expr` form names it | `Sheet::derive`; prompt is the ex prompt prefilled `:derive ` |
| `g=` | `setcol-expr` (selected rows) | `:setexpr <expr>` on the current column, selected rows | new `PlanStep::SetExpr { column, expr, rids }` = `when(rid ∈ rids).then(expr).otherwise(col)`; shares the `Arc` rid set S-18 introduces for `SetCells` |
| `z/` | `search-expr` | next row where `<pred>` is true | `view_plan().filter(sql_expr(pred))` first rid after the cursor |
| `z\` (`z\|` see E3) | `select-expr` / `unselect-expr` | `:select <pred>` / `:unselect <pred>` | rids of `filter(sql_expr(pred))`, background job like `\|` |
| — | — | `:filter-expr <pred>` | optional: a predicate filter alongside the regex filter (`FilterState` gains `Expr(String)`) |

Rules:
- Expressions are Polars SQL (`sql_expr`): `qty * price`, `upper(name)`, `amount > 100 AND region = 'east'`.
  Window functions work too (verified 2026-11-08 on polars-sql 0.55): `x / sum(x) OVER ()` (share of
  total), `sum(x) OVER (PARTITION BY g)`, `sum(x) OVER (ORDER BY d)` (running total),
  `x - lag(x) OVER (ORDER BY d)`. They run on the base plan, so `OVER ()` spans all source rows, not
  the filtered view — document it; a view-scoped variant would need the derive applied after the filter.
  Column names with spaces/odd chars: `"col name"`. One error text for every entry point: the Polars
  message, prefixed `expr:`.
- Validation before anything changes: `collect_schema()` on the new plan (as `derive` does); predicates
  must be Boolean (`a + 1` in `:select` → `expr: not a boolean: a + 1`).
- Exports: `SetExpr` → `pl.when(...)` / `CASE WHEN rid IN (...)`. `:select` changes only the view,
  nothing to export.
- History: `=`/`z/`/`:select` prompts share the ex history file (they are ex commands).

### Phase C (optional, evidence-gated): Python-flavoured subset
A translator `pyexpr → polars Expr` for the shapes in the VisiData corpus and in day-one muscle memory:
arithmetic and comparisons on column names, `and/or/not`, string methods (`.strip() .lower() .upper()
.replace(a,b) .startswith(x) len()`), `int() float() str()`, `curcol`. Anything else: refused with
`expr: python subset doesn't cover <node>; use SQL`. Parser: hand-written Pratt (≈300 lines, no new
dependency). Start only if (a) the corpus scan below shows ≥ 10 newly passing goldens, or (b) a user
asks twice. The corpus count today: `addcol-expr` 9 files, `setcol-expr` 3, `select-expr` 2, but most
chain into other UNTESTABLE longnames, so the real unlock is lower — measure first (task C0).

## 3. Phases

| Phase | Ships | Effort |
|---|---|---|
| A | `=` prompt → `:derive`; `z/` search-expr; `:select` / `:unselect` | 1 d |
| B | `g=` / `:setexpr` (`PlanStep::SetExpr`) + export; `:filter-expr` | 1–1.5 d |
| C0 | corpus scan: which UNTESTABLE files only need the subset (harness flag, no product code) | 0.5 d |
| C | Python subset translator — only if C0 or users justify it | 3–4 d |

## 4. Test plan
- Unit: `:select` predicate type check (bool ok, number refused, unknown column refused).
- Unit: `SetExpr` on selected rids only; null handling (`otherwise(col)` keeps nulls on unselected).
- Scenario: `=` `qty*price` Enter → new column named `qty*price`; `=` `total = qty*price` → `total`;
  bad expr → toast, no new step, `U` stack unchanged.
- Scenario: `:select amount > 100` after `[` sort → same rows as typing `,` on matching cells; survives
  a later filter (rid-keyed).
- Scenario: `z/ region = 'east'` wraps like `n`.
- Golden + export round trip: `expr-derive`, `expr-setexpr` (DuckDB and Polars rebuild the same TSV).
- Docs: the guide's "derive" card mentions `=`; doc-sync test covers the new key.
- Corpus (C only): translated files leave the UNTESTABLE list; `MIN_PASS` raised by what passes.

## 5. Decisions (confirmed 2026-11-08)
Keymap rule: S-26 §0 (default = VisiData; changed meanings only in the vim profile).
- **E1** `=` names the column with the expression text (VisiData); `name = expr` names it `name`.
- **E2** `g=` = setcol-expr on selected rows (VisiData).
- **E3** `z|` = select-expr (VisiData). The regex filter moves to `gz|` (free in both; VisiData `zF` is
  freq-summary) and `:filter`, in phase A's first commit, with the doc-sync test and the `docs-filter`
  recording updated.
