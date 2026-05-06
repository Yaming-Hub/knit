//! `knit learn` — infer a Weave schema from existing data.
//!
//! Reads data files (CSV, Parquet, JSON/JSONL) or directories,
//! profiles columns, fits distributions, detects relationships and
//! correlations, and assembles a complete Weave schema.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result};
use arrow::array::{Array, AsArray, LargeStringArray, StringArray};
use arrow::compute::concat_batches;
use arrow::datatypes::{DataType, TimeUnit};
use arrow::record_batch::RecordBatch;
use colored::Colorize;
use indicatif::{ProgressBar, ProgressStyle};
use serde::Serialize;
use serde_json;
use tracing::{debug, info};

use knit_learn::correlation::detect_correlations;
use knit_learn::fitting::{fit_categorical, fit_distribution, FitResult};
use knit_learn::ingest::{self, IngestionResult};
use knit_learn::profile::{compute_profiles, ColumnProfile};
use knit_learn::relationships::{detect_relationships, RelColumn, TableProfile};
use knit_learn::schema_assembly::{assemble_data_model, ColumnAnalysis, TableAnalysis};
use knit_learn::temporal::{detect_temporal_pattern, TemporalPatternSpec};
use knit_learn::type_inference::{infer_type, InferredType, StringPattern};

/// Intermediate struct for TOML serialization matching the Weave schema format.
#[derive(Serialize)]
struct RawOutputSchema {
    schema_version: String,
    model: RawOutputModel,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    entities: Vec<knit_core::Entity>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    relationships: Vec<knit_core::Relationship>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    correlations: Vec<knit_core::Correlation>,
}

