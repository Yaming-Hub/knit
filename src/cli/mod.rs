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

/// Parse `name=count` for `--dim` (e.g. "location=20").
pub fn parse_dim_spec(s: &str) -> Result<(String, u64), String> {
    let pos = s
        .find('=')
        .ok_or_else(|| format!("invalid NAME=COUNT: no `=` found in `{s}`"))?;
    let name = s[..pos].to_string();
    let count: u64 = s[pos + 1..]
        .parse()
        .map_err(|_| format!("invalid count in `{s}`: expected integer after `=`"))?;
    Ok((name, count))
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

    /// Global random seed (overrides blueprint seed).
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

    /// Blueprint parameter overrides (repeatable: --param key=value).
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

    /// Skip noise injection even if blueprint defines noise profiles.
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

    /// Load a WASM generator plugin (repeatable). Requires `wasm-plugins` feature.
    #[arg(long = "plugin", global = true)]
    pub plugins: Vec<String>,

    /// Load all `.wasm` plugins from a directory. Requires `wasm-plugins` feature.
    #[arg(long, global = true)]
    pub plugin_dir: Option<String>,

    /// Log output format.
    #[arg(long, global = true, value_enum)]
    pub log_format: Option<LogFormat>,

    /// Write all log events to a file (always JSON format).
    #[arg(long, global = true)]
    pub log_file: Option<String>,

    /// Tracing filter directive (e.g. "learn=debug,gen=info").
    /// Cannot be combined with -v/-q; overrides KNIT_LOG/RUST_LOG.
    #[arg(long, global = true, conflicts_with_all = ["quiet", "verbose"])]
    pub log_filter: Option<String>,

    /// Write a JSON decision report to this path.
    /// Captures all key decisions made during execution with reasoning and alternatives.
    #[arg(long, global = true)]
    pub decision_report: Option<String>,
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

/// Log output format.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LogFormat {
    /// Human-readable text (default for terminals).
    Text,
    /// Structured JSON (default when output is piped).
    Json,
}

/// Model output format for knit learn.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ModelFormat {
    /// Single flat TOML file (default).
    Flat,
    /// Structured directory (knit.toml, tables/, etc.).
    Structured,
}

