//! `knit generate` — full forward pipeline for synthetic data generation.
//!
//! Orchestrates: parse → validate → plan → generate → noise → bind.

use std::collections::HashMap;
use std::fs;
use std::io::BufWriter;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context, Result};
use arrow::array::ArrayRef;
use arrow::datatypes::{DataType as ArrowDataType, Field as ArrowField, Schema};
use arrow::record_batch::RecordBatch;
use colored::Colorize;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

use knit_bind::{Compression, OutputFormat, Sink, SinkConfig};
use knit_core::{CountSpec, DataModel, NoiseProfile};
use knit_gen::GenerationEngine;
use knit_noise::{Pipeline, PerturbConfig, ColumnFilter};
use knit_plan::ExecutionPlan;

use crate::{Cli, CompressionArg, Format};
use super::{load_schema, validate_model};

/// Run the generate command — full forward pipeline.
///
/// Loads the schema, validates, compiles a plan, generates data in batches,
/// and writes output files to the specified directory.
pub fn run(schema_path: &str, output_dir: &str, cli: &Cli) -> Result<()> {
    let start = Instant::now();

    // ── Parse & validate ────────────────────────────────────────────
    let mut model = load_schema(schema_path)
        .with_context(|| format!("failed to parse schema `{}`", schema_path))?;

    // Apply CLI overrides to the model before validation/compilation.
    if let Some(seed) = cli.seed {
        model.seed = seed;
    }
    // Apply --count override (absolute or scale factor)
    if let Some(ref count_str) = cli.count {
        apply_count_override(&mut model, count_str)?;
    }
    // Store params in the model (for plan metadata) and later pass to engine.
    for (key, value) in &cli.params {
        model.params.insert(
            key.clone(),
            knit_core::Value::String(value.clone()),
        );
    }

    let errors = validate_model(&model);
    if !errors.is_empty() {
        for err in &errors {
            eprintln!("{} {}", "error:".red().bold(), err);
        }
        bail!("schema has {} validation error(s)", errors.len());
    }

    if !cli.quiet {
        eprintln!(
            "{} schema {} ({} entities)",
            "✓".green().bold(),
            schema_path.cyan(),
            model.entities.len()
        );
    }

    // ── Compile plan ────────────────────────────────────────────────
    let plan = knit_plan::compile(&model)
        .map_err(|e| anyhow::anyhow!("plan compilation failed: {}", e))?;

    if !cli.quiet {
        eprintln!(
            "{} plan compiled ({} phases, ~{} rows)",
            "✓".green().bold(),
            plan.metadata.total_phases,
            format_count(plan.metadata.estimated_total_rows),
        );
    }

    // ── Dry-run: stop after plan compilation ────────────────────────
    if cli.dry_run {
        if cli.json {
            println!("{}", serde_json::to_string_pretty(&plan)?);
        } else {
            super::plan::print_plan(&plan);
        }
        return Ok(());
    }

    // ── Configure parallelism ────────────────────────────────────────
    if cli.parallel > 0 {
        rayon::ThreadPoolBuilder::new()
            .num_threads(cli.parallel)
            .build_global()
            .map_err(|e| anyhow::anyhow!("failed to set thread pool size: {}", e))?;
    }

    // ── Prepare output directory ────────────────────────────────────
    let out_path = Path::new(output_dir);
    fs::create_dir_all(out_path)
        .with_context(|| format!("failed to create output directory `{}`", output_dir))?;

    let format = map_format(cli.format);
    let compression = map_compression(cli.compression);
    let extension = format_extension(cli.format);

    // ── JSON start event ──────────────────────────────────────────
    let total_estimated_rows: u64 = plan
        .phases
        .iter()
        .flat_map(|p| &p.entity_plans)
        .map(|ep| ep.estimated_row_count)
        .sum();
    let entity_count = plan
        .phases
        .iter()
        .flat_map(|p| &p.entity_plans)
        .count();

    if cli.json {
        let start_event = serde_json::json!({
            "event": "start",
            "entities": entity_count,
            "total_rows": total_estimated_rows,
        });
        println!("{}", start_event);
    }

    // ── Set up progress bars ────────────────────────────────────────
    let multi = MultiProgress::new();
    let entity_bars = create_progress_bars(&plan, &multi, cli.quiet);

    // ── Generate ────────────────────────────────────────────────────
    let batch_size = if cli.batch_size > 0 {
        cli.batch_size
    } else {
        8192
    };
    // Convert model params to string map for the generation engine.
    let gen_params: std::collections::HashMap<String, String> = model
        .params
        .iter()
        .map(|(k, v)| (k.clone(), value_to_string(v)))
        .collect();
    let mut engine = GenerationEngine::with_batch_size(batch_size).with_params(gen_params);

    // Track sinks and schemas per entity
    let mut sinks: HashMap<String, Box<dyn Sink>> = HashMap::new();
    let mut entity_schemas: HashMap<String, Arc<Schema>> = HashMap::new();
    let mut total_rows: u64 = 0;
    let mut total_bytes: u64 = 0;

    // Build Arrow schemas from the plan for each entity
    for phase in &plan.phases {
        for ep in &phase.entity_plans {
            let arrow_schema = build_arrow_schema(ep);
            entity_schemas.insert(ep.entity_name.clone(), Arc::new(arrow_schema));
        }
    }

    // ── Build noise pipelines per entity ────────────────────────────
    let noise_pipelines = if cli.no_noise {
        HashMap::new()
    } else {
        build_noise_pipelines(&model.noise_profiles, model.seed)
    };

    if !cli.quiet && !noise_pipelines.is_empty() {
        let profile_count: usize = model.noise_profiles.len();
        eprintln!(
            "{} noise pipeline ({} profile(s) across {} entity/entities)",
            "✓".green().bold(),
            profile_count,
            noise_pipelines.len(),
        );
    }

    if !cli.quiet && cli.no_noise && !model.noise_profiles.is_empty() {
        eprintln!(
            "{} noise skipped (--no-noise flag, {} profile(s) ignored)",
            "⊘".yellow().bold(),
            model.noise_profiles.len(),
        );
    }

    // Track per-entity row counts for JSON progress events
    let mut entity_row_counts: HashMap<String, u64> = HashMap::new();
    let mut entity_total_rows: HashMap<String, u64> = HashMap::new();
    for phase in &plan.phases {
        for ep in &phase.entity_plans {
            entity_total_rows.insert(ep.entity_name.clone(), ep.estimated_row_count);
        }
    }
    let json_mode = cli.json;

    // Execute generation
    let mut batch_counters: HashMap<String, u64> = HashMap::new();
    engine
        .execute(&plan, |entity_name, batch: RecordBatch| {
            // Track pre-noise row count for progress reporting
            let row_count = batch.num_rows() as u64;
            total_rows += row_count;

            // ── Apply noise pipeline if configured for this entity ──
            let batch_idx = batch_counters.entry(entity_name.to_string()).or_insert(0);
            let batch = if let Some(pipeline) = noise_pipelines.get(entity_name) {
                let result = pipeline.run_with_offset(batch, *batch_idx).map_err(|e| {
                    knit_gen::GenError::Generation(format!(
                        "noise pipeline error for '{}': {}",
                        entity_name, e
                    ))
                })?;
                *batch_idx += 1;
                result
            } else {
                *batch_idx += 1;
                batch
            };

            // Track per-entity progress
            let done = entity_row_counts.entry(entity_name.to_string()).or_insert(0);
            *done += row_count;

            // Lazily create sink
            if !sinks.contains_key(entity_name) {
                let file_path = out_path.join(format!("{}.{}", entity_name, extension));
                let file = fs::File::create(&file_path).map_err(|e| {
                    knit_gen::GenError::Generation(format!(
                        "failed to create {}: {}",
                        file_path.display(),
                        e
                    ))
                })?;
                let writer: Box<dyn std::io::Write + Send> = Box::new(BufWriter::new(file));

                let schema = entity_schemas.get(entity_name).cloned().unwrap_or_else(|| {
                    batch.schema()
                });

                let sink_config = SinkConfig {
                    format,
                    compression,
                    ..SinkConfig::default()
                };
                let sink = knit_bind::create_sink(writer, schema, &sink_config).map_err(|e| {
                    knit_gen::GenError::Generation(format!("failed to create sink: {}", e))
                })?;
                sinks.insert(entity_name.to_string(), sink);
            }

            // Cast columns to match the declared Arrow schema (e.g. Int64 → Int32)
            let target_schema = entity_schemas.get(entity_name).cloned().unwrap_or_else(|| {
                batch.schema()
            });
            let batch = cast_batch_to_schema(&batch, &target_schema)?;

            // Write batch
            let sink = sinks.get_mut(entity_name).ok_or_else(|| {
                knit_gen::GenError::Generation(format!(
                    "sink for entity '{}' not found after creation",
                    entity_name
                ))
            })?;
            sink.write_batch(&batch)
                .map_err(|e| knit_gen::GenError::Generation(format!("sink write error: {}", e)))?;

            // Emit JSON progress event only after successful write
            if json_mode {
                let entity_total = entity_total_rows.get(entity_name).copied().unwrap_or(0);
                let progress_event = serde_json::json!({
                    "event": "progress",
                    "entity": entity_name,
                    "rows_done": *done,
                    "rows_total": entity_total,
                });
                println!("{}", progress_event);
            }

            // Update progress bar
            if let Some(pb) = entity_bars.get(entity_name) {
                pb.inc(row_count);
            }

            Ok(())
        })
        .context("generation failed")?;

    // ── Finish sinks ────────────────────────────────────────────────
    for (entity_name, sink) in sinks {
        match sink.finish() {
            Ok(stats) => {
                total_bytes += stats.bytes_written;
                if let Some(pb) = entity_bars.get(&entity_name) {
                    pb.finish_with_message(format!(
                        "{} — {} rows, {}",
                        entity_name,
                        format_count(stats.rows_written),
                        format_bytes(stats.bytes_written),
                    ));
                }
            }
            Err(e) => {
                eprintln!(
                    "{} failed to finalize {}: {}",
                    "warning:".yellow().bold(),
                    entity_name,
                    e
                );
            }
        }
    }

    // ── Summary ─────────────────────────────────────────────────────
    let elapsed = start.elapsed();
    let throughput = if elapsed.as_secs_f64() > 0.0 {
        total_rows as f64 / elapsed.as_secs_f64()
    } else {
        0.0
    };

    if cli.json {
        let complete_event = serde_json::json!({
            "event": "complete",
            "elapsed_ms": elapsed.as_millis() as u64,
            "rows": total_rows,
            "bytes": total_bytes,
            "throughput_rows_per_sec": throughput as u64,
            "output_dir": output_dir,
        });
        println!("{}", complete_event);
    } else if !cli.quiet {
        println!();
        println!("{}", "═══ Generation Complete ═══".green().bold());
        println!("  {} {}", "rows:".dimmed(), format_count(total_rows));
        println!("  {} {}", "bytes:".dimmed(), format_bytes(total_bytes));
        println!("  {} {:.2}s", "elapsed:".dimmed(), elapsed.as_secs_f64());
        println!(
            "  {} {}/s",
            "throughput:".dimmed(),
            format_count(throughput as u64)
        );
        println!("  {} {}", "output:".dimmed(), output_dir.cyan());
    }

    Ok(())
}

