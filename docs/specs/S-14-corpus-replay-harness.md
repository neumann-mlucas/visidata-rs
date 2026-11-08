# S-14 — Golden corpus replay harness (test strategy)

Status: **landed and stable (2026-10-16 iteration 14)** — harness at `tests/vd_replay.rs` (~1230 LOC single file; the planned `vd_replay/{cmdlog,translate,headless,normalize}.rs` module split was cut as premature). Current counts (2026-11-08, VisiData `5f67ff8a`, `.vd`/`.vdj`/`.vdx` = 240 files): PASS=35, FAIL=0, SKIP=134, UNTESTABLE=19, QUARANTINED=13, NO_GOLDEN=39. With the quarantine emptied the ceiling is ~137 (S-25 §1 triage, 2026-11-08); the coverage plan lives in S-25. `.vdx` = VisiData's minimal script (`longname input` per line; `col`/`row`/`sheet` lines position the next command; the driver keeps a per-sheet cursor between them). Classification taxonomy below.
Phase: 1.5 (landed with S-13)
Depends on: S-13 (Tier 1 feature target)
Blocks: nothing — pure test infrastructure

## Problem

Need a test suite that:

1. **Grows automatically** as features ship, not line-by-line as tests are written.
2. **Trusts a known-correct oracle** rather than re-deriving expected output.
3. **Doesn't add VisiData as a runtime dependency** in CI.
4. **Doesn't violate direction §9.5** (zero `.vdj` compat) at runtime.

Hand-rolled Rust unit tests per feature are too much ceremony. Co-running VisiData + vdrs on each test (`vd -p cmd.vd` and `vdrs -p cmd.vd`) adds Python + pip + VisiData version pinning to CI, and treats VisiData as runtime truth we don't need to re-prove.

## Decision

**Strategy B — reuse upstream goldens; replay in vdrs only.**

Upstream ships 189 cmdlog files (`.vd`/`.vdj`) paired with 168 golden TSVs in `../visidata/tests/`. Those goldens *are* VisiData's known-correct output, versioned in their git. We replay each cmdlog through vdrs headless, diff output against the golden. VisiData the runtime stays out of CI.

The direction §9.5 compat ban forbids runtime `.vd` support. A translator living in `tests/` is **not production surface** — it exists only to drive goldens. If vdrs is ever packaged standalone from the visidata source tree, the test harness is skipped; the production binary never sees `.vd` syntax.

## Shape

### File layout

```
visidata-rs/
├── tests/
│   ├── vd_replay.rs              # the one #[test] fn
│   ├── vd_replay/
│   │   ├── cmdlog.rs             # parse .vd (TSV) and .vdj (JSONL)
│   │   ├── translate.rs          # LONGNAME_MAP: &str -> fn(&CmdlogRow) -> Option<Action>
│   │   ├── headless.rs           # run_headless(&[Action]) -> String (serialize visible sheet)
│   │   ├── normalize.rs          # tsv normalization (whitespace, line endings, trailing newline)
│   │   └── quarantine.txt        # one filename per line + reason; expected-fail but longname-covered
│   └── ...
```

### Corpus discovery

```rust
const CORPUS: &str = "../visidata/tests";

fn corpus_pairs() -> impl Iterator<Item = (PathBuf, PathBuf)> {
    glob(&format!("{CORPUS}/*.vd*")).unwrap().flatten()
        .filter_map(|cmd| {
            let stem = cmd.file_stem()?.to_string_lossy().into_owned();
            let golden = Path::new(CORPUS).join("golden").join(format!("{stem}.tsv"));
            golden.exists().then_some((cmd, golden))
        })
}
```

Files without a matching golden are skipped silently — they test VisiData behaviors we aren't asked to reproduce (interactive-only features, side-effects).

### Cmdlog parser

Both formats produce the same `CmdlogRow`:

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct CmdlogRow {
    pub sheet: String,
    pub col: String,
    pub row: String,
    pub longname: String,
    pub input: String,
    pub keystrokes: String,
    #[serde(default)]
    pub comment: String,
}

