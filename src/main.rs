use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};

use reader::{OpenError, SourceKind};
use visidata_rs::sheet::Sheet;
use visidata_rs::{app, plugins, reader};

/// Modal terminal data explorer. Pre-alpha.
#[derive(Parser, Debug)]
#[command(name = "vdrs", version, about)]
struct Cli {
    /// Path to open; `-` for stdin.
    path: OsString,

    /// Force a format (useful for stdin or when the extension is wrong).
    #[arg(long, value_enum)]
    format: Option<Format>,

    /// Don't start the TUI — print shape, schema, and the first N rows, then exit.
    #[arg(long)]
    headless: bool,

    /// Rows to print in headless mode.
    #[arg(long, default_value_t = 10)]
    head: usize,
}

#[derive(ValueEnum, Debug, Clone, Copy)]
enum Format {
    Csv,
    Tsv,
    Parquet,
    Arrow,
    Jsonl,
}
impl From<Format> for SourceKind {
    fn from(f: Format) -> Self {
        match f {
            Format::Csv => SourceKind::Csv,
            Format::Tsv => SourceKind::Tsv,
            Format::Parquet => SourceKind::Parquet,
            Format::Arrow => SourceKind::Arrow,
            Format::Jsonl => SourceKind::Jsonl,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let fmt = cli.format.map(SourceKind::from);

    // Load Lua plugins before the initial read so `vd.register_loader` rules
    // can intercept a user-supplied extension. Plugin-load failure is reported
    // but non-fatal — users without an `init.lua` are the common case.
    let (plugins, plugin_warning) = match plugins::Plugins::load() {
        Ok(p) => (Some(p), None),
        Err(e) => (None, Some(format!("init.lua: {e}"))),
    };

    let loaded = match open_via_loader_or_native(&cli.path, fmt, plugins.as_ref()) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("vdrs: {e}");
            return ExitCode::from(1);
        }
    };
    if cli.headless {
        return match print_headless(&loaded, cli.head) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("vdrs: {e}");
                ExitCode::from(1)
            }
        };
    }
    let sheet = Sheet::from_source(0, loaded);
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(r) => r,
        Err(e) => {
            eprintln!("vdrs: tokio: {e}");
            return ExitCode::from(1);
        }
    };
    match rt.block_on(app::run(sheet, plugins, plugin_warning)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vdrs: {e}");
            ExitCode::from(1)
        }
    }
}

/// Consult registered Lua loaders by extension; if a rule matches, run the
/// Lua `open(path)` to produce a converted file, then scan that natively.
/// Falls through to `reader::open` when no plugin claims the extension.
fn open_via_loader_or_native(
    path: &OsStr,
    fmt: Option<SourceKind>,
    plugins: Option<&plugins::Plugins>,
) -> Result<reader::LoadedSource, OpenError> {
    if let Some(p) = plugins {
        if let Some(rule) = p.resolve_loader(Path::new(path)) {
            let converted = p
                .call_loader(&rule, Path::new(path))
                .map_err(|e| OpenError::Plugin(e.to_string()))?;
            return reader::open(converted.as_os_str(), fmt);
        }
    }
    reader::open(path, fmt)
}

fn print_headless(src: &reader::LoadedSource, head: usize) -> Result<(), AppError> {
    let df = src.lazy.clone().limit(head as u32).collect()?;
    println!("source : {} ({:?})", src.display_name, src.kind);
    println!("schema : {} columns", df.width());
    for (name, dtype) in df.schema().iter() {
        println!("  {name:<20} {dtype}");
    }
    println!("head   :");
    println!("{df}");
    Ok(())
}

#[derive(thiserror::Error, Debug)]
enum AppError {
    #[error(transparent)]
    Open(#[from] OpenError),
    #[error("polars: {0}")]
    Polars(#[from] polars::prelude::PolarsError),
}