/// Model metadata for TOML output.
#[derive(Serialize)]
struct RawOutputModel {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// Run the learn command: ingest data, analyse, and write a Weave schema.
///
/// `source` is a path to a single data file or a directory of files.
/// `output` is the path where the generated schema will be written.
/// `sample` limits each entity to at most N rows for faster profiling.
/// `state_path` enables incremental mode when provided.
/// `finalize` emits schema from existing state without processing new data.
/// `strict` errors on duplicate source paths (default: warn).
pub fn run(
    source: Option<&str>,
    output: &str,
    sample: Option<usize>,
    state_path: Option<&str>,
    finalize: bool,
    strict: bool,
    cli: &crate::Cli,
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

    // Route: incremental mode if --state is provided
    if let Some(state_file) = state_path {
        return run_incremental(source, output, sample, state_file, finalize, strict, cli);
    }

    // Batch mode (original behavior)
    let source = source.unwrap();
    run_batch(source, output, sample, cli)
}

/// Batch mode: load all data, profile, fit, emit schema (original behavior).
fn run_batch(source: &str, output: &str, sample: Option<usize>, cli: &crate::Cli) -> Result<()> {
    let source_path = Path::new(source);
    anyhow::ensure!(
        source_path.exists(),
        "source path does not exist: {}",
        source
    );

    if !cli.quiet {
        eprintln!(
            "{} Analysing {}",
            "learn:".green().bold(),
            source.cyan()
        );
        if let Some(n) = sample {
            eprintln!("  {} sampling first {} rows per entity", "→".dimmed(), n);
        }
    }

    // 1. Ingest
    let tables = ingest_source(source_path, sample)
        .with_context(|| format!("failed to ingest data from {source}"))?;

    if tables.is_empty() {
        anyhow::bail!("no supported data files found in {source}");
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

    // 5. Assemble data model
    let model_name = Path::new(output)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("learned")
        .to_string();
    let mut data_model = assemble_data_model(&model_name, &table_analyses);

    // 5b. Extract dictionaries for high-cardinality string columns
    let output_dir = Path::new(output)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let dict_count = extract_dictionaries(
        &mut data_model,
        &tables,
        output_dir,
        cli.quiet,
    )?;

    // 6. Serialize to TOML and write output
    // Wrap in RawSchema structure for proper TOML output
    let raw = RawOutputSchema {
        schema_version: data_model.schema_version.clone(),
        model: RawOutputModel {
            name: data_model.name.clone(),
            description: data_model.description.clone(),
        },
        entities: data_model.entities.clone(),
        relationships: data_model.relationships.clone(),
        correlations: data_model.correlations.clone(),
    };
    let schema_text = toml::to_string_pretty(&raw)
        .context("failed to serialize schema to TOML")?;

    // Add header comment
    let header = "# Auto-generated Weave schema\n# Generated by knit learn\n\n";
    let full_output = format!("{header}{schema_text}");

    std::fs::write(output, &full_output)
        .with_context(|| format!("failed to write output to {output}"))?;

    // 7. Summary
    let total_rels = relationships.len();
    let total_corrs: usize = table_analyses.iter().map(|t| t.correlations.len()).sum();

    if cli.json {
        let summary = serde_json::json!({
            "event": "complete",
            "output": output,
            "tables": table_analyses.len(),
            "columns": total_columns,
            "relationships": total_rels,
            "correlations": total_corrs,
            "dictionaries": dict_count,
        });
        println!("{}", summary);
    } else if !cli.quiet {
        eprintln!(
            "\n{} Wrote {} — {} table(s), {} column(s), {} relationship(s), {} correlation(s), {} dictionary(ies)",
            "✓".green().bold(),
            output.cyan(),
            table_analyses.len(),
            total_columns,
            total_rels,
            total_corrs,
            dict_count,
        );
    }

    Ok(())
}

/// Incremental mode: load/create state, ingest data, optionally finalize.
fn run_incremental(
    source: Option<&str>,
    output: &str,
    sample: Option<usize>,
    state_file: &str,
    finalize: bool,
    strict: bool,
    cli: &crate::Cli,
) -> Result<()> {
    use knit_learn::incremental::ingest_batches_to_state;
    use knit_learn::streaming::LearnState;

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

        let tables = ingest_source(source_path, sample)
            .with_context(|| format!("failed to ingest data from {source}"))?;

        if tables.is_empty() {
            anyhow::bail!("no supported data files found in {source}");
        }

        for table in &tables {
            let source_id = format!("{}:{}", source, table.entity);
            let is_dup = ingest_batches_to_state(
                &mut state,
                &table.entity,
                &table.batches,
                &source_id,
            );
            if is_dup {
                let msg = format!("duplicate source: {source_id}");
                if strict {
                    anyhow::bail!("{msg}");
                } else if !cli.quiet {
                    eprintln!("  {} {}", "⚠".yellow(), msg);
                }
            }
        }

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
        // Only emit schema if -o was explicitly provided or --finalize
        // Since output has a default, we always emit when finalize is set
        // or when source is provided with --state (update + finalize in one pass)
        if finalize {
            emit_schema_from_state(&state, output, cli)?;
        }
    }

    Ok(())
}

/// Emit a schema from the accumulated state.
fn emit_schema_from_state(
    state: &knit_learn::streaming::LearnState,
    output: &str,
    cli: &crate::Cli,
) -> Result<()> {
    use knit_learn::incremental::finalize_state;
    use knit_learn::schema_assembly::assemble_data_model;

    if !cli.quiet {
        eprintln!(
            "  {} Finalizing schema from state ({} table(s))",
            "→".dimmed(),
            state.tables.len(),
        );
    }

    let table_analyses = finalize_state(state);
    let model_name = Path::new(output)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("learned")
        .to_string();
    let data_model = assemble_data_model(&model_name, &table_analyses);

    // Serialize to TOML
    let raw = RawOutputSchema {
        schema_version: data_model.schema_version.clone(),
        model: RawOutputModel {
            name: data_model.name.clone(),
            description: data_model.description.clone(),
        },
        entities: data_model.entities.clone(),
        relationships: data_model.relationships.clone(),
        correlations: data_model.correlations.clone(),
    };
    let schema_text = toml::to_string_pretty(&raw)
        .context("failed to serialize schema to TOML")?;

    let header = "# Auto-generated Weave schema\n# Generated by knit learn (incremental)\n\n";
    let full_output = format!("{header}{schema_text}");

    std::fs::write(output, &full_output)
        .with_context(|| format!("failed to write output to {output}"))?;

    if !cli.quiet {
        eprintln!(
            "\n{} Wrote {} — {} table(s), {} column(s)",
            "✓".green().bold(),
            output.cyan(),
            table_analyses.len(),
            table_analyses.iter().map(|t| t.columns.len()).sum::<usize>(),
        );
    }

    Ok(())
}

/// Ingest data from a file or directory into per-table batches.
fn ingest_source(path: &Path, max_rows: Option<usize>) -> Result<Vec<IngestionResult>> {
    if path.is_dir() {
        info!(dir = %path.display(), "ingesting directory");
        ingest::ingest_directory_with_limit(path, max_rows)
            .map_err(|e| anyhow::anyhow!("{e}"))
    } else {
        info!(file = %path.display(), "ingesting single file");
        let entity = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("data")
            .to_string();
        let batches = ingest::read_auto_with_limit(path, max_rows)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let schema = batches
            .first()
            .map(|b| b.schema())
            .ok_or_else(|| anyhow::anyhow!("file produced no data"))?;
        Ok(vec![IngestionResult {
            entity,
            schema,
            batches,
        }])
    }
}

/// Analyse a single table: profile, fit distributions, detect patterns.
///
/// Returns a `TableAnalysis` for schema assembly and a `TableProfile` for
/// cross-table relationship detection.
fn analyse_table(table: &IngestionResult) -> Result<(TableAnalysis, TableProfile)> {
    let profiles = compute_profiles(&table.batches)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("profiling failed")?;

    let combined = concat_batches(&table.schema, &table.batches)
        .context("failed to concatenate batches")?;
    let row_count = combined.num_rows() as u64;

    let mut col_analyses = Vec::with_capacity(profiles.len());
    let mut rel_columns = Vec::new();

    for profile in &profiles {
        let col_analysis = analyse_column(profile, &combined);
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

    let analysis = TableAnalysis::new(
        table.entity.clone(),
        col_analyses,
        row_count,
    );

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
                let true_count = (0..ba.len()).filter(|&i| !ba.is_null(i) && ba.value(i)).count();
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
                    let str_values: Vec<String> = values.iter().map(|v| format!("{}", *v as i64)).collect();
                    let cat_fit = fit_categorical(&str_values);
                    let mut weights: Vec<(String, f64)> = cat_fit.weights.into_iter().collect();
                    weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
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
            string_patterns.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

            match inference.inferred_type {
                InferredType::Categorical => {
                    let owned: Vec<String> = refs.iter().filter_map(|s| s.map(String::from)).collect();
                    let cat_fit = fit_categorical(&owned);
                    let mut weights: Vec<(String, f64)> = cat_fit.weights.into_iter().collect();
                    weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
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
                        is_integer_valued = matches!(inference.inferred_type, InferredType::Integer);
                        distribution = fit_distribution(&nums);
                    }
                    // For low-cardinality numeric strings, also capture categorical weights
                    // so the generator can prefer exact value reproduction over distribution
                    let owned: Vec<String> = refs.iter().filter_map(|s| s.map(String::from)).collect();
                    let distinct: HashSet<&str> = owned.iter().map(|s| s.as_str()).collect();
                    if distinct.len() <= 50 {
                        let cat_fit = fit_categorical(&owned);
                        let mut weights: Vec<(String, f64)> = cat_fit.weights.into_iter().collect();
                        weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                        categorical_weights = Some(weights);
                    }
                    inferred_type = Some(inference.inferred_type);
                }
                ref other => {
                    // For string-detected dates, check if values contain time info
                    if matches!(other, InferredType::Date(_)) {
                        let non_null_refs: Vec<&str> = refs.iter()
                            .filter_map(|s| *s)
                            .filter(|v| !v.is_empty())
                            .collect();
                        let time_count = non_null_refs.iter()
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
                    weights.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                    categorical_weights = Some(weights);
                }
            }
        }
    }