/// Top-level subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Parse and validate a blueprint file.
    Validate {
        /// Path to the blueprint file (TOML or JSON).
        blueprint: String,
    },
    /// Show the execution plan without generating data.
    Plan {
        /// Path to the blueprint file (TOML or JSON).
        blueprint: String,
    },
    /// Generate synthetic data from a blueprint.
    Generate {
        /// Path to the blueprint file (TOML or JSON).
        blueprint: String,
        /// Output directory for generated files.
        #[arg(short, long, default_value = "output")]
        output: String,
        /// Generate only specific entities (repeatable). Dependencies are still
        /// resolved but only selected entities produce output files.
        #[arg(long = "entity")]
        entities: Vec<String>,
    },
    /// Blueprint manipulation operations.
    Blueprint {
        #[command(subcommand)]
        action: BlueprintAction,
    },
    /// Initialize a new knit project with a starter blueprint.
    Init {
        /// Output file path.
        #[arg(short, long, default_value = "blueprint.knit.toml")]
        output: String,
        /// Path to a template file or directory to copy from.
        /// If a file, copies it as the schema (plus sibling dictionaries).
        /// If a directory, copies all files from it.
        #[arg(long)]
        template: Option<String>,
    },
    /// Infer a knit blueprint from existing data files or directories.
    Learn {
        /// Path to data file or directory to learn from.
        source: Option<String>,
        /// Output blueprint file path (or directory for structured format).
        #[arg(short, long, default_value = "learned.knit.toml")]
        output: String,
        /// Maximum rows to read per entity (for faster profiling of large files).
        #[arg(long)]
        sample: Option<usize>,
        /// State file for incremental learning (creates if absent, updates if exists).
        #[arg(long)]
        state: Option<String>,
        /// Emit blueprint from existing state without processing new data.
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
        /// Output model format: flat (single TOML file) or structured (directory).
        #[arg(long, value_enum)]
        model_format: Option<ModelFormat>,
    },
    /// Inspect a learning state file or blueprint file.
    Inspect {
        /// Path to the state file (.json) or blueprint file (.toml) to inspect.
        #[arg(name = "FILE")]
        file: String,
        /// Show per-column details (cardinality, nulls, top values).
        #[arg(long)]
        columns: bool,
        /// Show actor, persona, and relationship summary (blueprint files only).
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
    /// Scale a learned blueprint along discovered dimensions (actors, time, custom).
    Scale {
        /// Path to the learned blueprint file.
        blueprint: String,
        /// Output directory for generated files.
        #[arg(short, long)]
        output: Option<String>,
        /// Show discovered dimensions without generating.
        #[arg(long)]
        analyze: bool,
        /// Target actor count (e.g. 100).
        #[arg(long)]
        actors: Option<u64>,
        /// Target time range (e.g. "52w", "6m", "+26w", "2024-01-01..2025-12-31").
        #[arg(long)]
        time: Option<String>,
        /// Scale custom dimension (repeatable: --dim name=count).
        #[arg(long = "dim", value_parser = parse_dim_spec)]
        dims: Vec<(String, u64)>,
        /// Additional uniform row multiplier (e.g. 2.0).
        #[arg(long)]
        count: Option<f64>,
        /// Override detected time cadence (e.g. "7d", "1w").
        #[arg(long)]
        cadence: Option<String>,
    },
    /// Tokenize a dataset for safe sharing (replace strings with opaque tokens).
    Tokenize {
        /// Path to dataset directory to tokenize (or restore).
        input: String,
        /// Output directory.
        #[arg(short, long)]
        output: String,
        /// Restore tokenized dataset to original using dictionary.
        #[arg(long)]
        restore: bool,
        /// Verify tokenized dataset matches original structure.
        #[arg(long)]
        verify: Option<String>,
        /// Token dictionary path (default: <output>/.knit-tokens.json).
        #[arg(long)]
        dictionary: Option<String>,
        /// Random seed for deterministic token generation.
        #[arg(long)]
        seed: Option<u64>,
        /// Also tokenize numeric values.
        #[arg(long)]
        tokenize_numbers: bool,
        /// Also tokenize date/timestamp values.
        #[arg(long)]
        tokenize_dates: bool,
        /// Also tokenize column headers.
        #[arg(long)]
        tokenize_headers: bool,
        /// Keep partition folder values as-is.
        #[arg(long)]
        preserve_partitions: bool,
        /// Only tokenize values in these columns (comma-separated, case-insensitive).
        #[arg(long, value_delimiter = ',', conflicts_with = "preserve_columns")]
        tokenize_columns: Option<Vec<String>>,
        /// Tokenize all columns except these (comma-separated, case-insensitive).
        #[arg(long, value_delimiter = ',', conflicts_with = "tokenize_columns")]
        preserve_columns: Option<Vec<String>>,
        /// Generate a detailed tokenization report after processing.
        #[arg(long)]
        report: bool,
    },
    /// Enrich a model with statistical knowledge from reference samples.
    Enrich {
        /// Path to the base blueprint file to enrich.
        blueprint: String,
        /// Path to reference data file (CSV, Parquet, JSON).
        #[arg(long = "ref")]
        reference: String,
        /// Output blueprint path (default: overwrite input).
        #[arg(short, long)]
        output: Option<String>,
        /// Only enrich this entity (default: auto-detect from filename).
        #[arg(long)]
        entity: Option<String>,
        /// Minimum confidence for column mapping (0.0–1.0).
        #[arg(long, default_value = "0.7")]
        min_confidence: f64,
        /// Maximum rows to read from reference.
        #[arg(long)]
        max_rows: Option<usize>,
        /// Show mapping plan without modifying the model.
        #[arg(long)]
        dry_run: bool,
    },
    /// Model directory operations (convert, info).
    Model {
        #[command(subcommand)]
        action: ModelAction,
    },
}

/// Blueprint subcommands.
#[derive(Subcommand, Debug)]
pub enum BlueprintAction {
    /// Flatten extends chain into a standalone blueprint.
    Expand {
        /// Path to the blueprint file.
        file: String,
    },
    /// Reformat blueprint to canonical style.
    Normalize {
        /// Path to the blueprint file.
        file: String,
    },
    /// Compare two blueprints and show differences.
    Diff {
        /// Path to the first blueprint file.
        a: String,
        /// Path to the second blueprint file.
        b: String,
    },
    /// Generate markdown documentation for a blueprint.
    Doc {
        /// Path to the blueprint file.
        file: String,
        /// Output file path (prints to stdout if omitted).
        #[arg(short, long)]
        output: Option<String>,
    },
}

/// Model directory subcommands.
#[derive(Subcommand, Debug)]
pub enum ModelAction {
    /// Convert between flat blueprint and structured model directory.
    Convert {
        /// Input path (flat .toml file or structured directory).
        input: String,
        /// Output path (directory for structured, .toml file for flat).
        #[arg(short, long)]
        output: String,
    },
    /// Show summary information about a model.
    Info {
        /// Model path (flat file or structured directory).
        input: String,
    },
}