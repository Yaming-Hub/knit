//! CLI command implementations and configuration.

pub mod commands;
pub mod config;
pub mod suggestions;

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

/// Parse `key=value` pairs for `--param`.
pub fn parse_key_val(s: &str) -> Result<(String, String), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid KEY=VALUE: no `=` found in `{s}`"))?;
    Ok((s[..pos].to_string(), s[pos + 1..].to_string()))
}

/// Knit — deterministic synthetic data generation.
#[derive(Parser, Debug)]
#[command(
    name = "knit",
    version = concat!(
        env!("CARGO_PKG_VERSION"),
        " (",
        env!("KNIT_GIT_HASH"),
        " ",
        env!("KNIT_COMMIT_DATE"),
        ")",
    ),
    about,
    long_about = None,
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Global random seed (overrides schema seed).
    #[arg(long, global = true)]
    pub seed: Option<u64>,

    /// Output format for generated data.
    #[arg(long, global = true, value_enum, default_value_t = Format::Parquet)]
    pub format: Format,

    /// Compression algorithm (parquet only).
    #[arg(long, global = true, value_enum, default_value_t = CompressionArg::Snappy)]
    pub compression: CompressionArg,

    /// Number of parallel workers (0 = auto).
    #[arg(long, global = true, default_value_t = 0)]
    pub parallel: usize,

    /// Rows per Arrow batch.
    #[arg(long, global = true, default_value_t = 8192)]
    pub batch_size: usize,

    /// Schema parameter overrides (repeatable: --param key=value).
    #[arg(long = "param", global = true, value_parser = parse_key_val)]
    pub params: Vec<(String, String)>,

    /// Dry-run mode (validate and plan only, do not generate).
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Emit machine-readable JSON output.
    #[arg(long, global = true)]
    pub json: bool,

    /// Suppress non-error output.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    pub quiet: bool,

    /// Enable verbose (debug) logging.
    #[arg(short, long, global = true)]
    pub verbose: bool,

    /// Skip noise injection even if schema defines noise profiles.
    #[arg(long, global = true)]
    pub no_noise: bool,

    /// Override row count for all entities (absolute value or scale factor).
    ///
    /// If the value ends with 'x' (e.g. "0.1x", "10x"), it's treated as a
    /// multiplier applied to each entity's configured count.
    /// Otherwise it's treated as an absolute row count for all entities.
    #[arg(long, global = true)]
    pub count: Option<String>,

    /// Include CREATE TABLE DDL in SQL output.
    #[arg(long, global = true)]
    pub sql_create_table: bool,

    /// Wrap SQL output in BEGIN/COMMIT transaction.
    #[arg(long, global = true)]
    pub sql_transaction: bool,
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
    Avro,
    Sql,
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
pub enum Command {
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
        /// Generate only specific entities (repeatable). Dependencies are still
        /// resolved but only selected entities produce output files.
        #[arg(long = "entity")]
        entities: Vec<String>,
    },
    /// Schema manipulation operations.
    Schema {
        #[command(subcommand)]
        action: SchemaAction,
    },
    /// Initialize a new knit project with a starter schema.
    Init {
        /// Output file path.
        #[arg(short, long, default_value = "schema.weave.toml")]
        output: String,
        /// Path to a template file or directory to copy from.
        /// If a file, copies it as the schema (plus sibling dictionaries).
        /// If a directory, copies all files from it.
        #[arg(long)]
        template: Option<String>,
    },
    /// Infer a Weave schema from existing data files or directories.
    Learn {
        /// Path to data file or directory to learn from.
        source: Option<String>,
        /// Output schema file path.
        #[arg(short, long, default_value = "learned.weave.toml")]
        output: String,
        /// Maximum rows to read per entity (for faster profiling of large files).
        #[arg(long)]
        sample: Option<usize>,
        /// State file for incremental learning (creates if absent, updates if exists).
        #[arg(long)]
        state: Option<String>,
        /// Emit schema from existing state without processing new data.
        #[arg(long)]
        finalize: bool,
        /// Error on duplicate source paths (default: warn).
        #[arg(long)]
        strict: bool,
        /// Learn only specific entities/tables (repeatable). Others are skipped.
        #[arg(long = "entity")]
        entities: Vec<String>,
        /// Enable human behavioral analysis (actor profiling, persona clustering, relationship graphs).
        #[arg(long)]
        actors: bool,
        /// Specify actor columns explicitly (repeatable). Skips auto-detection.
        #[arg(long = "actor-column")]
        actor_columns: Vec<String>,
        /// Maximum number of personas to discover (default: auto via silhouette score).
        #[arg(long)]
        personas: Option<usize>,
    },
    /// Inspect a learning state file or schema file.
    Inspect {
        /// Path to the state file (.json) or schema file (.toml) to inspect.
        #[arg(name = "FILE")]
        file: String,
        /// Show per-column details (cardinality, nulls, top values).
        #[arg(long)]
        columns: bool,
        /// Show actor, persona, and relationship summary (schema files only).
        #[arg(long)]
        actors: bool,
    },
    /// Generate shell completion scripts.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: Shell,
    },
    /// List available generator types with descriptions and examples.
    Generators,
}

/// Schema subcommands.
#[derive(Subcommand, Debug)]
pub enum SchemaAction {
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
    /// Generate markdown documentation for a schema.
    Doc {
        /// Path to the schema file.
        file: String,
        /// Output file path (prints to stdout if omitted).
        #[arg(short, long)]
        output: Option<String>,
    },
}