/// Map CLI format enum to knit-bind OutputFormat.
fn map_format(f: Format) -> OutputFormat {
    match f {
        Format::Parquet => OutputFormat::Parquet,
        Format::Csv => OutputFormat::Csv,
        Format::Json => OutputFormat::Json,
        Format::Jsonl => OutputFormat::Jsonl,
        Format::ArrowIpc => OutputFormat::ArrowIpc,
    }
}

/// Map CLI compression enum to knit-bind Compression.
fn map_compression(c: CompressionArg) -> Compression {
    match c {
        CompressionArg::None => Compression::None,
        CompressionArg::Snappy => Compression::Snappy,
        CompressionArg::Lz4 => Compression::Lz4,
        CompressionArg::Zstd => Compression::Zstd,
        // Gzip not supported by knit-bind Compression enum
        CompressionArg::Gzip => {
            tracing::warn!("gzip compression not supported for this format, using none");
            Compression::None
        }
    }
}

/// Get file extension for the output format.
fn format_extension(f: Format) -> &'static str {
    match f {
        Format::Parquet => "parquet",
        Format::Csv => "csv",
        Format::Json => "json",
        Format::Jsonl => "jsonl",
        Format::ArrowIpc => "arrow",
    }
}

/// Build an Arrow schema from an EntityPlan's field plans.
///
/// Uses the generator plan to infer the Arrow data type for each field.
fn build_arrow_schema(ep: &knit_plan::EntityPlan) -> Schema {
    let fields: Vec<ArrowField> = ep
        .field_plans
        .iter()
        .map(|fp| {
            let dt = resolve_arrow_type(fp);
            ArrowField::new(&fp.field_name, dt, true)
        })
        .collect();
    Schema::new(fields)
}

