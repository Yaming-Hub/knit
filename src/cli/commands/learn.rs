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

use crate::learn::correlation::detect_correlations;
use crate::learn::fitting::{fit_categorical, fit_distribution, FitResult};
use crate::learn::ingest::{self, IngestionResult};
use crate::learn::profile::{compute_profiles, ColumnProfile};
use crate::learn::relationships::{detect_relationships, RelColumn, TableProfile};
use crate::learn::schema_assembly::{assemble_data_model, ColumnAnalysis, TableAnalysis};
use crate::learn::temporal::{detect_temporal_pattern, TemporalPatternSpec};
use crate::learn::type_inference::{infer_type, InferredType, StringPattern};

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
        None => {
            let p = Path::new(output);
            p.extension().is_none() || p.is_dir()
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
                format!("failed to clean stale tables directory: {}", tables_dir.display())
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
    let source = source.unwrap();
    run_batch(source, output, sample, &entity_filter, actors_opts, model_format, review, cli)
}

/// Batch mode: load all data, profile, fit, emit blueprint (original behavior).
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

    // 5b. Extract dictionaries for high-cardinality string columns
    let use_structured = resolve_use_structured(output, model_format);
    let output_dir = resolve_asset_dir(output, use_structured);
    let dict_count = extract_dictionaries(&mut data_model, &tables, &output_dir, cli.quiet)?;

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
        let decisions = if let Some(logger) = crate::decision::global_logger() {
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
        let overrides = crate::learn::review::interactive_review(
            &mut data_model,
            &all_decisions,
            cli.quiet,
        );
        if overrides > 0 && !cli.quiet {
            eprintln!();
        }
    }

    // 6. Write output (flat TOML or structured directory)
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
    let mut state = if state_path.exists() {
        LearnState::load(state_path)
            .map_err(|e| anyhow::anyhow!("failed to load state: {e}"))?
            .expect("load() should return Some when file exists")
    } else {
        if finalize {
            anyhow::bail!("state file does not exist: {state_file}");
        }
        LearnState::new(42)
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
        let overrides = crate::learn::review::interactive_review(
            &mut data_model,
            &all_decisions,
            cli.quiet,
        );
        if overrides > 0 && !cli.quiet {
            eprintln!();
        }
    }

    // Write output (flat or structured)
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
        Ok(vec![IngestionResult {
            entity,
            schema,
            batches,
            companion: None,
            companion_path: None,
            source_layout: None,
            partition_by: None,
            partition_values: Vec::new(),
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
        if col_analysis.distribution.is_some() {
            if let Some(logger) = crate::decision::global_logger() {
                logger.set_last_context(
                    crate::decision::DecisionKind::DistributionFit,
                    &table.entity,
                    &profile.name,
                );
            }
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
    if let Some(ref str_prof) = profile.string {
        if !str_prof.patterns.is_empty() {
            // Patterns are stored as (pattern, match_rate) — not the same as top_values.
            // We don't have top-k in batch profiling, so skip for now.
        }
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
                DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64
                | DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
                    Some("integer".to_string())
                }
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
    use arrow::array::{as_string_array, AsArray};
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
    use arrow::array::{as_string_array, AsArray};
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
                || dict_path.components().any(|c| {
                    matches!(c, std::path::Component::ParentDir)
                })
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
                std::fs::create_dir_all(parent).with_context(|| {
                    format!("failed to create directory {}", parent.display())
                })?;
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
            // Check if this field uses a fallback faker generator that would benefit
            // from dictionary extraction. This includes "word" (generic fallback) and
            // "name" (detected from capitalized multi-word patterns) since the actual
            // source values are more domain-specific than faker output.
            let is_extractable_faker = matches!(
                &field.generator,
                Some(crate::core::GeneratorSpec::Faker { method, .. })
                    if method == "word" || method == "name" || method == "product_name"
            );
            if !is_extractable_faker {
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

            // Write dictionary file (sanitize filename components)
            let dict_filename = format!(
                "{}_{}.dict.txt",
                sanitize_filename_component(&entity.name),
                sanitize_filename_component(&field.name)
            );
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

            // Determine expansion strategy based on value structure
            let owned_clean: Vec<String> = clean_values.iter().map(|s| s.to_string()).collect();
            let expansion = detect_expansion_strategy(&owned_clean);

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
            // Check for extractable faker generators (same criteria as batch mode)
            let is_extractable_faker = matches!(
                &field.generator,
                Some(crate::core::GeneratorSpec::Faker { method, .. })
                    if method == "word" || method == "name" || method == "product_name"
            );
            if !is_extractable_faker {
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
            let dict_filename = format!(
                "{}_{}.dict.txt",
                sanitize_filename_component(&entity.name),
                sanitize_filename_component(&field.name)
            );
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
                let arr = col.as_any().downcast_ref::<StringArray>().unwrap();
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
                let arr = col.as_any().downcast_ref::<LargeStringArray>().unwrap();
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
    use crate::learn::clustering::{discover_personas, ClusteringConfig};
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
            if let Some(ns) = ns_name {
                if profiled_namespaces.contains(ns) {
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
                if let Some(max_k) = opts.max_personas {
                    if personas.len() > max_k {
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
            if let Some(ns) = col_to_ns.get(&(table.entity.clone(), actor_col.clone())) {
                if let Some(personas) = namespace_personas.get(ns) {
                    if table_analyses[i].personas.is_empty() {
                        table_analyses[i].personas = personas.clone();
                    }
                }
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
        assert!(result.is_ok(), "learn --model-format structured failed: {result:?}");
        assert!(output_dir.join("knit.toml").exists(), "should have knit.toml");
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
                p1: 15.0, p5: 20.0, p10: 25.0, p25: 38.0, p50: 50.0,
                p75: 62.0, p90: 75.0, p95: 80.0, p99: 85.0,
            },
            max_decimal_places: Some(2),
        });
        let analysis = make_analysis("score");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.distribution_shape, Some(crate::core::DistributionShape::Normal));
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
                p1: 1000.0, p5: 5000.0, p10: 10000.0, p25: 20000.0, p50: 35000.0,
                p75: 60000.0, p90: 100000.0, p95: 150000.0, p99: 500000.0,
            },
            max_decimal_places: Some(2),
        });
        let analysis = make_analysis("income");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.distribution_shape, Some(crate::core::DistributionShape::Skewed));
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
                p1: 0.0, p5: 0.0, p10: 1.0, p25: 3.0, p50: 10.0,
                p75: 50.0, p90: 200.0, p95: 1000.0, p99: 50000.0,
            },
            max_decimal_places: None,
        });
        let analysis = make_analysis("views");
        let traits = detect_field_traits(&profile, &analysis);
        assert_eq!(traits.distribution_shape, Some(crate::core::DistributionShape::LongTail));
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