//! `knit learn` — infer a Weave schema from existing data.
//!
//! Reads data files (CSV, Parquet, JSON/JSONL) or directories,
//! profiles columns, fits distributions, detects relationships and
//! correlations, and assembles a complete Weave schema.

use std::collections::HashSet;
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
pub fn run(source: &str, output: &str, sample: Option<usize>, cli: &crate::Cli) -> Result<()> {
    let source_path = Path::new(source);
    anyhow::ensure!(
        source_path.exists(),
        "source path does not exist: {}",
        source
    );

    if let Some(0) = sample {
        anyhow::bail!("--sample must be at least 1");
    }

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
    let data_model = assemble_data_model(&model_name, &table_analyses);

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
        });
        println!("{}", summary);
    } else if !cli.quiet {
        eprintln!(
            "\n{} Wrote {} — {} table(s), {} column(s), {} relationship(s), {} correlation(s)",
            "✓".green().bold(),
            output.cyan(),
            table_analyses.len(),
            total_columns,
            total_rels,
            total_corrs,
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
    let name_suggests_pk = name_lower == "id"
        || name_lower.ends_with("_id")
        || name_lower.ends_with("_key");

    // Require both uniqueness AND a PK-like name to avoid false positives
    is_unique && name_suggests_pk && profile.null_count == 0
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
            csv_path.to_str().unwrap(),
            output_path.to_str().unwrap(),
            None,
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
            dir.path().to_str().unwrap(),
            output_path.to_str().unwrap(),
            None,
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
        let result = run("nonexistent_path_12345.csv", "out.toml", None, &quiet_cli());
        assert!(result.is_err());
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
}