/// Resolve the Arrow data type for a field plan, considering both the declared
/// data_type and the generator plan.
fn resolve_arrow_type(fp: &knit_plan::FieldPlan) -> ArrowDataType {
    // If the declared data_type has a specific narrow type, use it
    match &fp.data_type {
        knit_core::DataType::Int32 => return ArrowDataType::Int32,
        knit_core::DataType::DatetimeUs => {
            return ArrowDataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, None)
        }
        _ => {}
    }
    infer_arrow_type(&fp.generator_plan)
}

/// Cast columns in a batch to match the target schema types.
/// Only casts when types differ and the cast is supported (e.g., Int64 → Int32).
fn cast_batch_to_schema(
    batch: &RecordBatch,
    target_schema: &Arc<Schema>,
) -> Result<RecordBatch, knit_gen::GenError> {
    let mut needs_cast = false;
    for (i, field) in target_schema.fields().iter().enumerate() {
        if i < batch.num_columns() && batch.column(i).data_type() != field.data_type() {
            needs_cast = true;
            break;
        }
    }
    if !needs_cast {
        return Ok(batch.clone());
    }

    let mut adjusted_fields: Vec<ArrowField> = target_schema.fields().iter().map(|f| f.as_ref().clone()).collect();

    let columns: Vec<ArrayRef> = batch
        .columns()
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let target_type = target_schema.field(i).data_type();
            if col.data_type() == target_type {
                col.clone()
            } else {
                match arrow::compute::cast(col.as_ref(), target_type) {
                    Ok(casted) => casted,
                    Err(e) => {
                        tracing::warn!(
                            column = i,
                            from = %col.data_type(),
                            to = %target_type,
                            error = %e,
                            "cast failed, keeping original type"
                        );
                        // Adjust the schema field to match the actual column type
                        adjusted_fields[i] = ArrowField::new(
                            adjusted_fields[i].name(),
                            col.data_type().clone(),
                            adjusted_fields[i].is_nullable(),
                        );
                        col.clone()
                    }
                }
            }
        })
        .collect();

    let final_schema = Arc::new(Schema::new(adjusted_fields));
    RecordBatch::try_new(final_schema, columns).map_err(|e| {
        knit_gen::GenError::Generation(format!("schema cast error: {}", e))
    })
}

