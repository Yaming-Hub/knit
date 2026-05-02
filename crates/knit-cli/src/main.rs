//! knit CLI — synthetic data generation toolset.
//!
//! Provides three core commands:
//! - `validate` — parse and validate a schema file
//! - `plan` — show the execution plan (dry run)
//! - `generate` — run the full forward pipeline

mod commands;

use clap::{Parser, Subcommand, ValueEnum};
use tracing_subscriber::EnvFilter;

use commands::{generate, plan, validate};

/// Knit — deterministic synthetic data generation.
#[derive(Parser, Debug)]
#[command(name = "knit", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Global random seed (overrides schema seed).
    #[arg(long, global = true)]
    seed: Option<u64>,

    /// Output format for generated data.
    #[arg(long, global = true, value_enum, default_value_t = Format::Parquet)]
    format: Format,

    /// Compression algorithm (parquet only).
    #[arg(long, global = true, value_enum, default_value_t = CompressionArg::Snappy)]
    compression: CompressionArg,

    /// Number of parallel workers (0 = auto).
    #[arg(long, global = true, default_value_t = 0)]
    parallel: usize,

    /// Rows per Arrow batch.
    #[arg(long, global = true, default_value_t = 8192)]
    batch_size: usize,

    /// Schema parameter overrides (repeatable: --param key=value).
    #[arg(long = "param", global = true, value_parser = parse_key_val)]
    params: Vec<(String, String)>,

    /// Dry-run mode (validate and plan only, do not generate).
    #[arg(long, global = true)]
    dry_run: bool,

    /// Emit machine-readable JSON output.
    #[arg(long, global = true)]
    json: bool,

    /// Suppress non-error output.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,

    /// Enable verbose (debug) logging.
    #[arg(short, long, global = true)]
    verbose: bool,
}

/// Supported output formats for generated data.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Format {
    Parquet,
    Csv,
    Json,
    Jsonl,
    #[value(name = "arrow")]
    ArrowIpc,
}

/// Compression algorithms.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CompressionArg {
    None,
    Snappy,
    Gzip,
    Lz4,
    Zstd,
}

/// Top-level subcommands.
#[derive(Subcommand, Debug)]
enum Command {
    /// Parse and validate a schema file.
    Validate {
        /// Path to the schema file (TOML or JSON).
        schema: String,
    },
    /// Show the execution plan without generating data.
    Plan {
        /// Path to the schema file (TOML or JSON).
        schema: String,
    },
    /// Generate synthetic data from a schema.
    Generate {
        /// Path to the schema file (TOML or JSON).
        schema: String,
        /// Output directory for generated files.
        #[arg(short, long, default_value = "output")]
        output: String,
    },
}

/// Parse `key=value` pairs for `--param`.
fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=VALUE: no `=` found in `{s}`"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

/// Initialise tracing based on verbosity flags.
fn init_tracing(quiet: bool, verbose: bool) {
    let filter = if quiet {
        "error"
    } else if verbose {
        "debug"
    } else {
        "info"
    };
    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(filter));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    init_tracing(cli.quiet, cli.verbose);

    match &cli.command {
        Command::Validate { schema } => validate::run(schema, &cli),
        Command::Plan { schema } => plan::run(schema, &cli),
        Command::Generate { schema, output } => {
            if cli.dry_run {
                plan::run(schema, &cli)
            } else {
                generate::run(schema, output, &cli)
            }
        }
    }
}
