use std::ffi::OsString;
use std::process::ExitCode;

use anyhow::{Context, anyhow};
use clap::Parser;

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

    /// Force a format (stdin, or a wrong extension): csv, tsv, parquet,
    /// arrow, jsonl, json, xlsx, sqlite, fixed, usv, lsv, jsonla, yaml, toml,
    /// html, zip, or a Lua loader's name.
    #[arg(short = 'f', long)]
    format: Option<String>,

    /// Don't start the TUI — print shape, schema, and the first N rows, then exit.
    #[arg(long)]
    headless: bool,

    /// Rows to print in headless mode.
    #[arg(long, default_value_t = 10)]
    head: usize,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("vdrs: {e:#}"); // `:#` = include the cause chain
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    // Load Lua plugins before the initial read so `vd.register_loader` rules
    // can intercept a user-supplied extension. Plugin-load failure is reported
    // but non-fatal — users without an `init.lua` are the common case.
    if !cli.headless {
        // before load: a file appearing later would count as an edit and reload
        plugins::ensure_default_config();
    }
    let (plugins, plugin_warning) = plugins::Plugins::load();
    visidata_rs::duckdb::set_cmd(plugins.options().duckdb_cmd);
    let plugins = Some(plugins);

    let path = match (&cli.session, cli.path) {
        (Some(s), _) => {
            let text = std::fs::read_to_string(s).with_context(|| s.clone())?;
            let root = visidata_rs::session::first_root(&text).map_err(|e| anyhow!("{s}: {e}"))?;
            OsString::from(root)
        }
        (None, Some(p)) => p,
        (None, None) => {
            eprintln!("vdrs: give a file to open, or -S <session>");
            return Ok(ExitCode::from(2));
        }
    };
    // anyhow!: OpenError's message already holds its source; `:#` would repeat it
    let loaded = plugins::open(plugins.as_ref(), &path, cli.format.as_deref())
        .map_err(|e| anyhow!("{e}"))?;
    if cli.headless {
        if let Some(w) = &plugin_warning {
            eprintln!("vdrs: {w}");
        }
        print_headless(&loaded, cli.head)?;
        return Ok(ExitCode::SUCCESS);
    }
    let sheet = Sheet::opened(0, loaded);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("tokio")?;
    let result = rt.block_on(app::run(sheet, plugins, plugin_warning, cli.session));
    rt.shutdown_background(); // or quitting waits for running sorts and loads
    result.map(|()| ExitCode::SUCCESS)
}

fn print_headless(src: &reader::LoadedSource, head: usize) -> anyhow::Result<()> {
    use std::io::Write;
    let df = src
        .lazy
        .clone()
        .limit(u32::try_from(head).unwrap_or(u32::MAX))
        .collect()
        .map_err(|e| anyhow!("polars: {e}"))?;
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
        other => other.context("write"),
    }
}