/// Infer the Arrow data type from a GeneratorPlan variant.
fn infer_arrow_type(gp: &knit_plan::GeneratorPlan) -> ArrowDataType {
    use knit_core::DistributionKind;

    match gp {
        knit_plan::GeneratorPlan::Distribution { kind, round, .. } => {
            if *round {
                ArrowDataType::Int64
            } else {
                match kind {
                    DistributionKind::Poisson
                    | DistributionKind::Bernoulli
                    | DistributionKind::Binomial
                    | DistributionKind::Geometric
                    | DistributionKind::Zipf => ArrowDataType::Int64,
                    _ => ArrowDataType::Float64,
                }
            }
        },
        knit_plan::GeneratorPlan::Sequence { .. } => ArrowDataType::Int64,
        knit_plan::GeneratorPlan::Uuid => ArrowDataType::Utf8,
        knit_plan::GeneratorPlan::Faker { category, .. } => match category.as_str() {
            "datetime" | "timestamp" => {
                ArrowDataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, None)
            }
            "date" => ArrowDataType::Date32,
            _ => ArrowDataType::Utf8,
        },
        knit_plan::GeneratorPlan::Pattern { .. } => ArrowDataType::Utf8,
        knit_plan::GeneratorPlan::OneOf { choices, .. } => infer_one_of_type(choices),
        knit_plan::GeneratorPlan::Constant(val) => match val {
            knit_core::Value::Null => ArrowDataType::Null,
            knit_core::Value::Bool(_) => ArrowDataType::Boolean,
            knit_core::Value::Int(_) => ArrowDataType::Int64,
            knit_core::Value::Float(_) => ArrowDataType::Float64,
            knit_core::Value::String(_) => ArrowDataType::Utf8,
            // Complex types (Array, Map, DateTime, etc.) are not yet supported
            // by ConstantGenerator — it emits NullArray for them.
            _ => ArrowDataType::Null,
        },
        knit_plan::GeneratorPlan::Derived { .. } => ArrowDataType::Float64,
        knit_plan::GeneratorPlan::ForeignKey { .. } => ArrowDataType::Int64,
        knit_plan::GeneratorPlan::Temporal { .. } => ArrowDataType::Int64,
        knit_plan::GeneratorPlan::Correlated { .. } => ArrowDataType::Float64,
        knit_plan::GeneratorPlan::Topology { .. } => ArrowDataType::Int64,
        knit_plan::GeneratorPlan::Composite { .. } => ArrowDataType::Utf8,
        knit_plan::GeneratorPlan::Unique { inner, .. } => infer_arrow_type(inner),
        knit_plan::GeneratorPlan::Conditional { default, .. } => infer_arrow_type(default),
    }
}

