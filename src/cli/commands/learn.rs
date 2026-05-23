//! `knit learn` — infer a knit blueprint from existing data.
//!
//! Reads data files (CSV, Parquet, JSON/JSONL) or directories,
//! profiles columns, fits distributions, detects relationships and
//! correlations, and assembles a complete knit blueprint.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use arrow::array::{Array, AsArray, LargeStringArray, StringArray};
use arrow::compute::concat_batches;
use arrow::datatypes::{DataType, TimeUnit};
use arrow::record_batch::RecordBatch;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use serde_json;
use tracing::{debug, info, info_span, warn};

use crate::learn::correlation::{detect_correlations, detect_conditional_distributions, detect_tuple_columns, detect_derived_text_columns, detect_grid_structures};
use crate::learn::fitting::{FitResult, fit_categorical, fit_distribution};
use crate::learn::ingest::{self, IngestionResult};
use crate::learn::profile::{ColumnProfile, compute_profiles};
use crate::learn::relationships::{RelColumn, TableProfile, detect_relationships};
use crate::learn::schema_assembly::{ColumnAnalysis, TableAnalysis, assemble_data_model};
use crate::learn::temporal::{TemporalPatternSpec, detect_temporal_pattern};
use crate::learn::type_inference::{InferredType, StringPattern, infer_type};

/// Options for the `--actors` behavioral analysis pipeline.
pub struct ActorsOpts {
    /// Explicitly specified actor columns (empty = auto-detect).
    pub explicit_columns: Vec<String>,
    /// Maximum number of personas (None = auto via silhouette score).
    pub max_personas: Option<usize>,
}

/// Intermediate struct for TOML serialization matching the knit blueprint format.
#[derive(Serialize)]
struct RawOutputSchema {
    blueprint_version: String,
    model: RawOutputModel,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    entities: Vec<crate::core::Entity>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    relationships: Vec<crate::core::Relationship>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    correlations: Vec<crate::core::Correlation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    personas: Vec<crate::core::Persona>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    actor_relationships: Vec<crate::core::ActorRelationship>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    companion_files: Vec<String>,
}

/// Model metadata for TOML output.
#[derive(Serialize)]
struct RawOutputModel {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// Determine whether the output should use structured format.
fn resolve_use_structured(output: &str, model_format: Option<crate::cli::ModelFormat>) -> bool {
    use crate::cli::ModelFormat;
    match model_format {
        Some(ModelFormat::Structured) => true,
        Some(ModelFormat::Flat) => false,
        // Default to structured (v2) unless output path ends in .toml (case-insensitive)
        None => {
            let p = Path::new(output);
            if p.is_dir() {
                return true;
            }
            match p.extension().and_then(|e| e.to_str()) {
                Some(ext) => !ext.eq_ignore_ascii_case("toml"),
                None => true,
            }
        }
    }
}

/// Compute the directory where asset files (dictionaries, companions) should be written.
/// For structured format the model directory itself is the root; for flat the parent of the schema file.
fn resolve_asset_dir(output: &str, use_structured: bool) -> PathBuf {
    if use_structured {
        PathBuf::from(output)
    } else {
        Path::new(output)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    }
}

/// Write a DataModel to disk in either flat or structured format.
fn write_model_output(
    data_model: &crate::core::DataModel,
    output: &str,
    use_structured: bool,
    header_comment: &str,
) -> Result<()> {
    if use_structured {
        let output_path = Path::new(output);
        // Clean stale table files from a previous run so the reader doesn't pick them up
        let tables_dir = output_path.join("tables");
        if tables_dir.is_dir() {
            std::fs::remove_dir_all(&tables_dir).with_context(|| {
                format!(
                    "failed to clean stale tables directory: {}",
                    tables_dir.display()
                )
            })?;
        }
        crate::model::writer::write_model_directory(data_model, output_path)
            .with_context(|| format!("failed to write structured model to {output}"))?;
    } else {
        let raw = RawOutputSchema {
            blueprint_version: data_model.blueprint_version.clone(),
            model: RawOutputModel {
                name: data_model.name.clone(),
                description: data_model.description.clone(),
            },
            entities: data_model.entities.clone(),
            relationships: data_model.relationships.clone(),
            correlations: data_model.correlations.clone(),
            personas: data_model.personas.clone(),
            actor_relationships: data_model.actor_relationships.clone(),
            companion_files: data_model.companion_files.clone(),
        };
        let schema_text =
            toml::to_string_pretty(&raw).context("failed to serialize schema to TOML")?;
        let full_output = format!("{header_comment}{schema_text}");
        std::fs::write(output, &full_output)
            .with_context(|| format!("failed to write output to {output}"))?;
    }
    Ok(())
}

/// Run the learn command: ingest data, analyse, and write a knit blueprint.
///
/// `source` is a path to a single data file or a directory of files.
/// `output` is the path where the generated schema will be written.
/// `sample` limits each entity to at most N rows for faster profiling.
/// `state_path` enables incremental mode when provided.
/// `finalize` emits schema from existing state without processing new data.
/// `strict` errors on duplicate source paths (default: warn).
#[allow(clippy::too_many_arguments)]
pub fn run(
    source: Option<&str>,
    output: &str,
    sample: Option<usize>,
    state_path: Option<&str>,
    finalize: bool,
    strict: bool,
    entities: &[String],
    actors_opts: Option<&ActorsOpts>,
    model_format: Option<crate::cli::ModelFormat>,
    review: bool,
    cli: &crate::cli::Cli,
) -> Result<()> {
    // Validate argument combinations
    if finalize && state_path.is_none() {
        anyhow::bail!("--finalize requires --state");
    }
    if source.is_none() && !finalize {
        anyhow::bail!("source path is required unless --finalize is specified");
    }

    if let Some(0) = sample {
        anyhow::bail!("--sample must be at least 1");
    }

    // --review requires the decision logger to be active
    if review && cli.decision_report.is_none() {
        // Auto-enable the decision logger for review mode
        let logger = crate::decision::DecisionLogger::new();
        crate::decision::set_global_logger(logger);
    }

    // Build entity filter set (empty = all)
    let entity_filter: HashSet<String> = entities.iter().cloned().collect();

    // Route: incremental mode if --state is provided
    if let Some(state_file) = state_path {
        if actors_opts.is_some() {
            anyhow::bail!("--actors is not supported with --state (incremental mode)");
        }
        return run_incremental(
            source,
            output,
            sample,
            state_file,
            finalize,
            strict,
            &entity_filter,
            model_format,
            review,
            cli,
        );
    }

    // Batch mode (original behavior)
    let source = source.ok_or_else(|| anyhow::anyhow!("source path is required in batch mode"))?;
    run_batch(
        source,
        output,
        sample,
        &entity_filter,
        actors_opts,
        model_format,
        review,
        cli,
    )
}

/// Batch mode: load all data, profile, fit, emit blueprint (original behavior).
#[allow(clippy::too_many_arguments)] // Keeps the batch path aligned with CLI-derived inputs.
fn run_batch(
    source: &str,
    output: &str,
    sample: Option<usize>,
    entity_filter: &HashSet<String>,
    actors_opts: Option<&ActorsOpts>,
    model_format: Option<crate::cli::ModelFormat>,
    review: bool,
    cli: &crate::cli::Cli,
) -> Result<()> {
    let _learn_span = info_span!("learn", source = %source).entered();
    let source_path = Path::new(source);
    anyhow::ensure!(
        source_path.exists(),
        "source path does not exist: {}",
        source
    );

    if !cli.quiet {
        eprintln!("{} Analysing {}", "learn:".green().bold(), source.cyan());
        if let Some(n) = sample {
            eprintln!("  {} sampling first {} rows per entity", "→".dimmed(), n);
        }
    }

    // 1. Ingest
    let ingest_start = std::time::Instant::now();
    let source_label = source_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(source)
        .to_string();
    let ingest_pb = if !cli.quiet {
        let style =
            ProgressStyle::with_template("{prefix:>16.cyan} {spinner:.green} {msg} ({elapsed})")
                .expect("hardcoded spinner template");
        let pb = ProgressBar::new_spinner();
        pb.set_style(style);
        pb.set_prefix("ingesting");
        pb.set_message(source_label);
        pb.enable_steady_tick(std::time::Duration::from_millis(100));
        Some(pb)
    } else {
        None
    };

    let tables = match ingest_source(source_path, sample) {
        Ok(t) => {
            if let Some(pb) = ingest_pb {
                pb.finish_and_clear();
            }
            t
        }
        Err(e) => {
            if let Some(pb) = ingest_pb {
                pb.abandon_with_message("failed");
            }
            return Err(e).with_context(|| format!("failed to ingest data from {source}"));
        }
    };

    if tables.is_empty() {
        anyhow::bail!("no supported data files found in {source}");
    }

    // Apply entity filter if specified
    let tables: Vec<_> = if entity_filter.is_empty() {
        tables
    } else {
        // Validate that all requested entity names exist in the ingested tables
        let available: HashSet<&str> = tables.iter().map(|t| t.entity.as_str()).collect();
        let mut unknown: Vec<&str> = entity_filter
            .iter()
            .filter(|name| !available.contains(name.as_str()))
            .map(|s| s.as_str())
            .collect();
        if !unknown.is_empty() {
            unknown.sort();
            let mut avail_sorted: Vec<&str> = available.into_iter().collect();
            avail_sorted.sort();
            anyhow::bail!(
                "unknown --entity name(s): {}; available: {}",
                unknown.join(", "),
                avail_sorted.join(", ")
            );
        }
        tables
            .into_iter()
            .filter(|t| entity_filter.contains(&t.entity))
            .collect()
    };

    let total_rows: u64 = tables
        .iter()
        .map(|t| t.batches.iter().map(|b| b.num_rows() as u64).sum::<u64>())
        .sum();
    let elapsed = ingest_start.elapsed();

    if !cli.quiet {
        eprintln!(
            "  {} loaded {} table(s), {} row(s) in {:.1}s",
            "→".dimmed(),
            tables.len(),
            format_count(total_rows),
            elapsed.as_secs_f64(),
        );
    }

    info!(tables = tables.len(), "ingestion complete");

    // 2. Per-table analysis
    let mut table_analyses: Vec<TableAnalysis> = Vec::new();
    let mut table_profiles_for_rels: Vec<TableProfile> = Vec::new();
    let mut total_columns: usize = 0;

    let pb = if !cli.quiet {
        let style = ProgressStyle::with_template(
            "{prefix:>16.cyan} [{bar:30.green/dim}] {pos}/{len} tables — {msg} ({eta})",
        )
        .expect("hardcoded progress bar template")
        .progress_chars("━╸─");
        let pb = ProgressBar::new(tables.len() as u64);
        pb.set_style(style);
        pb.set_prefix("profiling");
        pb.set_message("");
        Some(pb)
    } else {
        None
    };

    for table in &tables {
        if let Some(ref pb) = pb {
            pb.set_message(table.entity.clone());
        }
        let (analysis, rel_profile) = analyse_table(table)
            .with_context(|| format!("failed to analyse table {}", table.entity))?;
        total_columns += analysis.columns.len();
        table_analyses.push(analysis);
        table_profiles_for_rels.push(rel_profile);
        if let Some(ref pb) = pb {
            pb.inc(1);
        }
    }
    if let Some(pb) = pb {
        pb.finish_and_clear();
    }

    // 3. Cross-table relationship detection
    if !cli.quiet {
        eprintln!(
            "  {} detecting relationships across {} table(s)",
            "→".dimmed(),
            tables.len()
        );
    }
    let relationships = detect_relationships(&table_profiles_for_rels);
    info!(count = relationships.len(), "relationships detected");

    // Attach relationships to corresponding tables
    for rel in &relationships {
        if let Some(ta) = table_analyses.iter_mut().find(|t| t.name == rel.from_table) {
            ta.relationships.push(rel.clone());
        }
    }

    // 4. Per-table correlation detection
    let corr_pb = if !cli.quiet && tables.len() > 1 {
        let style = ProgressStyle::with_template(
            "{prefix:>16.cyan} [{bar:30.green/dim}] {pos}/{len} tables ({eta})",
        )
        .expect("hardcoded progress bar template")
        .progress_chars("━╸─");
        let pb = ProgressBar::new(tables.len() as u64);
        pb.set_style(style);
        pb.set_prefix("correlations");
        Some(pb)
    } else {
        None
    };

    for (i, table) in tables.iter().enumerate() {
        let profiles = compute_profiles(&table.batches)
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context("correlation profiling failed")?;
        let correlations = detect_correlations(&profiles, &table.batches);
        if !correlations.is_empty() {
            debug!(
                table = %table.entity,
                count = correlations.len(),
                "correlations found"
            );
        }
        table_analyses[i].correlations = correlations;

        // Detect categorical→numeric conditional distributions
        let cond_dists = detect_conditional_distributions(&profiles, &table.batches);
        if !cond_dists.is_empty() {
            debug!(
                table = %table.entity,
                count = cond_dists.len(),
                "conditional distributions found"
            );
        }
        table_analyses[i].conditional_distributions = cond_dists;

        // Detect co-occurring string tuple columns
        let tuple_groups = detect_tuple_columns(&profiles, &table.batches);
        if !tuple_groups.is_empty() {
            debug!(
                table = %table.entity,
                count = tuple_groups.len(),
                "tuple column groups found"
            );
        }
        table_analyses[i].tuple_groups = tuple_groups;

        // Detect geographic coordinate columns and merge into tuple groups
        detect_geographic_tuples(&mut table_analyses[i], &table.batches);

        // Detect derived text columns (e.g., full_name = first + " " + last)
        let derived_text = detect_derived_text_columns(&profiles, &table.batches);
        if !derived_text.is_empty() {
            debug!(
                table = %table.entity,
                count = derived_text.len(),
                "derived text columns found"
            );
        }
        table_analyses[i].derived_text_columns = derived_text;

        // Detect panel/grid structures (cross-product column pairs)
        let grids = detect_grid_structures(&profiles, &table.batches);
        if !grids.is_empty() {
            debug!(
                table = %table.entity,
                count = grids.len(),
                "grid structures found"
            );
        }
        table_analyses[i].grid_structures = grids;
        if let Some(ref pb) = corr_pb {
            pb.inc(1);
        }
    }
    if let Some(pb) = corr_pb {
        pb.finish_and_clear();
    }

    // 4b. Behavioral analysis (when --actors is enabled)
    let mut behavioral_stats = BehavioralStats::default();
    if let Some(opts) = actors_opts {
        if !cli.quiet {
            eprintln!("  {} running behavioral analysis", "→".dimmed(),);
        }
        behavioral_stats =
            run_behavioral_pipeline(&tables, &mut table_analyses, &relationships, opts, cli)?;
    }

    // 5. Assemble data model
    let model_name = Path::new(output)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("learned")
        .to_string();
    let mut data_model = assemble_data_model(&model_name, &table_analyses);

    // 5a2. Annotate entities with scaling dimension metadata
    {
        let analysis = crate::scale::analyze::analyze(&data_model);
        crate::learn::annotate::annotate_dimensions(&mut data_model, &analysis);
    }

    // 5b. Extract dictionaries for high-cardinality string columns
    let use_structured = resolve_use_structured(output, model_format);
    let output_dir = resolve_asset_dir(output, use_structured);
    // Ensure the output directory exists before writing dictionary files
    std::fs::create_dir_all(&output_dir)
        .with_context(|| format!("failed to create output dir: {}", output_dir.display()))?;
    let dict_count = extract_dictionaries(&mut data_model, &tables, &output_dir, cli.quiet)?;

    // 5b-tuple. Extract tuple dictionaries for co-occurring column groups
    let tuple_count =
        extract_tuple_dictionaries(&mut data_model, &table_analyses, &output_dir)?;
    if tuple_count > 0 && !cli.quiet {
        eprintln!("  Extracted {tuple_count} tuple dictionaries");
    }

    // 5b-fullrow. Extract full-row dictionaries for small categorical tables
    let fullrow_count =
        extract_full_row_dictionaries(&mut data_model, &tables, &output_dir, cli.quiet)?;
    if fullrow_count > 0 && !cli.quiet {
        eprintln!("  Extracted {fullrow_count} full-row dictionary table(s)");
    }

    // 5b2. Copy companion schema dictionary files (if any)
    let _companion_dict_count =
        copy_companion_dictionaries(&table_analyses, &output_dir, cli.quiet)?;

    // 5b3. Copy all non-data companion files (schema.json, etc.) from source
    //      to sit alongside the Learned blueprint, preserving relative paths.
    if source_path.is_dir() {
        let companion_count =
            copy_companion_files(source_path, &output_dir, &mut data_model, cli.quiet)?;
        if companion_count > 0 && !cli.quiet {
            eprintln!(
                "  {} copied {} companion file(s)",
                "→".dimmed(),
                companion_count,
            );
        }
    }

    // 5c. Validate the assembled schema and warn about issues
    let validation_errors = crate::blueprint::validate(&data_model);
    if !validation_errors.is_empty() && !cli.quiet {
        eprintln!(
            "\n{} Learned blueprint has {} validation warning(s):",
            "⚠".yellow().bold(),
            validation_errors.len(),
        );
        for err in &validation_errors {
            eprintln!("  • {}", err);
        }
        eprintln!(
            "  The schema was written but may need manual adjustment \
             before generating data.\n"
        );
    }

    // 5d. Interactive review of low-confidence decisions
    if review {
        let _decisions = if let Some(logger) = crate::decision::global_logger() {
            logger.low_confidence_decisions()
        } else {
            vec![]
        };
        // Include medium-confidence decisions with alternatives too
        let all_decisions = if let Some(logger) = crate::decision::global_logger() {
            logger.all_decisions()
        } else {
            vec![]
        };
        let overrides =
            crate::learn::review::interactive_review(&mut data_model, &all_decisions, cli.quiet);
        if overrides > 0 && !cli.quiet {
            eprintln!();
        }
    }

    // 6. Write output (flat TOML or structured directory)
    // Set blueprint version based on output format
    if use_structured {
        data_model.blueprint_version = "2.0".to_string();
    } else {
        data_model.blueprint_version = "1.0".to_string();
    }
    write_model_output(
        &data_model,
        output,
        use_structured,
        "# Auto-generated knit blueprint\n# Generated by knit learn\n\n",
    )?;

    // 7. Summary
    let total_rels = relationships.len();
    let total_corrs: usize = table_analyses.iter().map(|t| t.correlations.len()).sum();

    if cli.json {
        let mut summary = serde_json::json!({
            "event": "complete",
            "output": output,
            "tables": table_analyses.len(),
            "columns": total_columns,
            "relationships": total_rels,
            "correlations": total_corrs,
            "dictionaries": dict_count,
        });
        if behavioral_stats.actors_profiled > 0 {
            summary["actors"] = serde_json::json!(behavioral_stats.actors_profiled);
            summary["personas"] = serde_json::json!(behavioral_stats.personas_discovered);
            summary["actor_graphs"] = serde_json::json!(behavioral_stats.graphs_discovered);
            summary["actor_namespaces"] = serde_json::json!(behavioral_stats.actor_namespaces);
        }
        println!("{}", summary);
    } else if !cli.quiet {
        let mut line = format!(
            "\n{} Wrote {} — {} table(s), {} column(s), {} relationship(s), {} correlation(s), {} dictionary(ies)",
            "✓".green().bold(),
            output.cyan(),
            table_analyses.len(),
            total_columns,
            total_rels,
            total_corrs,
            dict_count,
        );
        if behavioral_stats.actors_profiled > 0 {
            line.push_str(&format!(
                ", {} namespace(s), {} actor(s), {} persona(s), {} graph(s)",
                behavioral_stats.actor_namespaces,
                behavioral_stats.actors_profiled,
                behavioral_stats.personas_discovered,
                behavioral_stats.graphs_discovered,
            ));
        }
        eprintln!("{}", line);
    }

    Ok(())
}

/// Incremental mode: load/create state, ingest data, optionally finalize.
#[allow(clippy::too_many_arguments)]
fn run_incremental(
    source: Option<&str>,
    output: &str,
    sample: Option<usize>,
    state_file: &str,
    finalize: bool,
    strict: bool,
    entity_filter: &HashSet<String>,
    model_format: Option<crate::cli::ModelFormat>,
    review: bool,
    cli: &crate::cli::Cli,
) -> Result<()> {
    use crate::learn::incremental::ingest_batches_to_state;
    use crate::learn::streaming::LearnState;

    let state_path = Path::new(state_file);

    // Load or create state
    let mut state = match LearnState::load(state_path)
        .map_err(|e| anyhow::anyhow!("failed to load state: {e}"))?
    {
        Some(s) => s,
        None => {
            if finalize {
                anyhow::bail!("state file does not exist: {state_file}");
            }
            LearnState::new(42)
        }
    };

    // Ingest new data if source is provided
    if let Some(source) = source {
        let source_path = Path::new(source);
        anyhow::ensure!(
            source_path.exists(),
            "source path does not exist: {}",
            source
        );

        if !cli.quiet {
            eprintln!(
                "{} Ingesting {} (incremental)",
                "learn:".green().bold(),
                source.cyan()
            );
        }

        let ingest_start = std::time::Instant::now();
        let source_label = source_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(source)
            .to_string();
        let ingest_pb = if !cli.quiet {
            let style = ProgressStyle::with_template(
                "{prefix:>16.cyan} {spinner:.green} {msg} ({elapsed})",
            )
            .expect("hardcoded spinner template");
            let pb = ProgressBar::new_spinner();
            pb.set_style(style);
            pb.set_prefix("ingesting");
            pb.set_message(source_label);
            pb.enable_steady_tick(std::time::Duration::from_millis(100));
            Some(pb)
        } else {
            None
        };

        let tables = match ingest_source(source_path, sample) {
            Ok(t) => {
                if let Some(pb) = ingest_pb {
                    pb.finish_and_clear();
                }
                t
            }
            Err(e) => {
                if let Some(pb) = ingest_pb {
                    pb.abandon_with_message("failed");
                }
                return Err(e).with_context(|| format!("failed to ingest data from {source}"));
            }
        };

        if tables.is_empty() {
            anyhow::bail!("no supported data files found in {source}");
        }

        // Apply entity filter if specified
        let tables: Vec<_> = if entity_filter.is_empty() {
            tables
        } else {
            // Validate that all requested entity names exist in the ingested tables
            let available: HashSet<&str> = tables.iter().map(|t| t.entity.as_str()).collect();
            let mut unknown: Vec<&str> = entity_filter
                .iter()
                .filter(|name| !available.contains(name.as_str()))
                .map(|s| s.as_str())
                .collect();
            if !unknown.is_empty() {
                unknown.sort();
                let mut avail_sorted: Vec<&str> = available.into_iter().collect();
                avail_sorted.sort();
                anyhow::bail!(
                    "unknown --entity name(s): {}; available: {}",
                    unknown.join(", "),
                    avail_sorted.join(", ")
                );
            }
            tables
                .into_iter()
                .filter(|t| entity_filter.contains(&t.entity))
                .collect()
        };

        let total_rows: u64 = tables
            .iter()
            .map(|t| t.batches.iter().map(|b| b.num_rows() as u64).sum::<u64>())
            .sum();
        let elapsed = ingest_start.elapsed();

        if !cli.quiet {
            eprintln!(
                "  {} loaded {} table(s), {} row(s) in {:.1}s",
                "→".dimmed(),
                tables.len(),
                format_count(total_rows),
                elapsed.as_secs_f64(),
            );
        }

        let state_pb = if !cli.quiet && tables.len() > 1 {
            let style = ProgressStyle::with_template(
                "{prefix:>16.cyan} [{bar:30.green/dim}] {pos}/{len} tables — {msg}",
            )
            .expect("hardcoded progress bar template")
            .progress_chars("━╸─");
            let pb = ProgressBar::new(tables.len() as u64);
            pb.set_style(style);
            pb.set_prefix("processing");
            Some(pb)
        } else {
            None
        };

        for table in &tables {
            if let Some(ref pb) = state_pb {
                pb.set_message(table.entity.clone());
            }
            let source_id = format!("{}:{}", source, table.entity);
            let is_dup =
                ingest_batches_to_state(&mut state, &table.entity, &table.batches, &source_id);
            if is_dup {
                let msg = format!("duplicate source: {source_id}");
                if strict {
                    anyhow::bail!("{msg}");
                } else if !cli.quiet {
                    if let Some(ref pb) = state_pb {
                        pb.suspend(|| eprintln!("  {} {}", "⚠".yellow(), msg));
                    } else {
                        eprintln!("  {} {}", "⚠".yellow(), msg);
                    }
                }
            }
            // Update per-table correlation evidence from this chunk's batches
            crate::learn::incremental::update_correlation_evidence(
                &mut state,
                &table.entity,
                &table.batches,
            );
            if let Some(ref pb) = state_pb {
                pb.inc(1);
            }
        }
        if let Some(pb) = state_pb {
            pb.finish_and_clear();
        }

        // Update relationship evidence after ingesting all tables from this source
        crate::learn::incremental::update_relationship_evidence(&mut state);

        // Save updated state
        state
            .save(state_path)
            .map_err(|e| anyhow::anyhow!("failed to save state: {e}"))?;

        if !cli.quiet {
            eprintln!(
                "  {} State saved to {} ({} table(s), {} chunk(s))",
                "→".dimmed(),
                state_file.cyan(),
                state.tables.len(),
                state.chunks.len(),
            );
        }
    }

    // Finalize if --finalize flag is set
    if finalize || source.is_some() {
        // Only emit blueprint if -o was explicitly provided or --finalize
        // Since output has a default, we always emit when finalize is set
        // or when source is provided with --state (update + finalize in one pass)
        if finalize {
            emit_blueprint_from_state(&state, output, entity_filter, model_format, review, cli)?;
        }
    }

    Ok(())
}

/// Emit a schema from the accumulated state.
fn emit_blueprint_from_state(
    state: &crate::learn::streaming::LearnState,
    output: &str,
    entity_filter: &HashSet<String>,
    model_format: Option<crate::cli::ModelFormat>,
    review: bool,
    cli: &crate::cli::Cli,
) -> Result<()> {
    use crate::learn::incremental::finalize_state;
    use crate::learn::schema_assembly::assemble_data_model;

    if !cli.quiet {
        eprintln!(
            "  {} Finalizing schema from state ({} table(s))",
            "→".dimmed(),
            state.tables.len(),
        );
    }

    let (mut table_analyses, finalized_rels) = finalize_state(state);

    // Apply entity filter to finalized tables
    if !entity_filter.is_empty() {
        table_analyses.retain(|t| entity_filter.contains(&t.name));
    }

    // Attach incrementally-detected relationships to their source TableAnalysis
    // so assemble_data_model can use them for generator/type decisions.
    for rel in &finalized_rels {
        use crate::learn::relationships::{RelationshipCandidate, RelationshipKind};
        let kind = match rel.kind {
            crate::learn::streaming::RelKind::OneToOne => RelationshipKind::OneToOne,
            crate::learn::streaming::RelKind::OneToMany => RelationshipKind::OneToMany,
        };
        let candidate = RelationshipCandidate {
            from_table: rel.from_table.clone(),
            from_column: rel.from_column.clone(),
            to_table: rel.to_table.clone(),
            to_column: rel.to_column.clone(),
            kind,
            confidence: rel.confidence,
            is_self_ref: rel.is_self_ref,
        };
        // Attach to the source (from) table
        if let Some(ta) = table_analyses.iter_mut().find(|t| t.name == rel.from_table) {
            ta.relationships.push(candidate);
        }
    }

    let model_name = Path::new(output)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("learned")
        .to_string();
    let mut data_model = assemble_data_model(&model_name, &table_analyses);

    // Annotate entities with scaling dimension metadata
    {
        let analysis = crate::scale::analyze::analyze(&data_model);
        crate::learn::annotate::annotate_dimensions(&mut data_model, &analysis);
    }

    // Extract dictionaries from reservoir samples for high-cardinality string columns
    let use_structured = resolve_use_structured(output, model_format);
    let output_dir = resolve_asset_dir(output, use_structured);
    let dict_count = extract_dictionaries_from_state(&mut data_model, state, &output_dir)?;

    if !cli.quiet && dict_count > 0 {
        eprintln!(
            "  {} Extracted {} dictionary file(s) from reservoir samples",
            "📖".dimmed(),
            dict_count,
        );
    }

    // Validate the assembled schema and warn about issues
    let validation_errors = crate::blueprint::validate(&data_model);
    if !validation_errors.is_empty() && !cli.quiet {
        eprintln!(
            "\n{} Learned blueprint has {} validation warning(s):",
            "⚠".yellow().bold(),
            validation_errors.len(),
        );
        for err in &validation_errors {
            eprintln!("  • {}", err);
        }
        eprintln!(
            "  The schema was written but may need manual adjustment \
             before generating data.\n"
        );
    }

    // Interactive review of low-confidence decisions
    if review {
        let all_decisions = if let Some(logger) = crate::decision::global_logger() {
            logger.all_decisions()
        } else {
            vec![]
        };
        let overrides =
            crate::learn::review::interactive_review(&mut data_model, &all_decisions, cli.quiet);
        if overrides > 0 && !cli.quiet {
            eprintln!();
        }
    }

    // Write output (flat or structured)
    // Set blueprint version based on output format
    if use_structured {
        data_model.blueprint_version = "2.0".to_string();
    } else {
        data_model.blueprint_version = "1.0".to_string();
    }
    write_model_output(
        &data_model,
        output,
        use_structured,
        "# Auto-generated knit blueprint\n# Generated by knit learn (incremental)\n\n",
    )?;

    if !cli.quiet {
        eprintln!(
            "\n{} Wrote {} — {} table(s), {} column(s)",
            "✓".green().bold(),
            output.cyan(),
            table_analyses.len(),
            table_analyses
                .iter()
                .map(|t| t.columns.len())
                .sum::<usize>(),
        );
    }

    Ok(())
}

/// Ingest data from a file or directory into per-table batches.
fn ingest_source(path: &Path, max_rows: Option<usize>) -> Result<Vec<IngestionResult>> {
    if path.is_dir() {
        info!(dir = %path.display(), "ingesting directory");
        ingest::ingest_directory_with_limit(path, max_rows).map_err(|e| anyhow::anyhow!("{e}"))
    } else {
        info!(file = %path.display(), "ingesting single file");
        let entity = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("data")
            .to_string();
        let batches =
            ingest::read_auto_with_limit(path, max_rows).map_err(|e| anyhow::anyhow!("{e}"))?;
        let schema = batches
            .first()
            .map(|b| b.schema())
            .ok_or_else(|| anyhow::anyhow!("file produced no data"))?;
        let source_format = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());
        Ok(vec![IngestionResult {
            entity,
            schema,
            batches,
            companion: None,
            companion_path: None,
            source_layout: None,
            partition_by: None,
            partition_values: Vec::new(),
            source_format,
        }])
    }
}

