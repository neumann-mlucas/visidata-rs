# S-24 — Hardening: module layout, sheet invariants, typed readers, release binaries

Status: **drafted 2026-11-08**; R1 and R2 shipped. Independent items; each is its own commit with no
user-visible change except R3–R5. Order: R1 before S-18 C (editing adds code to the same files),
R2 with S-18 E (both touch row bookkeeping), R3–R4 any time, R5 when the alpha is finished (direction §9.12: every planned feature in `docs/` implemented, tests robust).

## 0. Review findings, re-checked
| Finding (review 2026-11-08) | Re-check | Action |
|---|---|---|
| `app.rs` 2802 / `sheet.rs` 2450 lines | `app.rs`: ~1360 lines of code, the rest tests (`mod tests` at 1362, `scenario` at 1561). `sheet.rs`: ~1985 code, ~20 `*_from` constructors | R1 |
| All-public `Sheet` / `View` / `SheetState` | true; ~160 `.view.` and ~55 `.state.` uses outside `sheet.rs` | R2 (partial) |
| `PlanStep::SetCol { rids: Vec<u64> }` grows with rows | true; S-18 §2.1 already moves edit payloads behind `Arc` (`SetCells`) | fold into S-18 C: `SetCol.rids` → `Arc<[u64]>`, nothing here |
| `Lineage` clones the whole parent | **wrong**: `Sheet::derived` drops `window`, `undo`, `redo`; `LazyFrame` clones share the plan | none |
| xlsx / sqlite load every column as String | true (`src/reader.rs` cell → `String`) | R3 |
| CSV dates ISO-only; zero-padded codes lose zeros | true; `issue964-inner-join` golden fails on `082053` → `82053` | R4 |
| ~45 min source install | true (README) | R5 |

## 1. R1 — Module layout (0.5–1 d, mechanical)
- `src/app.rs` tests → `src/app/tests.rs` + `src/app/scenario.rs` (bodies unchanged).
- `src/app.rs` render code (`render*`, `render_titlebar/statusbar/which_key/plan_panel/pivot_preview`)
  → `src/app/render.rs`.
- `src/sheet.rs` → `src/sheet/` : `mod.rs` (Sheet, Sheets, View, plans), `derive.rs` (freq, pivot,
  describe, join, sql, dedupe, unpivot, rows_with_values, copy), `diff.rs`, `tail.rs`, `step.rs`
  (`PlanStep`, `Snapshot`, undo).
- Rule: moves only, no renames, `pub(crate)` where a move needs it. Proof: the full suite passes with
  no test edits; `git diff --stat` shows moves (`-M`).

## 2. R2 — Window invariants behind methods (1–2 d)
Not a general encapsulation pass (cost: every caller, benefit: little). Only the fields whose
consistency is a correctness rule:
- `window`, `revision`, `state.loaded_rev`, `state.window_rids`, `view.window_offset`,
  `view.total_rows`/`view.count_key`.
- Methods: `Sheet::invalidate(Reason)` (bump revision, reset count key when the row set changes),
  `Sheet::land_window(rev, df) -> bool` (applies only if `rev` is current; splits rids),
  `Sheet::window() -> Option<&DataFrame>`, `Sheet::row_count() -> Option<u64>`.
- Fields become private to `sheet`; callers that set them directly today (grep `revision +=`,
  `count_key = None`, `window =`) go through the methods.
- Proof: suite passes; new unit tests — a stale `land_window` is ignored; every `PlanStep` apply calls
  `invalidate` (one test over all step kinds).

As shipped (2026-11-08): `window`, `revision`, `state.loaded_rev`/`counting`/`window_rids`,
`view.total_rows`/`count_key` are private to `sheet`. Readers: `window()`, `window_rids()`,
`revision()`, `loaded_revision()`, `is_loaded()`, `is_counting()`, `row_count()`. Writers:
`begin_window()` (a materialize starts), `land_window`/`land_error` (dropped unless current),
`start_count`/`land_count` (dropped unless the key is current), `set_window` (pre-built rows),
`invalidate(Reason::Rows | Reason::Plan)`. `view.window_offset` stays public: only cursor
navigation writes it, and every write is followed by a materialize; a setter would enforce nothing.

## 3. R3 — Typed xlsx and sqlite columns (1 d)
- xlsx (calamine `Data`): per column, if every non-empty cell is `Int` → i64, `Float`/`Int` → f64,
  `Bool` → bool, `DateTime`/`DateTimeIso` → datetime; mixed → String (today's behavior). Header row
  excluded.
- sqlite: use the declared type affinity (`INTEGER`, `REAL`, `TEXT`, `BLOB`, `NUMERIC`) when every
  value's storage class agrees; otherwise String (SQLite is dynamically typed — the agree-check keeps
  today's lossless behavior for messy tables).
- Corpus check: the xlsx and sqlite goldens that pass today must still pass (VisiData shows typed
  values for xlsx; verify each before switching — if a golden depends on String rendering, keep String
  for that case and record why).
- Tests: unit per type rule incl. mixed columns; golden `xlsx-typed`, `sqlite-typed` with export.

## 4. R4 — CSV inference: no silent data change (1 d; ships as part of S-25 P2)
- Zero-padded integers: a column inferred as int where any sampled value matches `^[+-]?0\d` stays
  String (zip codes, IDs). Implementation: after schema inference, re-check int columns on the same
  sample rows Polars inferred from (`CSV_INFER_ROWS`) read as text; override via `with_schema_overwrite`.
- Dates: keep ISO auto-inference; other layouts stay text (inference guessing `01/02` wrong is worse
  than text). The lenient `@` already covers the rest by hand. No change.
- Tests: unit — `082053`, `-01`, `0`, `0.5`, `10` → expected dtypes; golden `zero-padded-ids`;
  `issue964-inner-join` leaves the quarantine if it then passes.

## 5. R5 — Release binaries (0.5–1 d; not before the alpha is finished — decision 2026-11-08)
- GitHub Actions release workflow on tag `v*`: build `vdrs` for x86_64/aarch64 Linux (musl), macOS
  (x86_64 + arm64), Windows; attach archives + SHA256 to the release. Prefer `cargo-dist` (generates the
  workflow and a shell installer) over a hand-written matrix; pin its version.
- README: binary install first, `cargo install` second (keep the 45 min warning there).
- Tests: the workflow runs `vdrs --headless` on a fixture per target before upload.
- Not here: distro packages (direction §2: different politics).

## 6. Test plan summary
Each item ends with the full suite green (`cargo test`, `cargo clippy --all-targets -D warnings`,
corpus replay at or above `MIN_PASS`). R1/R2 add no goldens; R3/R4 add goldens with export round trip.
