//! knit CLI — synthetic data generation toolset.

use clap::{CommandFactory, Parser};
use colored::Colorize;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

use knit::cli::commands::{generate, generators, init, inspect, learn, plan, schema, validate};
use knit::cli::config::resolve_config;
use knit::cli::{Cli, Command, LogFormat, SchemaAction};

/// Guard that must be held for the lifetime of the program to flush async writers.
struct _TracingGuard {
    _file_guard: Option<tracing_appender::non_blocking::WorkerGuard>,
}

/// Initialise tracing based on CLI flags and environment variables.
///
/// Precedence for filter: `--log-filter` > `KNIT_LOG` > `RUST_LOG` > `-q/-v` > default.
fn init_tracing(cli: &Cli) -> _TracingGuard {
    // Build the env filter with correct precedence.
    let env_filter = if let Some(ref f) = cli.log_filter {
        EnvFilter::new(f)
    } else if let Ok(f) = std::env::var("KNIT_LOG") {
        EnvFilter::new(f)
    } else {
        EnvFilter::try_from_default_env().unwrap_or_else(|_| {
            let level = if cli.quiet {
                "error"
            } else if cli.verbose {
                "debug"
            } else {
                "info"
            };
            EnvFilter::new(level)
        })
    };

    // Determine log format: explicit flag > KNIT_LOG_FORMAT env > auto-detect (tty=text, pipe=json).
    let use_json = match cli.log_format {
        Some(LogFormat::Json) => true,
        Some(LogFormat::Text) => false,
        None => {
            if let Ok(fmt_env) = std::env::var("KNIT_LOG_FORMAT") {
                fmt_env.eq_ignore_ascii_case("json")
            } else {
                use std::io::IsTerminal;
                !std::io::stderr().is_terminal()
            }
        }
    };

    // Build optional file layer (always JSON).
    let (file_layer, file_guard): (
        Option<Box<dyn tracing_subscriber::Layer<_> + Send + Sync>>,
        Option<tracing_appender::non_blocking::WorkerGuard>,
    ) = if let Some(ref path) = cli.log_file {
        match std::fs::File::create(path) {
            Ok(file) => {
                let (non_blocking, guard) = tracing_appender::non_blocking(file);
                let layer = fmt::layer()
                    .json()
                    .with_target(true)
                    .with_writer(non_blocking);
                (Some(Box::new(layer)), Some(guard))
            }
            Err(e) => {
                eprintln!("warning: failed to open log file '{}': {}", path, e);
                (None, None)
            }
        }
    } else {
        (None, None)
    };

    // Build stderr layer with chosen format, using boxed trait object for type erasure.
    let stderr_layer: Box<dyn tracing_subscriber::Layer<_> + Send + Sync> = if use_json {
        Box::new(
            fmt::layer()
                .json()
                .with_target(true)
                .with_writer(std::io::stderr),
        )
    } else {
        Box::new(
            fmt::layer()
                .with_target(false)
                .with_writer(std::io::stderr),
        )
    };

    Registry::default()
        .with(env_filter)
        .with(stderr_layer)
        .with(file_layer)
        .init();

    _TracingGuard {
        _file_guard: file_guard,
    }
}

fn main() -> anyhow::Result<()> {
    let mut cli = Cli::parse();
    let _tracing_guard = init_tracing(&cli);

    // Load config (file + env vars); CLI flags take final precedence.
    let config = resolve_config();

    // Apply config defaults where CLI flags weren't explicitly set.
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
        Command::Generate {
            schema,
            output,
            entities,
        } => generate::run(schema, output, entities, &cli),
        Command::Schema { action } => match action {
            SchemaAction::Expand { file } => schema::run_expand(file, cli.json),
            SchemaAction::Normalize { file } => schema::run_normalize(file, cli.json),
            SchemaAction::Diff { a, b } => schema::run_diff(a, b),
            SchemaAction::Doc { file, output } => schema::run_doc(file, output.as_deref()),
        },
        Command::Init { output, template } => init::run(output, template.as_deref()),
        Command::Learn {
            source,
            output,
            sample,
            state,
            finalize,
            strict,
            entities,
            actors,
            actor_columns,
            personas,
        } => {
            let actors_opts = if *actors || !actor_columns.is_empty() || personas.is_some() {
                Some(learn::ActorsOpts {
                    explicit_columns: actor_columns.clone(),
                    max_personas: *personas,
                })
            } else {
                None
            };
            learn::run(
                source.as_deref(),
                output,
                *sample,
                state.as_deref(),
                *finalize,
                *strict,
                entities,
                actors_opts.as_ref(),
                &cli,
            )
        }
        Command::Inspect {
            file,
            columns,
            actors,
        } => inspect::run(file, *columns, *actors, &cli),
        Command::Completions { shell } => {
            clap_complete::generate(*shell, &mut Cli::command(), "knit", &mut std::io::stdout());
            Ok(())
        }
        Command::Generators => generators::run(cli.json),
    }
    .inspect_err(|e| {
        if let Some(hint) = knit::cli::suggestions::suggest_fix(&e.to_string()) {
            eprintln!("{} {}", "hint:".cyan().bold(), hint);
        }
    })
}