/// Infer the Arrow data type for a `OneOf` generator from its choice values.
///
/// Mirrors the logic in `knit_gen::generators::one_of::infer_output_type`.
fn infer_one_of_type(choices: &[knit_core::WeightedChoice]) -> ArrowDataType {
    for c in choices {
        match &c.value {
            knit_core::Value::String(_) => return ArrowDataType::Utf8,
            knit_core::Value::Int(_) => return ArrowDataType::Int64,
            knit_core::Value::Float(_) => return ArrowDataType::Float64,
            knit_core::Value::Bool(_) => return ArrowDataType::Boolean,
            knit_core::Value::Null => continue,
            _ => return ArrowDataType::Utf8,
        }
    }
    ArrowDataType::Utf8
}

/// Create progress bars for each entity in the plan.
fn create_progress_bars(
    plan: &ExecutionPlan,
    multi: &MultiProgress,
    quiet: bool,
) -> HashMap<String, ProgressBar> {
    let mut bars = HashMap::new();
    if quiet {
        return bars;
    }

    let style = ProgressStyle::with_template(
        "{prefix:>16.cyan} [{bar:30.green/dim}] {pos}/{len} rows ({eta})",
    )
    .expect("hardcoded progress bar template")
    .progress_chars("━╸─");

    for phase in &plan.phases {
        for ep in &phase.entity_plans {
            let pb = multi.add(ProgressBar::new(ep.estimated_row_count));
            pb.set_style(style.clone());
            pb.set_prefix(ep.entity_name.clone());
            bars.insert(ep.entity_name.clone(), pb);
        }
    }
    bars
}

/// Format a row count with thousands separators.
fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// Format a byte count in human-readable form.
fn format_bytes(b: u64) -> String {
    if b >= 1_073_741_824 {
        format!("{:.1} GiB", b as f64 / 1_073_741_824.0)
    } else if b >= 1_048_576 {
        format!("{:.1} MiB", b as f64 / 1_048_576.0)
    } else if b >= 1024 {
        format!("{:.1} KiB", b as f64 / 1024.0)
    } else {
        format!("{} B", b)
    }
}

