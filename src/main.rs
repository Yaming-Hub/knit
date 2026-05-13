//! knit CLI — synthetic data generation toolset.

use clap::{CommandFactory, Parser};
use colored::Colorize;
use tracing_subscriber::{fmt, prelude::*, EnvFilter, Registry};

use knit::cli::commands::{enrich, generate, generators, init, inspect, learn, model, plan, scale, blueprint, tokenize, validate};
use knit::cli::config::resolve_config;
use knit::cli::{Cli, Command, LogFormat, ModelAction, ModelFormat, BlueprintAction};
use knit::decision::{self, DecisionLogger};

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

    // Initialize decision logger if report path is requested.
    let decision_logger = if cli.decision_report.is_some() {
        let logger = DecisionLogger::new();
        decision::set_global_logger(logger.clone());
        Some(logger)
    } else {
        None
    };

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

    let result: anyhow::Result<()> = match &cli.command {
        Command::Validate { blueprint } => validate::run(blueprint, &cli),
        Command::Plan { blueprint } => plan::run(blueprint, &cli),
        Command::Generate {
            blueprint,
            output,
            entities,
        } => generate::run(blueprint, output, entities, &cli),
        Command::Blueprint { action } => match action {
            BlueprintAction::Expand { file } => blueprint::run_expand(file, cli.json),
            BlueprintAction::Normalize { file } => blueprint::run_normalize(file, cli.json),
            BlueprintAction::Diff { a, b } => blueprint::run_diff(a, b),
            BlueprintAction::Doc { file, output } => blueprint::run_doc(file, output.as_deref()),
            BlueprintAction::Stats { file } => blueprint::run_stats(file, cli.json),
            BlueprintAction::Merge {
                base,
                overlay,
                output,
            } => blueprint::run_merge(base, overlay, output.as_deref(), cli.json),
            BlueprintAction::Graph { file, format } => blueprint::run_graph(file, &format),
            BlueprintAction::Lint { file } => blueprint::run_lint(file, cli.json),
            BlueprintAction::Subset {
                file,
                entities,
                no_deps,
                output,
            } => blueprint::run_subset(file, &entities, !no_deps, output.as_deref(), cli.json),
            BlueprintAction::Rename {
                file,
                entities,
                fields,
                output,
            } => blueprint::run_rename(file, &entities, &fields, output.as_deref(), cli.json),
            BlueprintAction::Export {
                file,
                format,
                dialect,
                no_fks,
                output,
            } => blueprint::run_export(file, &format, &dialect, !no_fks, output.as_deref()),
            BlueprintAction::Scaffold {
                name,
                entities,
                rels,
                output,
            } => blueprint::run_scaffold(&name, &entities, &rels, output.as_deref(), cli.json),
            BlueprintAction::Import {
                file,
                name,
                output,
            } => blueprint::run_import(&file, name.as_deref(), output.as_deref(), cli.json),
            BlueprintAction::Update {
                file,
                counts,
                describes,
                tags,
                untags,
                seed,
                locale,
                output,
            } => blueprint::run_update(
                &file,
                &counts,
                &describes,
                &tags,
                &untags,
                seed.clone(),
                locale.as_deref(),
                output.as_deref(),
                cli.json,
            ),
            BlueprintAction::Validate {
                file,
                data,
                entities,
                strict,
            } => blueprint::run_validate(&file, &data, &entities, *strict, cli.json),
            BlueprintAction::Derive {
                file,
                scale,
                counts,
                seed,
                locale,
                variant,
                excludes,
                output,
            } => blueprint::run_derive(
                &file,
                scale.as_deref(),
                &counts,
                seed.clone(),
                locale.as_deref(),
                variant.as_deref(),
                &excludes,
                output.as_deref(),
                cli.json,
            ),
            BlueprintAction::Sample {
                file,
                rows,
                entities,
                seed,
            } => blueprint::run_sample(&file, *rows, &entities, seed.clone(), cli.json),
            BlueprintAction::Test {
                file,
                rows,
                entities,
                seed,
                strict,
            } => blueprint::run_test(&file, *rows, &entities, seed.clone(), *strict, cli.json, &cli.params),
            BlueprintAction::Convert {
                file,
                format,
                output,
            } => blueprint::run_convert(&file, output.as_deref(), format),
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
            model_format,
            review,
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
                *model_format,
                *review,
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
        Command::Scale {
            blueprint,
            output,
            analyze,
            actors,
            time,
            dims,
            count,
            cadence,
            density,
        } => scale::run(
            blueprint,
            output.as_deref(),
            *analyze,
            *actors,
            time.as_deref(),
            dims,
            *count,
            cadence.as_deref(),
            density,
            &cli,
        ),
        Command::Tokenize {
            input,
            output,
            restore,
            verify,
            dictionary,
            seed,
            tokenize_numbers,
            tokenize_dates,
            tokenize_headers,
            preserve_partitions,
            tokenize_paths,
            tokenize_columns,
            preserve_columns,
            report,
            output_format,
        } => tokenize::run(
            input,
            output,
            dictionary.as_deref(),
            *restore,
            verify.as_deref(),
            *seed,
            *tokenize_numbers,
            *tokenize_dates,
            *tokenize_headers,
            *preserve_partitions,
            *tokenize_paths,
            tokenize_columns.clone(),
            preserve_columns.clone(),
            *report,
            output_format.as_deref(),
            cli.json,
        ),
        Command::Enrich {
            blueprint,
            reference,
            output,
            entity,
            min_confidence,
            max_rows,
            dry_run,
            interactive,
        } => enrich::run(
            blueprint,
            reference,
            output.as_deref(),
            entity.as_deref(),
            *min_confidence,
            *max_rows,
            *dry_run,
            *interactive,
            &cli,
        ),
        Command::Model { action } => match action {
            ModelAction::Convert { input, output } => model::run_convert(input, output),
            ModelAction::Info { input } => model::run_info(input),
        },
    }
    .inspect_err(|e| {
        if let Some(hint) = knit::cli::suggestions::suggest_fix(&e.to_string()) {
            eprintln!("{} {}", "hint:".cyan().bold(), hint);
        }
    });

    // Write decision report if requested (on both success and failure).
    if let (Some(logger), Some(report_path)) = (decision_logger, &cli.decision_report) {
        // Print low-confidence summary to stderr before writing full report
        let low_conf = logger.low_confidence_decisions();
        if !low_conf.is_empty() && !cli.quiet {
            eprintln!(
                "\n{} {} low-confidence decision(s) — review in report:",
                "⚠".yellow().bold(),
                low_conf.len(),
            );
            for d in low_conf.iter().take(5) {
                let loc = match (&d.entity, &d.column) {
                    (Some(e), Some(c)) => format!("{e}.{c}"),
                    (Some(e), None) => e.clone(),
                    _ => "—".to_string(),
                };
                eprintln!(
                    "  • [{loc}] {}: {}",
                    d.chosen, d.reason,
                );
            }
            if low_conf.len() > 5 {
                eprintln!("  … and {} more", low_conf.len() - 5);
            }
        }

        let pipeline = match &cli.command {
            Command::Learn { .. } => "learn",
            Command::Generate { .. } => "generate",
            Command::Scale { .. } => "scale",
            _ => "other",
        };
        let report = logger.into_report(pipeline);
        if let Err(e) = decision::write_report(&report, std::path::Path::new(report_path)) {
            eprintln!(
                "{} Failed to write decision report: {}",
                "warning:".yellow().bold(),
                e,
            );
        } else {
            eprintln!(
                "{} Decision report written to {} ({} decisions)",
                "info:".cyan().bold(),
                report_path,
                report.summary.total_decisions,
            );
        }
    }

    result
}