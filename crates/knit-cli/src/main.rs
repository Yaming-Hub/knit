//! knit CLI — synthetic data generation toolset.
//!
//! Provides commands for schema management, planning, and data generation:
//! - `validate` — parse and validate a schema file
//! - `plan` — show the execution plan (dry run)
//! - `generate` — run the full forward pipeline
//! - `schema expand|normalize|diff` — schema manipulation
//! - `init` — interactive project setup wizard
//! - `learn` — infer schema from data

mod commands;
mod config;
pub mod suggestions;

use clap::{Parser, Subcommand, ValueEnum};
use colored::Colorize;
use tracing_subscriber::EnvFilter;

use commands::{generate, init, learn, plan, schema, validate};
use config::resolve_config;

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

    /// Skip noise injection even if schema defines noise profiles.
    #[arg(long, global = true)]
    no_noise: bool,
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
    /// Schema manipulation operations.
    Schema {
        #[command(subcommand)]
        action: SchemaAction,
    },
    /// Initialize a new knit project with a starter schema.
    Init {
        /// Output file path.
        #[arg(short, long, default_value = ".weave.toml")]
        output: String,
    },
    /// Infer a Weave schema from existing data files or directories.
    Learn {
        /// Path to data file or directory to learn from.
        source: String,
        /// Output schema file path.
        #[arg(short, long, default_value = "learned.weave.toml")]
        output: String,
    },
}

/// Schema subcommands.
#[derive(Subcommand, Debug)]
enum SchemaAction {
    /// Flatten extends chain into a standalone schema.
    Expand {
        /// Path to the schema file.
        file: String,
    },
    /// Reformat schema to canonical style.
    Normalize {
        /// Path to the schema file.
        file: String,
    },
    /// Compare two schemas and show differences.
    Diff {
        /// Path to the first schema file.
        a: String,
        /// Path to the second schema file.
        b: String,
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
    let mut cli = Cli::parse();
    init_tracing(cli.quiet, cli.verbose);

    // Load config (file + env vars); CLI flags take final precedence.
    let config = resolve_config();

    // Apply config defaults where CLI flags weren't explicitly set.
    // CLI flags (non-default) > env vars > config file.
    if cli.seed.is_none() {
        cli.seed = config.seed;
    }
    if cli.parallel == 0 {
        if let Some(p) = config.parallel {
            cli.parallel = p;
        }
    }
    if cli.batch_size == 8192 {
        if let Some(bs) = config.batch_size {
            cli.batch_size = bs;
        }
    }

    match &cli.command {
        Command::Validate { schema } => validate::run(schema, &cli),
        Command::Plan { schema } => plan::run(schema, &cli),
        Command::Generate { schema, output } => {
            generate::run(schema, output, &cli)
        }
        Command::Schema { action } => match action {
            SchemaAction::Expand { file } => schema::run_expand(file, cli.json),
            SchemaAction::Normalize { file } => schema::run_normalize(file, cli.json),
            SchemaAction::Diff { a, b } => schema::run_diff(a, b),
        },
        Command::Init { output } => init::run(output),
        Command::Learn { source, output } => learn::run(source, output, &cli),
    }
    .inspect_err(|e| {
        if let Some(hint) = suggestions::suggest_fix(&e.to_string()) {
            eprintln!("{} {}", "hint:".cyan().bold(), hint);
        }
    })
}