/// Analyse a single table: profile, fit distributions, detect patterns.
///
/// Returns a `TableAnalysis` for schema assembly and a `TableProfile` for
/// cross-table relationship detection.
fn analyse_table(table: &IngestionResult) -> Result<(TableAnalysis, TableProfile)> {
    let _span = info_span!("table", name = %table.entity).entered();
    let profiles = compute_profiles(&table.batches)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("profiling failed")?;

    let combined =
        concat_batches(&table.schema, &table.batches).context("failed to concatenate batches")?;
    let row_count = combined.num_rows() as u64;

    let mut col_analyses = Vec::with_capacity(profiles.len());
    let mut rel_columns = Vec::new();

    for profile in &profiles {
        let col_analysis = analyse_column(profile, &combined);

        // Enrich distribution-fit decisions with entity/column context
        // (fit_distribution logs the decision without table/column context)
        if col_analysis.distribution.is_some()
            && let Some(logger) = crate::decision::global_logger()
        {
            logger.set_last_context(
                crate::decision::DecisionKind::DistributionFit,
                &table.entity,
                &profile.name,
            );
        }

        col_analyses.push(col_analysis);

        // Build RelColumn for relationship detection
        let distinct_values = extract_distinct_string_values(&combined, &profile.name);
        rel_columns.push(RelColumn {
            name: profile.name.clone(),
            is_primary_key: is_likely_primary_key(profile, row_count),
            distinct_values,
            row_count: profile.count - profile.null_count,
            distinct_count: profile.distinct_count.unwrap_or(0),
        });
    }

    // If multiple PK candidates detected, pick only one (best match to table name).
    let pk_count = rel_columns.iter().filter(|c| c.is_primary_key).count();
    if pk_count > 1 {
        let best_idx = pick_best_pk(&rel_columns, &table.entity);
        for (i, rc) in rel_columns.iter_mut().enumerate() {
            if rc.is_primary_key && i != best_idx {
                rc.is_primary_key = false;
            }
        }
    }

    // Mark detected PKs in column analyses
    for (ca, rc) in col_analyses.iter_mut().zip(rel_columns.iter()) {
        ca.is_primary_key = rc.is_primary_key;
    }

    let mut analysis = TableAnalysis::new(table.entity.clone(), col_analyses, row_count);
    // Attach companion schema if available
    analysis.companion = table.companion.clone();
    analysis.companion_path = table.companion_path.clone();
    analysis.source_layout = table.source_layout.clone();
    analysis.partition_by = table.partition_by.clone();
    analysis.partition_values = table.partition_values.clone();
    analysis.source_format = table.source_format.clone();

    // Detect sort order from source data
    if let Some(sort_order) = detect_sort_order(&combined, &analysis.columns) {
        debug!(
            table = %table.entity,
            column = %sort_order.column,
            direction = ?sort_order.direction,
            "detected sort order"
        );
        analysis.sort_order = Some(sort_order.clone());

        // For sorted numeric columns with high uniqueness, replace distribution
        // with a linear time-series (effectively a sequence). This ensures columns
        // like "year" (1880..2023) generate monotonically increasing values.
        if let Some(col) = analysis.columns.iter_mut().find(|c| c.name == sort_order.column) {
            if col.temporal_pattern.is_none() && col.distribution.is_some() {
                let values = extract_f64_column(&combined, &col.name);
                let n_valid = values.len() as f64;
                let distinct_count = {
                    let mut uniq = std::collections::HashSet::new();
                    for &(_, v) in &values {
                        uniq.insert(v.to_bits());
                    }
                    uniq.len()
                };
                // If >=80% of values are unique, convert to sequence-like time_series
                if n_valid > 1.0 && distinct_count as f64 / n_valid >= 0.8 && values.len() >= 10 {
                    let min_val = values.iter().map(|(_, y)| *y).fold(f64::INFINITY, f64::min);
                    let max_val = values.iter().map(|(_, y)| *y).fold(f64::NEG_INFINITY, f64::max);
                    let step = (max_val - min_val) / (n_valid - 1.0);

                    // Honor descending direction: negate slope so output counts down
                    let is_desc = matches!(sort_order.direction, crate::core::SortDirection::Desc);
                    let (baseline, slope) = if is_desc {
                        (max_val, -step)
                    } else {
                        (min_val, step)
                    };

                    use crate::core::TimeSeriesComponent;
                    col.time_series_spec = Some(crate::core::GeneratorSpec::TimeSeries {
                        baseline,
                        components: vec![TimeSeriesComponent::Trend { slope, degree: 1 }],
                        min: Some(min_val),
                        max: Some(max_val),
                        timestamp_field: None,
                    });
                    tracing::debug!(
                        column = %sort_order.column,
                        start = baseline,
                        step = slope,
                        descending = is_desc,
                        "sort column converted to sequence time-series"
                    );
                }
            }
        }
    }

    // Detect time-series trends in numeric columns relative to a sorted temporal column
    detect_time_series_trends(&combined, &mut analysis);

    // Detect arithmetic relationships between numeric columns (e.g., total = men + women)
    detect_arithmetic_relations(&combined, &mut analysis);

    // Detect temporal ordering (e.g., dropoff > pickup) and emit duration-based derivation
    detect_temporal_ordering(&combined, &mut analysis);

    // Detect cross-column constraints from source data
    let constraints = detect_column_constraints(&combined, &analysis.columns);
    if !constraints.is_empty() {
        debug!(
            table = %table.entity,
            count = constraints.len(),
            "detected column constraints"
        );
        // Store on analysis for propagation to Entity
        analysis.constraints = constraints;
    }

    let rel_profile = TableProfile {
        name: table.entity.clone(),
        columns: rel_columns,
    };

    debug!(
        table = %table.entity,
        cols = profiles.len(),
        rows = row_count,
        "table analysis complete"
    );

    Ok((analysis, rel_profile))
}

/// Detect if the source data is sorted by any column.
///
/// Checks numeric, string, and temporal columns for monotonic ordering.
/// Returns the first column found to be sorted (preferring temporal columns).
fn detect_sort_order(
    batch: &RecordBatch,
    columns: &[ColumnAnalysis],
) -> Option<crate::core::SortOrder> {
    use crate::core::SortOrder;

    if batch.num_rows() < 3 {
        return None;
    }

    // Prefer temporal columns, then numeric, then string
    let mut candidates: Vec<(usize, &str)> = Vec::new();
    for (i, col) in columns.iter().enumerate() {
        if col.temporal_pattern.is_some() {
            // Temporal columns get priority — insert at front
            candidates.insert(0, (i, &col.name));
        } else if col.distribution.is_some() || col.categorical_weights.is_some() {
            candidates.push((i, &col.name));
        }
    }

    for (col_idx, col_name) in &candidates {
        if let Some(arr) = batch.column_by_name(col_name) {
            if let Some(dir) = check_column_sorted(arr.as_ref()) {
                let _ = col_idx; // suppress unused warning
                return Some(SortOrder {
                    column: col_name.to_string(),
                    direction: dir,
                });
            }
        }
    }

    None
}

/// Check if an Arrow array is monotonically sorted.
///
/// Returns `Some(SortDirection)` if values are non-decreasing or non-increasing,
/// `None` otherwise. Null values are skipped.
fn check_column_sorted(arr: &dyn arrow::array::Array) -> Option<crate::core::SortDirection> {
    use arrow::array;

    match arr.data_type() {
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => {
            check_sorted_i64(arr)
        }
        DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
            check_sorted_uint(arr)
        }
        DataType::Float32 => {
            let a = arr.as_any().downcast_ref::<array::Float32Array>()?;
            check_sorted_float(a.iter().filter_map(|v| v.map(|x| x as f64)))
        }
        DataType::Float64 => {
            let a = arr.as_any().downcast_ref::<array::Float64Array>()?;
            check_sorted_float(a.iter().filter_map(|v| v.map(|x| x)))
        }
        DataType::Utf8 => {
            let a = arr.as_any().downcast_ref::<array::StringArray>()?;
            let vals: Vec<&str> = (0..a.len()).filter(|&i| !a.is_null(i)).map(|i| a.value(i)).collect();
            check_sorted_ord(&vals)
        }
        DataType::LargeUtf8 => {
            let a = arr.as_any().downcast_ref::<array::LargeStringArray>()?;
            let vals: Vec<&str> = (0..a.len()).filter(|&i| !a.is_null(i)).map(|i| a.value(i)).collect();
            check_sorted_ord(&vals)
        }
        DataType::Timestamp(TimeUnit::Second, _) => {
            let a = arr.as_any().downcast_ref::<array::TimestampSecondArray>()?;
            let vals: Vec<i64> = a.iter().filter_map(|v| v).collect();
            check_sorted_ord(&vals)
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let a = arr.as_any().downcast_ref::<array::TimestampMillisecondArray>()?;
            let vals: Vec<i64> = a.iter().filter_map(|v| v).collect();
            check_sorted_ord(&vals)
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            let a = arr.as_any().downcast_ref::<array::TimestampMicrosecondArray>()?;
            let vals: Vec<i64> = a.iter().filter_map(|v| v).collect();
            check_sorted_ord(&vals)
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            let a = arr.as_any().downcast_ref::<array::TimestampNanosecondArray>()?;
            let vals: Vec<i64> = a.iter().filter_map(|v| v).collect();
            check_sorted_ord(&vals)
        }
        DataType::Date32 => {
            let a = arr.as_any().downcast_ref::<array::Date32Array>()?;
            let vals: Vec<i32> = a.iter().filter_map(|v| v).collect();
            check_sorted_ord(&vals)
        }
        _ => None,
    }
}

/// Check if i64-coercible integer values are sorted.
fn check_sorted_i64(arr: &dyn arrow::array::Array) -> Option<crate::core::SortDirection> {
    use arrow::array;
    // Extract as i64 regardless of width
    let vals: Vec<i64> = match arr.data_type() {
        DataType::Int8 => arr.as_any().downcast_ref::<array::Int8Array>()?.iter().filter_map(|v| v.map(i64::from)).collect(),
        DataType::Int16 => arr.as_any().downcast_ref::<array::Int16Array>()?.iter().filter_map(|v| v.map(i64::from)).collect(),
        DataType::Int32 => arr.as_any().downcast_ref::<array::Int32Array>()?.iter().filter_map(|v| v.map(i64::from)).collect(),
        DataType::Int64 => arr.as_any().downcast_ref::<array::Int64Array>()?.iter().filter_map(|v| v).collect(),
        _ => return None,
    };
    check_sorted_ord(&vals)
}

/// Check if unsigned integer values are sorted (handles u64 without lossy cast).
fn check_sorted_uint(arr: &dyn arrow::array::Array) -> Option<crate::core::SortDirection> {
    use arrow::array;
    let vals: Vec<u64> = match arr.data_type() {
        DataType::UInt8 => arr.as_any().downcast_ref::<array::UInt8Array>()?.iter().filter_map(|v| v.map(u64::from)).collect(),
        DataType::UInt16 => arr.as_any().downcast_ref::<array::UInt16Array>()?.iter().filter_map(|v| v.map(u64::from)).collect(),
        DataType::UInt32 => arr.as_any().downcast_ref::<array::UInt32Array>()?.iter().filter_map(|v| v.map(u64::from)).collect(),
        DataType::UInt64 => arr.as_any().downcast_ref::<array::UInt64Array>()?.iter().filter_map(|v| v).collect(),
        _ => return None,
    };
    check_sorted_ord(&vals)
}

/// Check if an ordered sequence is sorted (ascending or descending).
fn check_sorted_ord<T: Ord>(vals: &[T]) -> Option<crate::core::SortDirection> {
    use crate::core::SortDirection;
    if vals.len() < 3 {
        return None;
    }
    let is_asc = vals.windows(2).all(|w| w[0] <= w[1]);
    if is_asc {
        return Some(SortDirection::Asc);
    }
    let is_desc = vals.windows(2).all(|w| w[0] >= w[1]);
    if is_desc {
        return Some(SortDirection::Desc);
    }
    None
}

/// Check if float values are sorted.
///
/// Uses simple `<=`/`>=` comparisons (NaN values are filtered out upstream).
fn check_sorted_float(iter: impl Iterator<Item = f64>) -> Option<crate::core::SortDirection> {
    use crate::core::SortDirection;
    let vals: Vec<f64> = iter.collect();
    if vals.len() < 3 {
        return None;
    }
    let is_asc = vals.windows(2).all(|w| w[0] <= w[1]);
    if is_asc {
        return Some(SortDirection::Asc);
    }
    let is_desc = vals.windows(2).all(|w| w[0] >= w[1]);
    if is_desc {
        return Some(SortDirection::Desc);
    }
    None
}

/// Detect linear time-series trends in numeric columns.
///
/// When a table has a sorted temporal column (date sequence), this function
/// checks each numeric column for a significant linear trend. If the R² of
/// a simple linear regression (value ~ row_index) exceeds a threshold, the
/// column is annotated with a `TimeSeries` generator spec containing
/// `Trend { slope }` and `Noise { std_dev }` components fitted from the data.
///
/// This replaces the default `Distribution` generator for trending columns,
/// preserving temporal structure in the generated output.
fn detect_time_series_trends(
    batch: &RecordBatch,
    analysis: &mut crate::learn::schema_assembly::TableAnalysis,
) {
    use crate::core::{GeneratorSpec, TimeSeriesComponent};

    // Need a sorted column as the time axis (temporal or monotonic integer)
    let sort_col_name = match &analysis.sort_order {
        Some(so) => so.column.clone(),
        None => return,
    };

    // Accept sorted temporal columns OR sorted integer/float columns as time axis.
    // Integer sequences like "year" (1880, 1881, ...) are valid time indices.
    let sort_col_is_valid_axis = analysis.columns.iter().any(|c| {
        c.name == sort_col_name
            && (c.temporal_pattern.is_some() || c.distribution.is_some())
    });
    if !sort_col_is_valid_axis {
        return;
    }

    // Need at least 10 rows for meaningful trend detection
    let n = batch.num_rows();
    if n < 10 {
        return;
    }

    // Minimum R² threshold for trend significance
    const R2_THRESHOLD: f64 = 0.3;

    // For each numeric column, fit linear regression: value = baseline + slope * t
    // Also try log-linear fit for exponential growth (e.g., stock prices)
    for col in &mut analysis.columns {
        // Skip the sort column itself, non-numeric columns, PKs, and columns already assigned
        if col.name == sort_col_name
            || col.is_primary_key
            || col.temporal_pattern.is_some()
            || col.time_series_spec.is_some()
        {
            continue;
        }

        // Only process numeric columns (those with a distribution fit)
        if col.distribution.is_none() {
            continue;
        }

        // Extract numeric values from the batch
        let values = extract_f64_column(batch, &col.name);
        if values.len() < 10 {
            continue;
        }

        // Compute linear regression: y = baseline + slope * x
        // where x is the original row index (preserving position for NULL gaps)
        let n_f = values.len() as f64;
        let mean_x: f64 = values.iter().map(|(i, _)| *i as f64).sum::<f64>() / n_f;
        let mean_y: f64 = values.iter().map(|(_, y)| *y).sum::<f64>() / n_f;

        let mut ss_xy = 0.0;
        let mut ss_xx = 0.0;
        let mut ss_yy = 0.0;
        for &(i, y) in &values {
            let x = i as f64;
            let dx = x - mean_x;
            let dy = y - mean_y;
            ss_xy += dx * dy;
            ss_xx += dx * dx;
            ss_yy += dy * dy;
        }

        if ss_xx < f64::EPSILON || ss_yy < f64::EPSILON {
            continue;
        }

        let slope = ss_xy / ss_xx;
        let r_squared = (ss_xy * ss_xy) / (ss_xx * ss_yy);

        // If linear R² is below threshold, try log-linear fit for exponential growth
        let (final_slope, final_r2) = if r_squared >= R2_THRESHOLD {
            (slope, r_squared)
        } else {
            // Linear R² too low — skip this column.
            // Note: exponential growth patterns (e.g., stock prices) would need a dedicated
            // exponential generator component, which doesn't exist yet. Emitting a linear
            // approximation for exponential data produces poor results.
            continue;
        };

        // Compute noise as standard deviation of first-differences minus the trend.
        // This captures step-to-step volatility rather than total variance around
        // the regression line, which can be orders of magnitude larger for strongly
        // trending series (e.g., stock prices where residual std >> per-step slope).
        let noise_std = if values.len() >= 3 {
            let diffs: Vec<f64> = values
                .windows(2)
                .map(|w| {
                    let dt = (w[1].0 as f64) - (w[0].0 as f64);
                    let dy = w[1].1 - w[0].1;
                    // Remove the expected trend contribution from the difference
                    dy - final_slope * dt
                })
                .collect();
            let n_diffs = diffs.len() as f64;
            let mean_diff: f64 = diffs.iter().sum::<f64>() / n_diffs;
            let var: f64 = diffs.iter().map(|d| (d - mean_diff).powi(2)).sum::<f64>()
                / (n_diffs - 1.0).max(1.0);
            var.sqrt()
        } else {
            // Fallback to residual std for very short series
            let residual_var: f64 = values
                .iter()
                .map(|&(i, y)| {
                    let predicted = mean_y + final_slope * (i as f64 - mean_x);
                    (y - predicted).powi(2)
                })
                .sum::<f64>() / (n_f - 2.0);
            residual_var.sqrt()
        };

        // Use the first observed value as baseline. The regression intercept
        // (mean_y - slope * mean_x) can be far from the actual starting value for
        // non-linear data (e.g., exponential stock prices), causing impossible
        // negative baselines. The first value ensures generation starts correctly.
        let baseline = values[0].1;

        let mut components = vec![
            TimeSeriesComponent::Trend { slope: final_slope, degree: 1 },
        ];
        if noise_std > f64::EPSILON {
            components.push(TimeSeriesComponent::Noise { std_dev: noise_std });
        }

        tracing::debug!(
            column = %col.name,
            slope = final_slope,
            r_squared = final_r2,
            noise_std = noise_std,
            baseline = baseline,
            "detected time-series trend"
        );

        // Don't set min/max for trended time series — the regression intercept
        // (baseline) can be well below the observed minimum, causing early values
        // to be clamped to a constant. Let the trend + noise model generate values
        // naturally.
        col.time_series_spec = Some(GeneratorSpec::TimeSeries {
            baseline,
            components,
            min: None,
            max: None,
            timestamp_field: None,
        });
    }
}