    let mut ca = ColumnAnalysis::new(profile.name.clone(), profile.null_rate, confidence);
    ca.distribution = distribution;
    ca.temporal_pattern = temporal_pattern;
    ca.categorical_weights = categorical_weights;
    ca.inferred_type = inferred_type;
    ca.string_patterns = string_patterns;
    ca.is_integer_valued = is_integer_valued;
    // Timestamp types have time-of-day; Date32/Date64 are date-only; string dates checked above
    ca.has_time_component = has_time_component || matches!(
        profile.data_type,
        DataType::Timestamp(_, _)
    );
    ca.temporal_range = temporal_range;
    ca.source_arrow_type = Some(profile.data_type.clone());
    ca.max_decimal_places = profile.numeric.as_ref().and_then(|n| n.max_decimal_places);
    ca
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
                if !a.is_null(i) { out.push(a.value(i) as f64); }
            }
        }
        DataType::Int16 => {
            let a = array.as_primitive::<arrow::datatypes::Int16Type>();
            for i in 0..a.len() {
                if !a.is_null(i) { out.push(a.value(i) as f64); }
            }
        }
        DataType::Int32 => {
            let a = array.as_primitive::<arrow::datatypes::Int32Type>();
            for i in 0..a.len() {
                if !a.is_null(i) { out.push(a.value(i) as f64); }
            }
        }
        DataType::Int64 => {
            let a = array.as_primitive::<arrow::datatypes::Int64Type>();
            for i in 0..a.len() {
                if !a.is_null(i) { out.push(a.value(i) as f64); }
            }
        }
        DataType::UInt8 => {
            let a = array.as_primitive::<arrow::datatypes::UInt8Type>();
            for i in 0..a.len() {
                if !a.is_null(i) { out.push(a.value(i) as f64); }
            }
        }
        DataType::UInt16 => {
            let a = array.as_primitive::<arrow::datatypes::UInt16Type>();
            for i in 0..a.len() {
                if !a.is_null(i) { out.push(a.value(i) as f64); }
            }
        }
        DataType::UInt32 => {
            let a = array.as_primitive::<arrow::datatypes::UInt32Type>();
            for i in 0..a.len() {
                if !a.is_null(i) { out.push(a.value(i) as f64); }
            }
        }
        DataType::UInt64 => {
            let a = array.as_primitive::<arrow::datatypes::UInt64Type>();
            for i in 0..a.len() {
                if !a.is_null(i) { out.push(a.value(i) as f64); }
            }
        }
        DataType::Float32 => {
            let a = array.as_primitive::<arrow::datatypes::Float32Type>();
            for i in 0..a.len() {
                if !a.is_null(i) { out.push(a.value(i) as f64); }
            }
        }
        DataType::Float64 => {
            let a = array.as_primitive::<arrow::datatypes::Float64Type>();
            for i in 0..a.len() {
                if !a.is_null(i) { out.push(a.value(i)); }
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
                            if !a.is_null(i) { out.push(a.value(i) as f64 / divisor); }
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
                    .map(|i| if a.is_null(i) { None } else { Some(a.value(i).to_string()) })
                    .collect()
            } else {
                Vec::new()
            }
        }
        DataType::LargeUtf8 => {
            if let Some(a) = col.as_any().downcast_ref::<LargeStringArray>() {
                (0..a.len())
                    .map(|i| if a.is_null(i) { None } else { Some(a.value(i).to_string()) })
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
                    items.push(serde_json::Value::String(
                        format!("{}", formatter.value(j)),
                    ));
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
                            map.insert(key, serde_json::Value::String(str_arr.value(j).to_string()));
                        }
                    }
                }
                _ => {
                    let options = arrow::util::display::FormatOptions::default();
                    if let Ok(formatter) = arrow::util::display::ArrayFormatter::try_new(values.as_ref(), &options) {
                        for (j, key) in (start..end).zip(key_strs) {
                            map.insert(key, serde_json::Value::String(format!("{}", formatter.value(j))));
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
                        if set.len() >= cap { break; }
                    }
                }
            }
        }
        DataType::LargeUtf8 => {
            if let Some(a) = col.as_any().downcast_ref::<LargeStringArray>() {
                for i in 0..a.len() {
                    if !a.is_null(i) {
                        set.insert(a.value(i).to_string());
                        if set.len() >= cap { break; }
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
    let has_camel_id = (profile.name.ends_with("Id") || profile.name.ends_with("ID"))
        && profile.name.len() > 2;
    // Also support all-lowercase "id" suffix (e.g. "userid", "customerid") but exclude
    // common English words that happen to end in "id"
    let has_lower_id = name_lower.ends_with("id")
        && name_lower.len() > 2
        && !matches!(
            name_lower.as_str(),
            "valid" | "invalid" | "rapid" | "timid" | "vivid" | "stupid"
                | "hybrid" | "morbid" | "orchid" | "fluid" | "void" | "android"
                | "paid" | "said" | "laid"
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

/// Extract dictionaries for high-cardinality string columns.
///
/// Walks the assembled data model looking for fields with `Faker { method: "word" }`
/// generators (the fallback for free-text columns). For each such field, extracts
/// unique non-null string values from the source data, writes them to a `.dict.txt`
/// file, and replaces the generator with `Dictionary { file, expansion }`.
///
/// Returns the number of dictionary files written.
fn extract_dictionaries(
    model: &mut knit_core::DataModel,
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
                Some(knit_core::GeneratorSpec::Faker { method, .. })
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
            let mut file = std::fs::File::create(&dict_path)
                .with_context(|| format!("failed to create dictionary file '{}'", dict_path.display()))?;

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
            field.generator = Some(knit_core::GeneratorSpec::Dictionary {
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
    let token_counts: Vec<usize> = values.iter().map(|v| v.split_whitespace().count()).collect();

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
            if tokens.len() == mode_count { Some(tokens) } else { None }
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

    if has_reuse { "combinatorial".to_string() } else { "sample".to_string() }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Create a quiet Cli for testing (suppresses output).
    fn quiet_cli() -> crate::Cli {
        use clap::Parser;
        crate::Cli::parse_from(["knit", "--quiet", "validate", "x.toml"])
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

        let output_path = dir.path().join("learned.weave.toml");
        let result = run(
            Some(csv_path.to_str().unwrap()),
            output_path.to_str().unwrap(),
            None,
            None,
            false,
            false,
            &quiet_cli(),
        );
        assert!(result.is_ok(), "learn failed: {result:?}");
        assert!(output_path.exists(), "output file not created");

        let content = std::fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("[model]"), "should have [model] section");
        assert!(content.contains("users"), "should reference the table name");
        assert!(content.contains("[[entities]]"), "should have [[entities]] section");
        assert!(content.contains("[[entities.fields]]"), "should have fields");

        // Verify the output is valid TOML
        let parsed: toml::Value = toml::from_str(&content)
            .expect("output should be valid TOML");
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

        let output_path = dir.path().join("schema.weave.toml");
        let result = run(
            Some(dir.path().to_str().unwrap()),
            output_path.to_str().unwrap(),
            None,
            None,
            false,
            false,
            &quiet_cli(),
        );
        assert!(result.is_ok(), "learn failed: {result:?}");
        assert!(output_path.exists());

        let content = std::fs::read_to_string(&output_path).unwrap();
        assert!(content.contains("customers"));
        assert!(content.contains("orders"));

        // Verify valid TOML
        let _: toml::Value = toml::from_str(&content)
            .expect("output should be valid TOML");
    }

    #[test]
    fn learn_nonexistent_path_errors() {
        let result = run(Some("nonexistent_path_12345.csv"), "out.toml", None, None, false, false, &quiet_cli());
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

        let output_path = dir.path().join("schema.weave.toml");
        let result = run(
            Some(dir.path().to_str().unwrap()),
            output_path.to_str().unwrap(),
            None,
            None,
            false,
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
        assert_eq!(best, 1, "should prefer PeopleHistoricalId for PeopleHistorical_test");
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
}