/// Build a single noise [`Pipeline`] per entity from the schema's [`NoiseProfile`]s.
///
/// All perturbators for the same entity are merged into one pipeline so that
/// the pipeline's internal stage ordering (clean → constrained → breaking)
/// is respected. Each perturbator carries its own rate and column filter
/// overrides via [`knit_noise::PerturbOverrides`], so multiple profiles targeting the
/// same entity with different rates and column sets work correctly.
fn build_noise_pipelines(
    profiles: &[NoiseProfile],
    model_seed: u64,
) -> HashMap<String, Pipeline> {
    use knit_noise::{
        NullInjector, TypoInjector, OutlierInjector, DuplicateInjector,
        PerturbOverrides,
    };

    let mut entity_pipelines: HashMap<String, Pipeline> = HashMap::new();

    for (prof_idx, profile) in profiles.iter().enumerate() {
        if profile.entity.is_empty() {
            tracing::warn!(name = %profile.name, "noise profile has no entity target, skipping");
            continue;
        }

        let has_any = profile.null_rate > 0.0
            || profile.typo_rate > 0.0
            || profile.outlier_rate > 0.0
            || profile.duplicate_rate > 0.0;

        if !has_any {
            continue;
        }

        let col_filter = if profile.fields.is_empty() {
            None // use pipeline default (All)
        } else {
            Some(ColumnFilter::ByName(profile.fields.clone()))
        };

        let prof_seed = model_seed.wrapping_add(prof_idx as u64 * 1000);

        let pipeline = entity_pipelines
            .entry(profile.entity.clone())
            .or_insert_with(|| {
                let cfg = PerturbConfig::default()
                    .with_probability(0.0)
                    .with_seed(prof_seed);
                Pipeline::new(cfg)
            });

        // Helper to build overrides with this profile's rate and column filter.
        let make_overrides = |rate: f64| PerturbOverrides {
            probability: Some(rate),
            columns: col_filter.clone(),
        };

        // Add perturbators with their individual rates and column filters
        if profile.null_rate > 0.0 {
            pipeline.add_with_overrides(Box::new(NullInjector::new()), make_overrides(profile.null_rate));
        }
        if profile.typo_rate > 0.0 {
            pipeline.add_with_overrides(Box::new(TypoInjector::new()), make_overrides(profile.typo_rate));
        }
        if profile.outlier_rate > 0.0 {
            pipeline.add_with_overrides(Box::new(OutlierInjector::new(5.0)), make_overrides(profile.outlier_rate));
        }
        if profile.duplicate_rate > 0.0 {
            pipeline.add_with_overrides(Box::new(DuplicateInjector::new()), make_overrides(profile.duplicate_rate));
        }
    }

    entity_pipelines
}

/// Apply `--count` override to all entities in the model.
///
/// - `"500"` → set all entities to exactly 500 rows
/// - `"0.1x"` → multiply each entity's count by 0.1 (10% sample)
/// - `"10x"` → multiply each entity's count by 10
pub(crate) fn apply_count_override(model: &mut DataModel, count_str: &str) -> Result<()> {
    if let Some(factor_str) = count_str.strip_suffix('x') {
        let factor: f64 = factor_str
            .parse()
            .with_context(|| format!("invalid count multiplier: '{count_str}'"))?;
        if !factor.is_finite() || factor <= 0.0 {
            bail!("count multiplier must be a finite positive number, got '{count_str}'");
        }
        for entity in &mut model.entities {
            let current = match &entity.count {
                CountSpec::Fixed(n) => *n,
                CountSpec::Range { max, .. } => *max,
                CountSpec::Distribution(_) => 1000,
            };
            let scaled = (current as f64 * factor).round() as u64;
            entity.count = CountSpec::Fixed(scaled.max(1));
        }
    } else {
        let count: u64 = count_str
            .parse()
            .with_context(|| format!("invalid count value: '{count_str}'"))?;
        if count == 0 {
            bail!("count must be at least 1");
        }
        for entity in &mut model.entities {
            entity.count = CountSpec::Fixed(count);
        }
    }
    Ok(())
}

/// Convert a knit_core::Value to its string representation for param injection.
fn value_to_string(v: &knit_core::Value) -> String {
    match v {
        knit_core::Value::String(s) => s.clone(),
        knit_core::Value::Int(n) => n.to_string(),
        knit_core::Value::Float(f) => f.to_string(),
        knit_core::Value::Bool(b) => b.to_string(),
        knit_core::Value::Null => String::new(),
        knit_core::Value::Array(arr) => serde_json::to_string(arr).unwrap_or_default(),
        knit_core::Value::Map(map) => serde_json::to_string(map).unwrap_or_default(),
    }
}