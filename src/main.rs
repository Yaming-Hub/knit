//! knit CLI — synthetic data generation toolset.

use clap::{CommandFactory, Parser};
use colored::Colorize;
use tracing_subscriber::EnvFilter;

use knit::cli::commands::{generate, generators, init, inspect, learn, plan, schema, validate};
use knit::cli::config::resolve_config;
use knit::cli::{Cli, Command, SchemaAction};

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