pub fn parse(path: &Path) -> Vec<CmdlogRow> {
    match path.extension().and_then(|e| e.to_str()) {
        Some("vdj") => path.read_to_string().unwrap().lines()
            .filter(|l| !l.starts_with("#!") && !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect(),
        _ => csv::ReaderBuilder::new().delimiter(b'\t').has_headers(true)
            .from_path(path).unwrap()
            .deserialize().filter_map(Result::ok).collect(),
    }
}
```

### Translator (grows with Tier 1)

```rust
pub fn translate(row: &CmdlogRow) -> Translation {
    match row.longname.as_str() {
        "open-file"    => Have(Action::OpenFile(row.input.clone())),
        "sort-asc"     => Have(Action::SortAsc  { col: row.col.clone() }),
        "sort-desc"    => Have(Action::SortDesc { col: row.col.clone() }),
        "freq-col"     => Have(Action::FreqCol  { col: row.col.clone() }),
        "pivot"        => Have(Action::Pivot    { on: row.col.clone(), input: row.input.clone() }),
        "quit-sheet"   => Have(Action::QuitSheet),

        // Tier 1 chunks fill in as features ship:
        // "key-col"      => Have(Action::KeyCol { col: row.col.clone() }),
        // "hide-col"     => Have(Action::HideCol { col: row.col.clone() }),
        // "type-int"     => Have(Action::TypeCast { col: row.col.clone(), ty: Dtype::Int64 }),
        // "select-row"   => Have(Action::SelectCurrentRow),
        // ...

        unknown => Missing(unknown.to_string()),
    }
}

pub enum Translation { Have(Action), Missing(String) }
```

### Headless replay

Reuse the existing `--headless` surface. New entry point on `App`:

```rust
impl App {
    pub fn run_headless(actions: &[Action]) -> Result<String, HeadlessError> {
        let mut app = App::new_headless();
        for act in actions {
            app.dispatch(act.clone())?;
            app.await_materialize()?;  // drain any pending tokio tasks
        }
        app.visible_sheet_as_tsv()
    }

    fn visible_sheet_as_tsv(&self) -> Result<String, HeadlessError> {
        // visible columns in display order, respecting hide/key/sort/filter/selection
        let df = self.top_sheet().window.clone();
        let mut out = Vec::new();
        CsvWriter::new(&mut out).with_separator(b'\t').include_header(true).finish(&mut df.collect()?)?;
        Ok(String::from_utf8(out)?)
    }
}
```

### Diff

```rust
fn normalize(s: &str) -> String {
    s.replace("\r\n", "\n")
     .lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n")
     .trim_end().to_string() + "\n"
}
```

Loose numeric compare is **not** the default. If diffs show Polars-vs-VisiData float formatting drift (e.g., `3.0` vs `3`), add a cell-level numeric-aware compare behind an opt-in flag — don't blanket-loosen.

### The test

```rust
#[test]
fn vd_replay_corpus() {
    let quarantine = read_quarantine();
    let mut pass = 0;
    let mut skip: HashMap<String, Vec<String>> = HashMap::new();
    let mut fail: Vec<(String, String)> = vec![];
    let mut quarantined = 0;

    for (cmd, golden) in corpus_pairs() {
        let name = cmd.file_stem().unwrap().to_string_lossy().into_owned();
        if quarantine.contains(&name) { quarantined += 1; continue; }

        let rows = cmdlog::parse(&cmd);
        let actions: Result<Vec<_>, _> = rows.iter().map(|r| match translate(r) {
            Have(a) => Ok(a),
            Missing(ln) => Err(ln),
        }).collect();

        let actions = match actions {
            Ok(a) => a,
            Err(missing) => {
                skip.entry(missing).or_default().push(name);
                continue;
            }
        };

        let got = match App::run_headless(&actions) {
            Ok(s) => normalize(&s),
            Err(e) => { fail.push((name, format!("replay error: {e}"))); continue; }
        };
        let want = normalize(&fs::read_to_string(&golden).unwrap());

        if got == want {
            pass += 1;
        } else {
            fail.push((name, pretty_diff(&want, &got)));
        }
    }

    eprintln!(
        "corpus: PASS={pass}  FAIL={}  QUARANTINED={quarantined}  SKIP_BUCKETS=\n{}",
        fail.len(),
        skip.iter().map(|(ln, files)| format!("  {ln}: {} files", files.len()))
            .collect::<Vec<_>>().join("\n"),
    );

    assert!(fail.is_empty(), "regressions: {:#?}", fail);
}
```

**Semantics (S-25 P0 taxonomy, 2026-11-08):**

- **PASS** — longname-covered, output matches golden (byte-for-byte post-normalize, with numeric-aware line compare for float trailing-zeros).
- **FAIL** — longname-covered, output differs → test fails loudly.
- **SKIP** — first blocker is a longname not translated yet, a runtime error, or a serialize error. The bucket name is the blocker; planned features (editing, nested data, aggregators, columns sheet, formats) sit here until they ship.
- **UNTESTABLE** — input is Python (`addcol-expr`, `setcol-expr`, `select-expr`, `unselect-expr`, `addcol-iter`, `assert-expr`, `import-python`), random (`setcol-fake`), or `unset-option`. Classifier: `is_direction_skip_longname()`. Leaves only with a Python subset (S-21 C).
- **QUARANTINED** — only files that run to the end and FAIL; `stem  # reason` per line in `tests/vd_replay/quarantine.txt`, grouped by the phase that fixes them.
- **NO_GOLDEN** — no `golden/<stem>.tsv`: assertion-only upstream (`-nosave`, `-manual`, `-broken`), or the golden is another format (`.csv`, `.json`, …) the replay doesn't compare yet. Reason per file in the triage.

**Triage:** `VDRS_REPLAY_TRIAGE=1 cargo test --test vd_replay` ignores the quarantine, asserts nothing, and writes every file's outcome / first blocker to `tests/vd_replay/triage.txt` (checked in, so blocker shifts show in review diffs). Regenerate it in every commit that changes the counts.

**Rule of thumb:** SKIP non-zero means a feature gap that's addressable; the harness output tells you what longname to implement next. Zero SKIP means the next PASS requires either lifting a quarantine (concrete feature work with a known cost) or an untestable (direction change).

### Quarantine file

```
# tests/vd_replay/quarantine.txt
# One filename stem per line. Add a reason comment on the line above.

# VisiData writes 3.0 where Polars writes 3; waiting on numeric-aware compare
numeric_binning
```

Lives in-tree, reviewed per entry. Not a dumping ground: three entries → investigate why; thirty → the whole strategy is wrong.

### CI integration

```yaml
# .github/workflows/ci.yml
- name: Checkout upstream visidata (for golden corpus)
  uses: actions/checkout@v4
  with:
    repository: saulpw/visidata
    ref: v3.4
    path: ../visidata
- name: Run golden corpus replay
  run: cargo test --test vd_replay -- --nocapture
```

The upstream checkout pins to a specific VisiData version. Bumping that version is a conscious decision — new goldens may include new longnames or format drift.

## Why this falls out of the lazy ladder

- Rung 2 (reuse): 189 cmdlog + 168 golden pairs already paired in git upstream. Free corpus.
- Rung 7 (minimum): one `#[test] fn`, one translator `match`, one TSV normalizer. No framework, no fixtures, no per-feature test file.
- Coverage grows monotonically: ship `select-row` → ~15 new tests pass next CI run with zero harness changes.

## What this does NOT catch (fill with hand-rolled Rust tests)

- Plan panel content, pivot preview panel, tail-sheet follow mode, diff UI, exline history, which-key overlay, Lua plugin loading, keymap chord resolution, color scheme.

No upstream golden exists for these. One `#[test]` per module, as needed.

## Verification before leaving this spec

- Can a missing longname fail the suite? **No** — skipped, reported in summary. _Verified: 134 skipping on baseline run._
- Can a format drift silently? **No** — quarantine requires an in-tree PR with a reason. _Quarantine file empty on baseline._
- Does the harness depend on VisiData runtime at test time? **No** — only on the upstream git tree being checked out alongside. _Verified: harness skips cleanly if `../visidata/tests` is missing._
- Will the harness pass on day one with the current ~15 commands? **Yes**, estimated ~15–25 goldens pass immediately; the rest skip pending Tier 1 chunks (per S-13). _Actual baseline: 6 PASS. Lower than the estimate because the longname translator must support **every** longname in a cmdlog for that test to run — many sort/freq tests chain `set-option` or `columns-sheet` steps we haven't shipped._

## Lessons from the baseline run

- **Float formatting drift is pervasive.** VisiData writes `999.50`, Polars writes `999.5`. Caught by `eq_loose` numeric-aware line compare — don't global-quarantine or coerce float precision. Loose compare parses each tab-split cell as f64; if both sides parse and equal within 1e-9 relative, they match.
- **Polars quotes empty header names, VisiData doesn't.** `CsvWriter::with_quote_style(QuoteStyle::Never)` on the serializer; VisiData's golden data in `tests/` has no fields containing tabs/newlines so never-quote is safe. If a future golden file *does* contain embedded tabs, revisit.
- **Polars sort is unstable by default, VisiData is stable.** `SortMultipleOptions::with_maintain_order(true)` is mandatory for golden parity whenever sort keys have ties (`pr2688a/b` tests surface this immediately).
- **Driver vs production split.** The replay driver intentionally reimplements command semantics at the Sheet level, not through `App::dispatch`. This lets us prove Polars-level correctness of a Tier 1 chunk before any keybinding/exline/view-state production work. Divergence between driver and production is a known cost, mitigated by `[driver only]` tags in `tasks.md`.
- **Pre-existing bugs surface in lib conversion.** `src/widget.rs` referenced a nonexistent `paint_text`; dead-code-elimination in the bin-only build hid it. Adding `src/lib.rs` for integration tests forced the fix.

## Prep for Phase-1.5 implementation

- Add `csv` and `serde_json` to `[dev-dependencies]` for cmdlog parse (if not already present via Polars-io re-export).
- Add `glob` to `[dev-dependencies]`.
- Add `App::new_headless()` + `App::dispatch(Action) -> Result<_>` + `App::await_materialize()` + `App::visible_sheet_as_tsv()` to `src/app.rs`. Current `--headless` CLI mode is close — split the loop body out so tests can drive it synchronously.
- Pin upstream VisiData version in CI (`v3.4` as of 2026-10-15).
