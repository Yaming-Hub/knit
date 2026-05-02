//! `knit generate` — full forward pipeline for synthetic data generation.
//!
//! Orchestrates: parse → validate → plan → generate → (noise) → bind.

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
use knit_gen::GenerationEngine;
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
    let model = load_schema(schema_path)
        .with_context(|| format!("failed to parse schema `{}`", schema_path))?;

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

    // ── Prepare output directory ────────────────────────────────────
    let out_path = Path::new(output_dir);
    fs::create_dir_all(out_path)
        .with_context(|| format!("failed to create output directory `{}`", output_dir))?;

    let format = map_format(cli.format);
    let compression = map_compression(cli.compression);
    let extension = format_extension(cli.format);

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

    // Execute generation
    engine
        .execute(&plan, |entity_name, batch: RecordBatch| {
            let row_count = batch.num_rows() as u64;
            total_rows += row_count;

            // Update progress
            if let Some(pb) = entity_bars.get(entity_name) {
                pb.inc(row_count);
            }

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
            let sink = sinks.get_mut(entity_name).unwrap();
            sink.write_batch(&batch)
                .map_err(|e| knit_gen::GenError::Generation(format!("sink write error: {}", e)))?;

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
        let summary = serde_json::json!({
            "rows": total_rows,
            "bytes": total_bytes,
            "elapsed_ms": elapsed.as_millis(),
            "throughput_rows_per_sec": throughput as u64,
            "output_dir": output_dir,
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
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
        // Gzip not in knit-bind Compression, fall back to None
        CompressionArg::Gzip => Compression::None,
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
    match gp {
        knit_plan::GeneratorPlan::Distribution { .. } => ArrowDataType::Float64,
        knit_plan::GeneratorPlan::Sequence { .. } => ArrowDataType::Int64,
        knit_plan::GeneratorPlan::Uuid => ArrowDataType::Utf8,
        knit_plan::GeneratorPlan::Faker { .. } => ArrowDataType::Utf8,
        knit_plan::GeneratorPlan::Pattern { .. } => ArrowDataType::Utf8,
        knit_plan::GeneratorPlan::OneOf { .. } => ArrowDataType::Utf8,
        knit_plan::GeneratorPlan::Constant(_) => ArrowDataType::Utf8,
        knit_plan::GeneratorPlan::Derived { .. } => ArrowDataType::Float64,
        knit_plan::GeneratorPlan::ForeignKey { .. } => ArrowDataType::Int64,
        knit_plan::GeneratorPlan::Temporal { .. } => ArrowDataType::Int64,
        knit_plan::GeneratorPlan::Correlated { .. } => ArrowDataType::Float64,
        knit_plan::GeneratorPlan::Topology { .. } => ArrowDataType::Int64,
        knit_plan::GeneratorPlan::Composite { .. } => ArrowDataType::Utf8,
    }
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
    .unwrap()
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