/// Detect temporal ordering between datetime column pairs.
///
/// When two datetime columns always satisfy `col_b >= col_a` (within tolerance),
/// emit a derived spec for col_b as `col_a + duration` where duration statistics
/// are fitted from the observed differences.
fn detect_temporal_ordering(
    batch: &RecordBatch,
    analysis: &mut crate::learn::schema_assembly::TableAnalysis,
) {
    use crate::core::GeneratorSpec;

    let n = batch.num_rows();
    if n < 10 {
        return;
    }

    // Find all temporal columns
    let temporal_cols: Vec<String> = analysis
        .columns
        .iter()
        .filter(|c| c.temporal_pattern.is_some())
        .map(|c| c.name.clone())
        .collect();

    if temporal_cols.len() < 2 {
        return;
    }

    // For each pair of temporal columns, check ordering
    for i in 0..temporal_cols.len() {
        for j in (i + 1)..temporal_cols.len() {
            let col_a = &temporal_cols[i];
            let col_b = &temporal_cols[j];

            // Extract timestamps as epoch seconds
            let ts_a = extract_timestamps_as_epoch(batch, col_a);
            let ts_b = extract_timestamps_as_epoch(batch, col_b);

            if ts_a.len() < 10 || ts_b.len() < 10 || ts_a.len() != ts_b.len() {
                continue;
            }

            // Check: is col_b >= col_a for >=95% of rows?
            let mut total = 0u64;
            let mut ordered = 0u64;
            let mut diffs: Vec<f64> = Vec::new();
            for k in 0..ts_a.len() {
                if let (Some(a), Some(b)) = (ts_a[k], ts_b[k]) {
                    total += 1;
                    if b >= a {
                        ordered += 1;
                        diffs.push(b - a);
                    }
                }
            }

            if total < 10 {
                continue;
            }

            let (base_col, derived_col) =
                if ordered as f64 / total as f64 >= 0.95 {
                    (col_a.clone(), col_b.clone())
                } else {
                    // Try reverse: col_a >= col_b
                    let mut rev_ordered = 0u64;
                    let mut rev_diffs: Vec<f64> = Vec::new();
                    for k in 0..ts_a.len() {
                        if let (Some(a), Some(b)) = (ts_a[k], ts_b[k]) {
                            if a >= b {
                                rev_ordered += 1;
                                rev_diffs.push(a - b);
                            }
                        }
                    }
                    if rev_ordered as f64 / total as f64 >= 0.95 {
                        diffs = rev_diffs;
                        (col_b.clone(), col_a.clone())
                    } else {
                        continue;
                    }
                };

            if diffs.is_empty() {
                continue;
            }

            // Compute duration statistics (mean and std in seconds)
            let n_diffs = diffs.len() as f64;
            let mean_dur = diffs.iter().sum::<f64>() / n_diffs;
            let var_dur = diffs.iter().map(|d| (d - mean_dur).powi(2)).sum::<f64>() / n_diffs;
            let std_dur = var_dur.sqrt();

            // Emit Relative temporal spec: derived_col = base_col + Normal(mean, std)
            use crate::core::{RelativeOffset, Value as CoreValue};
            let offset = RelativeOffset::Simple(CoreValue::Float(mean_dur));

            tracing::debug!(
                base = %base_col,
                derived = %derived_col,
                mean_duration_secs = mean_dur,
                std_duration_secs = std_dur,
                "detected temporal ordering constraint"
            );

            if let Some(col) = analysis.columns.iter_mut().find(|c| c.name == derived_col) {
                col.derived_spec = Some(GeneratorSpec::Relative {
                    anchor: base_col.clone(),
                    offset,
                });
                // NOTE: we do NOT clear temporal_pattern here — it is still needed by
                // infer_data_type() to preserve the Datetime/Date type. The schema_assembly
                // build_generator() function now checks derived_spec BEFORE temporal_pattern,
                // so the Relative generator will be used for generation while the column
                // retains its correct temporal data type.
            }
        }
    }
}

/// Extract timestamps from a column as epoch seconds (f64).
fn extract_timestamps_as_epoch(
    batch: &RecordBatch,
    col_name: &str,
) -> Vec<Option<f64>> {
    use arrow::array::*;
    use arrow::datatypes::DataType;

    let Some(arr) = batch.column_by_name(col_name) else {
        return Vec::new();
    };
    let n = arr.len();
    let mut result = Vec::with_capacity(n);

    match arr.data_type() {
        DataType::Timestamp(unit, _) => {
            use arrow::datatypes::TimeUnit;
            let multiplier = match unit {
                TimeUnit::Second => 1.0,
                TimeUnit::Millisecond => 0.001,
                TimeUnit::Microsecond => 0.000_001,
                TimeUnit::Nanosecond => 0.000_000_001,
            };
            if let Some(a) = arr.as_any().downcast_ref::<TimestampNanosecondArray>() {
                for i in 0..n {
                    result.push(if a.is_null(i) { None } else { Some(a.value(i) as f64 * 0.000_000_001) });
                }
            } else if let Some(a) = arr.as_any().downcast_ref::<TimestampMicrosecondArray>() {
                for i in 0..n {
                    result.push(if a.is_null(i) { None } else { Some(a.value(i) as f64 * 0.000_001) });
                }
            } else if let Some(a) = arr.as_any().downcast_ref::<TimestampMillisecondArray>() {
                for i in 0..n {
                    result.push(if a.is_null(i) { None } else { Some(a.value(i) as f64 * 0.001) });
                }
            } else if let Some(a) = arr.as_any().downcast_ref::<TimestampSecondArray>() {
                for i in 0..n {
                    result.push(if a.is_null(i) { None } else { Some(a.value(i) as f64) });
                }
            } else {
                // Generic timestamp: use the multiplier
                let _ = multiplier;
                return Vec::new();
            }
        }
        DataType::Utf8 => {
            // Parse string timestamps
            let a = arr.as_any().downcast_ref::<StringArray>();
            let Some(a) = a else { return Vec::new() };
            for i in 0..n {
                if a.is_null(i) {
                    result.push(None);
                } else {
                    let s = a.value(i);
                    // Try common datetime formats
                    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S") {
                        result.push(Some(dt.and_utc().timestamp() as f64));
                    } else if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S") {
                        result.push(Some(dt.and_utc().timestamp() as f64));
                    } else if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
                        result.push(Some(
                            d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() as f64,
                        ));
                    } else {
                        result.push(None);
                    }
                }
            }
        }
        _ => return Vec::new(),
    }

    result
}

