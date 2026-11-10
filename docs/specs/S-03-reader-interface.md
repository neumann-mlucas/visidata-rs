# S-03 — Reader interface

Status: **draft** (unapproved)
Phase: 0
Depends on: S-01 (`Source` enum slot), S-02 (`tokio` runtime)
Research: [`../research/tui-patterns.md`](../research/tui-patterns.md) Q2
Phase 0 formats: CSV, TSV, Parquet, Arrow IPC, JSON Lines, stdin

## Problem

Phase 0 needs to open a handful of formats and hand each one back as a Polars `LazyFrame`. The abstraction has to (a) be a trait (per user call), (b) stay small — Phase 0 is 5 formats, (c) leave a hole for Lua-provided sources in Phase 3, (d) survive `cat x.csv | visidata-rs` without choking on a non-seekable stdin.

## Decision

**`trait Source` + `#[enum_dispatch]` over `AnySource`.** Static dispatch, no boxing, a `Lua` variant reserved for Phase 3. Extension-only detection via `PathBuf::extension()` — no magic bytes in Phase 0 (user call). Stdin via a csvlens-style `SeekableFile` tempfile fallback. Readers return `LazyFrame` wherever Polars has a native `scan_*`; a `.slice()` fallback for formats that don't scan.

## Shape

```rust
#[enum_dispatch]
pub trait Source: Send {
    fn schema(&mut self) -> impl Future<Output = Result<Schema, OpenError>> + Send;
    fn lazy(&self) -> Result<LazyFrame, OpenError>;         // fast path when Polars scans natively
    fn total_rows_hint(&self) -> Option<usize>;             // Parquet knows; CSV doesn't
}

#[enum_dispatch(Source)]
pub enum AnySource {
    Csv(CsvSource),
    Tsv(TsvSource),
    Parquet(ParquetSource),
    Arrow(ArrowSource),
    Jsonl(JsonlSource),
    Stdin(StdinSource),
    // Phase 3 hole:
    // Lua(LuaSource),
}

#[derive(thiserror::Error, Debug)]
pub enum OpenError {
    #[error("io: {0}")] Io(#[from] io::Error),
    #[error("parse: {0}")] Parse(String),
    #[error("unknown format for {path}")] UnknownFormat { path: PathBuf },
    #[error("polars: {0}")] Polars(#[from] polars::prelude::PolarsError),
}

pub fn detect(path: &Path) -> Result<SourceKind, OpenError> {
    match path.extension().and_then(OsStr::to_str) {
        Some("csv")                 => Ok(SourceKind::Csv),
        Some("tsv") | Some("tab")   => Ok(SourceKind::Tsv),
        Some("parquet") | Some("pq")=> Ok(SourceKind::Parquet),
        Some("arrow") | Some("ipc") | Some("feather") => Ok(SourceKind::Arrow),
        Some("jsonl") | Some("ndjson") => Ok(SourceKind::Jsonl),
        _ => Err(OpenError::UnknownFormat { path: path.to_owned() }),
    }
}

pub fn open(path: &Path) -> Result<AnySource, OpenError> {
    let kind = if path == Path::new("-") { SourceKind::Stdin } else { detect(path)? };
    AnySource::from_kind(kind, path)
}
```

**Per-variant construction** uses Polars `scan_*`:

- `CsvSource::new(path)` wraps `LazyCsvReader::new(path).with_has_header(true).finish()?`.
- `ParquetSource::new(path)` wraps `scan_parquet(path, Default::default())?`.
- `ArrowSource::new(path)` wraps `scan_ipc(path, Default::default())?`.
- `JsonlSource::new(path)` wraps `LazyJsonLineReader::new(path).finish()?`.

**Stdin**: `StdinSource` holds a `SeekableFile` — on construction, copy stdin to `tempfile::NamedTempFile`, detect format from a user `--format` flag or by sampling the first line, then wrap the tempfile as a `CsvSource` / `JsonlSource` / etc. This mirrors csvlens' `src/io.rs` approach. Non-seekable stdin can't drive Polars lazy readers directly without rewinds.

**Reader options** are per-source structs, not a generic `Opts` bag:

```rust
pub struct CsvOpts {
    pub delimiter: u8,       // default b','
    pub has_header: bool,    // default true
    pub quote: Option<u8>,   // default Some(b'"')
    pub skip_rows: usize,
    pub infer_schema_length: Option<usize>, // None = all
}
```

CLI flags that override: `--csv-delim`, `--csv-no-header`, `--csv-quote`. VisiData's `set-type-str` muscle memory lands in Phase 1 after S-05.

**Error policy** (echoes S-02's three tiers):

- Open fails → `OpenError`; the sheet never joins the stack. UI shows a modal `Confirm` with the message.
- Materialize fails later → `MaterializeError`, non-fatal; `window` stays with its last-good contents, status-line toast.
- Decoding error mid-row (e.g. bad UTF-8 inside a CSV row) → Polars' `ignore_errors` default **off**. We expose `--ignore-errors` and a `:set ignore_errors true` ex-command later. Phase 0 fails hard; users who need VisiData's "show error row" UX wait for S-05.

## Why not <alternative>

- **Free functions, no trait** (what csvlens does). csvlens is single-format; we're multi. User's call stands — trait with enum_dispatch gives Lua its seat (Phase 3) without a vtable now.
- **`Box<dyn Source>`.** The trait has an `async fn`; `dyn` + async is still painful in late 2026. Static dispatch via `enum_dispatch` sidesteps it.
- **Magic-bytes detection in Phase 0** (`infer` crate). User said extension-only. `infer` lands in Phase 1 or when the first user runs `cat x.parquet | visidata-rs` — then we need magic.
- **`SourceKind` + factory function in one trait.** Keeps construction a per-variant concern. Factory in `AnySource::from_kind` is dumb and OK.
- **Streaming CSV like csvlens.** Polars `scan_csv` already gives us lazy + predicate pushdown. We don't need csvlens' byte-offset position table for Phase 0 — Polars handles slicing. If a user tails a growing CSV, that's Phase 4 (streaming sources).

## Open sub-decisions

- Should `Source` be `async` top to bottom, or sync with Polars doing the async internally? Polars' lazy API is sync-callable but it schedules on its own thread pool. We spawn `tokio::task::spawn_blocking` for `.collect()` calls per S-02. The trait method `lazy()` can stay sync. `schema()` is async only because stdin may need its tempfile write to finish.
- Should `OpenError` wrap `polars::PolarsError` or stringify it? Wrap. Users deserve a programmatic distinction between "file not found" and "bad schema".
- Where do format-specific options live? CLI flags for Phase 0; TOML config in Phase 3. Not in `Source` methods — construction concern, not read concern.
- Tempfile cleanup on crash: `tempfile::NamedTempFile` drops to `/tmp` cleanup; if the panic hook aborts, the OS cleans. OK.

## Verification before leaving this spec

- Can S-02 open a 10 GB Parquet without reading all of it? Yes — `scan_parquet` is lazy.
- Can a Phase 3 Lua plugin register a new source without touching the enum? Not for the enum itself — but a `Lua(LuaSource)` variant covers all Lua-provided readers through one trampoline. Spec confirms the hole.
- Can stdin be pivoted? Yes once tempfiled; downstream the sheet is file-backed.
- Does `detect()` ever fail silently? No — returns `OpenError::UnknownFormat`.

## Prep for Phase 0 implementation

Not spec content, but blocking the first commit of real code:

- Pick `tempfile` 3.x; add to Cargo.toml when StdinSource is written.
- Confirm Polars feature flags cover Arrow IPC (`ipc` feature, separate from `arrow`-the-crate).
