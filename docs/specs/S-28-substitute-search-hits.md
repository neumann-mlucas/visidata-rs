# S-28 — `:s` substitute on a column, search-hit sheet

Status: **drafted 2026-11-08**, not started.
Depends on: S-18 C (edits as steps; `SetCells`/`SetCol` patterns), S-26 B (ranges), S-15 §3.3 (search).

## 1. VisiData reference
- `*` `addcol-regex-subst`: new column `<col>_re` = regex replace of the current column.
- `g*` `setcol-regex-subst`: replace in place on selected rows. Corpus: `setcol-regex-subst-all` sits in
  UNTESTABLE today (S-14), reachable after this spec.
- No hit list: `/` `n` `N` only (`features/hlsearch.py` highlights matches).

## 2. `:s` — substitute
Grammar (vim): `:[range]s/<regex>/<replacement>/[g][i]`, any delimiter after `s`.
- Scope: current column. Rows: the range (S-26 B), else the selection if any, else every row of the
  view (vim's `%` default would surprise on a 10M-row file — decision S1).
- `g` = all matches in a cell (else first), `i` = case-insensitive. Replacement uses `$1`/`${name}`
  (Rust `regex` syntax); `\1` accepted and rewritten to `$1` for vim/VisiData muscle memory.
- New step `PlanStep::Replace { column, pattern, replacement, all, rids: Option<Arc<[u64]>> }` →
  Polars `str.replace`/`str.replace_all` under `when(rid ∈ rids)`; non-string columns: cast to text
  first only if the user confirms (`amount is f64; replace as text? y/N`).
- `*` (VisiData) = `:s` into a **new** column `<col>_re` (`PlanStep::Derive` with the replace expr);
  `g*` = `:s` on selected rows in place. Keys kept as VisiData; S-26 D's vim `*` lives only in the vim
  profile.
- Undo, `:save`, `:export` (Polars `str.replace_all`, DuckDB `regexp_replace(…, 'g')`) via the step.
- Status: `replaced 1,204 cells in name`.
- Read-only sheets (S-18 D3): derived sheets take no steps, so `:s`, `*` and `g*` are all refused there
  with the S-18 read-only message.

## 3. Search-hit sheet (quickfix)
- `g/` (VisiData `search-cols`; vdrs `/` already searches every column) is free in vdrs → `:hits
  <text>` / `g/` pushes a sheet: one row per hit `row, column, value` (+ key column values), from
  `Sheet::search_hits` (already returns `(rid, col)` pairs; background job).
- `Enter` on a hit: pop back to the parent with the cursor on that cell; the hit sheet stays on the
  stack, `S` / `<C-^>` returns to it (vim `:copen` + `:cc`).
- No `]q`/`[q`: `n`/`N` on the parent already walk the same hits. Being a sheet, the hit list gets `F`
  on `column` (where the matches sit), sort and filter for free.
- Cap: 100k hits, status says `100,000+ hits (capped)`; `ponytail:` cap, raise when asked.
- Derived sheet `Origin::Hits { parent, needle }`, read-only, not exported (S2).

## 4. Phases
| Phase | Ships | Effort |
|---|---|---|
| A | `:s` in place + `*` / `g*`, `PlanStep::Replace`, export | 1.5–2 d |
| B | hit sheet `g/` / `:hits`, `Enter` jumps back | 1 d |

## 5. Test plan
- Unit: `:s` parser — delimiters, flags, escapes, `\1` → `$1`, empty replacement, bad regex message.
- Unit: `Replace` on selected rids only; nulls stay null; `g` vs first-only.
- Scenario: `:s/ +$//` on selected rows; `U`; `*` adds `name_re`; read-only refusal on a freq sheet.
- Golden + export round trip (DuckDB + Polars): `subst-col`, `subst-new-col`.
- Scenario: `g/foo` → hit sheet with the expected (row, column) list; `Enter` lands on the cell; cap
  message on a generated 200k-hit frame.
- Corpus: translate `addcol-regex-subst`, `setcol-regex-subst(-all)`; move them out of UNTESTABLE.

## 6. Decisions (open)
- **S1** `:s` default rows without range/selection: whole view (recommend, with the count in the
  status line) vs. current row (vim).
- **S2** export the hit sheet: no (recommend).