/// Extract a column's values as (row_index, f64) from a RecordBatch.
///
/// Handles Int8..Int64, UInt8..UInt64, Float32, Float64, and Utf8 (parsed).
/// Preserves original row indices so regression aligns with the generator's
/// use of global row index as the time variable.
/// Returns an empty vec if the column is not found or not numeric.
fn extract_f64_column(batch: &RecordBatch, col_name: &str) -> Vec<(usize, f64)> {
    use arrow::array::*;
    use arrow::datatypes::DataType as ArrowDT;

    let idx = match batch.schema().index_of(col_name) {
        Ok(i) => i,
        Err(_) => return Vec::new(),
    };
    let arr = batch.column(idx);
    let n = arr.len();
    let mut out = Vec::with_capacity(n);

    macro_rules! extract_typed {
        ($arr:expr, $ty:ty) => {{
            let a = $arr.as_any().downcast_ref::<$ty>().unwrap();
            for i in 0..n {
                if !a.is_null(i) {
                    let val = a.value(i) as f64;
                    if val.is_finite() {
                        out.push((i, val));
                    }
                }
            }
        }};
    }

    match arr.data_type() {
        ArrowDT::Float64 => extract_typed!(arr, Float64Array),
        ArrowDT::Float32 => extract_typed!(arr, Float32Array),
        ArrowDT::Int64 => extract_typed!(arr, Int64Array),
        ArrowDT::Int32 => extract_typed!(arr, Int32Array),
        ArrowDT::Int16 => extract_typed!(arr, Int16Array),
        ArrowDT::Int8 => extract_typed!(arr, Int8Array),
        ArrowDT::UInt64 => extract_typed!(arr, UInt64Array),
        ArrowDT::UInt32 => extract_typed!(arr, UInt32Array),
        ArrowDT::UInt16 => extract_typed!(arr, UInt16Array),
        ArrowDT::UInt8 => extract_typed!(arr, UInt8Array),
        ArrowDT::Utf8 => {
            let a = arr.as_any().downcast_ref::<StringArray>().unwrap();
            for i in 0..n {
                if !a.is_null(i) {
                    if let Ok(v) = a.value(i).parse::<f64>() {
                        if v.is_finite() {
                            out.push((i, v));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    out
}

/// Detect geographic coordinate columns (lat/lon pairs) and merge them into
/// existing tuple groups or create new groups.
///
/// Detection is purely data-driven:
/// - Latitude: float column where all non-null values are in [-90, 90]
/// - Longitude: float column where all non-null values are in [-180, 180]
///   (and at least some values outside [-90, 90] to disambiguate from lat)
///
/// When a lat/lon pair is found, it is attached to the nearest existing tuple
/// group (if any group shares adjacent string columns), or forms a new group
/// with neighboring string columns.
fn detect_geographic_tuples(
    analysis: &mut TableAnalysis,
    batches: &[arrow::record_batch::RecordBatch],
) {
    use arrow::array::Array;

    if batches.is_empty() {
        return;
    }
    let schema = batches[0].schema();
    let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
    if total_rows < 5 {
        return;
    }

    // Find candidate lat/lon columns by value range across ALL batches
    let mut lat_candidates: Vec<usize> = Vec::new();
    let mut lon_candidates: Vec<usize> = Vec::new();

    for (col_idx, field) in schema.fields().iter().enumerate() {
        if !field.data_type().is_numeric() {
            continue;
        }
        // Collect values across all batches
        let mut non_null: Vec<f64> = Vec::new();
        for batch in batches {
            let n = batch.num_rows();
            if let Some(vals) = extract_nullable_f64_values(batch.column(col_idx).as_ref(), n) {
                non_null.extend(vals.iter().filter_map(|v| *v));
            }
        }
        if non_null.len() < 5 {
            continue;
        }

        let min_val = non_null.iter().cloned().fold(f64::INFINITY, f64::min);
        let max_val = non_null.iter().cloned().fold(f64::NEG_INFINITY, f64::max);

        // Latitude: all values in [-90, 90]
        if min_val >= -90.0 && max_val <= 90.0 {
            lat_candidates.push(col_idx);
        }
        // Longitude: all values in [-180, 180] with some outside [-90, 90]
        if min_val >= -180.0 && max_val <= 180.0 && (min_val < -90.0 || max_val > 90.0) {
            lon_candidates.push(col_idx);
        }
    }

    // If no explicit lon candidates, try pairing lat candidates that are adjacent
    // (handles European/African data where all lon values are in [-90, 90]).
    // Use column name hints or pair by adjacency when exactly two candidates exist.
    if lon_candidates.is_empty() && lat_candidates.len() >= 2 {
        // Look for adjacent lat candidate pairs — the second is likely longitude
        let mut paired = false;
        for i in 0..lat_candidates.len() {
            for j in (i + 1)..lat_candidates.len() {
                let a = lat_candidates[i];
                let b = lat_candidates[j];
                let distance = (a as isize - b as isize).unsigned_abs();
                if distance <= 2 {
                    // Treat the first as lat, second as lon (by schema order)
                    lon_candidates.push(b);
                    paired = true;
                    break;
                }
            }
            if paired {
                break;
            }
        }
        // Remove the lon candidate from lat_candidates
        for l in &lon_candidates {
            lat_candidates.retain(|x| x != l);
        }
    }

    // Match lat/lon pairs: prefer adjacent columns in schema order
    let mut used_lats: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut used_lons: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut geo_pairs: Vec<(usize, usize)> = Vec::new(); // (lat_idx, lon_idx)

    for &lat_idx in &lat_candidates {
        // Find nearest lon that's adjacent (within 2 positions)
        let best_lon = lon_candidates
            .iter()
            .filter(|&&l| !used_lons.contains(&l))
            .min_by_key(|&&l| (l as isize - lat_idx as isize).unsigned_abs());
        if let Some(&lon_idx) = best_lon {
            let distance = (lon_idx as isize - lat_idx as isize).unsigned_abs();
            if distance <= 2 {
                used_lats.insert(lat_idx);
                used_lons.insert(lon_idx);
                geo_pairs.push((lat_idx, lon_idx));
            }
        }
    }

    if geo_pairs.is_empty() {
        return;
    }

    // For each geo pair, find associated string columns and build/extend tuple groups
    let col_names: Vec<String> = schema.fields().iter().map(|f| f.name().clone()).collect();

    for (lat_idx, lon_idx) in &geo_pairs {
        let lat_name = &col_names[*lat_idx];
        let lon_name = &col_names[*lon_idx];

        // Check if an existing tuple group already contains related columns
        // (look for groups whose columns are adjacent to this lat/lon pair)
        let geo_min = (*lat_idx).min(*lon_idx);
        let geo_max = (*lat_idx).max(*lon_idx);

        let existing_group = analysis.tuple_groups.iter_mut().find(|g| {
            g.columns.iter().any(|col_name| {
                if let Some(idx) = col_names.iter().position(|n| n == col_name) {
                    // Column is within 3 positions of the lat/lon pair
                    idx <= geo_max + 3 && idx + 3 >= geo_min
                } else {
                    false
                }
            })
        });

        if let Some(group) = existing_group {
            // Extend existing group with lat/lon columns
            if !group.columns.contains(lat_name) {
                group.columns.push(lat_name.clone());
            }
            if !group.columns.contains(lon_name) {
                group.columns.push(lon_name.clone());
            }
            // Re-extract tuples to include the new columns (across all batches)
            let all_cols: Vec<usize> = group
                .columns
                .iter()
                .filter_map(|name| col_names.iter().position(|n| n == name))
                .collect();
            let mut tuples: Vec<Vec<String>> = Vec::new();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for batch in batches {
                for row in 0..batch.num_rows() {
                    let mut values: Vec<String> = Vec::new();
                    let mut any_null = false;
                    for &ci in &all_cols {
                        let col = batch.column(ci);
                        if col.is_null(row) {
                            any_null = true;
                            break;
                        }
                        values.push(array_value_to_string(col.as_ref(), row));
                    }
                    if any_null {
                        continue;
                    }
                    let key = values.join("\t");
                    if seen.insert(key) {
                        tuples.push(values);
                    }
                }
            }
            group.tuples = tuples;
        } else {
            // Create a new tuple group with nearby columns + lat/lon
            let mut group_cols: Vec<String> = Vec::new();
            let total_columns = col_names.len();

            // Find string columns within 3 positions (or all string columns if schema is small)
            let search_range = if total_columns <= 8 {
                0..total_columns
            } else {
                geo_min.saturating_sub(3)..(geo_max + 4).min(total_columns)
            };

            for idx in search_range {
                if idx == *lat_idx || idx == *lon_idx {
                    continue;
                }
                let field = &schema.fields()[idx];
                // Only include string columns in the tuple (numeric columns
                // use their own generators; Dictionary/TupleLookup produces strings).
                if *field.data_type() == arrow::datatypes::DataType::Utf8 {
                    group_cols.push(col_names[idx].clone());
                }
            }

            // Only create a group if there's at least one string column
            if group_cols.is_empty() {
                continue;
            }

            // Put highest-cardinality string column first (as primary)
            group_cols.sort_by(|a, b| {
                let card_a = analysis
                    .columns
                    .iter()
                    .find(|c| c.name == *a)
                    .and_then(|c| c.stats.as_ref())
                    .and_then(|s| s.distinct_count)
                    .unwrap_or(0);
                let card_b = analysis
                    .columns
                    .iter()
                    .find(|c| c.name == *b)
                    .and_then(|c| c.stats.as_ref())
                    .and_then(|s| s.distinct_count)
                    .unwrap_or(0);
                card_b.cmp(&card_a)
            });

            // Add lat/lon after string columns
            group_cols.push(lat_name.clone());
            group_cols.push(lon_name.clone());

            // Extract unique tuples (across all batches)
            let all_cols: Vec<usize> = group_cols
                .iter()
                .filter_map(|name| col_names.iter().position(|n| n == name))
                .collect();
            let mut tuples: Vec<Vec<String>> = Vec::new();
            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            for batch in batches {
                for row in 0..batch.num_rows() {
                    let mut values: Vec<String> = Vec::new();
                    let mut any_null = false;
                    for &ci in &all_cols {
                        let col = batch.column(ci);
                        if col.is_null(row) {
                            any_null = true;
                            break;
                        }
                        values.push(array_value_to_string(col.as_ref(), row));
                    }
                    if any_null {
                        continue;
                    }
                    let key = values.join("\t");
                    if seen.insert(key) {
                        tuples.push(values);
                    }
                }
            }

            if tuples.len() >= 3 {
                tracing::debug!(
                    lat = %lat_name,
                    lon = %lon_name,
                    columns = ?group_cols,
                    tuples = tuples.len(),
                    "geographic tuple group created"
                );
                analysis.tuple_groups.push(
                    crate::learn::correlation::TupleGroup {
                        columns: group_cols,
                        tuples,
                    },
                );
            }
        }
    }
}

/// Convert an Arrow array value at a given row to a string representation.
fn array_value_to_string(arr: &dyn arrow::array::Array, row: usize) -> String {
    if arr.is_null(row) {
        return String::new();
    }
    match arr.data_type() {
        arrow::datatypes::DataType::Utf8 => {
            let sa = arr
                .as_any()
                .downcast_ref::<arrow::array::StringArray>()
                .unwrap();
            sa.value(row).to_string()
        }
        arrow::datatypes::DataType::Int64 => {
            let ia = arr
                .as_any()
                .downcast_ref::<arrow::array::Int64Array>()
                .unwrap();
            ia.value(row).to_string()
        }
        arrow::datatypes::DataType::Float64 => {
            let fa = arr
                .as_any()
                .downcast_ref::<arrow::array::Float64Array>()
                .unwrap();
            fa.value(row).to_string()
        }
        arrow::datatypes::DataType::Int32 => {
            let ia = arr
                .as_any()
                .downcast_ref::<arrow::array::Int32Array>()
                .unwrap();
            ia.value(row).to_string()
        }
        arrow::datatypes::DataType::Float32 => {
            let fa = arr
                .as_any()
                .downcast_ref::<arrow::array::Float32Array>()
                .unwrap();
            fa.value(row).to_string()
        }
        arrow::datatypes::DataType::UInt64 => {
            let ua = arr
                .as_any()
                .downcast_ref::<arrow::array::UInt64Array>()
                .unwrap();
            ua.value(row).to_string()
        }
        _ => format!("{:?}", arr.data_type()),
    }
}

/// Detect arithmetic relationships between numeric columns.
///
/// Tests all triples (target, a, b) of numeric columns to find cases where
/// `target ≈ a + b`, `target ≈ a - b`, `target ≈ a * b`, or `target ≈ a / b`
/// holds for ≥95% of non-null rows. When a relationship is found, sets
/// `derived_spec` on the target column to a `Derived` expression.
///
/// This captures patterns like `Total = Men + Women` or `ShareWomen = Women / Total`.
fn detect_arithmetic_relations(batch: &RecordBatch, analysis: &mut TableAnalysis) {
    use crate::core::GeneratorSpec;

    let n = batch.num_rows();
    if n < 10 {
        return;
    }

    // Collect numeric columns with their values (only non-null rows as full vectors for alignment)
    let mut numeric_data: Vec<(String, Vec<Option<f64>>)> = Vec::new();
    for col in &analysis.columns {
        if let Some(arr) = batch.column_by_name(&col.name) {
            if let Some(vals) = extract_nullable_f64_values(arr.as_ref(), n) {
                numeric_data.push((col.name.clone(), vals));
            }
        }
    }

    if numeric_data.len() < 3 {
        return;
    }

    // Track which columns have been assigned a derived expression
    let mut derived_columns: Vec<(String, String)> = Vec::new(); // (target_name, expr)

    // For each potential target column, test all pairs of source columns
    for target_idx in 0..numeric_data.len() {
        let (ref target_name, ref target_vals) = numeric_data[target_idx];

        // Skip columns with very few non-null values
        let non_null_count = target_vals.iter().filter(|v| v.is_some()).count();
        if non_null_count < 10 {
            continue;
        }

        let mut best_match: Option<(String, f64)> = None; // (expr, error_rate)

        for i in 0..numeric_data.len() {
            if i == target_idx {
                continue;
            }
            for j in 0..numeric_data.len() {
                if j == target_idx || j == i {
                    continue;
                }
                // Only test ordered pairs (i, j) for non-commutative ops
                let (ref name_a, ref vals_a) = numeric_data[i];
                let (ref name_b, ref vals_b) = numeric_data[j];

                // Test: target = a + b
                if i < j {
                    let error_rate =
                        compute_relation_error(target_vals, vals_a, vals_b, ArithOp::Add, n);
                    if error_rate < 0.05
                        && best_match.as_ref().map_or(true, |(_, e)| error_rate < *e)
                    {
                        best_match =
                            Some((format!("${{{name_a}}} + ${{{name_b}}}"), error_rate));
                    }
                }

                // Test: target = a - b
                {
                    let error_rate =
                        compute_relation_error(target_vals, vals_a, vals_b, ArithOp::Sub, n);
                    if error_rate < 0.05
                        && best_match.as_ref().map_or(true, |(_, e)| error_rate < *e)
                    {
                        best_match =
                            Some((format!("${{{name_a}}} - ${{{name_b}}}"), error_rate));
                    }
                }

                // Test: target = a * b
                if i < j {
                    let error_rate =
                        compute_relation_error(target_vals, vals_a, vals_b, ArithOp::Mul, n);
                    if error_rate < 0.05
                        && best_match.as_ref().map_or(true, |(_, e)| error_rate < *e)
                    {
                        best_match =
                            Some((format!("${{{name_a}}} * ${{{name_b}}}"), error_rate));
                    }
                }

                // Test: target = a / b
                {
                    let error_rate =
                        compute_relation_error(target_vals, vals_a, vals_b, ArithOp::Div, n);
                    if error_rate < 0.05
                        && best_match.as_ref().map_or(true, |(_, e)| error_rate < *e)
                    {
                        best_match =
                            Some((format!("${{{name_a}}} / ${{{name_b}}}"), error_rate));
                    }
                }
            }
        }

        if let Some((expr, _error_rate)) = best_match {
            debug!(
                target_col = %target_name,
                expr = %expr,
                "detected arithmetic relationship"
            );
            derived_columns.push((target_name.clone(), expr));
        }
    }

    // Second pass: detect multi-column sums (target ≈ a + b + c + ...).
    // For each column not already derived, check if it equals the sum of a subset
    // of other columns. Uses greedy addition: add columns that reduce the residual.
    for target_idx in 0..numeric_data.len() {
        let (ref target_name, ref target_vals) = numeric_data[target_idx];

        // Skip if already found via triple detection
        if derived_columns.iter().any(|(name, _)| name == target_name) {
            continue;
        }

        let non_null_count = target_vals.iter().filter(|v| v.is_some()).count();
        if non_null_count < 10 {
            continue;
        }

        // Only try multi-sum when there are enough other columns
        if numeric_data.len() < 4 {
            continue;
        }

        // Compute target mean to filter: target should be larger than most components
        let target_mean: f64 = target_vals.iter().filter_map(|v| *v).sum::<f64>()
            / non_null_count as f64;
        if target_mean.abs() < f64::EPSILON {
            continue;
        }

        // Collect candidate addend columns (those with smaller mean than target)
        let mut candidates: Vec<usize> = Vec::new();
        for i in 0..numeric_data.len() {
            if i == target_idx {
                continue;
            }
            let (_, ref vals) = numeric_data[i];
            let count = vals.iter().filter(|v| v.is_some()).count();
            if count < 10 {
                continue;
            }
            let mean: f64 = vals.iter().filter_map(|v| *v).sum::<f64>() / count as f64;
            // Only include columns with same sign and smaller magnitude
            if mean.signum() == target_mean.signum() && mean.abs() < target_mean.abs() * 0.95 {
                candidates.push(i);
            }
        }

        if candidates.len() < 3 {
            continue;
        }

        // Greedy: try summing ALL candidates and check if it matches
        let mut sum_vals: Vec<Option<f64>> = vec![Some(0.0); n];
        let mut used_cols: Vec<usize> = Vec::new();
        for &c_idx in &candidates {
            let (_, ref vals) = numeric_data[c_idx];
            let mut new_sum = sum_vals.clone();
            for i in 0..n {
                match (new_sum[i], vals[i]) {
                    (Some(s), Some(v)) => new_sum[i] = Some(s + v),
                    _ => new_sum[i] = None,
                }
            }
            sum_vals = new_sum;
            used_cols.push(c_idx);
        }

        // Check error rate of the full sum
        let mut checked = 0u64;
        let mut mismatches = 0u64;
        for i in 0..n {
            let (Some(t), Some(s)) = (target_vals[i], sum_vals[i]) else {
                continue;
            };
            if !t.is_finite() || !s.is_finite() {
                continue;
            }
            checked += 1;
            let denom = t.abs().max(s.abs()).max(1.0);
            let diff = (t - s).abs() / denom;
            if !diff.is_finite() || diff > 0.02 {
                mismatches += 1;
            }
        }

        if checked >= 10 && (mismatches as f64 / checked as f64) < 0.05 {
            // Build the sum expression
            let expr_parts: Vec<String> = used_cols
                .iter()
                .map(|&idx| format!("${{{}}}", numeric_data[idx].0))
                .collect();
            let expr = expr_parts.join(" + ");
            debug!(
                target_col = %target_name,
                expr = %expr,
                components = used_cols.len(),
                "detected multi-column sum"
            );
            derived_columns.push((target_name.clone(), expr));
        }
    }

    // Resolve circular dependencies by iteratively removing the lowest-priority
    // derived column from cycles until a valid DAG remains.
    // Priority: division > multiplication > addition > subtraction
    let valid_derived = resolve_derived_cycles(derived_columns);

    // Apply derived specs to the analysis columns
    for (target_name, expr) in valid_derived {
        if let Some(col) = analysis.columns.iter_mut().find(|c| c.name == target_name) {
            col.derived_spec = Some(GeneratorSpec::Derived { expr });
        }
    }
}

/// Arithmetic operation type for relationship testing.
#[derive(Clone, Copy)]
enum ArithOp {
    Add,
    Sub,
    Mul,
    Div,
}

/// Compute the fraction of rows where `target != a op b` (within tolerance).
///
/// Tolerance is relative: values match if `|target - expected| / max(1, |target|) < 0.01`.
fn compute_relation_error(
    target: &[Option<f64>],
    a: &[Option<f64>],
    b: &[Option<f64>],
    op: ArithOp,
    n: usize,
) -> f64 {
    let mut checked = 0u64;
    let mut mismatches = 0u64;

    for i in 0..n {
        let (Some(t), Some(va), Some(vb)) = (target[i], a[i], b[i]) else {
            continue;
        };

        // Skip non-finite source values
        if !t.is_finite() || !va.is_finite() || !vb.is_finite() {
            continue;
        }

        let expected = match op {
            ArithOp::Add => va + vb,
            ArithOp::Sub => va - vb,
            ArithOp::Mul => va * vb,
            ArithOp::Div => {
                if vb == 0.0 {
                    // Count zero-divisor rows as mismatches — the generated
                    // output would produce null for these rows.
                    checked += 1;
                    mismatches += 1;
                    continue;
                }
                va / vb
            }
        };

        // Guard against NaN/Inf results from overflow
        if !expected.is_finite() {
            checked += 1;
            mismatches += 1;
            continue;
        }

        checked += 1;
        let denom = t.abs().max(expected.abs()).max(1.0);
        let diff = (t - expected).abs() / denom;
        if !diff.is_finite() || diff > 0.01 {
            mismatches += 1;
        }
    }

    if checked < 10 {
        return 1.0; // Not enough data
    }

    mismatches as f64 / checked as f64
}

/// Extract f64 values from an Arrow array, preserving nulls as `None`.
///
/// Returns a Vec of length `n` with `Some(val)` for non-null numeric values
/// and `None` for nulls. Returns `None` if the array is not numeric.
fn extract_nullable_f64_values(
    arr: &dyn arrow::array::Array,
    n: usize,
) -> Option<Vec<Option<f64>>> {
    use arrow::array;
    use arrow::datatypes::DataType;

    macro_rules! extract_nullable {
        ($arr:expr, $ty:ty, $n:expr) => {{
            let a = $arr.as_any().downcast_ref::<$ty>()?;
            Some((0..$n).map(|i| if a.is_null(i) { None } else { Some(a.value(i) as f64) }).collect())
        }};
    }

    match arr.data_type() {
        DataType::Int8 => extract_nullable!(arr, array::Int8Array, n),
        DataType::Int16 => extract_nullable!(arr, array::Int16Array, n),
        DataType::Int32 => extract_nullable!(arr, array::Int32Array, n),
        DataType::Int64 => extract_nullable!(arr, array::Int64Array, n),
        DataType::UInt8 => extract_nullable!(arr, array::UInt8Array, n),
        DataType::UInt16 => extract_nullable!(arr, array::UInt16Array, n),
        DataType::UInt32 => extract_nullable!(arr, array::UInt32Array, n),
        DataType::UInt64 => extract_nullable!(arr, array::UInt64Array, n),
        DataType::Float32 => extract_nullable!(arr, array::Float32Array, n),
        DataType::Float64 => extract_nullable!(arr, array::Float64Array, n),
        _ => None,
    }
}

/// Extract field references from a derived expression (e.g., `${a} + ${b}` → `["a", "b"]`).
fn extract_expr_refs(expr: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let mut rest = expr;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        if let Some(end) = after.find('}') {
            refs.push(after[..end].to_string());
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
    refs
}

/// Resolve circular dependencies among detected derived columns.
///
/// Uses DFS cycle detection. When a cycle is found, removes the lowest-priority
/// edge (subtraction < addition < multiplication < division). Linear chains
/// (A derives from B, B derives from C) are valid and preserved.
fn resolve_derived_cycles(mut candidates: Vec<(String, String)>) -> Vec<(String, String)> {
    loop {
        // Build set of derived column names
        let derived_set: std::collections::HashSet<String> =
            candidates.iter().map(|(n, _)| n.clone()).collect();

        // Build dependency graph: target → list of derived sources it depends on
        let deps: std::collections::HashMap<String, Vec<String>> = candidates
            .iter()
            .map(|(target, expr)| {
                let sources = extract_expr_refs(expr);
                let derived_sources: Vec<String> = sources
                    .into_iter()
                    .filter(|s| derived_set.contains(s))
                    .collect();
                (target.clone(), derived_sources)
            })
            .collect();

        // Check if any column can reach itself (true cycle)
        let cycle_idx = candidates.iter().position(|(target, _)| {
            let mut seen = std::collections::HashSet::new();
            reaches_self_owned(target, target, &deps, &mut seen)
        });

        match cycle_idx {
            Some(_) => {
                // Among cycle participants, remove the one with lowest priority
                let mut cycle_participants: Vec<usize> = Vec::new();
                for (i, (target, _)) in candidates.iter().enumerate() {
                    let mut seen = std::collections::HashSet::new();
                    if reaches_self_owned(target, target, &deps, &mut seen) {
                        cycle_participants.push(i);
                    }
                }

                let worst = cycle_participants
                    .iter()
                    .copied()
                    .min_by_key(|&i| op_priority(&candidates[i].1))
                    .unwrap_or(0);

                candidates.remove(worst);
            }
            None => break,
        }
    }

    candidates
}

/// Check if `start` can reach itself via the dependency graph (owned strings).
fn reaches_self_owned(
    current: &str,
    start: &str,
    deps: &std::collections::HashMap<String, Vec<String>>,
    seen: &mut std::collections::HashSet<String>,
) -> bool {
    if let Some(neighbors) = deps.get(current) {
        for neighbor in neighbors {
            if neighbor == start {
                return true;
            }
            if seen.insert(neighbor.clone())
                && reaches_self_owned(neighbor, start, deps, seen)
            {
                return true;
            }
        }
    }
    false
}

/// Assign priority to an arithmetic expression for cycle resolution.
/// Higher priority = more likely to keep.
fn op_priority(expr: &str) -> u8 {
    if expr.contains(" / ") {
        3 // Division (ratio) — highest value
    } else if expr.contains(" * ") {
        2
    } else if expr.contains(" + ") {
        1
    } else {
        0 // Subtraction — lowest priority (redundant with addition)
    }
}

/// Detect cross-column constraints from source data.
///
/// Checks all pairs of numeric columns for ordering relationships (A ≤ B for
/// all rows) and emits `Constraint::Range` for each numeric column's observed
/// min/max bounds. Also detects OHLC-like patterns (open ≤ high, low ≤ close).
fn detect_column_constraints(
    batch: &RecordBatch,
    columns: &[ColumnAnalysis],
) -> Vec<crate::core::Constraint> {
    use crate::core::{Constraint, Value};

    if batch.num_rows() < 2 {
        return Vec::new();
    }

    let mut constraints = Vec::new();

    // Collect numeric column indices and their f64 values
    let mut numeric_cols: Vec<(&str, Vec<f64>)> = Vec::new();
    for col in columns {
        if let Some(arr) = batch.column_by_name(&col.name) {
            if let Some(vals) = extract_f64_values(arr.as_ref()) {
                if vals.len() >= 2 {
                    numeric_cols.push((&col.name, vals));
                }
            }
        }
    }

    // Emit Range constraints from observed min/max for each numeric column.
    // For time-series columns, use a soft floor (0 for non-negative data) instead
    // of tight [min, max] clamping which breaks trended series where the regression
    // baseline differs from the observed bounds.
    for (name, vals) in &numeric_cols {
        let is_time_series =
            columns.iter().any(|c| c.name == *name && c.time_series_spec.is_some());
        let min = vals.iter().copied().fold(f64::INFINITY, f64::min);
        let max = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if !min.is_finite() || !max.is_finite() || min >= max {
            continue;
        }
        if is_time_series {
            // Only emit a non-negativity floor when all observed values are >= 0.
            if min >= 0.0 {
                constraints.push(Constraint::Range {
                    field: name.to_string(),
                    min: Some(Value::Float(0.0)),
                    max: None,
                });
            }
        } else {
            constraints.push(Constraint::Range {
                field: name.to_string(),
                min: Some(Value::Float(min)),
                max: Some(Value::Float(max)),
            });
        }
    }

    // Detect pairwise ordering: A ≤ B for all rows
    for i in 0..numeric_cols.len() {
        for j in (i + 1)..numeric_cols.len() {
            let (name_a, vals_a) = &numeric_cols[i];
            let (name_b, vals_b) = &numeric_cols[j];
            let len = vals_a.len().min(vals_b.len());
            if len < 2 {
                continue;
            }

            // Check A ≤ B
            let a_le_b = (0..len).all(|k| vals_a[k] <= vals_b[k]);
            if a_le_b {
                constraints.push(Constraint::Check {
                    expr: format!("{} <= {}", name_a, name_b),
                });
                continue;
            }

            // Check B ≤ A
            let b_le_a = (0..len).all(|k| vals_b[k] <= vals_a[k]);
            if b_le_a {
                constraints.push(Constraint::Check {
                    expr: format!("{} <= {}", name_b, name_a),
                });
            }
        }
    }

    constraints
}

/// Extract f64 values from an Arrow array (numeric types only).
fn extract_f64_values(arr: &dyn arrow::array::Array) -> Option<Vec<f64>> {
    use arrow::array;
    use arrow::datatypes::DataType;

    match arr.data_type() {
        DataType::Int8 => Some(
            arr.as_any()
                .downcast_ref::<array::Int8Array>()?
                .iter()
                .filter_map(|v| v.map(f64::from))
                .collect(),
        ),
        DataType::Int16 => Some(
            arr.as_any()
                .downcast_ref::<array::Int16Array>()?
                .iter()
                .filter_map(|v| v.map(f64::from))
                .collect(),
        ),
        DataType::Int32 => Some(
            arr.as_any()
                .downcast_ref::<array::Int32Array>()?
                .iter()
                .filter_map(|v| v.map(f64::from))
                .collect(),
        ),
        DataType::Int64 => Some(
            arr.as_any()
                .downcast_ref::<array::Int64Array>()?
                .iter()
                .filter_map(|v| v.map(|x| x as f64))
                .collect(),
        ),
        DataType::UInt8 => Some(
            arr.as_any()
                .downcast_ref::<array::UInt8Array>()?
                .iter()
                .filter_map(|v| v.map(f64::from))
                .collect(),
        ),
        DataType::UInt16 => Some(
            arr.as_any()
                .downcast_ref::<array::UInt16Array>()?
                .iter()
                .filter_map(|v| v.map(f64::from))
                .collect(),
        ),
        DataType::UInt32 => Some(
            arr.as_any()
                .downcast_ref::<array::UInt32Array>()?
                .iter()
                .filter_map(|v| v.map(f64::from))
                .collect(),
        ),
        DataType::UInt64 => Some(
            arr.as_any()
                .downcast_ref::<array::UInt64Array>()?
                .iter()
                .filter_map(|v| v.map(|x| x as f64))
                .collect(),
        ),
        DataType::Float32 => Some(
            arr.as_any()
                .downcast_ref::<array::Float32Array>()?
                .iter()
                .filter_map(|v| v.map(f64::from))
                .collect(),
        ),
        DataType::Float64 => Some(
            arr.as_any()
                .downcast_ref::<array::Float64Array>()?
                .iter()
                .filter_map(|v| v)
                .collect(),
        ),
        _ => None,
    }
}

/// Analyse a single column: fit distributions, detect temporal patterns,
/// run type inference for string columns.
fn analyse_column(profile: &ColumnProfile, batch: &RecordBatch) -> ColumnAnalysis {
    // Short-circuit for always-null or always-empty-string columns.
    // These need no distribution fitting, pattern detection, or type inference.
    let effective_empty_rate = profile.null_rate + profile.empty_string_rate;
    if effective_empty_rate >= 1.0 {
        let mut ca = ColumnAnalysis::new(profile.name.clone(), profile.null_rate, 1.0);
        ca.empty_string_rate = profile.empty_string_rate;
        ca.source_arrow_type = Some(profile.data_type.clone());
        debug!(col = %profile.name, null_rate = profile.null_rate,
               empty_rate = profile.empty_string_rate, "always-null column detected");
        if let Some(logger) = crate::decision::global_logger() {
            logger
                .builder(crate::decision::DecisionKind::NullHandling)
                .phase("learn/analyse")
                .column(&*profile.name)
                .chosen("always-null")
                .reason(format!(
                    "null_rate={:.2} + empty_rate={:.2} >= 1.0, skipping analysis",
                    profile.null_rate, profile.empty_string_rate
                ))
                .confidence(crate::decision::Confidence::High)
                .record();
        }
        ca.stats = Some(build_column_stats(profile));
        ca.traits = Some(detect_field_traits(profile, &ca));
        return ca;
    }

    let mut distribution: Option<FitResult> = None;
    let mut temporal_pattern: Option<TemporalPatternSpec> = None;
    let mut categorical_weights: Option<Vec<(String, f64)>> = None;
    let mut confidence = 1.0;
    let mut is_integer_valued = false;
    let mut has_time_component = false;

    // Complex types (List, Map, Struct) → serialize to display strings, treat as categorical
    if is_complex_type(&profile.data_type) {
        let display_values = extract_complex_display_values(batch, &profile.name);
        let mut ca = ColumnAnalysis::new(profile.name.clone(), profile.null_rate, 0.8);
        ca.source_arrow_type = Some(profile.data_type.clone());
        if !display_values.is_empty() {
            let cat_fit = fit_categorical(&display_values);
            let mut weights: Vec<(String, f64)> = cat_fit.weights.into_iter().collect();
            weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            ca.categorical_weights = Some(weights);
            ca.inferred_type = Some(InferredType::Categorical);
        }
        // Always return here for complex types (even if all-null)
        ca.stats = Some(build_column_stats(profile));
        ca.traits = Some(detect_field_traits(profile, &ca));
        return ca;
    }

    // Boolean columns (Arrow auto-detected) → weighted OneOf
    if matches!(profile.data_type, DataType::Boolean) {
        let col_idx = batch.schema().index_of(&profile.name).ok();
        if let Some(idx) = col_idx {
            let arr = batch.column(idx);
            let bool_arr = arr.as_any().downcast_ref::<arrow::array::BooleanArray>();
            if let Some(ba) = bool_arr {
                let non_null_count = (0..ba.len()).filter(|&i| !ba.is_null(i)).count();
                let true_count = (0..ba.len())
                    .filter(|&i| !ba.is_null(i) && ba.value(i))
                    .count();
                let true_rate = if non_null_count > 0 {
                    true_count as f64 / non_null_count as f64
                } else {
                    0.5
                };
                let mut ca = ColumnAnalysis::new(profile.name.clone(), profile.null_rate, 1.0);
                ca.inferred_type = Some(InferredType::Boolean);
                let mut weights = vec![
                    ("true".to_string(), true_rate),
                    ("false".to_string(), 1.0 - true_rate),
                ];
                weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                ca.categorical_weights = Some(weights);
                ca.is_integer_valued = false;
                ca.stats = Some(build_column_stats(profile));
                ca.traits = Some(detect_field_traits(profile, &ca));
                return ca;
            }
        }
    }

    // Numeric columns → distribution fitting
    if profile.numeric.is_some() {
        let values = extract_numeric_values(batch, &profile.name);
        if !values.is_empty() {
            // Check if all values are integers
            is_integer_valued = values.iter().all(|v| v.fract() == 0.0);

            // For low-cardinality integer-valued numeric columns (≤20 distinct values),
            // prefer categorical to preserve exact source values (e.g., constant columns,
            // enum-like integers, status codes)
            if is_integer_valued {
                let distinct_vals: HashSet<i64> = values.iter().map(|v| *v as i64).collect();
                if distinct_vals.len() <= 20 {
                    let str_values: Vec<String> =
                        values.iter().map(|v| format!("{}", *v as i64)).collect();
                    let cat_fit = fit_categorical(&str_values);
                    let mut weights: Vec<(String, f64)> = cat_fit.weights.into_iter().collect();
                    weights
                        .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    categorical_weights = Some(weights);
                }
            }

            if categorical_weights.is_none() {
                distribution = fit_distribution(&values);
                if let Some(ref fit) = distribution {
                    confidence = (1.0 - fit.best.ks_stat).max(0.0);
                    debug!(
                        col = %profile.name,
                        dist = fit.best.distribution.name(),
                        ks = fit.best.ks_stat,
                        "distribution fitted"
                    );
                }
            }
        }
    }

    // Temporal columns → range capture and pattern detection
    let mut temporal_range: Option<(f64, f64)> = None;
    if profile.temporal.is_some() {
        let ts_values = extract_timestamp_seconds(batch, &profile.name);
        if !ts_values.is_empty() {
            temporal_range = Some((ts_values[0], ts_values[ts_values.len() - 1]));
        }
        if ts_values.len() >= 3 {
            temporal_pattern = detect_temporal_pattern(&ts_values);
            if let Some(ref spec) = temporal_pattern {
                confidence = spec.confidence;
                debug!(
                    col = %profile.name,
                    pattern = ?spec.pattern,
                    "temporal pattern detected"
                );
            }
        }
    }

    // String columns → type inference and categorical fitting
    let mut inferred_type: Option<InferredType> = None;
    let mut string_patterns: Vec<(StringPattern, f64)> = Vec::new();

    if profile.string.is_some() {
        let string_values = extract_string_values(batch, &profile.name);
        let refs: Vec<Option<&str>> = string_values.iter().map(|s| s.as_deref()).collect();

        if !refs.is_empty() {
            let inference = infer_type(&refs, 0.20);
            confidence = inference.confidence;
            string_patterns = inference.patterns.into_iter().collect();
            // Sort by match rate descending for deterministic generator selection
            string_patterns
                .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            match inference.inferred_type {
                InferredType::Categorical => {
                    let owned: Vec<String> =
                        refs.iter().filter_map(|s| s.map(String::from)).collect();
                    let cat_fit = fit_categorical(&owned);
                    let mut weights: Vec<(String, f64)> = cat_fit.weights.into_iter().collect();
                    weights
                        .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    categorical_weights = Some(weights);
                    inferred_type = Some(InferredType::Categorical);
                }
                InferredType::Integer | InferredType::Float => {
                    // Try parsing as numeric for distribution fitting
                    let nums: Vec<f64> = refs
                        .iter()
                        .filter_map(|s| s.and_then(|v| v.parse::<f64>().ok()))
                        .collect();
                    if !nums.is_empty() {
                        is_integer_valued =
                            matches!(inference.inferred_type, InferredType::Integer);
                        distribution = fit_distribution(&nums);
                    }
                    // For low-cardinality numeric strings, also capture categorical weights
                    // so the generator can prefer exact value reproduction over distribution
                    let owned: Vec<String> =
                        refs.iter().filter_map(|s| s.map(String::from)).collect();
                    let distinct: HashSet<&str> = owned.iter().map(|s| s.as_str()).collect();
                    if distinct.len() <= 50 {
                        let cat_fit = fit_categorical(&owned);
                        let mut weights: Vec<(String, f64)> = cat_fit.weights.into_iter().collect();
                        weights.sort_by(|a, b| {
                            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
                        });
                        categorical_weights = Some(weights);
                    }
                    inferred_type = Some(inference.inferred_type);
                }
                ref other => {
                    // For string-detected dates, check if values contain time info
                    if matches!(other, InferredType::Date(_)) {
                        let non_null_refs: Vec<&str> = refs
                            .iter()
                            .filter_map(|s| *s)
                            .filter(|v| !v.is_empty())
                            .collect();
                        let time_count = non_null_refs
                            .iter()
                            .filter(|v| v.contains('T') || (v.contains(' ') && v.len() > 10))
                            .count();
                        if time_count as f64 / non_null_refs.len().max(1) as f64 > 0.5 {
                            has_time_component = true;
                        }
                    }
                    inferred_type = Some(other.clone());
                }
            }

            // Catch-all: for any string column with low cardinality (≤50 distinct values),
            // capture categorical weights if not already set, so the generator can preserve
            // exact source values (covers UUID, hex, name patterns, etc.)
            if categorical_weights.is_none() {
                let owned: Vec<String> = refs.iter().filter_map(|s| s.map(String::from)).collect();
                let distinct: HashSet<&str> = owned.iter().map(|s| s.as_str()).collect();
                if distinct.len() <= 50 {
                    let cat_fit = fit_categorical(&owned);
                    let mut weights: Vec<(String, f64)> = cat_fit.weights.into_iter().collect();
                    weights
                        .sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    categorical_weights = Some(weights);
                }
            }
        }
    }

    let mut ca = ColumnAnalysis::new(profile.name.clone(), profile.null_rate, confidence);
    ca.empty_string_rate = profile.empty_string_rate;
    ca.distribution = distribution;
    ca.temporal_pattern = temporal_pattern;
    ca.categorical_weights = categorical_weights;
    ca.inferred_type = inferred_type;
    ca.string_patterns = string_patterns;
    ca.is_integer_valued = is_integer_valued;
    // Timestamp types have time-of-day; Date32/Date64 are date-only; string dates checked above
    ca.has_time_component =
        has_time_component || matches!(profile.data_type, DataType::Timestamp(_, _));
    ca.temporal_range = temporal_range;
    ca.source_arrow_type = Some(profile.data_type.clone());
    ca.max_decimal_places = profile.numeric.as_ref().and_then(|n| n.max_decimal_places);

    // Build column stats from the profile
    ca.stats = Some(build_column_stats(profile));

    // Detect qualitative traits
    ca.traits = Some(detect_field_traits(profile, &ca));

    ca
}

/// Build [`ColumnStats`] from a [`ColumnProfile`].
fn build_column_stats(profile: &ColumnProfile) -> crate::core::ColumnStats {
    use crate::core::{ColumnStats, StatsPercentiles};

    let mut stats = ColumnStats {
        distinct_count: profile.distinct_count,
        null_rate: Some(profile.null_rate),
        ..Default::default()
    };

    // Numeric stats
    if let Some(ref num) = profile.numeric {
        stats.min = Some(num.min);
        stats.max = Some(num.max);
        stats.mean = Some(num.mean);
        stats.std = Some(num.std_dev);
        stats.percentiles = Some(StatsPercentiles {
            p25: num.percentiles.p25,
            p50: num.percentiles.p50,
            p75: num.percentiles.p75,
            p95: num.percentiles.p95,
            p99: num.percentiles.p99,
        });
    }

    // String stats
    if let Some(ref str_prof) = profile.string {
        stats.min_length = Some(str_prof.min_length as u32);
        stats.max_length = Some(str_prof.max_length as u32);
        stats.avg_length = Some(str_prof.avg_length);
    }

    // Categorical top values (from categorical_weights if available in profile)
    // For batch learn, we extract top values from the cardinality tracker
    if let Some(ref str_prof) = profile.string
        && !str_prof.patterns.is_empty()
    {
        // Patterns are stored as (pattern, match_rate) — not the same as top_values.
        // We don't have top-k in batch profiling, so skip for now.
    }

    // Temporal stats
    if let Some(ref temp) = profile.temporal {
        stats.min_temporal = Some(temp.min.clone());
        stats.max_temporal = Some(temp.max.clone());
    }

    stats
}

/// Detect qualitative [`FieldTraits`] from a column's profile and analysis.
///
/// Heuristics:
/// - **semantic**: maps `InferredType` → human-readable label; falls back to
///   arrow type for natively-typed columns.
/// - **pii**: detects PII-like string patterns (email, phone, name).
/// - **cardinality**: buckets cardinality_ratio into low / medium / high / unique.
/// - **distribution_shape**: classifies from skewness + kurtosis (numeric only).
/// - **trend**: reserved for future temporal-trend detection (always `None` for now).
fn detect_field_traits(
    profile: &ColumnProfile,
    analysis: &ColumnAnalysis,
) -> crate::core::FieldTraits {
    use crate::core::{Cardinality, DistributionShape, FieldTraits};

    // --- semantic ---
    let semantic = match &analysis.inferred_type {
        Some(InferredType::Integer) => Some("integer".to_string()),
        Some(InferredType::Float) => Some("float".to_string()),
        Some(InferredType::Boolean) => Some("boolean".to_string()),
        Some(InferredType::Date(_)) => Some("date".to_string()),
        Some(InferredType::Uuid) => Some("uuid".to_string()),
        Some(InferredType::Categorical) => Some("categorical".to_string()),
        Some(InferredType::Text) => Some("text".to_string()),
        None => {
            // Fall back to arrow type for natively-typed columns
            match &profile.data_type {
                DataType::Boolean => Some("boolean".to_string()),
                DataType::Int8
                | DataType::Int16
                | DataType::Int32
                | DataType::Int64
                | DataType::UInt8
                | DataType::UInt16
                | DataType::UInt32
                | DataType::UInt64 => Some("integer".to_string()),
                DataType::Float16 | DataType::Float32 | DataType::Float64 => {
                    Some("float".to_string())
                }
                DataType::Date32 | DataType::Date64 | DataType::Timestamp(_, _) => {
                    Some("timestamp".to_string())
                }
                _ => None,
            }
        }
    };

    // Override semantic for pattern-detected types (email, uuid, phone, etc.)
    let semantic = {
        let best = analysis
            .string_patterns
            .iter()
            .filter(|(_, rate)| *rate > 0.8)
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        match best {
            Some((StringPattern::Email, _)) => Some("email".to_string()),
            Some((StringPattern::Phone, _)) => Some("phone".to_string()),
            Some((StringPattern::Url, _)) => Some("url".to_string()),
            Some((StringPattern::Uuid, _)) => Some("uuid".to_string()),
            Some((StringPattern::Name, _)) => Some("name".to_string()),
            Some((StringPattern::Date, _)) => Some("date".to_string()),
            Some((StringPattern::HexString(_), _)) => Some("hex_string".to_string()),
            _ => semantic,
        }
    };

    // --- pii ---
    let pii = {
        let has_pii_pattern = analysis.string_patterns.iter().any(|(pat, rate)| {
            *rate > 0.5
                && matches!(
                    pat,
                    StringPattern::Email | StringPattern::Phone | StringPattern::Name
                )
        });
        if has_pii_pattern {
            Some(true)
        } else {
            None // omit false to keep output clean
        }
    };

    // --- cardinality ---
    let cardinality = profile.cardinality_ratio.map(|ratio| {
        if ratio >= 0.99 {
            Cardinality::Unique
        } else if ratio >= 0.30 {
            Cardinality::High
        } else if ratio >= 0.01 {
            Cardinality::Medium
        } else {
            Cardinality::Low
        }
    });

    // --- distribution_shape (numeric only) ---
    let distribution_shape = profile.numeric.as_ref().map(|num| {
        let skew = num.skewness.abs();
        let kurt = num.kurtosis; // excess kurtosis: 0 = normal
        if skew < 0.5 && kurt.abs() < 1.0 {
            // Symmetric, near-normal kurtosis → could be uniform or normal
            // Use p10-p90 span vs full range to distinguish.
            // Uniform: p10-p90 ≈ 80% of range. Normal: p10-p90 ≈ much less.
            let range = num.max - num.min;
            if range > 0.0 {
                let central_span = num.percentiles.p90 - num.percentiles.p10;
                let central_ratio = central_span / range;
                // Uniform: ratio ≈ 0.8. Normal: ratio typically < 0.65.
                if central_ratio > 0.70 {
                    DistributionShape::Uniform
                } else {
                    DistributionShape::Normal
                }
            } else {
                DistributionShape::Uniform
            }
        } else if skew >= 0.5 && kurt < 3.0 {
            DistributionShape::Skewed
        } else if kurt >= 3.0 {
            DistributionShape::LongTail
        } else {
            DistributionShape::Normal
        }
    });

    FieldTraits {
        semantic,
        pii,
        cardinality,
        trend: None, // v2: temporal trend detection
        distribution_shape,
    }
}

/// Extract f64 values from a numeric column in a record batch.
fn extract_numeric_values(batch: &RecordBatch, col_name: &str) -> Vec<f64> {
    let Some(idx) = batch.schema().index_of(col_name).ok() else {
        return Vec::new();
    };
    let col = batch.column(idx);
    extract_f64_from_array(col.as_ref())
}

/// Recursively extract f64 from various numeric Arrow array types.
fn extract_f64_from_array(array: &dyn Array) -> Vec<f64> {
    let mut out = Vec::with_capacity(array.len());
    match array.data_type() {
        DataType::Int8 => {
            let a = array.as_primitive::<arrow::datatypes::Int8Type>();
            for i in 0..a.len() {
                if !a.is_null(i) {
                    out.push(a.value(i) as f64);
                }
            }
        }
        DataType::Int16 => {
            let a = array.as_primitive::<arrow::datatypes::Int16Type>();
            for i in 0..a.len() {
                if !a.is_null(i) {
                    out.push(a.value(i) as f64);
                }
            }
        }
        DataType::Int32 => {
            let a = array.as_primitive::<arrow::datatypes::Int32Type>();
            for i in 0..a.len() {
                if !a.is_null(i) {
                    out.push(a.value(i) as f64);
                }
            }
        }
        DataType::Int64 => {
            let a = array.as_primitive::<arrow::datatypes::Int64Type>();
            for i in 0..a.len() {
                if !a.is_null(i) {
                    out.push(a.value(i) as f64);
                }
            }
        }
        DataType::UInt8 => {
            let a = array.as_primitive::<arrow::datatypes::UInt8Type>();
            for i in 0..a.len() {
                if !a.is_null(i) {
                    out.push(a.value(i) as f64);
                }
            }
        }
        DataType::UInt16 => {
            let a = array.as_primitive::<arrow::datatypes::UInt16Type>();
            for i in 0..a.len() {
                if !a.is_null(i) {
                    out.push(a.value(i) as f64);
                }
            }
        }
        DataType::UInt32 => {
            let a = array.as_primitive::<arrow::datatypes::UInt32Type>();
            for i in 0..a.len() {
                if !a.is_null(i) {
                    out.push(a.value(i) as f64);
                }
            }
        }
        DataType::UInt64 => {
            let a = array.as_primitive::<arrow::datatypes::UInt64Type>();
            for i in 0..a.len() {
                if !a.is_null(i) {
                    out.push(a.value(i) as f64);
                }
            }
        }
        DataType::Float32 => {
            let a = array.as_primitive::<arrow::datatypes::Float32Type>();
            for i in 0..a.len() {
                if !a.is_null(i) {
                    out.push(a.value(i) as f64);
                }
            }
        }
        DataType::Float64 => {
            let a = array.as_primitive::<arrow::datatypes::Float64Type>();
            for i in 0..a.len() {
                if !a.is_null(i) {
                    out.push(a.value(i));
                }
            }
        }
        _ => {}
    }
    out
}

/// Extract epoch-seconds from timestamp columns.
fn extract_timestamp_seconds(batch: &RecordBatch, col_name: &str) -> Vec<f64> {
    let Some(idx) = batch.schema().index_of(col_name).ok() else {
        return Vec::new();
    };
    let col = batch.column(idx);
    let mut out = Vec::new();

    let divisor_and_array: Option<(f64, &dyn Array)> = match col.data_type() {
        DataType::Timestamp(TimeUnit::Second, _) => Some((1.0, col.as_ref())),
        DataType::Timestamp(TimeUnit::Millisecond, _) => Some((1_000.0, col.as_ref())),
        DataType::Timestamp(TimeUnit::Microsecond, _) => Some((1_000_000.0, col.as_ref())),
        DataType::Timestamp(TimeUnit::Nanosecond, _) => Some((1_000_000_000.0, col.as_ref())),
        DataType::Date32 => {
            // Date32 = days since epoch → convert to seconds
            if let Some(a) = col.as_any().downcast_ref::<arrow::array::Date32Array>() {
                for i in 0..a.len() {
                    if !a.is_null(i) {
                        out.push(a.value(i) as f64 * 86_400.0);
                    }
                }
            }
            out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            return out;
        }
        DataType::Date64 => {
            // Date64 = milliseconds since epoch → convert to seconds
            if let Some(a) = col.as_any().downcast_ref::<arrow::array::Date64Array>() {
                for i in 0..a.len() {
                    if !a.is_null(i) {
                        out.push(a.value(i) as f64 / 1_000.0);
                    }
                }
            }
            out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            return out;
        }
        _ => None,
    };

    if let Some((divisor, arr)) = divisor_and_array {
        // Use i64 values from the underlying primitive array
        if let Some(prim) = arr.as_any().downcast_ref::<arrow::array::Int64Array>() {
            for i in 0..prim.len() {
                if !prim.is_null(i) {
                    out.push(prim.value(i) as f64 / divisor);
                }
            }
        } else {
            // Fallback: try each concrete timestamp type
            macro_rules! try_ts {
                ($ty:ty) => {
                    if let Some(a) = arr.as_any().downcast_ref::<$ty>() {
                        for i in 0..a.len() {
                            if !a.is_null(i) {
                                out.push(a.value(i) as f64 / divisor);
                            }
                        }
                    }
                };
            }
            try_ts!(arrow::array::TimestampSecondArray);
            try_ts!(arrow::array::TimestampMillisecondArray);
            try_ts!(arrow::array::TimestampMicrosecondArray);
            try_ts!(arrow::array::TimestampNanosecondArray);
        }
    }
    out.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    out
}

/// Extract string values from a UTF-8 column.
fn extract_string_values(batch: &RecordBatch, col_name: &str) -> Vec<Option<String>> {
    let Some(idx) = batch.schema().index_of(col_name).ok() else {
        return Vec::new();
    };
    let col = batch.column(idx);
    match col.data_type() {
        DataType::Utf8 => {
            if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
                (0..a.len())
                    .map(|i| {
                        if a.is_null(i) {
                            None
                        } else {
                            Some(a.value(i).to_string())
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
        DataType::LargeUtf8 => {
            if let Some(a) = col.as_any().downcast_ref::<LargeStringArray>() {
                (0..a.len())
                    .map(|i| {
                        if a.is_null(i) {
                            None
                        } else {
                            Some(a.value(i).to_string())
                        }
                    })
                    .collect()
            } else {
                Vec::new()
            }
        }
        _ => Vec::new(),
    }
}

/// Check if a data type is a complex/nested type (List, Map, Struct).
fn is_complex_type(dt: &DataType) -> bool {
    matches!(
        dt,
        DataType::List(_)
            | DataType::LargeList(_)
            | DataType::FixedSizeList(_, _)
            | DataType::Map(_, _)
            | DataType::Struct(_)
    )
}

/// Extract display-string representations of complex-typed column values.
///
/// Uses Arrow's display formatter to produce human-readable text. Caps at 1000
/// non-null values to bound memory usage for high-cardinality columns.
fn extract_complex_display_values(batch: &RecordBatch, col_name: &str) -> Vec<String> {
    let Some(idx) = batch.schema().index_of(col_name).ok() else {
        return Vec::new();
    };
    let col = batch.column(idx);
    let mut values = Vec::new();
    let cap = 1000;

    // Serialize complex types as JSON for faithful round-trip reconstruction
    for i in 0..col.len() {
        if col.is_null(i) {
            continue;
        }
        if let Some(json_str) = complex_value_to_json(col.as_ref(), i) {
            values.push(json_str);
        }
        if values.len() >= cap {
            break;
        }
    }
    values
}

/// Serialize list elements to a JSON array string.
fn list_value_to_json(value_arr: &dyn arrow::array::Array) -> String {
    use arrow::array::{AsArray, as_string_array};
    use arrow::datatypes::DataType as ADT;

    let mut items = Vec::new();
    match value_arr.data_type() {
        ADT::Utf8 => {
            let str_arr = as_string_array(value_arr);
            for j in 0..str_arr.len() {
                if str_arr.is_null(j) {
                    items.push(serde_json::Value::Null);
                } else {
                    items.push(serde_json::Value::String(str_arr.value(j).to_string()));
                }
            }
        }
        ADT::Int32 => {
            let int_arr = value_arr.as_primitive::<arrow::datatypes::Int32Type>();
            for j in 0..int_arr.len() {
                if int_arr.is_null(j) {
                    items.push(serde_json::Value::Null);
                } else {
                    items.push(serde_json::Value::Number(int_arr.value(j).into()));
                }
            }
        }
        ADT::Int64 => {
            let int_arr = value_arr.as_primitive::<arrow::datatypes::Int64Type>();
            for j in 0..int_arr.len() {
                if int_arr.is_null(j) {
                    items.push(serde_json::Value::Null);
                } else {
                    items.push(serde_json::Value::Number(int_arr.value(j).into()));
                }
            }
        }
        _ => {
            let options = arrow::util::display::FormatOptions::default();
            if let Ok(formatter) =
                arrow::util::display::ArrayFormatter::try_new(value_arr, &options)
            {
                for j in 0..value_arr.len() {
                    items.push(serde_json::Value::String(format!("{}", formatter.value(j))));
                }
            }
        }
    }
    serde_json::to_string(&items).unwrap_or_else(|_| "[]".to_string())
}

/// Serialize a single complex-type value (List, Map) at index `i` to JSON string.
fn complex_value_to_json(array: &dyn arrow::array::Array, i: usize) -> Option<String> {
    use arrow::array::{AsArray, as_string_array};
    use arrow::datatypes::DataType as ADT;

    match array.data_type() {
        ADT::List(_) => {
            let list_arr = array.as_list_opt::<i32>()?;
            let value_arr = list_arr.value(i);
            Some(list_value_to_json(value_arr.as_ref()))
        }
        ADT::LargeList(_) => {
            let list_arr = array.as_list_opt::<i64>()?;
            let value_arr = list_arr.value(i);
            Some(list_value_to_json(value_arr.as_ref()))
        }
        ADT::Map(_, _) => {
            let map_arr = array.as_map();
            let offsets = map_arr.offsets();
            let start = offsets[i] as usize;
            let end = offsets[i + 1] as usize;
            let keys = map_arr.keys();
            let values = map_arr.values();
            let mut map = serde_json::Map::new();

            let key_strs: Vec<String> = match keys.data_type() {
                ADT::Utf8 => {
                    let str_arr = as_string_array(keys.as_ref());
                    (start..end).map(|j| str_arr.value(j).to_string()).collect()
                }
                _ => (start..end).map(|j| format!("key_{}", j)).collect(),
            };

            match values.data_type() {
                ADT::Int32 => {
                    let int_arr = values.as_primitive::<arrow::datatypes::Int32Type>();
                    for (j, key) in (start..end).zip(key_strs) {
                        if int_arr.is_null(j) {
                            map.insert(key, serde_json::Value::Null);
                        } else {
                            map.insert(key, serde_json::Value::Number(int_arr.value(j).into()));
                        }
                    }
                }
                ADT::Int64 => {
                    let int_arr = values.as_primitive::<arrow::datatypes::Int64Type>();
                    for (j, key) in (start..end).zip(key_strs) {
                        if int_arr.is_null(j) {
                            map.insert(key, serde_json::Value::Null);
                        } else {
                            map.insert(key, serde_json::Value::Number(int_arr.value(j).into()));
                        }
                    }
                }
                ADT::Utf8 => {
                    let str_arr = as_string_array(values.as_ref());
                    for (j, key) in (start..end).zip(key_strs) {
                        if str_arr.is_null(j) {
                            map.insert(key, serde_json::Value::Null);
                        } else {
                            map.insert(
                                key,
                                serde_json::Value::String(str_arr.value(j).to_string()),
                            );
                        }
                    }
                }
                _ => {
                    let options = arrow::util::display::FormatOptions::default();
                    if let Ok(formatter) =
                        arrow::util::display::ArrayFormatter::try_new(values.as_ref(), &options)
                    {
                        for (j, key) in (start..end).zip(key_strs) {
                            map.insert(
                                key,
                                serde_json::Value::String(format!("{}", formatter.value(j))),
                            );
                        }
                    }
                }
            }
            serde_json::to_string(&map).ok()
        }
        _ => None,
    }
}

/// Extract distinct string representations of column values for relationship detection.
fn extract_distinct_string_values(batch: &RecordBatch, col_name: &str) -> HashSet<String> {
    let Some(idx) = batch.schema().index_of(col_name).ok() else {
        return HashSet::new();
    };
    let col = batch.column(idx);
    let mut set = HashSet::new();
    let cap = 10_000;

    match col.data_type() {
        DataType::Utf8 => {
            if let Some(a) = col.as_any().downcast_ref::<StringArray>() {
                for i in 0..a.len() {
                    if !a.is_null(i) {
                        set.insert(a.value(i).to_string());
                        if set.len() >= cap {
                            break;
                        }
                    }
                }
            }
        }
        DataType::LargeUtf8 => {
            if let Some(a) = col.as_any().downcast_ref::<LargeStringArray>() {
                for i in 0..a.len() {
                    if !a.is_null(i) {
                        set.insert(a.value(i).to_string());
                        if set.len() >= cap {
                            break;
                        }
                    }
                }
            }
        }
        _ => {
            // For numeric types, convert to string for relationship matching
            let values = extract_f64_from_array(col.as_ref());
            for v in values.iter().take(cap) {
                if v.fract() == 0.0 {
                    set.insert(format!("{}", *v as i64));
                } else {
                    set.insert(format!("{v}"));
                }
            }
        }
    }
    set
}

/// Heuristic: a column is likely a PK if it has unique values and its name
/// suggests it is a key (contains "id" or "_key" suffix).
fn is_likely_primary_key(profile: &ColumnProfile, row_count: u64) -> bool {
    if row_count == 0 {
        return false;
    }
    let is_unique = profile
        .cardinality_ratio
        .map(|r| (r - 1.0).abs() < 1e-9)
        .unwrap_or(false);
    let name_lower = profile.name.to_lowercase();
    // Check CamelCase "Id"/"ID" suffix: original name must end with "Id" or "ID" (capital I)
    let has_camel_id =
        (profile.name.ends_with("Id") || profile.name.ends_with("ID")) && profile.name.len() > 2;
    // Also support all-lowercase "id" suffix (e.g. "userid", "customerid") but exclude
    // common English words that happen to end in "id"
    let has_lower_id = name_lower.ends_with("id")
        && name_lower.len() > 2
        && !matches!(
            name_lower.as_str(),
            "valid"
                | "invalid"
                | "rapid"
                | "timid"
                | "vivid"
                | "stupid"
                | "hybrid"
                | "morbid"
                | "orchid"
                | "fluid"
                | "void"
                | "android"
                | "paid"
                | "said"
                | "laid"
        );
    let name_suggests_pk = name_lower == "id"
        || name_lower.ends_with("_id")
        || name_lower.ends_with("_key")
        || has_camel_id
        || has_lower_id;

    // Require both uniqueness AND a PK-like name to avoid false positives
    is_unique && name_suggests_pk && profile.null_count == 0
}

/// When a table has multiple PK candidates, pick the best one.
/// Preference order:
/// 1. Column named exactly "id"
/// 2. Column named "{table_name}id" or "{table_name}_id" (matches table name)
/// 3. First candidate in column order (positional heuristic — first column is often PK)
fn pick_best_pk(columns: &[RelColumn], table_name: &str) -> usize {
    let table_lower = table_name.to_lowercase();
    // Strip common suffixes from table name for matching (e.g. "PeopleHistorical_test" → "peoplehistorical")
    let table_stem = table_lower
        .trim_end_matches("_test")
        .trim_end_matches("_tests")
        .trim_end_matches('s'); // strip plural

    let candidates: Vec<usize> = columns
        .iter()
        .enumerate()
        .filter(|(_, c)| c.is_primary_key)
        .map(|(i, _)| i)
        .collect();

    // Priority 1: column named exactly "id"
    for &i in &candidates {
        if columns[i].name.to_lowercase() == "id" {
            return i;
        }
    }

    // Priority 2: column name matches "{table_stem}id" or "{table_stem}_id"
    for &i in &candidates {
        let col_lower = columns[i].name.to_lowercase();
        let col_stem = col_lower
            .trim_end_matches("id")
            .trim_end_matches("_id")
            .trim_end_matches('_');
        if col_stem == table_stem {
            return i;
        }
    }

    // Priority 3: first candidate by position
    candidates[0]
}

/// Copy companion schema dictionary files to the output directory.
///
/// When a structured dataset includes `Schema/schema.json` with dictionary
/// definitions, this function copies the referenced dictionary CSV files
/// to a `Mappings/` subdirectory alongside the generated schema. This
/// preserves the dictionary encoding pattern so generated data can use
/// the same ID mappings.
///
/// Returns the number of dictionary files copied.
fn copy_companion_dictionaries(
    table_analyses: &[TableAnalysis],
    output_dir: &Path,
    quiet: bool,
) -> Result<usize> {
    use std::collections::HashSet;

    let mut copied: HashSet<String> = HashSet::new();
    let mut total_copied = 0;

    for ta in table_analyses {
        let (Some(companion), Some(companion_path)) = (&ta.companion, &ta.companion_path) else {
            continue;
        };

        if companion.dictionaries.is_empty() {
            continue;
        }

        let dict_dir = companion.resolve_dictionary_dir(companion_path);
        let Some(source_dict_dir) = dict_dir else {
            debug!(
                table = %ta.name,
                "companion schema has dictionaries but dictionary_path not found"
            );
            continue;
        };

        // Create output Mappings/ directory
        let out_mappings = output_dir.join("Mappings");
        if !out_mappings.exists() {
            std::fs::create_dir_all(&out_mappings)
                .with_context(|| format!("failed to create {}", out_mappings.display()))?;
        }

        for dict in &companion.dictionaries {
            // Avoid copying the same dictionary file twice (shared across entities)
            if copied.contains(&dict.path) {
                continue;
            }

            // Reject unsafe paths (absolute or containing ".." components)
            let dict_path = std::path::Path::new(&dict.path);
            if dict_path.is_absolute()
                || dict_path
                    .components()
                    .any(|c| matches!(c, std::path::Component::ParentDir))
            {
                warn!(
                    dict = %dict.name,
                    path = %dict.path,
                    "skipping dictionary with unsafe path"
                );
                continue;
            }

            let src = source_dict_dir.join(&dict.path);
            let dst = out_mappings.join(&dict.path);

            if src.exists() {
                std::fs::copy(&src, &dst).with_context(|| {
                    format!(
                        "failed to copy dictionary {} → {}",
                        src.display(),
                        dst.display()
                    )
                })?;
                copied.insert(dict.path.clone());
                total_copied += 1;
                debug!(
                    dict = %dict.name,
                    src = %src.display(),
                    dst = %dst.display(),
                    "copied companion dictionary"
                );
            } else {
                warn!(
                    dict = %dict.name,
                    path = %src.display(),
                    "companion dictionary file not found"
                );
            }
        }
    }

    if total_copied > 0 && !quiet {
        eprintln!(
            "  {} copied {} companion dictionary file(s)",
            "→".dimmed(),
            total_copied,
        );
    }

    Ok(total_copied)
}

/// Known data file extensions that should NOT be treated as companion files.
const DATA_EXTENSIONS: &[&str] = &["csv", "tsv", "parquet", "json", "jsonl", "arrow", "avro"];

/// Copy all non-data files from the source directory to `output_dir`, preserving
/// relative paths. Records each copied file's relative path in
/// `data_model.companion_files` so the generate command can reproduce them.
///
/// "Non-data" means any file whose extension is not a known data format,
/// OR data-format files inside special directories like `Schema/` or `Mappings/`.
/// This captures schema definitions, dictionary CSVs, and other auxiliary assets.
fn copy_companion_files(
    source_root: &Path,
    output_dir: &Path,
    data_model: &mut crate::core::DataModel,
    quiet: bool,
) -> Result<usize> {
    let mut copied = 0;
    let mut stack: Vec<std::path::PathBuf> = vec![source_root.to_path_buf()];

    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            if !path.is_file() {
                continue;
            }

            let ext = path
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();

            // Compute relative path from source root
            let rel = match path.strip_prefix(source_root) {
                Ok(r) => r,
                Err(_) => continue,
            };

            // Determine if this file is inside a special (non-data) directory
            let in_special_dir = rel.components().any(|c| {
                let name = c.as_os_str().to_string_lossy().to_lowercase();
                name == "mappings" || name == "schema"
            });

            // Skip data files unless they're in a special directory
            if DATA_EXTENSIONS.contains(&ext.as_str()) && !in_special_dir {
                continue;
            }

            // Reject unsafe paths
            if rel.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir | std::path::Component::RootDir
                )
            }) {
                warn!(path = %rel.display(), "skipping companion file with unsafe path");
                continue;
            }

            let dst = output_dir.join(rel);

            // Skip if source and destination are the same file
            if path == dst {
                continue;
            }

            // Create parent directories
            if let Some(parent) = dst.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create directory {}", parent.display()))?;
            }

            std::fs::copy(&path, &dst).with_context(|| {
                format!(
                    "failed to copy companion file {} → {}",
                    path.display(),
                    dst.display()
                )
            })?;

            let rel_str = rel.to_string_lossy().to_string();
            if let Some(logger) = crate::decision::global_logger() {
                let reason = if in_special_dir {
                    format!("in special directory, ext={ext}")
                } else {
                    format!("ext={ext} not in data formats")
                };
                logger
                    .builder(crate::decision::DecisionKind::CompanionClassification)
                    .phase("learn/companion")
                    .entity(&rel_str)
                    .chosen("companion")
                    .reason(reason)
                    .confidence(crate::decision::Confidence::High)
                    .record();
            }
            data_model.companion_files.push(rel_str);
            copied += 1;
        }
    }

    // Sort for deterministic output
    data_model.companion_files.sort();
    data_model.companion_files.dedup();

    if copied > 0 && !quiet {
        debug!(count = copied, "copied companion files from source");
    }

    Ok(copied)
}
///
/// Walks the assembled data model looking for fields with `Faker { method: "word" }`
/// generators (the fallback for free-text columns). For each such field, extracts
/// unique non-null string values from the source data, writes them to a `.dict.txt`
/// file, and replaces the generator with `Dictionary { file, expansion }`.
///
/// Returns the number of dictionary files written.
fn extract_dictionaries(
    model: &mut crate::core::DataModel,
    tables: &[IngestionResult],
    output_dir: &Path,
    quiet: bool,
) -> Result<usize> {
    use std::io::Write;

    let mut dict_count = 0;

    for entity in &mut model.entities {
        // Find the matching source table
        let source_table = tables.iter().find(|t| t.entity == entity.name);
        let Some(table) = source_table else {
            continue;
        };

        for field in &mut entity.fields {
            // Check if this field uses a generator that would benefit from
            // dictionary extraction. This covers:
            // 1. Faker fallbacks (word, name, product_name, sentence, text, etc.)
            //    — source values are more domain-specific than faker output
            // 2. Truncated OneOf generators (≥200 string choices indicates the
            //    categorical cap was hit and values were lost)
            let is_extractable = match &field.generator {
                Some(crate::core::GeneratorSpec::Faker { method, .. }) => {
                    matches!(
                        method.as_str(),
                        "word"
                            | "name"
                            | "product_name"
                            | "sentence"
                            | "text"
                            | "paragraph"
                            | "company"
                            | "catch_phrase"
                            | "bs"
                            | "job"
                    )
                }
                Some(crate::core::GeneratorSpec::OneOf { choices }) => {
                    // A string-valued OneOf at the 200-choice cap MAY have been truncated.
                    // We check below whether the source data actually has more unique values.
                    choices.len() == 200
                        && choices.iter().all(|c| {
                            matches!(c.value, crate::core::Value::String(_))
                        })
                }
                _ => false,
            };
            if !is_extractable {
                continue;
            }

            // Extract unique string values from source data
            let unique_values = extract_unique_strings_from_batches(&table.batches, &field.name);
            if unique_values.is_empty() {
                continue;
            }

            // Only extract dictionary if there are enough distinct values
            // (categorical columns with ≤50 values are handled by one_of already)
            if unique_values.len() <= 50 {
                continue;
            }

            // For OneOf generators at the 200-choice cap, only extract if the source
            // actually has MORE unique values than the OneOf (confirming truncation).
            // If unique_values.len() == OneOf.len(), the OneOf wasn't truncated and
            // already preserves frequency weights that a Dictionary would lose.
            if let Some(crate::core::GeneratorSpec::OneOf { choices }) = &field.generator {
                if unique_values.len() <= choices.len() {
                    continue;
                }
            }

            // Write dictionary file (sanitize filename components)
            let dict_filename = truncate_filename(format!(
                "{}_{}.dict.txt",
                sanitize_filename_component(&entity.name),
                sanitize_filename_component(&field.name)
            ));
            let dict_path = output_dir.join(&dict_filename);
            let mut file = std::fs::File::create(&dict_path).with_context(|| {
                format!("failed to create dictionary file '{}'", dict_path.display())
            })?;

            // Filter out values containing line breaks (would corrupt one-per-line format)
            let clean_values: Vec<&String> = unique_values
                .iter()
                .filter(|v| !v.contains('\n') && !v.contains('\r'))
                .collect();

            if clean_values.is_empty() {
                continue;
            }

            for val in &clean_values {
                writeln!(file, "{}", val)?;
            }

            // Determine expansion strategy based on value structure.
            // Use "shuffle" (each value exactly once) when distinct values == row count,
            // indicating every row has a unique value for this column.
            let owned_clean: Vec<String> = clean_values.iter().map(|s| s.to_string()).collect();
            let row_count = table.batches.iter().map(|b| b.num_rows()).sum::<usize>();
            let expansion = if clean_values.len() == row_count {
                "shuffle".to_string()
            } else {
                detect_expansion_strategy(&owned_clean)
            };

            // Replace generator with dictionary reference
            field.generator = Some(crate::core::GeneratorSpec::Dictionary {
                file: dict_filename.clone(),
                expansion,
            });

            dict_count += 1;

            if !quiet {
                eprintln!(
                    "  {} extracted {} → {} unique values ({})",
                    "📖".dimmed(),
                    dict_filename,
                    clean_values.len(),
                    field.name,
                );
            }
        }
    }

    Ok(dict_count)
}

/// Extract co-occurring tuple dictionaries as TSV files.
///
/// For each detected tuple group, writes a TSV dictionary file containing the
/// unique tuples, sets the primary column to a Dictionary generator, and sets
/// secondary columns to TupleLookup generators.
fn extract_tuple_dictionaries(
    model: &mut crate::core::DataModel,
    table_analyses: &[crate::learn::schema_assembly::TableAnalysis],
    output_dir: &Path,
) -> Result<usize> {
    use std::io::Write;

    let mut count = 0;

    for analysis in table_analyses {
        if analysis.tuple_groups.is_empty() {
            continue;
        }
        let entity = model.entities.iter_mut().find(|e| e.name == analysis.name);
        let Some(entity) = entity else { continue };

        for group in &analysis.tuple_groups {
            if group.columns.len() < 2 || group.tuples.is_empty() {
                continue;
            }

            let primary = &group.columns[0];

            // For 2-column tuples where the primary already has a Dictionary,
            // replace it with a tuple dictionary to preserve cross-column coherence.
            // The standalone dictionary loses the relationship between columns.
            if group.columns.len() == 2 {
                if let Some(field) = entity.fields.iter_mut().find(|f| f.name == *primary) {
                    if let Some(crate::core::GeneratorSpec::Dictionary { ref file, .. }) =
                        field.generator
                    {
                        // Remove the standalone dictionary file — tuple subsumes it
                        let old_path = output_dir.join(file);
                        let _ = std::fs::remove_file(&old_path);
                        field.generator = None;
                    }
                }
            }

            // For 3+ column groups, replace existing dictionaries — the tuple
            // dictionary subsumes them and provides multi-column coherence.
            // Delete orphaned dictionary files from prior extraction.
            if group.columns.len() >= 3 {
                for col_name in &group.columns {
                    if let Some(field) = entity.fields.iter_mut().find(|f| f.name == *col_name) {
                        if let Some(crate::core::GeneratorSpec::Dictionary { ref file, .. }) =
                            field.generator
                        {
                            let old_path = output_dir.join(file);
                            let _ = std::fs::remove_file(&old_path);
                            field.generator = None;
                        }
                    }
                }
            }

            // Skip if primary column has a date/datetime type (Dictionary produces strings)
            let primary_is_date = entity
                .fields
                .iter()
                .find(|f| f.name == *primary)
                .is_some_and(|f| matches!(
                    f.data_type,
                    crate::core::DataType::Date
                        | crate::core::DataType::Datetime
                        | crate::core::DataType::DatetimeUs
                        | crate::core::DataType::Datetimetz
                        | crate::core::DataType::Time
                ));
            if primary_is_date {
                continue;
            }

            let file_name = truncate_filename(format!(
                "{}__{}.tsv",
                sanitize_filename(&entity.name),
                group
                    .columns
                    .iter()
                    .map(|c| sanitize_filename(c))
                    .collect::<Vec<_>>()
                    .join("_")
            ));
            let file_path = output_dir.join(&file_name);

            // Write TSV file (escape tabs/newlines in values)
            let mut file = std::fs::File::create(&file_path)
                .with_context(|| format!("failed to create tuple dictionary {file_name}"))?;
            for tuple in &group.tuples {
                let escaped: Vec<String> = tuple
                    .iter()
                    .map(|v| escape_tsv_value(v))
                    .collect();
                let line = escaped.join("\t");
                writeln!(file, "{line}")?;
            }

            // Write a separate primary-only dictionary file (one value per line).
            // Use raw values (not TSV-escaped) because the Dictionary generator
            // reads lines verbatim, and TupleLookup keys in the TSV are also raw.
            let primary_dict_name = truncate_filename(format!(
                "{}_{}.dict.txt",
                sanitize_filename(&entity.name),
                sanitize_filename(primary)
            ));
            let primary_dict_path = output_dir.join(&primary_dict_name);
            let mut pdict = std::fs::File::create(&primary_dict_path)
                .with_context(|| format!("failed to create primary dict {primary_dict_name}"))?;
            let mut written_count = 0usize;
            for tuple in &group.tuples {
                if let Some(pv) = tuple.first() {
                    // Skip values with newlines (would break line-based dict format)
                    if !pv.contains('\n') && !pv.contains('\r') {
                        writeln!(pdict, "{pv}")?;
                        written_count += 1;
                    }
                }
            }

            // Set primary column to Dictionary generator (reads from primary-only file)
            // Use "shuffle" when every tuple is unique (distinct_count == row_count),
            // ensuring each value appears exactly once in the output.
            let row_count = match &entity.count {
                crate::core::CountSpec::Fixed(n) => *n as usize,
                _ => 0,
            };
            let expansion = if written_count == row_count && row_count > 0 {
                "shuffle".to_string()
            } else {
                "sample".to_string()
            };
            if let Some(field) = entity.fields.iter_mut().find(|f| f.name == *primary) {
                field.generator = Some(crate::core::GeneratorSpec::Dictionary {
                    file: primary_dict_name,
                    expansion,
                });
            }

            // Set secondary columns to TupleLookup generators (reads from TSV)
            for (col_idx, col_name) in group.columns.iter().enumerate().skip(1) {
                if let Some(field) = entity.fields.iter_mut().find(|f| f.name == *col_name) {
                    field.generator = Some(crate::core::GeneratorSpec::TupleLookup {
                        source_field: primary.clone(),
                        file: file_name.clone(),
                        column: col_idx,
                    });
                }
            }

            count += 1;
            tracing::debug!(
                entity = %entity.name,
                columns = ?group.columns,
                tuples = group.tuples.len(),
                file = %file_name,
                "wrote tuple dictionary"
            );
        }
    }

    Ok(count)
}

/// Sanitize a string for use as a filename component.
fn sanitize_filename(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

/// Maximum filename length (excluding directory path).
///
/// Windows MAX_PATH is 260 chars total. We reserve headroom for the output
/// directory path and file extension. 200 chars for the stem is safe.
const MAX_FILENAME_LEN: usize = 200;

/// Truncate a filename stem if it exceeds [`MAX_FILENAME_LEN`], appending a
/// short hash suffix to preserve uniqueness.
fn truncate_filename(name: String) -> String {
    if name.len() <= MAX_FILENAME_LEN {
        return name;
    }
    // Use a simple FNV-like hash of the full name for uniqueness
    let hash = {
        let mut h: u64 = 0xcbf29ce484222325;
        for b in name.as_bytes() {
            h ^= *b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        h
    };
    let suffix = format!("_{:016x}", hash);
    let keep = MAX_FILENAME_LEN - suffix.len();
    let truncated = &name[..keep];
    format!("{}{}", truncated, suffix)
}

/// Escape tab, newline, and carriage return characters for TSV output.
fn escape_tsv_value(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// Unescape a TSV value produced by [`escape_tsv_value`].
pub fn unescape_tsv_value(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('t') => result.push('\t'),
                Some('n') => result.push('\n'),
                Some('r') => result.push('\r'),
                Some('\\') => result.push('\\'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(c);
        }
    }
    result
}

/// Extract full-row dictionaries for small, mostly-categorical tables.
///
/// When a table has <1000 rows and ≥50% of its columns are string/categorical,
/// this function writes ALL unique rows as a TSV dictionary and sets the first string
/// column as a Dictionary generator with all others as TupleLookup. This provides
/// perfect row-level coherence for small reference/lookup tables.
fn extract_full_row_dictionaries(
    model: &mut crate::core::DataModel,
    tables: &[crate::learn::ingest::IngestionResult],
    output_dir: &Path,
    quiet: bool,
) -> Result<usize> {
    use arrow::array::Array;
    use std::collections::HashSet;
    use std::io::Write;

    const MAX_ROWS_FOR_FULL_ROW_DICT: usize = 6000;
    const MIN_STRING_COLUMN_RATIO: f64 = 0.5;
    // For small schemas (≤6 columns), relax the string ratio requirement
    // since they are often reference/lookup tables regardless of column types.
    const SMALL_SCHEMA_THRESHOLD: usize = 6;
    // For very small tables (≤200 rows), always use full-row dict regardless
    // of string ratio — row coherence matters more than column type distribution.
    const SMALL_TABLE_ALWAYS_THRESHOLD: usize = 200;

    let mut count = 0;

    for entity in &mut model.entities {
        // Skip if entity already has all columns covered by tuple dicts
        let all_covered = entity.fields.iter().all(|f| {
            matches!(
                f.generator,
                Some(crate::core::GeneratorSpec::Dictionary { .. })
                    | Some(crate::core::GeneratorSpec::TupleLookup { .. })
            )
        });
        if all_covered {
            continue;
        }

        // Skip entities with a primary key — these generate new rows, not lookups
        let has_pk = entity.fields.iter().any(|f| f.primary_key.unwrap_or(false));
        if has_pk {
            continue;
        }

        // Check column composition: need ≥50% string/categorical columns,
        // or ≥1 string column for small schemas (≤6 columns) which are
        // likely reference/lookup tables.
        let total_cols = entity.fields.len();
        if total_cols < 2 {
            continue;
        }
        let string_cols = entity.fields.iter().filter(|f| {
            matches!(
                f.data_type,
                crate::core::DataType::String | crate::core::DataType::Uuid
            ) || matches!(
                &f.generator,
                Some(crate::core::GeneratorSpec::OneOf { .. })
                    | Some(crate::core::GeneratorSpec::Dictionary { .. })
                    | Some(crate::core::GeneratorSpec::Faker { .. })
            )
        }).count();
        if string_cols == 0 {
            continue;
        }
        let string_ratio = string_cols as f64 / total_cols as f64;

        // Find source data
        let source_table = tables.iter().find(|t| t.entity == entity.name);
        let Some(table) = source_table else { continue };

        // Count total rows
        let total_rows: usize = table.batches.iter().map(|b| b.num_rows()).sum();
        if total_rows == 0 || total_rows >= MAX_ROWS_FOR_FULL_ROW_DICT {
            continue;
        }

        // Skip string ratio check for small schemas or small tables
        let is_small_table = total_rows <= SMALL_TABLE_ALWAYS_THRESHOLD;
        if total_cols > SMALL_SCHEMA_THRESHOLD && string_ratio < MIN_STRING_COLUMN_RATIO && !is_small_table {
            continue;
        }

        // Collect column names from entity fields (in field order)
        let field_names: Vec<String> = entity.fields.iter().map(|f| f.name.clone()).collect();

        // Extract unique rows as string tuples
        let mut unique_rows: Vec<Vec<String>> = Vec::new();
        let mut seen: HashSet<Vec<String>> = HashSet::new();

        for batch in &table.batches {
            for row_idx in 0..batch.num_rows() {
                let mut row: Vec<String> = Vec::with_capacity(field_names.len());
                let mut valid = true;
                for col_name in &field_names {
                    let col_idx = match batch.schema().index_of(col_name) {
                        Ok(idx) => idx,
                        Err(_) => { valid = false; break; }
                    };
                    let col = batch.column(col_idx);
                    if col.is_null(row_idx) {
                        row.push(String::new());
                    } else {
                        let val = arrow::util::display::array_value_to_string(col, row_idx)
                            .unwrap_or_default();
                        row.push(val);
                    }
                }
                if !valid {
                    continue;
                }
                if seen.insert(row.clone()) {
                    unique_rows.push(row);
                }
            }
        }

        // Only apply full-row dict if unique rows ≤ threshold
        if unique_rows.is_empty() || unique_rows.len() > MAX_ROWS_FOR_FULL_ROW_DICT {
            continue;
        }

        // Remove any existing tuple dict or dictionary files for this entity's columns
        // since the full-row dictionary supersedes them.
        let old_files: Vec<String> = entity.fields.iter().filter_map(|f| {
            match &f.generator {
                Some(crate::core::GeneratorSpec::Dictionary { file, .. })
                | Some(crate::core::GeneratorSpec::TupleLookup { file, .. }) => {
                    Some(file.clone())
                }
                _ => None,
            }
        }).collect();
        for old_file in &old_files {
            let old_path = output_dir.join(old_file);
            let _ = std::fs::remove_file(&old_path);
        }
        for field in entity.fields.iter_mut() {
            if matches!(
                &field.generator,
                Some(crate::core::GeneratorSpec::Dictionary { .. })
                    | Some(crate::core::GeneratorSpec::TupleLookup { .. })
            ) {
                field.generator = None;
            }
        }

        // Write full-row TSV file (columns in field order)
        let file_name = format!(
            "{}__fullrow.tsv",
            sanitize_filename(&entity.name),
        );
        let file_path = output_dir.join(&file_name);
        let mut file = std::fs::File::create(&file_path)
            .with_context(|| format!("failed to create full-row dictionary {file_name}"))?;
        for row in &unique_rows {
            let escaped: Vec<String> = row.iter().map(|v| escape_tsv_value(v)).collect();
            writeln!(file, "{}", escaped.join("\t"))?;
        }

        let row_count = unique_rows.len();

        // Set all columns to RowLookup — the engine picks a shared random row
        // index per output record for all RowLookup fields sharing the same file.
        for (col_idx, col_name) in field_names.iter().enumerate() {
            if let Some(field) = entity.fields.iter_mut().find(|f| f.name == *col_name) {
                field.generator = Some(crate::core::GeneratorSpec::RowLookup {
                    file: file_name.clone(),
                    column: col_idx,
                    row_count,
                });
            }
        }

        // Remove conditional_distribution correlations targeting columns that
        // are now covered by the full-row dictionary (TupleLookup provides exact
        // values, making distribution-based correlations counterproductive).
        model.correlations.retain(|corr| {
            let is_cond_dist = corr
                .correlation_type
                .as_deref()
                .map(|t| t == "conditional_distribution")
                .unwrap_or(false);
            if !is_cond_dist || corr.entity != entity.name {
                return true; // keep non-conditional or other-entity correlations
            }
            // Drop if the dependent field is one of our full-row dict columns
            if let Some(dep) = &corr.dependent {
                if field_names.contains(dep) {
                    tracing::debug!(
                        entity = %entity.name,
                        dependent = %dep,
                        "removing conditional_distribution override (superseded by full-row dictionary)"
                    );
                    return false;
                }
            }
            true
        });

        count += 1;
        if !quiet {
            eprintln!(
                "  {} full-row dictionary: {} ({} unique rows, {} columns)",
                "📦".dimmed(),
                entity.name,
                unique_rows.len(),
                field_names.len(),
            );
        }
        tracing::info!(
            entity = %entity.name,
            unique_rows = unique_rows.len(),
            columns = field_names.len(),
            "extracted full-row dictionary"
        );
    }

    Ok(count)
}

/// Extract dictionaries from state reservoir samples for incremental finalize.
///
/// Similar to [`extract_dictionaries`] but uses the reservoir samples stored in
/// `LearnState` instead of raw data batches. This enables dictionary extraction
/// during incremental finalize when source data is no longer available.
fn extract_dictionaries_from_state(
    model: &mut crate::core::DataModel,
    state: &crate::learn::streaming::LearnState,
    output_dir: &Path,
) -> Result<usize> {
    use std::io::Write;

    let mut dict_count = 0;

    for entity in &mut model.entities {
        // Find the matching table state
        let table_state = state.tables.get(&entity.name);
        let Some(ts) = table_state else {
            continue;
        };

        for field in &mut entity.fields {
            // Check for extractable generators (same criteria as batch mode):
            // 1. Faker fallbacks (word, name, product_name, sentence, text, etc.)
            // 2. Truncated OneOf generators (≥200 string choices)
            let is_extractable = match &field.generator {
                Some(crate::core::GeneratorSpec::Faker { method, .. }) => {
                    matches!(
                        method.as_str(),
                        "word"
                            | "name"
                            | "product_name"
                            | "sentence"
                            | "text"
                            | "paragraph"
                            | "company"
                            | "catch_phrase"
                            | "bs"
                            | "job"
                    )
                }
                Some(crate::core::GeneratorSpec::OneOf { choices }) => {
                    choices.len() == 200
                        && choices.iter().all(|c| {
                            matches!(c.value, crate::core::Value::String(_))
                        })
                }
                _ => false,
            };
            if !is_extractable {
                continue;
            }

            // Find the column state to access its reservoir sample
            let col_state = ts.columns.iter().find(|c| c.name == field.name);
            let Some(cs) = col_state else {
                continue;
            };

            // Get unique values from reservoir sample (normalized: trimmed, non-empty)
            let reservoir_items = cs.reservoir.items();
            if reservoir_items.is_empty() {
                continue;
            }

            // Use HLL cardinality estimate to determine if this column is a dictionary
            // candidate. This is more reliable than counting reservoir uniques alone,
            // since the reservoir may under-represent tail values for large datasets.
            let estimated_cardinality = cs.estimated_cardinality() as usize;
            if estimated_cardinality <= 50 {
                continue;
            }

            // For OneOf generators at the 200-choice cap, only extract if the estimated
            // cardinality exceeds the OneOf size (confirming truncation).
            if let Some(crate::core::GeneratorSpec::OneOf { choices }) = &field.generator {
                if estimated_cardinality <= choices.len() {
                    continue;
                }
            }

            // Normalize: trim whitespace and skip empty strings (matches batch behavior)
            let mut unique_values: Vec<&str> = reservoir_items
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect::<std::collections::HashSet<&str>>()
                .into_iter()
                .collect();
            unique_values.sort_unstable();

            if unique_values.is_empty() {
                continue;
            }

            // Filter out values containing line breaks
            let clean_values: Vec<&str> = unique_values
                .into_iter()
                .filter(|v| !v.contains('\n') && !v.contains('\r'))
                .collect();

            if clean_values.is_empty() {
                continue;
            }

            // Write dictionary file
            let dict_filename = truncate_filename(format!(
                "{}_{}.dict.txt",
                sanitize_filename_component(&entity.name),
                sanitize_filename_component(&field.name)
            ));
            let dict_path = output_dir.join(&dict_filename);
            let mut file = std::fs::File::create(&dict_path).with_context(|| {
                format!("failed to create dictionary file '{}'", dict_path.display())
            })?;

            for val in &clean_values {
                writeln!(file, "{}", val)?;
            }

            // Determine expansion strategy
            let owned_clean: Vec<String> = clean_values.iter().map(|s| s.to_string()).collect();
            let expansion = detect_expansion_strategy(&owned_clean);

            // Replace generator with dictionary reference
            field.generator = Some(crate::core::GeneratorSpec::Dictionary {
                file: dict_filename,
                expansion,
            });

            dict_count += 1;
        }
    }

    Ok(dict_count)
}

/// Maximum number of entries in an extracted dictionary file.
///
/// Prevents unbounded memory/disk usage for very high-cardinality columns.
const MAX_DICTIONARY_ENTRIES: usize = 10_000;

/// Sanitize a string for use as a filename component.
///
/// Replaces characters that are invalid in Windows/Unix filenames with underscores,
/// and collapses sequences of underscores. Prevents path traversal by stripping
/// separators and `..`.
fn sanitize_filename_component(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '.' => '_',
            c if c.is_ascii_control() => '_',
            _ => c,
        })
        .collect();
    // Collapse multiple underscores and trim leading/trailing underscores
    let mut result = String::new();
    let mut last_was_underscore = true; // trim leading
    for c in sanitized.chars() {
        if c == '_' {
            if !last_was_underscore {
                result.push('_');
            }
            last_was_underscore = true;
        } else {
            result.push(c);
            last_was_underscore = false;
        }
    }
    // Trim trailing underscore
    while result.ends_with('_') {
        result.pop();
    }
    if result.is_empty() {
        "unnamed".to_string()
    } else {
        result
    }
}

/// Extract unique non-null string values from record batches for a given column.
///
/// Stops after [`MAX_DICTIONARY_ENTRIES`] unique values to bound memory usage.
fn extract_unique_strings_from_batches(batches: &[RecordBatch], col_name: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut values = Vec::new();

    'outer: for batch in batches {
        let Some(idx) = batch.schema().index_of(col_name).ok() else {
            continue;
        };
        let col = batch.column(idx);

        match col.data_type() {
            DataType::Utf8 => {
                let arr = col
                    .as_any()
                    .downcast_ref::<StringArray>()
                    .expect("Utf8 column must downcast to StringArray");
                for i in 0..arr.len() {
                    if values.len() >= MAX_DICTIONARY_ENTRIES {
                        break 'outer;
                    }
                    if !arr.is_null(i) {
                        let val = arr.value(i).trim();
                        if !val.is_empty() && seen.insert(val.to_string()) {
                            values.push(val.to_string());
                        }
                    }
                }
            }
            DataType::LargeUtf8 => {
                let arr = col
                    .as_any()
                    .downcast_ref::<LargeStringArray>()
                    .expect("LargeUtf8 column must downcast to LargeStringArray");
                for i in 0..arr.len() {
                    if values.len() >= MAX_DICTIONARY_ENTRIES {
                        break 'outer;
                    }
                    if !arr.is_null(i) {
                        let val = arr.value(i).trim();
                        if !val.is_empty() && seen.insert(val.to_string()) {
                            values.push(val.to_string());
                        }
                    }
                }
            }
            _ => {}
        }
    }

    values
}

/// Detect the best expansion strategy based on dictionary value structure.
///
/// Uses heuristics:
/// - If most values are multi-word with consistent token count AND tokens reuse
///   across entries at each position → "combinatorial"
/// - Otherwise → "sample"
fn detect_expansion_strategy(values: &[String]) -> String {
    if values.is_empty() || values.len() < 5 {
        return "sample".to_string();
    }

    // High-cardinality columns (many distinct values) should use sample mode.
    // Combinatorial expansion on names/identifiers produces nonsense combinations.
    // Note: callers typically pass deduplicated values, so uniqueness_ratio ≈ 1.0;
    // the effective threshold is unique_count > 50.
    let unique_count = {
        let mut set = HashSet::new();
        for v in values {
            set.insert(v.as_str());
        }
        set.len()
    };
    if unique_count > 50 {
        return "sample".to_string();
    }

    // Count tokens per value
    let token_counts: Vec<usize> = values
        .iter()
        .map(|v| v.split_whitespace().count())
        .collect();

    // Check if values are multi-word with consistent structure
    let multi_word = token_counts.iter().filter(|&&c| c > 1).count();
    if multi_word as f64 / values.len() as f64 <= 0.8 {
        return "sample".to_string();
    }

    // Check consistent token count
    let mode_count = most_common_count(&token_counts);
    let mode_matches = token_counts.iter().filter(|&&c| c == mode_count).count();
    if mode_matches as f64 / values.len() as f64 <= 0.6 {
        return "sample".to_string();
    }

    // Additional check: verify token reuse at positions (evidence of combinatorial structure).
    // If tokens repeat across entries at the same position, combinatorial makes sense.
    let conforming: Vec<Vec<&str>> = values
        .iter()
        .filter_map(|v| {
            let tokens: Vec<&str> = v.split_whitespace().collect();
            if tokens.len() == mode_count {
                Some(tokens)
            } else {
                None
            }
        })
        .collect();

    if conforming.len() < 5 {
        return "sample".to_string();
    }

    // Check that at least one position has token reuse (not all unique)
    let mut has_reuse = false;
    for pos in 0..mode_count {
        let mut position_tokens = HashSet::new();
        for entry in &conforming {
            position_tokens.insert(entry[pos]);
        }
        // If tokens at this position are fewer than entries, there's reuse
        if position_tokens.len() < conforming.len() {
            has_reuse = true;
            break;
        }
    }

    if has_reuse {
        "combinatorial".to_string()
    } else {
        "sample".to_string()
    }
}

/// Find the most common value in a slice of counts.
fn most_common_count(counts: &[usize]) -> usize {
    let mut freq: HashMap<usize, usize> = HashMap::new();
    for &c in counts {
        *freq.entry(c).or_insert(0) += 1;
    }
    freq.into_iter()
        .max_by_key(|(_, count)| *count)
        .map(|(val, _)| val)
        .unwrap_or(1)
}

/// Aggregated statistics from the behavioral analysis pipeline.
#[derive(Default)]
struct BehavioralStats {
    actors_profiled: usize,
    personas_discovered: usize,
    graphs_discovered: usize,
    actor_namespaces: usize,
}

/// Run the full behavioral analysis pipeline: actor profiling → persona clustering → relationship graphs.
///
/// Populates `table_analyses` with discovered personas and actor relationship specs so that
/// `assemble_data_model` can emit them into the schema.
fn run_behavioral_pipeline(
    tables: &[IngestionResult],
    table_analyses: &mut [TableAnalysis],
    relationships: &[crate::learn::relationships::RelationshipCandidate],
    opts: &ActorsOpts,
    cli: &crate::cli::Cli,
) -> Result<BehavioralStats> {
    use crate::learn::actor_graph::{RelationshipAccumulator, RelationshipDiscoveryConfig};
    use crate::learn::actor_registry::build_actor_registry;
    use crate::learn::behavioral::ActorProfiler;
    use crate::learn::clustering::{ClusteringConfig, discover_personas};
    use crate::learn::schema_assembly::score_actor_column;

    let mut stats = BehavioralStats::default();

    // Validate explicit columns exist in at least one table
    if !opts.explicit_columns.is_empty() {
        let all_columns: HashSet<&str> = tables
            .iter()
            .flat_map(|t| t.schema.fields().iter().map(|f| f.name().as_str()))
            .collect();
        let unknown: Vec<&str> = opts
            .explicit_columns
            .iter()
            .filter(|c| !all_columns.contains(c.as_str()))
            .map(|s| s.as_str())
            .collect();
        if !unknown.is_empty() {
            anyhow::bail!(
                "unknown --actor-column name(s): {}; available columns: {}",
                unknown.join(", "),
                {
                    let mut avail: Vec<&str> = all_columns.into_iter().collect();
                    avail.sort();
                    avail.join(", ")
                }
            );
        }
    }

    // Phase 1: Detect actor columns across all tables (pre-pass for registry)
    let mut all_actor_cols: Vec<(String, Vec<String>)> = Vec::new();
    for table in tables.iter() {
        let actor_cols: Vec<String> = if !opts.explicit_columns.is_empty() {
            let schema_cols: HashSet<&str> = table
                .schema
                .fields()
                .iter()
                .map(|f| f.name().as_str())
                .collect();
            opts.explicit_columns
                .iter()
                .filter(|c| schema_cols.contains(c.as_str()))
                .cloned()
                .collect()
        } else {
            table
                .schema
                .fields()
                .iter()
                .filter(|f| score_actor_column(f.name()) >= 0.6)
                .map(|f| f.name().clone())
                .collect()
        };
        all_actor_cols.push((table.entity.clone(), actor_cols));
    }

    // Phase 1b: Build actor registry for cross-entity unification
    let registry = build_actor_registry(&all_actor_cols, relationships);

    if !cli.quiet && !registry.namespaces.is_empty() {
        eprintln!(
            "    {} {} actor namespace(s) resolved",
            "→".dimmed(),
            registry.namespaces.len(),
        );
        for ns in registry.namespaces.values() {
            let col_strs: Vec<String> = ns
                .columns
                .iter()
                .map(|(e, c)| format!("{}.{}", e, c))
                .collect();
            eprintln!(
                "      {} — {}{}",
                ns.name.cyan(),
                col_strs.join(", "),
                if let Some(ref src) = ns.source_entity {
                    format!(" (source: {})", src)
                } else {
                    String::new()
                },
            );
        }
    }
    for warning in &registry.warnings {
        if !cli.quiet {
            eprintln!("    {} {}", "warn:".yellow(), warning);
        }
        tracing::warn!(%warning, "actor identity resolution");
    }

    stats.actor_namespaces = registry.namespaces.len();

    // Build reverse lookup: (entity, column) → namespace name
    let mut col_to_ns: HashMap<(String, String), String> = HashMap::new();
    for (ns_name, ns) in &registry.namespaces {
        for (entity, col) in &ns.columns {
            col_to_ns.insert((entity.clone(), col.clone()), ns_name.clone());
        }
    }

    // Phase 2a: Mark explicit actor columns on analyses
    for (i, analysis) in table_analyses.iter_mut().enumerate() {
        let actor_cols = &all_actor_cols[i].1;
        if !opts.explicit_columns.is_empty() {
            for col in &mut analysis.columns {
                if actor_cols.contains(&col.name) {
                    col.is_actor_column = true;
                }
            }
        }
    }

    // Phase 2b: Namespace-driven profiling and clustering.
    // For each namespace, pick the primary (first) column as the profiling source.
    // All tables with columns in that namespace share the resulting personas.
    let mut namespace_personas: HashMap<String, Vec<crate::learn::clustering::PersonaSpec>> =
        HashMap::new();
    let mut profiled_namespaces: HashSet<String> = HashSet::new();

    for (i, table) in tables.iter().enumerate() {
        let actor_cols = &all_actor_cols[i].1;
        if actor_cols.is_empty() {
            continue;
        }

        let temporal_col = table
            .schema
            .fields()
            .iter()
            .find(|f| matches!(f.data_type(), DataType::Timestamp(_, _)))
            .map(|f| f.name().clone());

        let feature_cols: Vec<String> = table
            .schema
            .fields()
            .iter()
            .filter(|f| {
                let name = f.name();
                if actor_cols.contains(name) {
                    return false;
                }
                if temporal_col.as_deref() == Some(name.as_str()) {
                    return false;
                }
                matches!(
                    f.data_type(),
                    DataType::Utf8
                        | DataType::LargeUtf8
                        | DataType::Int8
                        | DataType::Int16
                        | DataType::Int32
                        | DataType::Int64
                        | DataType::UInt8
                        | DataType::UInt16
                        | DataType::UInt32
                        | DataType::UInt64
                        | DataType::Float32
                        | DataType::Float64
                )
            })
            .map(|f| f.name().clone())
            .collect();

        for actor_col in actor_cols.iter() {
            // Check if this column's namespace has already been profiled
            let ns_name = col_to_ns.get(&(table.entity.clone(), actor_col.clone()));
            if let Some(ns) = ns_name
                && profiled_namespaces.contains(ns)
            {
                if !cli.quiet {
                    eprintln!(
                        "    {} skipping {}.{} — namespace '{}' already profiled",
                        "→".dimmed(),
                        table.entity.cyan(),
                        actor_col,
                        ns,
                    );
                }
                continue;
            }

            if !cli.quiet {
                let ns_label = ns_name.map_or(String::new(), |n| format!(" [namespace: {}]", n));
                eprintln!(
                    "    {} profiling actors on {}.{}{}",
                    "→".dimmed(),
                    table.entity.cyan(),
                    actor_col,
                    ns_label,
                );
            }

            let mut profiler = ActorProfiler::new(actor_col.clone(), temporal_col.clone());

            let feature_refs: Vec<&str> = feature_cols.iter().map(|s| s.as_str()).collect();
            for batch in &table.batches {
                profiler.observe_batch(batch, &feature_refs);
            }

            let profiles = profiler.finalize();
            if profiles.is_empty() {
                continue;
            }

            stats.actors_profiled += profiles.len();

            if !cli.quiet {
                eprintln!("    {} {} actor(s) profiled", "→".dimmed(), profiles.len(),);
            }

            // Persona clustering
            let mut cluster_config = ClusteringConfig::default();
            if let Some(max_k) = opts.max_personas {
                cluster_config.min_actors = cluster_config.min_actors.min(max_k);
            }

            if let Some(result) = discover_personas(&profiles, &cluster_config) {
                let mut personas = result.personas;
                if let Some(max_k) = opts.max_personas
                    && personas.len() > max_k
                {
                    personas.sort_by(|a, b| {
                        b.weight
                            .partial_cmp(&a.weight)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    personas.truncate(max_k);
                    let total: f64 = personas.iter().map(|p| p.weight).sum();
                    if total > 0.0 {
                        for p in &mut personas {
                            p.weight /= total;
                        }
                    }
                }

                stats.personas_discovered += personas.len();

                if !cli.quiet {
                    eprintln!(
                        "    {} {} persona(s) discovered (silhouette: {:.3})",
                        "→".dimmed(),
                        personas.len(),
                        result.silhouette_score,
                    );
                }

                // Store personas for this namespace so other columns can inherit
                if let Some(ns) = ns_name {
                    profiled_namespaces.insert(ns.clone());
                    namespace_personas.insert(ns.clone(), personas);
                } else {
                    // No namespace — store directly on this table's analysis
                    table_analyses[i].personas = personas;
                }
            }

            // Mark namespace as profiled even without personas
            if let Some(ns) = ns_name {
                profiled_namespaces.insert(ns.clone());
            }
        }
    }

    // Phase 2c: Distribute namespace personas to all tables that reference them.
    // The primary profiling table gets the personas directly; other tables in the
    // same namespace inherit them so the assembled model uses shared personas.
    for (i, table) in tables.iter().enumerate() {
        let actor_cols = &all_actor_cols[i].1;
        for actor_col in actor_cols.iter() {
            if let Some(ns) = col_to_ns.get(&(table.entity.clone(), actor_col.clone()))
                && let Some(personas) = namespace_personas.get(ns)
                && table_analyses[i].personas.is_empty()
            {
                table_analyses[i].personas = personas.clone();
            }
        }
    }

    // Phase 2d: Actor-to-actor relationship discovery (per-table, column pairs)
    for (i, table) in tables.iter().enumerate() {
        let actor_cols = &all_actor_cols[i].1;
        if actor_cols.len() >= 2 {
            let graph_config = RelationshipDiscoveryConfig::default();
            for ci in 0..actor_cols.len() {
                for cj in (ci + 1)..actor_cols.len() {
                    let mut accumulator = RelationshipAccumulator::new(
                        actor_cols[ci].clone(),
                        actor_cols[cj].clone(),
                        table.entity.clone(),
                    );

                    for batch in &table.batches {
                        accumulator.observe_batch(batch);
                    }

                    if let Some(spec) = accumulator.finalize(&graph_config) {
                        if !cli.quiet {
                            eprintln!(
                                "    {} actor graph discovered: {} ({} → {})",
                                "→".dimmed(),
                                spec.name,
                                actor_cols[ci],
                                actor_cols[cj],
                            );
                        }
                        stats.graphs_discovered += 1;
                        table_analyses[i].actor_relationships.push(spec);
                    }
                }
            }
        }
    }

    info!(
        actors = stats.actors_profiled,
        personas = stats.personas_discovered,
        graphs = stats.graphs_discovered,
        "behavioral analysis complete"
    );

    Ok(stats)
}

/// Format a count in human-readable form (e.g., 1.2M, 3.5K).
fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Create a quiet Cli for testing (suppresses output).
    fn quiet_cli() -> crate::cli::Cli {
        use clap::Parser;
        crate::cli::Cli::parse_from(["knit", "--quiet", "validate", "x.toml"])
    }

    #[test]
    fn learn_from_csv_produces_valid_toml() {
        let dir = tempfile::tempdir().unwrap();
        let csv_path = dir.path().join("users.csv");
        let mut f = std::fs::File::create(&csv_path).unwrap();
        writeln!(f, "id,name,age,status").unwrap();
        writeln!(f, "1,Alice,30,active").unwrap();
        writeln!(f, "2,Bob,25,inactive").unwrap();
        writeln!(f, "3,Carol,35,active").unwrap();
        writeln!(f, "4,Dave,28,pending").unwrap();
        writeln!(f, "5,Eve,40,active").unwrap();
        drop(f);

        let output_path = dir.path().join("learned.knit.toml");
        let result = run(
            Some(csv_path.to_str().unwrap()),
            output_path.to_str().unwrap(),
            None,
            None,
            false,
            false,
            &[],
            None,
            None,
            false,
            &quiet_cli(),
        );
        assert!(result.is_ok(), "learn failed: {result:?}");
        assert!(output_path.exists(), "output file not created");

        let content = std::fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("[model]"), "should have [model] section");
        assert!(content.contains("users"), "should reference the table name");
        assert!(
            content.contains("[[entities]]"),
            "should have [[entities]] section"
        );
        assert!(
            content.contains("[[entities.fields]]"),
            "should have fields"
        );

        // Verify the output is valid TOML
        let parsed: toml::Value = toml::from_str(&content).expect("output should be valid TOML");
        assert!(parsed.get("model").is_some());
        assert!(parsed.get("entities").is_some());
    }

    #[test]
    fn learn_from_directory_produces_output() {
        let dir = tempfile::tempdir().unwrap();

        let mut f = std::fs::File::create(dir.path().join("customers.csv")).unwrap();
        writeln!(f, "id,name,email").unwrap();
        writeln!(f, "1,Alice,alice@example.com").unwrap();
        writeln!(f, "2,Bob,bob@example.com").unwrap();
        drop(f);

        let mut f = std::fs::File::create(dir.path().join("orders.csv")).unwrap();
        writeln!(f, "order_id,customer_id,amount").unwrap();
        writeln!(f, "100,1,9.99").unwrap();
        writeln!(f, "101,2,19.50").unwrap();
        writeln!(f, "102,1,5.25").unwrap();
        drop(f);

        let output_path = dir.path().join("blueprint.knit.toml");
        let result = run(
            Some(dir.path().to_str().unwrap()),
            output_path.to_str().unwrap(),
            None,
            None,
            false,
            false,
            &[],
            None,
            None,
            false,
            &quiet_cli(),
        );
        assert!(result.is_ok(), "learn failed: {result:?}");
        assert!(output_path.exists());

        let content = std::fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("customers"));
        assert!(content.contains("orders"));

        // Verify valid TOML
        let _: toml::Value = toml::from_str(&content).expect("output should be valid TOML");
    }

    #[test]
    fn learn_nonexistent_path_errors() {
        let result = run(
            Some("nonexistent_path_12345.csv"),
            "out.toml",
            None,
            None,
            false,
            false,
            &[],
            None,
            None,
            false,
            &quiet_cli(),
        );
        assert!(result.is_err());
    }

    #[test]
    fn learn_table_with_multiple_id_columns_picks_one_pk() {
        let dir = tempfile::tempdir().unwrap();
        // Table with two unique columns ending in "Id" — only one should be PK
        let mut f = std::fs::File::create(dir.path().join("people_historical.csv")).unwrap();
        writeln!(f, "PersonId,PeopleHistoricalId,Name").unwrap();
        for i in 1..=10 {
            writeln!(f, "{},{},Person{}", i, i + 100, i).unwrap();
        }
        drop(f);

        let output_path = dir.path().join("blueprint.knit.toml");
        let result = run(
            Some(dir.path().to_str().unwrap()),
            output_path.to_str().unwrap(),
            None,
            None,
            false,
            false,
            &[],
            None,
            None,
            false,
            &quiet_cli(),
        );
        assert!(result.is_ok(), "learn failed: {result:?}");

        let content = std::fs::read_to_string(&output_path).unwrap();
        let pk_count = content.matches("primary_key = true").count();
        assert_eq!(pk_count, 1, "should have exactly 1 PK, got {pk_count}");
        // Should prefer PeopleHistoricalId (matches table name)
        assert!(
            content.contains("name = \"PeopleHistoricalId\"")
                && content.contains("primary_key = true"),
            "PeopleHistoricalId should be the chosen PK"
        );
    }

    #[test]
    fn is_likely_primary_key_id_column() {
        let profile = ColumnProfile {
            name: "user_id".to_string(),
            data_type: DataType::Int64,
            count: 10,
            null_count: 0,
            null_rate: 0.0,
            empty_string_rate: 0.0,
            distinct_count: Some(10),
            cardinality_ratio: Some(1.0),
            numeric: None,
            string: None,
            temporal: None,
        };
        assert!(is_likely_primary_key(&profile, 10));
    }

    #[test]
    fn unique_non_id_column_is_not_pk() {
        let profile = ColumnProfile {
            name: "name".to_string(),
            data_type: DataType::Utf8,
            count: 5,
            null_count: 0,
            null_rate: 0.0,
            empty_string_rate: 0.0,
            distinct_count: Some(5),
            cardinality_ratio: Some(1.0),
            numeric: None,
            string: None,
            temporal: None,
        };
        assert!(
            !is_likely_primary_key(&profile, 5),
            "unique 'name' column should not be detected as PK"
        );
    }

    #[test]
    fn camel_case_id_detected_as_pk() {
        let profile = ColumnProfile {
            name: "CustomerID".to_string(),
            data_type: DataType::Int64,
            count: 10,
            null_count: 0,
            null_rate: 0.0,
            empty_string_rate: 0.0,
            distinct_count: Some(10),
            cardinality_ratio: Some(1.0),
            numeric: None,
            string: None,
            temporal: None,
        };
        assert!(is_likely_primary_key(&profile, 10));
    }

    #[test]
    fn lowercase_id_suffix_detected_as_pk() {
        let profile = ColumnProfile {
            name: "userid".to_string(),
            data_type: DataType::Int64,
            count: 10,
            null_count: 0,
            null_rate: 0.0,
            empty_string_rate: 0.0,
            distinct_count: Some(10),
            cardinality_ratio: Some(1.0),
            numeric: None,
            string: None,
            temporal: None,
        };
        assert!(is_likely_primary_key(&profile, 10));
    }

    #[test]
    fn word_ending_in_id_not_detected_as_pk() {
        // "valid" ends with "id" but is excluded as a common English word
        let profile = ColumnProfile {
            name: "valid".to_string(),
            data_type: DataType::Int64,
            count: 10,
            null_count: 0,
            null_rate: 0.0,
            empty_string_rate: 0.0,
            distinct_count: Some(10),
            cardinality_ratio: Some(1.0),
            numeric: None,
            string: None,
            temporal: None,
        };
        assert!(!is_likely_primary_key(&profile, 10));
    }

    #[test]
    fn pick_best_pk_prefers_table_name_match() {
        use std::collections::HashSet;
        let columns = vec![
            RelColumn {
                name: "PersonId".to_string(),
                is_primary_key: true,
                distinct_values: HashSet::new(),
                row_count: 10,
                distinct_count: 10,
            },
            RelColumn {
                name: "PeopleHistoricalId".to_string(),
                is_primary_key: true,
                distinct_values: HashSet::new(),
                row_count: 10,
                distinct_count: 10,
            },
        ];
        // "PeopleHistorical_test" → stem "peoplehistorical" matches "PeopleHistoricalId"
        let best = pick_best_pk(&columns, "PeopleHistorical_test");
        assert_eq!(
            best, 1,
            "should prefer PeopleHistoricalId for PeopleHistorical_test"
        );
    }

    #[test]
    fn pick_best_pk_prefers_id_column() {
        use std::collections::HashSet;
        let columns = vec![
            RelColumn {
                name: "id".to_string(),
                is_primary_key: true,
                distinct_values: HashSet::new(),
                row_count: 10,
                distinct_count: 10,
            },
            RelColumn {
                name: "UserId".to_string(),
                is_primary_key: true,
                distinct_values: HashSet::new(),
                row_count: 10,
                distinct_count: 10,
            },
        ];
        let best = pick_best_pk(&columns, "users");
        assert_eq!(best, 0, "should prefer plain 'id' column");
    }

    #[test]
    fn learn_with_actors_flag_produces_actor_entities() {
        let dir = tempfile::tempdir().unwrap();

        // Create a messages table with sender/recipient actor columns
        let csv_path = dir.path().join("messages.csv");
        let mut f = std::fs::File::create(&csv_path).unwrap();
        writeln!(f, "message_id,sender_id,recipient_id,body,status").unwrap();
        // Generate enough data for actor profiling (need ≥4 distinct actors)
        for i in 1..=50 {
            let sender = (i % 8) + 1; // 8 distinct senders
            let recipient = ((i + 3) % 8) + 1;
            let body = format!("Message number {i}");
            let status = if i % 3 == 0 { "read" } else { "unread" };
            writeln!(f, "{i},{sender},{recipient},{body},{status}").unwrap();
        }
        drop(f);

        let output_path = dir.path().join("blueprint.knit.toml");
        let opts = ActorsOpts {
            explicit_columns: vec![],
            max_personas: None,
        };
        let result = run(
            Some(csv_path.to_str().unwrap()),
            output_path.to_str().unwrap(),
            None,
            None,
            false,
            false,
            &[],
            Some(&opts),
            None,
            false,
            &quiet_cli(),
        );
        assert!(result.is_ok(), "learn --actors failed: {result:?}");

        let content = std::fs::read_to_string(&output_path).unwrap();
        // Should detect sender_id and recipient_id as actor columns
        assert!(
            content.contains("actor_column = true"),
            "should mark actor columns: {content}"
        );
    }

    #[test]
    fn learn_with_explicit_actor_column() {
        let dir = tempfile::tempdir().unwrap();

        let csv_path = dir.path().join("events.csv");
        let mut f = std::fs::File::create(&csv_path).unwrap();
        writeln!(f, "event_id,user_id,action,value").unwrap();
        for i in 1..=30 {
            let user = (i % 6) + 1;
            let action = if i % 2 == 0 { "click" } else { "view" };
            writeln!(f, "{i},{user},{action},{}", i * 10).unwrap();
        }
        drop(f);

        let output_path = dir.path().join("blueprint.knit.toml");
        let opts = ActorsOpts {
            explicit_columns: vec!["user_id".to_string()],
            max_personas: None,
        };
        let result = run(
            Some(csv_path.to_str().unwrap()),
            output_path.to_str().unwrap(),
            None,
            None,
            false,
            false,
            &[],
            Some(&opts),
            None,
            false,
            &quiet_cli(),
        );
        assert!(result.is_ok(), "learn --actor-column failed: {result:?}");

        let content = std::fs::read_to_string(&output_path).unwrap();
        assert!(
            content.contains("actor_column = true"),
            "should mark user_id as actor: {content}"
        );
    }

    #[test]
    fn learn_actors_with_incremental_errors() {
        let result = run(
            Some("data.csv"),
            "out.toml",
            None,
            Some("state.json"),
            false,
            false,
            &[],
            Some(&ActorsOpts {
                explicit_columns: vec![],
                max_personas: None,
            }),
            None,
            false,
            &quiet_cli(),
        );
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("not supported with --state"),
            "should error: {msg}"
        );
    }

    #[test]
    fn learn_structured_output_creates_directory() {
        let dir = tempfile::tempdir().unwrap();
        let csv_path = dir.path().join("data.csv");
        let mut f = std::fs::File::create(&csv_path).unwrap();
        writeln!(f, "id,name,age").unwrap();
        writeln!(f, "1,Alice,30").unwrap();
        writeln!(f, "2,Bob,25").unwrap();
        writeln!(f, "3,Carol,35").unwrap();
        drop(f);

        let output_dir = dir.path().join("my_model");
        let result = run(
            Some(csv_path.to_str().unwrap()),
            output_dir.to_str().unwrap(),
            None,
            None,
            false,
            false,
            &[],
            None,
            Some(crate::cli::ModelFormat::Structured),
            false,
            &quiet_cli(),
        );
        assert!(
            result.is_ok(),
            "learn --model-format structured failed: {result:?}"
        );
        assert!(
            output_dir.join("knit.toml").exists(),
            "should have knit.toml"
        );
        assert!(output_dir.join("tables").exists(), "should have tables/");
        assert!(
            output_dir.join("tables").join("data.toml").exists(),
            "should have tables/data.toml"
        );
    }

    // ── detect_field_traits tests ────────────────────────────────────

    fn make_profile(name: &str) -> ColumnProfile {
        ColumnProfile {
            name: name.to_string(),
            data_type: DataType::Utf8,
            count: 100,
            null_count: 0,
            null_rate: 0.0,
            empty_string_rate: 0.0,
            distinct_count: Some(50),
            cardinality_ratio: Some(0.5),
            numeric: None,
            string: None,
            temporal: None,
        }
    }

    fn make_analysis(name: &str) -> ColumnAnalysis {
        ColumnAnalysis::new(name.to_string(), 0.0, 1.0)
    }

    #[test]
    fn traits_cardinality_unique() {
        let mut profile = make_profile("id");
        profile.cardinality_ratio = Some(0.99);
        let analysis = make_analysis("id");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.cardinality, Some(crate::core::Cardinality::Unique));
    }

    #[test]
    fn traits_cardinality_high() {
        let mut profile = make_profile("col");
        profile.cardinality_ratio = Some(0.5);
        let analysis = make_analysis("col");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.cardinality, Some(crate::core::Cardinality::High));
    }

    #[test]
    fn traits_cardinality_medium() {
        let mut profile = make_profile("col");
        profile.cardinality_ratio = Some(0.10);
        let analysis = make_analysis("col");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.cardinality, Some(crate::core::Cardinality::Medium));
    }

    #[test]
    fn traits_cardinality_low() {
        let mut profile = make_profile("col");
        profile.cardinality_ratio = Some(0.005);
        let analysis = make_analysis("col");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.cardinality, Some(crate::core::Cardinality::Low));
    }

    #[test]
    fn traits_semantic_from_inferred_type() {
        let profile = make_profile("email_col");
        let mut analysis = make_analysis("email_col");
        analysis.inferred_type = Some(InferredType::Uuid);
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.semantic.as_deref(), Some("uuid"));
    }

    #[test]
    fn traits_semantic_pattern_overrides_inferred() {
        let profile = make_profile("email_col");
        let mut analysis = make_analysis("email_col");
        analysis.inferred_type = Some(InferredType::Text);
        analysis.string_patterns = vec![(StringPattern::Email, 0.95)];
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.semantic.as_deref(), Some("email"));
    }

    #[test]
    fn traits_pii_detected_for_email() {
        let profile = make_profile("email");
        let mut analysis = make_analysis("email");
        analysis.string_patterns = vec![(StringPattern::Email, 0.9)];
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.pii, Some(true));
    }

    #[test]
    fn traits_pii_not_set_for_non_pii() {
        let profile = make_profile("status");
        let mut analysis = make_analysis("status");
        analysis.inferred_type = Some(InferredType::Categorical);
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.pii, None);
    }

    #[test]
    fn traits_distribution_normal() {
        use crate::learn::profile::{NumericProfile, Percentiles};
        let mut profile = make_profile("score");
        profile.data_type = DataType::Float64;
        // Normal: symmetric (skew≈0), mesokurtic (kurt≈0), IQR/range > 0.55
        profile.numeric = Some(NumericProfile {
            min: 10.0,
            max: 90.0,
            mean: 50.0,
            median: 50.0,
            std_dev: 15.0,
            skewness: 0.1,
            kurtosis: 0.0,
            percentiles: Percentiles {
                p1: 15.0,
                p5: 20.0,
                p10: 25.0,
                p25: 38.0,
                p50: 50.0,
                p75: 62.0,
                p90: 75.0,
                p95: 80.0,
                p99: 85.0,
            },
            max_decimal_places: Some(2),
        });
        let analysis = make_analysis("score");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(
            traits.distribution_shape,
            Some(crate::core::DistributionShape::Normal)
        );
    }

    #[test]
    fn traits_distribution_skewed() {
        use crate::learn::profile::{NumericProfile, Percentiles};
        let mut profile = make_profile("income");
        profile.data_type = DataType::Float64;
        profile.numeric = Some(NumericProfile {
            min: 0.0,
            max: 1000000.0,
            mean: 50000.0,
            median: 35000.0,
            std_dev: 80000.0,
            skewness: 2.5,
            kurtosis: 1.0,
            percentiles: Percentiles {
                p1: 1000.0,
                p5: 5000.0,
                p10: 10000.0,
                p25: 20000.0,
                p50: 35000.0,
                p75: 60000.0,
                p90: 100000.0,
                p95: 150000.0,
                p99: 500000.0,
            },
            max_decimal_places: Some(2),
        });
        let analysis = make_analysis("income");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(
            traits.distribution_shape,
            Some(crate::core::DistributionShape::Skewed)
        );
    }

    #[test]
    fn traits_distribution_long_tail() {
        use crate::learn::profile::{NumericProfile, Percentiles};
        let mut profile = make_profile("views");
        profile.data_type = DataType::Int64;
        profile.numeric = Some(NumericProfile {
            min: 0.0,
            max: 10000000.0,
            mean: 1000.0,
            median: 10.0,
            std_dev: 100000.0,
            skewness: 5.0,
            kurtosis: 50.0,
            percentiles: Percentiles {
                p1: 0.0,
                p5: 0.0,
                p10: 1.0,
                p25: 3.0,
                p50: 10.0,
                p75: 50.0,
                p90: 200.0,
                p95: 1000.0,
                p99: 50000.0,
            },
            max_decimal_places: None,
        });
        let analysis = make_analysis("views");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(
            traits.distribution_shape,
            Some(crate::core::DistributionShape::LongTail)
        );
    }

    #[test]
    fn traits_no_distribution_for_non_numeric() {
        let profile = make_profile("name");
        let analysis = make_analysis("name");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.distribution_shape, None);
    }

    #[test]
    fn traits_semantic_from_arrow_type() {
        let mut profile = make_profile("age");
        profile.data_type = DataType::Int32;
        let analysis = make_analysis("age");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.semantic.as_deref(), Some("integer"));
    }

    #[test]
    fn dictionary_extraction_for_high_cardinality_names() {
        // CSV with 100 unique company names → should extract a dictionary file
        // Uses "company"-like column name to trigger the new faker("company") path
        let dir = tempfile::tempdir().unwrap();
        let csv_path = dir.path().join("organizations.csv");
        let mut f = std::fs::File::create(&csv_path).unwrap();
        writeln!(f, "id,company").unwrap();
        for i in 1..=100 {
            writeln!(f, "{i},Acme Corp Division {i:03}").unwrap();
        }
        drop(f);

        let output_path = dir.path().join("blueprint.knit.toml");
        let result = run(
            Some(csv_path.to_str().unwrap()),
            output_path.to_str().unwrap(),
            None,
            None,
            false,
            false,
            &[],
            None,
            None,
            false,
            &quiet_cli(),
        );
        assert!(result.is_ok(), "learn failed: {result:?}");

        let content = std::fs::read_to_string(&output_path).unwrap();
        // The company column should use a dictionary generator since it has
        // 100 unique values — more than the one_of categorical threshold of 50
        assert!(
            content.contains("type = \"dictionary\""),
            "should extract dictionary for high-cardinality string column; got:\n{content}"
        );
        // Dictionary file should exist
        let dict_files: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "txt"))
            .collect();
        assert!(
            !dict_files.is_empty(),
            "should have created at least one .dict.txt file"
        );
    }

    #[test]
    fn dictionary_extraction_for_truncated_oneof() {
        // CSV with 250 unique categories → OneOf truncates at 200 → dictionary extraction
        // Source has MORE unique values than the 200 cap, confirming truncation.
        let dir = tempfile::tempdir().unwrap();
        let csv_path = dir.path().join("products.csv");
        let mut f = std::fs::File::create(&csv_path).unwrap();
        writeln!(f, "id,product_type").unwrap();
        // 250 unique product types, each appearing ~4 times (1000 rows total)
        for i in 1..=1000 {
            let product_type = format!("ProductType_{:03}", (i % 250) + 1);
            writeln!(f, "{i},{product_type}").unwrap();
        }
        drop(f);

        let output_path = dir.path().join("blueprint.knit.toml");
        let result = run(
            Some(csv_path.to_str().unwrap()),
            output_path.to_str().unwrap(),
            None,
            None,
            false,
            false,
            &[],
            None,
            None,
            false,
            &quiet_cli(),
        );
        assert!(result.is_ok(), "learn failed: {result:?}");

        let content = std::fs::read_to_string(&output_path).unwrap();
        // With 250 unique values, the OneOf cap of 200 truncated data.
        // Dictionary extraction should replace it since source has more values.
        assert!(
            content.contains("type = \"dictionary\""),
            "should extract dictionary for truncated OneOf; got:\n{content}"
        );
    }

    #[test]
    fn no_dictionary_extraction_for_exact_200_oneof() {
        // CSV with exactly 200 unique categories → OneOf is NOT truncated → keep OneOf
        let dir = tempfile::tempdir().unwrap();
        let csv_path = dir.path().join("categories.csv");
        let mut f = std::fs::File::create(&csv_path).unwrap();
        writeln!(f, "id,category").unwrap();
        // Exactly 200 unique categories, each appearing 5 times (1000 rows total)
        for i in 1..=1000 {
            let category = format!("Category_{:03}", (i % 200) + 1);
            writeln!(f, "{i},{category}").unwrap();
        }
        drop(f);

        let output_path = dir.path().join("blueprint.knit.toml");
        let result = run(
            Some(csv_path.to_str().unwrap()),
            output_path.to_str().unwrap(),
            None,
            None,
            false,
            false,
            &[],
            None,
            None,
            false,
            &quiet_cli(),
        );
        assert!(result.is_ok(), "learn failed: {result:?}");

        let content = std::fs::read_to_string(&output_path).unwrap();
        // With exactly 200 unique values, the OneOf was NOT truncated.
        // Should keep the weighted OneOf, not extract a dictionary.
        assert!(
            content.contains("type = \"one_of\""),
            "should keep OneOf for non-truncated 200-category column; got:\n{content}"
        );
    }

    #[test]
    fn detect_sort_order_ascending_int() {
        use arrow::array::Int64Array;
        use arrow::datatypes::{Field, Schema};
        use std::sync::Arc;

        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int64, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int64Array::from(vec![1, 2, 3, 4, 5]))],
        )
        .unwrap();

        let mut col = ColumnAnalysis::new("id".to_string(), 0.0, 1.0);
        col.categorical_weights = Some(vec![("1".into(), 0.2), ("2".into(), 0.2)]);
        let cols = vec![col];
        let result = super::detect_sort_order(&batch, &cols);
        assert!(result.is_some(), "should detect sorted int column");
        let so = result.unwrap();
        assert_eq!(so.column, "id");
        assert_eq!(so.direction, crate::core::SortDirection::Asc);
    }

    #[test]
    fn detect_sort_order_descending_float() {
        use arrow::array::Float64Array;
        use arrow::datatypes::{Field, Schema};
        use std::sync::Arc;

        let schema = Arc::new(Schema::new(vec![Field::new("val", DataType::Float64, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Float64Array::from(vec![9.0, 7.5, 3.2, 1.0]))],
        )
        .unwrap();

        let mut col = ColumnAnalysis::new("val".to_string(), 0.0, 1.0);
        // Mark as numeric candidate so detect_sort_order considers it
        col.categorical_weights = Some(vec![("9.0".into(), 0.25), ("7.5".into(), 0.25)]);
        let result = super::detect_sort_order(&batch, &[col]);
        assert!(result.is_some(), "should detect descending float column");
        let so = result.unwrap();
        assert_eq!(so.column, "val");
        assert_eq!(so.direction, crate::core::SortDirection::Desc);
    }

    #[test]
    fn detect_sort_order_unsorted_returns_none() {
        use arrow::array::Int64Array;
        use arrow::datatypes::{Field, Schema};
        use std::sync::Arc;

        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int64Array::from(vec![3, 1, 4, 1, 5]))],
        )
        .unwrap();

        let mut col = ColumnAnalysis::new("x".to_string(), 0.0, 1.0);
        col.categorical_weights = Some(vec![("3".into(), 0.2)]);
        let cols = vec![col];
        let result = super::detect_sort_order(&batch, &cols);
        assert!(result.is_none(), "unsorted column should return None");
    }

    #[test]
    fn detect_sort_order_too_few_rows_returns_none() {
        use arrow::array::Int64Array;
        use arrow::datatypes::{Field, Schema};
        use std::sync::Arc;

        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int64, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int64Array::from(vec![1, 2]))],
        )
        .unwrap();

        let cols = vec![ColumnAnalysis::new("x".to_string(), 0.0, 1.0)];
        let result = super::detect_sort_order(&batch, &cols);
        assert!(result.is_none(), "fewer than 3 rows should return None");
    }

    #[test]
    fn detect_range_constraints_from_numeric_column() {
        use arrow::array::Float64Array;
        use arrow::datatypes::{Field, Schema};
        use std::sync::Arc;

        let schema = Arc::new(Schema::new(vec![
            Field::new("price", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Float64Array::from(vec![10.0, 20.0, 30.0, 50.0]))],
        )
        .unwrap();

        let mut col = ColumnAnalysis::new("price".to_string(), 0.0, 1.0);
        col.categorical_weights = Some(vec![("10".into(), 0.25)]);
        let constraints = super::detect_column_constraints(&batch, &[col]);

        let range = constraints.iter().find(|c| matches!(c, crate::core::Constraint::Range { field, .. } if field == "price"));
        assert!(range.is_some(), "should detect range constraint for numeric column");
        if let Some(crate::core::Constraint::Range { min, max, .. }) = range {
            assert_eq!(*min, Some(crate::core::Value::Float(10.0)));
            assert_eq!(*max, Some(crate::core::Value::Float(50.0)));
        }
    }

    #[test]
    fn detect_ordering_constraint_between_columns() {
        use arrow::array::Float64Array;
        use arrow::datatypes::{Field, Schema};
        use std::sync::Arc;

        let schema = Arc::new(Schema::new(vec![
            Field::new("low", DataType::Float64, false),
            Field::new("high", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0])),
                Arc::new(Float64Array::from(vec![5.0, 6.0, 7.0])),
            ],
        )
        .unwrap();

        let mut col_low = ColumnAnalysis::new("low".to_string(), 0.0, 1.0);
        col_low.categorical_weights = Some(vec![("1".into(), 0.33)]);
        let mut col_high = ColumnAnalysis::new("high".to_string(), 0.0, 1.0);
        col_high.categorical_weights = Some(vec![("5".into(), 0.33)]);

        let constraints = super::detect_column_constraints(&batch, &[col_low, col_high]);

        let check = constraints.iter().find(|c| {
            matches!(c, crate::core::Constraint::Check { expr } if expr.contains("<="))
        });
        assert!(check.is_some(), "should detect ordering constraint low <= high");
    }

    #[test]
    fn no_ordering_constraint_for_unrelated_columns() {
        use arrow::array::Float64Array;
        use arrow::datatypes::{Field, Schema};
        use std::sync::Arc;

        let schema = Arc::new(Schema::new(vec![
            Field::new("a", DataType::Float64, false),
            Field::new("b", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Float64Array::from(vec![5.0, 1.0, 8.0])),
                Arc::new(Float64Array::from(vec![2.0, 9.0, 3.0])),
            ],
        )
        .unwrap();

        let mut col_a = ColumnAnalysis::new("a".to_string(), 0.0, 1.0);
        col_a.categorical_weights = Some(vec![("5".into(), 0.33)]);
        let mut col_b = ColumnAnalysis::new("b".to_string(), 0.0, 1.0);
        col_b.categorical_weights = Some(vec![("2".into(), 0.33)]);

        let constraints = super::detect_column_constraints(&batch, &[col_a, col_b]);

        let check = constraints.iter().find(|c| {
            matches!(c, crate::core::Constraint::Check { expr } if expr.contains("<="))
        });
        assert!(check.is_none(), "unrelated columns should not produce ordering constraint");
    }
}
