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
    // Note: model.params are currently passed through to the plan metadata
    // but not yet consumed by generators. This wiring prepares for future
    // parameterized expression support.
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
    let mut engine = GenerationEngine::with_batch_size(batch_size);

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
            let dt = infer_arrow_type(&fp.generator_plan);
            ArrowField::new(&fp.field_name, dt, true)
        })
        .collect();
    Schema::new(fields)
}

/// Infer the Arrow data type from a GeneratorPlan variant.
fn infer_arrow_type(gp: &knit_plan::GeneratorPlan) -> ArrowDataType {
    use knit_core::DistributionKind;

    match gp {
        knit_plan::GeneratorPlan::Distribution { kind, .. } => match kind {
            DistributionKind::Poisson
            | DistributionKind::Bernoulli
            | DistributionKind::Binomial
            | DistributionKind::Geometric
            | DistributionKind::Zipf => ArrowDataType::Int64,
            _ => ArrowDataType::Float64,
        },
        knit_plan::GeneratorPlan::Sequence { .. } => ArrowDataType::Int64,
        knit_plan::GeneratorPlan::Uuid => ArrowDataType::Utf8,
        knit_plan::GeneratorPlan::Faker { .. } => ArrowDataType::Utf8,
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
/// is respected. The highest probability across profiles is used as the
/// pipeline-level config; individual perturbators respect their own rate
/// through the pipeline's probability setting per type.
///
/// Since the Pipeline uses one global probability for all its perturbators,
/// we create separate pipelines per noise *type* with the correct rate,
/// but collect them into a wrapper that runs them in stage order.
/// Actually — to preserve correct ordering — we use a single Pipeline
/// with the default probability, and accept that all perturbators share it.
/// For fine-grained per-type rates, we use one Pipeline per entity with
/// the maximum rate and rely on each perturbator only affecting its target
/// type... BUT the current Perturbator trait doesn't support per-instance rates.
///
/// **Chosen approach:** One Pipeline per entity. The pipeline probability is
/// set to the maximum rate across all noise types. This is an approximation
/// that slightly over-perturbs for lower-rate types. A future improvement
/// would add per-perturbator rate configuration.
fn build_noise_pipelines(
    profiles: &[NoiseProfile],
    model_seed: u64,
) -> HashMap<String, Pipeline> {
    use knit_noise::{
        NullInjector, TypoInjector, OutlierInjector, DuplicateInjector,
    };

    let mut entity_pipelines: HashMap<String, Pipeline> = HashMap::new();

    for (prof_idx, profile) in profiles.iter().enumerate() {
        if profile.entity.is_empty() {
            tracing::warn!(name = %profile.name, "noise profile has no entity target, skipping");
            continue;
        }

        // Compute the maximum rate across all noise types in this profile
        let max_rate = profile.null_rate
            .max(profile.typo_rate)
            .max(profile.outlier_rate)
            .max(profile.duplicate_rate);

        if max_rate <= 0.0 {
            continue;
        }

        let col_filter = if profile.fields.is_empty() {
            ColumnFilter::All
        } else {
            ColumnFilter::ByName(profile.fields.clone())
        };

        let prof_seed = model_seed.wrapping_add(prof_idx as u64 * 1000);

        let pipeline = entity_pipelines
            .entry(profile.entity.clone())
            .or_insert_with(|| {
                let cfg = PerturbConfig::default()
                    .with_probability(max_rate)
                    .with_seed(prof_seed)
                    .with_columns_filter(col_filter.clone());
                Pipeline::new(cfg)
            });

        // Add perturbators based on non-zero rates
        if profile.null_rate > 0.0 {
            pipeline.add(Box::new(NullInjector::new()));
        }
        if profile.typo_rate > 0.0 {
            pipeline.add(Box::new(TypoInjector::new()));
        }
        if profile.outlier_rate > 0.0 {
            pipeline.add(Box::new(OutlierInjector::new(5.0)));
        }
        if profile.duplicate_rate > 0.0 {
            pipeline.add(Box::new(DuplicateInjector::new()));
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
