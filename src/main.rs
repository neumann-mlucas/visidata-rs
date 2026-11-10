use std::ffi::OsString;
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
    #[arg(required_unless_present = "session")]
    path: Option<OsString>,

    /// Restore a session (`:mksession`, or the one saved on quit).
    #[arg(
        short = 'S',
        long = "session",
        value_name = "FILE",
        conflicts_with = "path"
    )]
    session: Option<String>,

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
    Json,
    Xlsx,
    Sqlite,
}
impl From<Format> for SourceKind {
    fn from(f: Format) -> Self {
        match f {
            Format::Csv => SourceKind::Csv,
            Format::Tsv => SourceKind::Tsv,
            Format::Parquet => SourceKind::Parquet,
            Format::Arrow => SourceKind::Arrow,
            Format::Jsonl => SourceKind::Jsonl,
            Format::Json => SourceKind::Json,
            Format::Xlsx => SourceKind::Xlsx,
            Format::Sqlite => SourceKind::Sqlite,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let fmt = cli.format.map(SourceKind::from);

    // Load Lua plugins before the initial read so `vd.register_loader` rules
    // can intercept a user-supplied extension. Plugin-load failure is reported
    // but non-fatal — users without an `init.lua` are the common case.
    if !cli.headless {
        // before load: a file appearing later would count as an edit and reload
        plugins::ensure_default_config();
    }
    let (plugins, plugin_warning) = plugins::Plugins::load();
    visidata_rs::duckdb::set_cmd(plugins.duckdb_cmd());
    let plugins = Some(plugins);

    let path = match (&cli.session, cli.path) {
        (Some(s), _) => {
            let text = match std::fs::read_to_string(s) {
                Ok(t) => t,
                Err(e) => {
                    eprintln!("vdrs: {s}: {e}");
                    return ExitCode::from(1);
                }
            };
            match visidata_rs::session::first_root(&text) {
                Some(p) => OsString::from(p),
                None => {
                    eprintln!("vdrs: {s}: no :open line, nothing to restore");
                    return ExitCode::from(1);
                }
            }
        }
        (None, Some(p)) => p,
        (None, None) => {
            eprintln!("vdrs: give a file to open, or -S <session>");
            return ExitCode::from(2);
        }
    };
    let loaded = match plugins::open(plugins.as_ref(), &path, fmt) {
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
    // Several worksheets / tables → start on the list; `<CR>` opens one.
    let parts = match &loaded.path {
        Some(p) => reader::list_parts(p, loaded.kind).unwrap_or_default(),
        None => Vec::new(),
    };
    let sheet = if parts.is_empty() && loaded.kind != SourceKind::Duckdb {
        Sheet::from_source(0, loaded)
    } else {
        Sheet::parts_list(0, loaded, parts)
    };
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
    let result = rt.block_on(app::run(sheet, plugins, plugin_warning, cli.session));
    rt.shutdown_background(); // or quitting waits for running sorts and loads
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("vdrs: {e:#}"); // `:#` = include the cause chain
            ExitCode::from(1)
        }
    }
}

fn print_headless(src: &reader::LoadedSource, head: usize) -> Result<(), AppError> {
    use std::io::Write;
    let df = src.lazy.clone().limit(head as u32).collect()?;
    let mut out = std::io::stdout().lock();
    let written = (|| -> std::io::Result<()> {
        writeln!(out, "source : {} ({:?})", src.display_name, src.kind)?;
        writeln!(out, "schema : {} columns", df.width())?;
        for (name, dtype) in df.schema().iter() {
            writeln!(out, "  {name:<20} {dtype}")?;
        }
        writeln!(out, "head   :")?;
        writeln!(out, "{df}")
    })();
    match written {
        // `vdrs --headless x | head`: the reader left early; that's fine.
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        other => Ok(other?),
    }
}

#[derive(thiserror::Error, Debug)]
enum AppError {
    #[error(transparent)]
    Open(#[from] OpenError),
    #[error("polars: {0}")]
    Polars(#[from] polars::prelude::PolarsError),
    #[error("write: {0}")]
    Io(#[from] std::io::Error),
}
