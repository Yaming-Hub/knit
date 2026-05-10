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

use crate::bind::{Compression, OutputFormat, Sink, SinkConfig};
use crate::core::{CountSpec, DataModel, NoiseProfile};
use crate::gen::{generate_graph, ActorPool, GenerationEngine};
use crate::noise::{ColumnFilter, PerturbConfig, Pipeline};
use crate::plan::ExecutionPlan;

use super::{load_schema, validate_model};
use crate::cli::{Cli, CompressionArg, Format};

/// Run the generate command — full forward pipeline.
///
/// Loads the schema, validates, compiles a plan, generates data in batches,
/// and writes output files to the specified directory.
pub fn run(schema_path: &str, output_dir: &str, entity_filter: &[String], cli: &Cli) -> Result<()> {
    let _gen_span = tracing::info_span!("generate", schema = %schema_path).entered();
    let start = Instant::now();

    // ── Load WASM plugins (if any) ──────────────────────────────────
    load_plugins(cli)?;

    // ── Parse & validate ────────────────────────────────────────────
    let mut model = load_schema(schema_path)
        .with_context(|| format!("failed to parse schema `{}`", schema_path))?;

    // Apply CLI overrides to the model before validation/compilation.
    if let Some(seed) = cli.seed {
        model.seed = seed;
    }
    // Store params in the model first so count expressions can reference them.
    for (key, value) in &cli.params {
        model
            .params
            .insert(key.clone(), crate::core::Value::String(value.clone()));
    }
    // Apply --count override (absolute or scale factor) after params are set.
    if let Some(ref count_str) = cli.count {
        apply_count_override(&mut model, count_str)?;
    }

    let errors = validate_model(&model);
    if !errors.is_empty() {
        for err in &errors {
            eprintln!("{} {}", "error:".red().bold(), err);
        }
        bail!("schema has {} validation error(s)", errors.len());
    }

    // Validate --entity filter references existing entities
    let entity_names: std::collections::HashSet<&str> =
        model.entities.iter().map(|e| e.name.as_str()).collect();
    for name in entity_filter {
        if !entity_names.contains(name.as_str()) {
            bail!(
                "unknown entity '{}' in --entity filter; available: {}",
                name,
                model
                    .entities
                    .iter()
                    .map(|e| e.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    // Build the output filter set (empty = all entities)
    let output_entities: std::collections::HashSet<&str> = if entity_filter.is_empty() {
        std::collections::HashSet::new()
    } else {
        entity_filter.iter().map(|s| s.as_str()).collect()
    };

    if !cli.quiet {
        if entity_filter.is_empty() {
            eprintln!(
                "{} schema {} ({} entities)",
                "✓".green().bold(),
                schema_path.cyan(),
                model.entities.len()
            );
        } else {
            eprintln!(
                "{} schema {} (generating {} of {} entities)",
                "✓".green().bold(),
                schema_path.cyan(),
                entity_filter.len(),
                model.entities.len()
            );
        }
    }

    // ── Compile plan ────────────────────────────────────────────────
    let mut plan = crate::plan::compile(&model)
        .map_err(|e| anyhow::anyhow!("plan compilation failed: {}", e))?;

    // ── Resolve dictionary and external lookup files ──────────────
    let schema_dir = Path::new(schema_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    resolve_dictionary_plans(&mut plan, schema_dir)?;
    resolve_external_lookup_plans(&mut plan, schema_dir)?;

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
            let behavioral = super::plan::BehavioralSummary::from_model(&model);
            super::plan::print_plan(&plan, &behavioral);
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
    let avro_codec = map_avro_codec(cli.compression);
    let extension = format_extension(cli.format);

    // ── JSON start event ──────────────────────────────────────────
    let total_estimated_rows: u64 = plan
        .phases
        .iter()
        .flat_map(|p| &p.entity_plans)
        .filter(|ep| {
            output_entities.is_empty() || output_entities.contains(ep.entity_name.as_str())
        })
        .map(|ep| ep.estimated_row_count)
        .sum();
    let entity_count = plan
        .phases
        .iter()
        .flat_map(|p| &p.entity_plans)
        .filter(|ep| {
            output_entities.is_empty() || output_entities.contains(ep.entity_name.as_str())
        })
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
    let entity_bars = create_progress_bars(&plan, &multi, cli.quiet, &output_entities);

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

    // ── Collect missing-field specs per entity ───────────────────────
    let missing_field_specs = if cli.no_noise {
        HashMap::new()
    } else {
        collect_missing_field_specs(&model.noise_profiles, model.seed)
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

    // ── Build actor pool and relationship graphs ─────────────────────
    // Only materialize behavioral pipeline when the plan has actor pools.
    if !plan.actor_pool.pools.is_empty() {
        let actor_pool = ActorPool::from_plan(&plan.actor_pool, model.seed);
        let graphs: Vec<crate::gen::GeneratedGraph> = plan
            .actor_pool
            .graph_plans
            .iter()
            .filter_map(|gp| {
                if !actor_pool.has_entity(&gp.from_entity) || !actor_pool.has_entity(&gp.to_entity)
                {
                    if !cli.quiet {
                        eprintln!(
                            "{} graph '{}' skipped: entity not in pool (from: {}, to: {})",
                            "⚠".yellow().bold(),
                            gp.name,
                            gp.from_entity,
                            gp.to_entity,
                        );
                    }
                    return None;
                }
                Some(generate_graph(gp, &actor_pool, model.seed))
            })
            .collect();

        if !cli.quiet {
            let total_actors: u64 = plan.actor_pool.pools.iter().map(|p| p.actor_count).sum();
            eprintln!(
                "{} actor pool ({} entity/entities, {} actors, {} graph(s))",
                "✓".green().bold(),
                plan.actor_pool.pools.len(),
                format_count(total_actors),
                graphs.len(),
            );
        }

        // Materialize graphs for future InteractionGenerator use.
        let _graphs = graphs;

        // Pass actor pool to engine for persona-weighted FK generation.
        engine = engine.with_actor_pool(Arc::new(actor_pool));

        // Build graph adjacency lists for graph-aware FK generation.
        engine.build_graphs(&plan);
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

    // ── Build partition config per entity ────────────────────────────
    // For entities with hive-style partitioning, pre-compute the partition
    // value list and cumulative weights for round-robin row assignment.
    struct PartitionConfig {
        partition_key: String,
        /// Base directory under output root (e.g. "Collab/Results").
        base_path: Option<String>,
        /// Partition values sorted by name, with cumulative weight thresholds.
        /// Each entry is (value, cumulative_weight).
        values: Vec<(String, f64)>,
    }
    let mut partition_configs: HashMap<String, PartitionConfig> = HashMap::new();
    for entity in &model.entities {
        if let Some(output) = &entity.output {
            if let Some(partition_key) = &output.partition_by {
                if !output.partition_values.is_empty() {
                    let mut cumulative = 0.0;
                    let values: Vec<(String, f64)> = output
                        .partition_values
                        .iter()
                        .map(|pv| {
                            cumulative += pv.weight;
                            (pv.value.clone(), cumulative)
                        })
                        .collect();
                    partition_configs.insert(
                        entity.name.clone(),
                        PartitionConfig {
                            partition_key: partition_key.clone(),
                            base_path: output.path.clone(),
                            values,
                        },
                    );
                }
            }
        }
    }

    // For partitioned entities, use separate sinks keyed by (entity, partition_value)
    let mut partition_sinks: HashMap<(String, String), Box<dyn Sink>> = HashMap::new();
    // Track cumulative row index per entity for deterministic partition assignment
    let mut partition_row_idx: HashMap<String, usize> = HashMap::new();

    // Execute generation
    let mut batch_counters: HashMap<String, u64> = HashMap::new();
    engine
        .execute(&plan, |entity_name, batch: RecordBatch| {
            // Skip output for entities not in the filter (but still generate
            // for FK key-store population)
            if !output_entities.is_empty() && !output_entities.contains(entity_name) {
                return Ok(());
            }

            // Track row count only for output entities
            let row_count = batch.num_rows() as u64;
            total_rows += row_count;

            // ── Apply noise pipeline if configured for this entity ──
            let batch_idx = batch_counters.entry(entity_name.to_string()).or_insert(0);
            let batch = if let Some(pipeline) = noise_pipelines.get(entity_name) {
                let result = pipeline.run_with_offset(batch, *batch_idx).map_err(|e| {
                    crate::gen::GenError::Generation(format!(
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
            let done = entity_row_counts
                .entry(entity_name.to_string())
                .or_insert(0);
            *done += row_count;

            // Cast columns to match the declared Arrow schema (e.g. Int64 → Int32)
            let target_schema = entity_schemas
                .get(entity_name)
                .cloned()
                .unwrap_or_else(|| batch.schema());
            let batch = cast_batch_to_schema(&batch, &target_schema)?;

            // For CSV format, flatten nested columns (List, Map) to JSON strings
            // since CSV doesn't support nested structures.
            let batch = if matches!(format, OutputFormat::Csv) {
                flatten_nested_columns(&batch)?
            } else {
                batch
            };

            // ── Partitioned vs flat output ──────────────────────────────
            if let Some(pc) = partition_configs.get(entity_name) {
                // Assign each row to a partition based on cumulative weights
                let n = batch.num_rows();
                let base_idx = partition_row_idx.entry(entity_name.to_string()).or_insert(0);
                let total_entity_rows = entity_total_rows.get(entity_name).copied().unwrap_or(n as u64) as usize;

                // Build per-partition row indices
                let mut partition_rows: HashMap<&str, Vec<usize>> = HashMap::new();
                for row in 0..n {
                    let global_row = *base_idx + row;
                    // Use fractional position to pick partition by weight
                    let frac = if total_entity_rows > 0 {
                        (global_row as f64 + 0.5) / total_entity_rows as f64
                    } else {
                        0.0
                    };
                    // Find the partition whose cumulative weight covers this fraction
                    let pval = pc.values.iter()
                        .find(|(_, cw)| frac < *cw)
                        .map(|(v, _)| v.as_str())
                        .unwrap_or_else(|| pc.values.last().map(|(v, _)| v.as_str()).unwrap_or("unknown"));
                    partition_rows.entry(pval).or_default().push(row);
                }
                *base_idx += n;

                // Write each partition's rows
                for (pval, indices) in &partition_rows {
                    let key = (entity_name.to_string(), pval.to_string());

                    // Lazily create partition sink
                    if !partition_sinks.contains_key(&key) {
                        let base_dir = if let Some(base) = &pc.base_path {
                            let base_p = std::path::Path::new(base);
                            if base_p.is_absolute()
                                || base_p.components().any(|c| {
                                    matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir)
                                })
                            {
                                return Err(crate::gen::GenError::Generation(format!(
                                    "unsafe output path for entity '{}': {}", entity_name, base
                                )));
                            }
                            out_path.join(base)
                        } else {
                            out_path.to_path_buf()
                        };
                        // Sanitize partition value for use as directory name
                        let safe_pval = pval.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|', '.'], "_");
                        if safe_pval.is_empty() || safe_pval == ".." || safe_pval.starts_with('/') || safe_pval.starts_with('\\') {
                            return Err(crate::gen::GenError::Generation(format!(
                                "unsafe partition value for entity '{}': {}", entity_name, pval
                            )));
                        }
                        let part_dir = base_dir.join(format!("{}={}", pc.partition_key, safe_pval));
                        fs::create_dir_all(&part_dir).map_err(|e| {
                            crate::gen::GenError::Generation(format!("failed to create {}: {}", part_dir.display(), e))
                        })?;
                        let file_path = part_dir.join(format!("{}.{}", entity_name, extension));
                        let file = fs::File::create(&file_path).map_err(|e| {
                            crate::gen::GenError::Generation(format!("failed to create {}: {}", file_path.display(), e))
                        })?;
                        let writer: Box<dyn std::io::Write + Send> = Box::new(BufWriter::new(file));
                        let schema = entity_schemas
                            .get(entity_name)
                            .cloned()
                            .unwrap_or_else(|| batch.schema());
                        let schema = if matches!(format, OutputFormat::Csv) {
                            Arc::new(flatten_schema_for_csv(&schema))
                        } else {
                            schema
                        };
                        let entity_missing = missing_field_specs.get(entity_name).cloned().unwrap_or_default();
                        let sink_config = SinkConfig {
                            format,
                            compression,
                            record_name: entity_name.to_string(),
                            avro_codec,
                            missing_field_specs: entity_missing,
                            sql_create_table: cli.sql_create_table,
                            sql_transaction: cli.sql_transaction,
                            ..SinkConfig::default()
                        };
                        let sink = crate::bind::create_sink(writer, schema, &sink_config).map_err(|e| {
                            crate::gen::GenError::Generation(format!("failed to create sink: {}", e))
                        })?;
                        partition_sinks.insert(key.clone(), sink);
                    }

                    // Slice the batch to only include this partition's rows
                    let indices_arr = arrow::array::UInt32Array::from(
                        indices.iter().map(|&i| i as u32).collect::<Vec<_>>()
                    );
                    let part_batch = arrow::compute::take_record_batch(&batch, &indices_arr)
                        .map_err(|e| crate::gen::GenError::Generation(format!("partition split error: {}", e)))?;

                    let sink = partition_sinks.get_mut(&key).unwrap();
                    sink.write_batch(&part_batch)
                        .map_err(|e| crate::gen::GenError::Generation(format!("sink write error: {}", e)))?;
                }
            } else {
                // ── Non-partitioned (flat) output ───────────────────────
                // Lazily create sink
                if !sinks.contains_key(entity_name) {
                    let file_path = {
                        let output_subdir = model.entities.iter()
                            .find(|e| e.name == entity_name)
                            .and_then(|e| e.output.as_ref())
                            .and_then(|o| o.path.as_ref());
                        if let Some(subdir) = output_subdir {
                            let subdir_path = std::path::Path::new(subdir);
                            if subdir_path.is_absolute()
                                || subdir_path.components().any(|c| {
                                    matches!(c, std::path::Component::ParentDir | std::path::Component::RootDir)
                                })
                            {
                                return Err(crate::gen::GenError::Generation(format!(
                                    "unsafe output path for entity '{}': {}", entity_name, subdir
                                )));
                            }
                            let dir = out_path.join(subdir);
                            fs::create_dir_all(&dir).map_err(|e| {
                                crate::gen::GenError::Generation(format!("failed to create {}: {}", dir.display(), e))
                            })?;
                            dir.join(format!("{}.{}", entity_name, extension))
                        } else {
                            out_path.join(format!("{}.{}", entity_name, extension))
                        }
                    };
                    let file = fs::File::create(&file_path).map_err(|e| {
                        crate::gen::GenError::Generation(format!("failed to create {}: {}", file_path.display(), e))
                    })?;
                    let writer: Box<dyn std::io::Write + Send> = Box::new(BufWriter::new(file));
                    let schema = entity_schemas.get(entity_name).cloned().unwrap_or_else(|| batch.schema());
                    let schema = if matches!(format, OutputFormat::Csv) {
                        Arc::new(flatten_schema_for_csv(&schema))
                    } else {
                        schema
                    };
                    let entity_missing = missing_field_specs.get(entity_name).cloned().unwrap_or_default();
                    if !entity_missing.is_empty()
                        && !matches!(format, OutputFormat::Json | OutputFormat::Jsonl)
                    {
                        tracing::warn!(
                            entity = entity_name,
                            "missing_field noise has no effect on {} output; only JSON/JSONL can omit fields",
                            format!("{:?}", format).to_lowercase()
                        );
                    }
                    let sink_config = SinkConfig {
                        format,
                        compression,
                        record_name: entity_name.to_string(),
                        avro_codec,
                        missing_field_specs: entity_missing,
                        sql_create_table: cli.sql_create_table,
                        sql_transaction: cli.sql_transaction,
                        ..SinkConfig::default()
                    };
                    let sink = crate::bind::create_sink(writer, schema, &sink_config).map_err(|e| {
                        crate::gen::GenError::Generation(format!("failed to create sink: {}", e))
                    })?;
                    sinks.insert(entity_name.to_string(), sink);
                }

                let sink = sinks.get_mut(entity_name).ok_or_else(|| {
                    crate::gen::GenError::Generation(format!("sink for entity '{}' not found", entity_name))
                })?;
                sink.write_batch(&batch)
                    .map_err(|e| crate::gen::GenError::Generation(format!("sink write error: {}", e)))?;
            }

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

    // Finish partition sinks
    for ((entity_name, pval), sink) in partition_sinks {
        match sink.finish() {
            Ok(stats) => {
                total_bytes += stats.bytes_written;
                tracing::debug!(
                    entity = %entity_name,
                    partition = %pval,
                    rows = stats.rows_written,
                    bytes = stats.bytes_written,
                    "partition sink finalized"
                );
            }
            Err(e) => {
                eprintln!(
                    "{} failed to finalize {}[{}]: {}",
                    "warning:".yellow().bold(),
                    entity_name,
                    pval,
                    e
                );
            }
        }
    }

    // ── Copy companion files ────────────────────────────────────────
    // Copy non-data files (schema.json, dictionaries, etc.) from the
    // schema's directory to the output directory.
    if !model.companion_files.is_empty() {
        let schema_dir = Path::new(schema_path)
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let mut companion_copied = 0u64;
        for rel_path_str in &model.companion_files {
            let rel_path = Path::new(rel_path_str);
            // Reject unsafe paths
            if rel_path.is_absolute()
                || rel_path.components().any(|c| {
                    matches!(
                        c,
                        std::path::Component::ParentDir | std::path::Component::RootDir
                    )
                })
            {
                tracing::warn!(path = %rel_path_str, "skipping companion file with unsafe path");
                continue;
            }
            let src = schema_dir.join(rel_path);
            let dst = out_path.join(rel_path);
            if src.exists() {
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent).ok();
                }
                if let Err(e) = fs::copy(&src, &dst) {
                    tracing::warn!(
                        src = %src.display(),
                        dst = %dst.display(),
                        error = %e,
                        "failed to copy companion file"
                    );
                } else {
                    companion_copied += 1;
                }
            } else {
                tracing::debug!(path = %src.display(), "companion file not found, skipping");
            }
        }
        if companion_copied > 0 && !cli.quiet {
            eprintln!(
                "  {} copied {} companion file(s)",
                "→".dimmed(),
                companion_copied,
            );
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
        Format::Avro => OutputFormat::Avro,
        Format::Sql => OutputFormat::Sql,
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
        Format::Avro => "avro",
        Format::Sql => "sql",
    }
}

/// Map CLI compression enum to Avro codec.
fn map_avro_codec(c: CompressionArg) -> crate::bind::AvroCodec {
    match c {
        CompressionArg::Snappy => crate::bind::AvroCodec::Snappy,
        CompressionArg::Gzip => crate::bind::AvroCodec::Deflate,
        CompressionArg::None => crate::bind::AvroCodec::Null,
        other => {
            tracing::warn!(
                "{:?} compression not supported for Avro, using null (no compression)",
                other
            );
            crate::bind::AvroCodec::Null
        }
    }
}

/// Build an Arrow schema from an EntityPlan's field plans.
///
/// Uses the generator plan to infer the Arrow data type for each field.
fn build_arrow_schema(ep: &crate::plan::EntityPlan) -> Schema {
    // Sort field plans by schema_position so the Arrow schema matches
    // the declared column order (not dependency order).
    let mut sorted: Vec<&crate::plan::FieldPlan> = ep.field_plans.iter().collect();
    sorted.sort_by_key(|fp| fp.schema_position);
    let fields: Vec<ArrowField> = sorted
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
fn resolve_arrow_type(fp: &crate::plan::FieldPlan) -> ArrowDataType {
    // If the declared data_type has a specific narrow type, use it
    match &fp.data_type {
        crate::core::DataType::Bool => return ArrowDataType::Boolean,
        crate::core::DataType::Int32 => return ArrowDataType::Int32,
        crate::core::DataType::Datetime => {
            return ArrowDataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, None)
        }
        crate::core::DataType::DatetimeUs => {
            return ArrowDataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, None)
        }
        crate::core::DataType::Array => {
            // Detect element type from OneOf choices if possible
            let elem_type = detect_list_element_type(fp);
            return ArrowDataType::List(Arc::new(ArrowField::new("element", elem_type, true)));
        }
        crate::core::DataType::Object => {
            // Build Struct type recursively from sub-field plans
            let struct_fields: Vec<ArrowField> = fp
                .sub_field_plans
                .iter()
                .map(|sfp| {
                    let child_type = resolve_arrow_type(sfp);
                    ArrowField::new(&sfp.field_name, child_type, true)
                })
                .collect();
            return ArrowDataType::Struct(struct_fields.into());
        }
        crate::core::DataType::Map => {
            let (key_type, val_type) = detect_map_kv_types(fp);
            let entries_field = ArrowField::new(
                &fp.field_name,
                ArrowDataType::Struct(
                    vec![
                        ArrowField::new("key", key_type, false),
                        ArrowField::new("value", val_type, true),
                    ]
                    .into(),
                ),
                false,
            );
            return ArrowDataType::Map(Arc::new(entries_field), false);
        }
        _ => {}
    }

    // Plugins cannot declare their output type at plan time — use the field's declared data_type.
    if matches!(&fp.generator_plan, crate::plan::GeneratorPlan::Plugin { .. }) {
        return default_arrow_for_data_type(&fp.data_type);
    }

    let generator_type = infer_arrow_type(&fp.generator_plan);

    // If declared type is String/Uuid but generator produces non-string (e.g. FK generator
    // that now produces StringArray, or sequence for numeric-string columns), force Utf8 output
    if (fp.data_type == crate::core::DataType::String || fp.data_type == crate::core::DataType::Uuid)
        && generator_type != ArrowDataType::Utf8
    {
        return ArrowDataType::Utf8;
    }

    generator_type
}

/// Detect the element type for a List column by inspecting OneOf choices or distribution kind.
fn detect_list_element_type(fp: &crate::plan::FieldPlan) -> ArrowDataType {
    // Vector-valued distributions have known element types
    if let crate::plan::GeneratorPlan::Distribution { kind, .. } = &fp.generator_plan {
        return match kind {
            crate::core::DistributionKind::Dirichlet => ArrowDataType::Float64,
            crate::core::DistributionKind::Multinomial => ArrowDataType::Int64,
            _ => ArrowDataType::Utf8,
        };
    }
    if let crate::plan::GeneratorPlan::OneOf { choices, .. } = &fp.generator_plan {
        // Try parsing first non-empty choice as JSON array to detect element type
        for choice in choices {
            let s = match &choice.value {
                crate::core::Value::String(s) => s.clone(),
                _ => continue,
            };
            if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(&s) {
                // Check all numeric values to determine appropriate int width
                let mut has_number = false;
                let mut fits_i32 = true;
                let mut is_float = false;
                for item in &arr {
                    match item {
                        serde_json::Value::Number(n) => {
                            has_number = true;
                            if n.is_f64() && !n.is_i64() && !n.is_u64() {
                                is_float = true;
                            } else if let Some(v) = n.as_i64() {
                                if v < i32::MIN as i64 || v > i32::MAX as i64 {
                                    fits_i32 = false;
                                }
                            }
                        }
                        serde_json::Value::Null => {}
                        _ => return ArrowDataType::Utf8,
                    }
                }
                if has_number {
                    if is_float {
                        return ArrowDataType::Float64;
                    } else if fits_i32 {
                        return ArrowDataType::Int32;
                    } else {
                        return ArrowDataType::Int64;
                    }
                }
            }
        }
    }
    ArrowDataType::Utf8
}

/// Detect key and value types for a Map column by inspecting OneOf choices.
fn detect_map_kv_types(fp: &crate::plan::FieldPlan) -> (ArrowDataType, ArrowDataType) {
    if let crate::plan::GeneratorPlan::OneOf { choices, .. } = &fp.generator_plan {
        for choice in choices {
            let s = match &choice.value {
                crate::core::Value::String(s) => s.clone(),
                _ => continue,
            };
            if let Ok(map) = serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&s)
            {
                if let Some(val) = map.values().next() {
                    let val_type = match val {
                        serde_json::Value::Number(n) => {
                            if n.is_i64() || n.is_u64() {
                                ArrowDataType::Int32
                            } else {
                                ArrowDataType::Float64
                            }
                        }
                        _ => ArrowDataType::Utf8,
                    };
                    return (ArrowDataType::Utf8, val_type);
                }
            }
        }
    }
    (ArrowDataType::Utf8, ArrowDataType::Utf8)
}

/// Convert a string array to a ListArray by parsing JSON.
fn string_to_list_array(
    col: &ArrayRef,
    element_type: &ArrowDataType,
    element_field_name: &str,
) -> Option<ArrayRef> {
    use arrow::array::{
        as_string_array, Array, GenericListArray, Int32Builder, Int64Builder, ListBuilder,
        StringBuilder,
    };

    // Only attempt conversion if source column is actually a string array
    if !matches!(
        col.data_type(),
        ArrowDataType::Utf8 | ArrowDataType::LargeUtf8
    ) {
        return None;
    }
    let str_arr = as_string_array(col.as_ref());

    let built_array: Option<ArrayRef> = match element_type {
        ArrowDataType::Utf8 => {
            let mut builder = ListBuilder::new(StringBuilder::new());
            for i in 0..str_arr.len() {
                if str_arr.is_null(i) {
                    builder.append_null();
                } else {
                    let val = str_arr.value(i);
                    if let Ok(items) = serde_json::from_str::<Vec<Option<String>>>(val) {
                        for item in &items {
                            match item {
                                Some(s) => builder.values().append_value(s),
                                None => builder.values().append_null(),
                            }
                        }
                        builder.append(true);
                    } else {
                        // Treat as single-element list
                        builder.values().append_value(val);
                        builder.append(true);
                    }
                }
            }
            Some(Arc::new(builder.finish()) as ArrayRef)
        }
        ArrowDataType::Int32 => {
            let mut builder = ListBuilder::new(Int32Builder::new());
            for i in 0..str_arr.len() {
                if str_arr.is_null(i) {
                    builder.append_null();
                } else {
                    let val = str_arr.value(i);
                    if let Ok(items) = serde_json::from_str::<Vec<Option<i32>>>(val) {
                        for item in &items {
                            match item {
                                Some(v) => builder.values().append_value(*v),
                                None => builder.values().append_null(),
                            }
                        }
                        builder.append(true);
                    } else {
                        builder.values().append_null();
                    }
                }
            }
            Some(Arc::new(builder.finish()) as ArrayRef)
        }
        ArrowDataType::Int64 => {
            let mut builder = ListBuilder::new(Int64Builder::new());
            for i in 0..str_arr.len() {
                if str_arr.is_null(i) {
                    builder.append_null();
                } else {
                    let val = str_arr.value(i);
                    if let Ok(items) = serde_json::from_str::<Vec<Option<i64>>>(val) {
                        for item in &items {
                            match item {
                                Some(v) => builder.values().append_value(*v),
                                None => builder.values().append_null(),
                            }
                        }
                        builder.append(true);
                    } else {
                        builder.values().append_null();
                    }
                }
            }
            Some(Arc::new(builder.finish()) as ArrayRef)
        }
        _ => None,
    };

    // Re-wrap with correct element field name to match source schema
    built_array.map(|arr| {
        let list_arr = arr
            .as_any()
            .downcast_ref::<GenericListArray<i32>>()
            .expect("string_to_list_array builds ListArray<i32> via ListBuilder");
        let field = Arc::new(ArrowField::new(
            element_field_name,
            element_type.clone(),
            true,
        ));
        let new_list = GenericListArray::<i32>::new(
            field,
            list_arr.offsets().clone(),
            list_arr.values().clone(),
            list_arr.nulls().cloned(),
        );
        Arc::new(new_list) as ArrayRef
    })
}

/// Convert a string array to a MapArray by parsing JSON.
fn string_to_map_array(
    col: &ArrayRef,
    key_type: &ArrowDataType,
    value_type: &ArrowDataType,
    field_names: Option<arrow::array::MapFieldNames>,
) -> Option<ArrayRef> {
    use arrow::array::{as_string_array, Array, Int32Builder, MapBuilder, StringBuilder};

    // Only attempt conversion if source column is actually a string array
    if !matches!(
        col.data_type(),
        ArrowDataType::Utf8 | ArrowDataType::LargeUtf8
    ) {
        return None;
    }
    let str_arr = as_string_array(col.as_ref());

    // Only support Map<String, Int32> and Map<String, String> for now
    if *key_type != ArrowDataType::Utf8 {
        return None;
    }

    match value_type {
        ArrowDataType::Int32 => {
            let mut builder =
                MapBuilder::new(field_names, StringBuilder::new(), Int32Builder::new());
            for i in 0..str_arr.len() {
                if str_arr.is_null(i) {
                    builder.append(false).ok()?;
                } else {
                    let val = str_arr.value(i);
                    if let Ok(map) =
                        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(val)
                    {
                        for (k, v) in &map {
                            builder.keys().append_value(k);
                            if v.is_null() {
                                builder.values().append_null();
                            } else {
                                builder
                                    .values()
                                    .append_value(v.as_i64().unwrap_or(0) as i32);
                            }
                        }
                        builder.append(true).ok()?;
                    } else {
                        builder.append(true).ok()?;
                    }
                }
            }
            Some(Arc::new(builder.finish()) as ArrayRef)
        }
        ArrowDataType::Utf8 => {
            let mut builder =
                MapBuilder::new(field_names, StringBuilder::new(), StringBuilder::new());
            for i in 0..str_arr.len() {
                if str_arr.is_null(i) {
                    builder.append(false).ok()?;
                } else {
                    let val = str_arr.value(i);
                    if let Ok(map) =
                        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(val)
                    {
                        for (k, v) in &map {
                            builder.keys().append_value(k);
                            if v.is_null() {
                                builder.values().append_null();
                            } else {
                                builder
                                    .values()
                                    .append_value(v.as_str().unwrap_or_default());
                            }
                        }
                        builder.append(true).ok()?;
                    } else {
                        builder.append(true).ok()?;
                    }
                }
            }
            Some(Arc::new(builder.finish()) as ArrayRef)
        }
        _ => None,
    }
}

/// Cast columns in a batch to match the target schema types.
/// Only casts when types differ and the cast is supported (e.g., Int64 → Int32).
fn cast_batch_to_schema(
    batch: &RecordBatch,
    target_schema: &Arc<Schema>,
) -> Result<RecordBatch, crate::gen::GenError> {
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

    let mut adjusted_fields: Vec<ArrowField> = target_schema
        .fields()
        .iter()
        .map(|f| f.as_ref().clone())
        .collect();

    let columns: Vec<ArrayRef> = batch
        .columns()
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let target_type = target_schema.field(i).data_type();
            if col.data_type() == target_type {
                col.clone()
            } else {
                // Try complex type conversion (string → list/map) before standard cast
                let complex_result = match target_type {
                    ArrowDataType::List(elem_field) => {
                        string_to_list_array(col, elem_field.data_type(), elem_field.name())
                    }
                    ArrowDataType::Map(entries_field, _) => {
                        if let ArrowDataType::Struct(fields) = entries_field.data_type() {
                            if fields.len() == 2 {
                                let key_type = fields[0].data_type();
                                let val_type = fields[1].data_type();
                                let map_field_names = Some(arrow::array::MapFieldNames {
                                    entry: entries_field.name().to_string(),
                                    key: fields[0].name().to_string(),
                                    value: fields[1].name().to_string(),
                                });
                                string_to_map_array(col, key_type, val_type, map_field_names)
                            } else {
                                None
                            }
                        } else {
                            None
                        }
                    }
                    _ => None,
                };

                if let Some(converted) = complex_result {
                    converted
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
            }
        })
        .collect();

    let final_schema = Arc::new(Schema::new(adjusted_fields));
    RecordBatch::try_new(final_schema, columns)
        .map_err(|e| crate::gen::GenError::Generation(format!("schema cast error: {}", e)))
}

/// Infer the Arrow data type from a GeneratorPlan variant.
fn infer_arrow_type(gp: &crate::plan::GeneratorPlan) -> ArrowDataType {
    use crate::core::DistributionKind;

    match gp {
        crate::plan::GeneratorPlan::Distribution { kind, round, .. } => {
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
        }
        crate::plan::GeneratorPlan::Sequence { .. } => ArrowDataType::Int64,
        crate::plan::GeneratorPlan::CyclicValues { .. } => ArrowDataType::Utf8,
        crate::plan::GeneratorPlan::Uuid => ArrowDataType::Utf8,
        crate::plan::GeneratorPlan::Faker { category, .. } => match category.as_str() {
            "datetime" | "timestamp" => {
                ArrowDataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, None)
            }
            "date" => ArrowDataType::Date32,
            _ => ArrowDataType::Utf8,
        },
        crate::plan::GeneratorPlan::Pattern { .. } => ArrowDataType::Utf8,
        crate::plan::GeneratorPlan::OneOf { choices, .. } => infer_one_of_type(choices),
        crate::plan::GeneratorPlan::Constant(val) => match val {
            crate::core::Value::Null => ArrowDataType::Utf8,
            crate::core::Value::Bool(_) => ArrowDataType::Boolean,
            crate::core::Value::Int(_) => ArrowDataType::Int64,
            crate::core::Value::Float(_) => ArrowDataType::Float64,
            crate::core::Value::String(_) => ArrowDataType::Utf8,
            // Complex types (Array, Map, DateTime, etc.) are not yet supported
            // by ConstantGenerator — it emits NullArray for them.
            _ => ArrowDataType::Utf8,
        },
        crate::plan::GeneratorPlan::Derived { .. } => ArrowDataType::Float64,
        crate::plan::GeneratorPlan::ForeignKey { .. } => ArrowDataType::Int64,
        crate::plan::GeneratorPlan::Temporal { .. } => ArrowDataType::Int64,
        crate::plan::GeneratorPlan::Correlated { .. } => ArrowDataType::Float64,
        crate::plan::GeneratorPlan::Topology { .. } => ArrowDataType::Int64,
        crate::plan::GeneratorPlan::Composite { .. } => ArrowDataType::Utf8,
        crate::plan::GeneratorPlan::Unique { inner, .. } => infer_arrow_type(inner),
        crate::plan::GeneratorPlan::Conditional { default, .. } => infer_arrow_type(default),
        crate::plan::GeneratorPlan::Dictionary { .. } => ArrowDataType::Utf8,
        crate::plan::GeneratorPlan::GraphTarget { .. } => ArrowDataType::Int64,
        crate::plan::GeneratorPlan::PersonaField { .. } => ArrowDataType::Float64,
        crate::plan::GeneratorPlan::ActorTemporal { .. } => {
            ArrowDataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None)
        }
        crate::plan::GeneratorPlan::ThreadRef { .. } => ArrowDataType::Int64,
        // Plugin output type unknown at plan time — default to Utf8
        crate::plan::GeneratorPlan::Plugin { .. } => ArrowDataType::Utf8,
        crate::plan::GeneratorPlan::ExternalLookup { .. } => ArrowDataType::Utf8,
        // Struct output type is built from sub-field plans at runtime
        crate::plan::GeneratorPlan::Struct => ArrowDataType::Utf8,
        crate::plan::GeneratorPlan::NumericTimeSeries { .. } => ArrowDataType::Float64,
        crate::plan::GeneratorPlan::EventStream { .. } => {
            ArrowDataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None)
        }
    }
}

/// Map a declared field data_type to a reasonable default Arrow type.
/// Used for plugin generators where we can't infer from the plan.
fn default_arrow_for_data_type(dt: &crate::core::DataType) -> ArrowDataType {
    match dt {
        crate::core::DataType::Int => ArrowDataType::Int64,
        crate::core::DataType::Int32 => ArrowDataType::Int32,
        crate::core::DataType::Float => ArrowDataType::Float64,
        crate::core::DataType::Bool => ArrowDataType::Boolean,
        crate::core::DataType::String => ArrowDataType::Utf8,
        crate::core::DataType::Uuid => ArrowDataType::Utf8,
        crate::core::DataType::Date => ArrowDataType::Date32,
        crate::core::DataType::Time => ArrowDataType::Time64(arrow::datatypes::TimeUnit::Nanosecond),
        crate::core::DataType::Datetime => {
            ArrowDataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, None)
        }
        crate::core::DataType::DatetimeUs => {
            ArrowDataType::Timestamp(arrow::datatypes::TimeUnit::Microsecond, None)
        }
        crate::core::DataType::Datetimetz => {
            ArrowDataType::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, Some("UTC".into()))
        }
        crate::core::DataType::Duration => {
            ArrowDataType::Duration(arrow::datatypes::TimeUnit::Millisecond)
        }
        crate::core::DataType::Bytes => ArrowDataType::Binary,
        crate::core::DataType::Array => {
            ArrowDataType::List(Arc::new(ArrowField::new("element", ArrowDataType::Utf8, true)))
        }
        crate::core::DataType::Map => ArrowDataType::Utf8,
        crate::core::DataType::Object => ArrowDataType::Utf8, // struct handled at plan level
        crate::core::DataType::Custom(ref name) => {
            unreachable!("custom type '{}' should be resolved before planning", name)
        }
    }
}

/// Infer the Arrow data type for a `OneOf` generator from its choice values.
///
/// Mirrors the logic in `crate::gen::generators::one_of::infer_output_type`.
fn infer_one_of_type(choices: &[crate::core::WeightedChoice]) -> ArrowDataType {
    for c in choices {
        match &c.value {
            crate::core::Value::String(_) => return ArrowDataType::Utf8,
            crate::core::Value::Int(_) => return ArrowDataType::Int64,
            crate::core::Value::Float(_) => return ArrowDataType::Float64,
            crate::core::Value::Bool(_) => return ArrowDataType::Boolean,
            crate::core::Value::Null => continue,
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
    output_entities: &std::collections::HashSet<&str>,
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
            // Only show progress bars for entities that will be output
            if !output_entities.is_empty() && !output_entities.contains(ep.entity_name.as_str()) {
                continue;
            }
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
/// overrides via [`crate::noise::PerturbOverrides`], so multiple profiles targeting the
/// same entity with different rates and column sets work correctly.
fn build_noise_pipelines(profiles: &[NoiseProfile], model_seed: u64) -> HashMap<String, Pipeline> {
    use crate::noise::{
        DuplicateInjector, FkViolateInjector, NullInjector, OutlierInjector, PerturbOverrides,
        SwapInjector, TemporalSpikeInjector, TruncateInjector, TypoInjector,
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
            || profile.duplicate_rate > 0.0
            || profile.swap_rate > 0.0
            || profile.truncate_rate > 0.0
            || profile.fk_violate_rate > 0.0
            || profile.temporal_spike_rate > 0.0;

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

        // Compile scope expression once (if present).
        let scope_expr = profile.scope.as_ref().map(|s| {
            crate::gen::expr::parser::parse(&s.where_expr).unwrap_or_else(|e| {
                tracing::warn!(
                    name = %profile.name,
                    error = %e,
                    "failed to parse scope expression, ignoring scope"
                );
                // Return a constant-true expression as fallback
                crate::gen::expr::ast::Expr::Literal(
                    crate::gen::expr::ast::LiteralValue::Bool(true),
                )
            })
        });

        // Helper to build overrides with this profile's rate and column filter.
        let make_overrides = |rate: f64| PerturbOverrides {
            probability: Some(rate),
            columns: col_filter.clone(),
            scope_expr: scope_expr.clone(),
        };

        // Add perturbators with their individual rates and column filters
        if profile.null_rate > 0.0 {
            pipeline.add_with_overrides(
                Box::new(NullInjector::new()),
                make_overrides(profile.null_rate),
            );
        }
        if profile.typo_rate > 0.0 {
            pipeline.add_with_overrides(
                Box::new(TypoInjector::new()),
                make_overrides(profile.typo_rate),
            );
        }
        if profile.outlier_rate > 0.0 {
            pipeline.add_with_overrides(
                Box::new(OutlierInjector::new(5.0)),
                make_overrides(profile.outlier_rate),
            );
        }
        if profile.duplicate_rate > 0.0 {
            pipeline.add_with_overrides(
                Box::new(DuplicateInjector::new()),
                make_overrides(profile.duplicate_rate),
            );
        }
        if profile.swap_rate > 0.0 {
            pipeline.add_with_overrides(
                Box::new(SwapInjector::new()),
                make_overrides(profile.swap_rate),
            );
        }
        if profile.truncate_rate > 0.0 {
            pipeline.add_with_overrides(
                Box::new(TruncateInjector::new()),
                make_overrides(profile.truncate_rate),
            );
        }
        if profile.fk_violate_rate > 0.0 {
            pipeline.add_with_overrides(
                Box::new(FkViolateInjector::new()),
                make_overrides(profile.fk_violate_rate),
            );
        }
        if profile.temporal_spike_rate > 0.0 {
            pipeline.add_with_overrides(
                Box::new(TemporalSpikeInjector::new()),
                make_overrides(profile.temporal_spike_rate),
            );
        }
    }

    entity_pipelines
}

/// Collect [`MissingFieldSpec`]s per entity from noise profiles.
///
/// Returns a map from entity name to a list of missing-field specs.
/// These are passed to JSON/JSONL sinks to omit fields at serialization time.
fn collect_missing_field_specs(
    profiles: &[NoiseProfile],
    model_seed: u64,
) -> HashMap<String, Vec<crate::bind::MissingFieldSpec>> {
    let mut result: HashMap<String, Vec<crate::bind::MissingFieldSpec>> = HashMap::new();

    for (prof_idx, profile) in profiles.iter().enumerate() {
        if profile.entity.is_empty() || profile.missing_field_rate <= 0.0 {
            continue;
        }

        let prof_seed = model_seed
            .wrapping_add(prof_idx as u64 * 1000)
            .wrapping_add(0xDEAD_BEEF); // distinct from pipeline seeds

        let fields = if profile.fields.is_empty() {
            // When no fields specified, the specs will be built later
            // when we know the schema. For now, store an empty marker.
            tracing::warn!(
                profile = %profile.name,
                "missing_field noise with no target fields; specify fields explicitly"
            );
            continue;
        } else {
            profile.fields.clone()
        };

        let specs = result.entry(profile.entity.clone()).or_default();
        for (fi, field) in fields.iter().enumerate() {
            specs.push(crate::bind::MissingFieldSpec {
                field: field.clone(),
                probability: profile.missing_field_rate,
                seed: prof_seed.wrapping_add(fi as u64 * 7),
            });
        }
    }

    result
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
                CountSpec::Expression { .. } => {
                    // Resolve the expression first, then scale.
                    crate::plan::partition::resolve_count(&entity.count, &model.params)
                        .unwrap_or(1000)
                }
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

/// Convert a crate::core::Value to its string representation for param injection.
fn value_to_string(v: &crate::core::Value) -> String {
    match v {
        crate::core::Value::String(s) => s.clone(),
        crate::core::Value::Int(n) => n.to_string(),
        crate::core::Value::Float(f) => f.to_string(),
        crate::core::Value::Bool(b) => b.to_string(),
        crate::core::Value::Null => String::new(),
        crate::core::Value::Array(arr) => serde_json::to_string(arr).unwrap_or_default(),
        crate::core::Value::Map(map) => serde_json::to_string(map).unwrap_or_default(),
    }
}

/// Flatten a schema for CSV output by converting nested types (List, Map, Struct)
/// to Utf8 (they'll be serialized as JSON strings).
fn flatten_schema_for_csv(schema: &Schema) -> Schema {
    let fields: Vec<ArrowField> = schema
        .fields()
        .iter()
        .map(|f| {
            if is_nested_type(f.data_type()) {
                ArrowField::new(f.name(), ArrowDataType::Utf8, f.is_nullable())
            } else {
                f.as_ref().clone()
            }
        })
        .collect();
    Schema::new(fields)
}

/// Convert nested columns (List, Map, Struct) to JSON string representation
/// for formats that don't support nested structures (e.g. CSV).
fn flatten_nested_columns(batch: &RecordBatch) -> Result<RecordBatch, crate::gen::GenError> {
    use arrow::array::StringArray;

    let schema = batch.schema();
    let mut columns: Vec<arrow::array::ArrayRef> = Vec::with_capacity(batch.num_columns());
    let mut fields: Vec<ArrowField> = Vec::with_capacity(batch.num_columns());

    for (i, field) in schema.fields().iter().enumerate() {
        let col = batch.column(i);
        if is_nested_type(field.data_type()) {
            let json_strings: Vec<Option<String>> = (0..col.len())
                .map(|row| {
                    if col.is_null(row) {
                        None
                    } else {
                        Some(array_value_to_json_string(col, row))
                    }
                })
                .collect();
            let string_arr = StringArray::from(json_strings);
            columns.push(Arc::new(string_arr));
            fields.push(ArrowField::new(
                field.name(),
                ArrowDataType::Utf8,
                field.is_nullable(),
            ));
        } else {
            columns.push(Arc::clone(col));
            fields.push(field.as_ref().clone());
        }
    }

    RecordBatch::try_new(Arc::new(Schema::new(fields)), columns)
        .map_err(|e| crate::gen::GenError::Generation(format!("flatten error: {e}")))
}

/// Check if an Arrow data type is a nested/complex type.
fn is_nested_type(dt: &ArrowDataType) -> bool {
    matches!(
        dt,
        ArrowDataType::List(_)
            | ArrowDataType::LargeList(_)
            | ArrowDataType::FixedSizeList(_, _)
            | ArrowDataType::Map(_, _)
            | ArrowDataType::Struct(_)
    )
}

/// Serialize a single array element at the given row index to a JSON string.
fn array_value_to_json_string(arr: &dyn arrow::array::Array, row: usize) -> String {
    use arrow::array::*;
    use serde_json::Value as JVal;

    fn arr_to_json(arr: &dyn Array, row: usize) -> JVal {
        if arr.is_null(row) {
            return JVal::Null;
        }
        match arr.data_type() {
            ArrowDataType::Utf8 => {
                let a = arr.as_any().downcast_ref::<StringArray>().unwrap();
                JVal::String(a.value(row).to_string())
            }
            ArrowDataType::LargeUtf8 => {
                let a = arr.as_any().downcast_ref::<LargeStringArray>().unwrap();
                JVal::String(a.value(row).to_string())
            }
            ArrowDataType::Int32 => {
                let a = arr.as_any().downcast_ref::<Int32Array>().unwrap();
                JVal::Number(a.value(row).into())
            }
            ArrowDataType::Int64 => {
                let a = arr.as_any().downcast_ref::<Int64Array>().unwrap();
                JVal::Number(a.value(row).into())
            }
            ArrowDataType::Float64 => {
                let a = arr.as_any().downcast_ref::<Float64Array>().unwrap();
                serde_json::Number::from_f64(a.value(row))
                    .map(JVal::Number)
                    .unwrap_or(JVal::Null)
            }
            ArrowDataType::Boolean => {
                let a = arr.as_any().downcast_ref::<BooleanArray>().unwrap();
                JVal::Bool(a.value(row))
            }
            ArrowDataType::Int8 => {
                let a = arr.as_any().downcast_ref::<Int8Array>().unwrap();
                JVal::Number(a.value(row).into())
            }
            ArrowDataType::Int16 => {
                let a = arr.as_any().downcast_ref::<Int16Array>().unwrap();
                JVal::Number(a.value(row).into())
            }
            ArrowDataType::UInt8 => {
                let a = arr.as_any().downcast_ref::<UInt8Array>().unwrap();
                JVal::Number(a.value(row).into())
            }
            ArrowDataType::UInt16 => {
                let a = arr.as_any().downcast_ref::<UInt16Array>().unwrap();
                JVal::Number(a.value(row).into())
            }
            ArrowDataType::UInt32 => {
                let a = arr.as_any().downcast_ref::<UInt32Array>().unwrap();
                JVal::Number(a.value(row).into())
            }
            ArrowDataType::UInt64 => {
                let a = arr.as_any().downcast_ref::<UInt64Array>().unwrap();
                JVal::Number(a.value(row).into())
            }
            ArrowDataType::Float32 => {
                let a = arr.as_any().downcast_ref::<Float32Array>().unwrap();
                serde_json::Number::from_f64(a.value(row) as f64)
                    .map(JVal::Number)
                    .unwrap_or(JVal::Null)
            }
            ArrowDataType::Date32 | ArrowDataType::Date64 | ArrowDataType::Timestamp(_, _) => {
                let formatted =
                    arrow::util::display::array_value_to_string(arr, row).unwrap_or_default();
                JVal::String(formatted)
            }
            ArrowDataType::List(_) => {
                let list = arr.as_any().downcast_ref::<ListArray>().unwrap();
                let values = list.value(row);
                let items: Vec<JVal> = (0..values.len())
                    .map(|i| arr_to_json(values.as_ref(), i))
                    .collect();
                JVal::Array(items)
            }
            ArrowDataType::LargeList(_) => {
                let list = arr.as_any().downcast_ref::<LargeListArray>().unwrap();
                let values = list.value(row);
                let items: Vec<JVal> = (0..values.len())
                    .map(|i| arr_to_json(values.as_ref(), i))
                    .collect();
                JVal::Array(items)
            }
            ArrowDataType::Map(_, _) => {
                let map = arr.as_any().downcast_ref::<MapArray>().unwrap();
                let entries = map.value(row);
                let struct_arr = entries.as_any().downcast_ref::<StructArray>().unwrap();
                let keys = struct_arr.column(0);
                let vals = struct_arr.column(1);
                let mut obj = serde_json::Map::new();
                for i in 0..entries.len() {
                    let key = arr_to_json(keys.as_ref(), i);
                    let val = arr_to_json(vals.as_ref(), i);
                    // Stringify non-string keys to preserve all entries
                    let k = match key {
                        JVal::String(s) => s,
                        other => other.to_string(),
                    };
                    obj.insert(k, val);
                }
                JVal::Object(obj)
            }
            ArrowDataType::Struct(fields) => {
                let s = arr.as_any().downcast_ref::<StructArray>().unwrap();
                let mut obj = serde_json::Map::new();
                for (fi, field) in fields.iter().enumerate() {
                    let col = s.column(fi);
                    obj.insert(field.name().clone(), arr_to_json(col.as_ref(), row));
                }
                JVal::Object(obj)
            }
            _ => {
                // Fallback: use Arrow's built-in display for the specific row value
                JVal::String(
                    arrow::util::display::array_value_to_string(arr, row)
                        .unwrap_or_else(|_| format!("<unsupported: {:?}>", arr.data_type())),
                )
            }
        }
    }

    serde_json::to_string(&arr_to_json(arr, row)).unwrap_or_default()
}

/// Resolve dictionary file references in the compiled plan.
///
/// Walks all entity plans and loads dictionary files from disk, replacing
/// the empty `entries` vec with actual file contents. File paths are
/// resolved relative to the schema directory.
fn resolve_dictionary_plans(plan: &mut ExecutionPlan, schema_dir: &Path) -> Result<()> {
    for phase in &mut plan.phases {
        for entity_plan in &mut phase.entity_plans {
            for field_plan in &mut entity_plan.field_plans {
                resolve_dict_in_generator(&mut field_plan.generator_plan, schema_dir)?;
            }
        }
    }
    Ok(())
}

/// Recursively resolve dictionary generators (handles Unique/Conditional wrapping).
fn resolve_dict_in_generator(plan: &mut crate::plan::GeneratorPlan, schema_dir: &Path) -> Result<()> {
    use std::io::BufRead;

    match plan {
        crate::plan::GeneratorPlan::Dictionary {
            entries,
            source_file,
            ..
        } => {
            if let Some(file_path) = source_file.take() {
                // Reject absolute paths to prevent path traversal
                if Path::new(&file_path).is_absolute() {
                    bail!(
                        "dictionary file path must be relative to schema directory, got absolute path: '{}'",
                        file_path
                    );
                }
                // Reject paths that escape schema_dir via ..
                if file_path.contains("..") {
                    bail!(
                        "dictionary file path must not contain '..': '{}'",
                        file_path
                    );
                }
                let full_path = schema_dir.join(&file_path);
                let file = std::fs::File::open(&full_path).with_context(|| {
                    format!(
                        "failed to open dictionary file '{}' (resolved to '{}')",
                        file_path,
                        full_path.display()
                    )
                })?;
                let reader = std::io::BufReader::new(file);
                *entries = reader
                    .lines()
                    .filter_map(|line| {
                        let line = line.ok()?;
                        let trimmed = line.trim().to_string();
                        if trimmed.is_empty() {
                            None
                        } else {
                            Some(trimmed)
                        }
                    })
                    .collect();
                tracing::debug!(
                    dict_file = %file_path,
                    entries = entries.len(),
                    "loaded dictionary"
                );
            }
        }
        crate::plan::GeneratorPlan::Unique { inner, .. } => {
            resolve_dict_in_generator(inner, schema_dir)?;
        }
        crate::plan::GeneratorPlan::Conditional {
            branches, default, ..
        } => {
            for (_, branch_plan) in branches {
                resolve_dict_in_generator(branch_plan, schema_dir)?;
            }
            resolve_dict_in_generator(default, schema_dir)?;
        }
        _ => {}
    }
    Ok(())
}

/// Resolve external lookup file references in the compiled plan.
///
/// Walks all entity plans and loads external lookup source files from disk,
/// populating the `entries` and `weights` vecs. File paths are resolved
/// relative to the schema directory.
fn resolve_external_lookup_plans(plan: &mut ExecutionPlan, schema_dir: &Path) -> Result<()> {
    for phase in &mut plan.phases {
        for entity_plan in &mut phase.entity_plans {
            for field_plan in &mut entity_plan.field_plans {
                resolve_lookup_in_generator(&mut field_plan.generator_plan, schema_dir)?;
            }
        }
    }
    Ok(())
}

/// Recursively resolve external lookup generators (handles Unique/Conditional wrapping).
fn resolve_lookup_in_generator(
    plan: &mut crate::plan::GeneratorPlan,
    schema_dir: &Path,
) -> Result<()> {
    match plan {
        crate::plan::GeneratorPlan::ExternalLookup {
            entries,
            weights,
            source_file,
            source_column,
            weight_column,
            source_format,
            sampling,
            ..
        } => {
            if let (Some(file_path), Some(column), Some(format)) = (
                source_file.take(),
                source_column.take(),
                source_format.take(),
            ) {
                // Path traversal protection
                if Path::new(&file_path).is_absolute() {
                    bail!(
                        "external lookup source path must be relative, got absolute path: '{}'",
                        file_path
                    );
                }
                if file_path.contains("..") {
                    bail!(
                        "external lookup source path must not contain '..': '{}'",
                        file_path
                    );
                }

                let full_path = schema_dir.join(&file_path);

                // Canonicalize both paths and verify the resolved file is under schema_dir.
                // This catches symlink/junction escapes.
                if let (Ok(canonical_dir), Ok(canonical_file)) = (
                    std::fs::canonicalize(schema_dir),
                    std::fs::canonicalize(&full_path),
                ) {
                    if !canonical_file.starts_with(&canonical_dir) {
                        bail!(
                            "external lookup source '{}' resolves outside schema directory",
                            file_path
                        );
                    }
                }
                // If canonicalize fails (e.g. file doesn't exist), the open call
                // below will produce a clear error message.
                let wc = weight_column.take();
                let need_weights = *sampling == crate::core::SamplingMode::Weighted;

                let (loaded_entries, loaded_weights) =
                    load_lookup_file(&full_path, &column, &format, wc.as_deref(), &file_path)?;

                if loaded_entries.is_empty() {
                    bail!(
                        "external lookup source '{}' column '{}' contains no values",
                        file_path,
                        column
                    );
                }

                if need_weights {
                    if let Some(ref w) = loaded_weights {
                        let total: f64 = w.iter().sum();
                        if total <= 0.0 || !total.is_finite() {
                            bail!(
                                "external lookup '{}' weight column has invalid total weight: {}",
                                file_path,
                                total
                            );
                        }
                    } else {
                        bail!(
                            "external lookup '{}' uses weighted sampling but no weights were loaded",
                            file_path
                        );
                    }
                }

                *entries = loaded_entries;
                *weights = loaded_weights;

                tracing::debug!(
                    source = %file_path,
                    column = %column,
                    entries = entries.len(),
                    "loaded external lookup"
                );
            }
        }
        crate::plan::GeneratorPlan::Unique { inner, .. } => {
            resolve_lookup_in_generator(inner, schema_dir)?;
        }
        crate::plan::GeneratorPlan::Conditional {
            branches, default, ..
        } => {
            for (_, branch_plan) in branches {
                resolve_lookup_in_generator(branch_plan, schema_dir)?;
            }
            resolve_lookup_in_generator(default, schema_dir)?;
        }
        _ => {}
    }
    Ok(())
}

/// Load values (and optional weights) from a CSV, JSON, or Parquet file.
fn load_lookup_file(
    path: &Path,
    column: &str,
    format: &crate::core::LookupFormat,
    weight_column: Option<&str>,
    display_path: &str,
) -> Result<(Vec<String>, Option<Vec<f64>>)> {
    match format {
        crate::core::LookupFormat::Csv => {
            load_lookup_csv(path, column, weight_column, display_path)
        }
        crate::core::LookupFormat::Json => {
            load_lookup_json(path, column, weight_column, display_path)
        }
        crate::core::LookupFormat::Parquet => {
            load_lookup_parquet(path, column, weight_column, display_path)
        }
    }
}

/// Load from CSV with header row.
fn load_lookup_csv(
    path: &Path,
    column: &str,
    weight_column: Option<&str>,
    display_path: &str,
) -> Result<(Vec<String>, Option<Vec<f64>>)> {
    let mut reader = csv::Reader::from_path(path).with_context(|| {
        format!(
            "failed to open CSV lookup file '{}' (resolved to '{}')",
            display_path,
            path.display()
        )
    })?;

    let headers = reader.headers()?.clone();
    let col_idx = headers.iter().position(|h| h == column).ok_or_else(|| {
        anyhow::anyhow!(
            "column '{}' not found in CSV '{}'; available: {:?}",
            column,
            display_path,
            headers.iter().collect::<Vec<_>>()
        )
    })?;

    let weight_idx = weight_column
        .map(|wc| {
            headers.iter().position(|h| h == wc).ok_or_else(|| {
                anyhow::anyhow!(
                    "weight column '{}' not found in CSV '{}'; available: {:?}",
                    wc,
                    display_path,
                    headers.iter().collect::<Vec<_>>()
                )
            })
        })
        .transpose()?;

    let mut entries = Vec::new();
    let mut weights: Option<Vec<f64>> = weight_idx.map(|_| Vec::new());

    for result in reader.records() {
        let record =
            result.with_context(|| format!("failed to read CSV record from '{}'", display_path))?;
        if let Some(val) = record.get(col_idx) {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                entries.push(trimmed.to_string());
                if let (Some(ref mut w), Some(wi)) = (&mut weights, weight_idx) {
                    let weight_str = record.get(wi).unwrap_or("");
                    let weight: f64 = weight_str.trim().parse().map_err(|_| {
                        anyhow::anyhow!(
                            "non-numeric weight '{}' in CSV '{}' for column '{}'",
                            weight_str,
                            display_path,
                            weight_column.unwrap_or("?")
                        )
                    })?;
                    if weight < 0.0 || !weight.is_finite() {
                        bail!(
                            "invalid weight '{}' in CSV '{}' for column '{}'",
                            weight_str,
                            display_path,
                            weight_column.unwrap_or("?")
                        );
                    }
                    w.push(weight);
                }
            }
        }
    }

    Ok((entries, weights))
}

/// Load from JSON (array of objects).
fn load_lookup_json(
    path: &Path,
    column: &str,
    weight_column: Option<&str>,
    display_path: &str,
) -> Result<(Vec<String>, Option<Vec<f64>>)> {
    let content = std::fs::read_to_string(path).with_context(|| {
        format!(
            "failed to read JSON lookup file '{}' (resolved to '{}')",
            display_path,
            path.display()
        )
    })?;

    let arr: Vec<serde_json::Value> = serde_json::from_str(&content).with_context(|| {
        format!(
            "failed to parse JSON array from '{}' — expected array of objects",
            display_path
        )
    })?;

    let mut entries = Vec::with_capacity(arr.len());
    let mut weights: Option<Vec<f64>> = weight_column.map(|_| Vec::with_capacity(arr.len()));

    for obj in &arr {
        let val = obj.get(column).ok_or_else(|| {
            anyhow::anyhow!(
                "JSON object in '{}' missing column '{}'",
                display_path,
                column
            )
        })?;

        let s = match val {
            serde_json::Value::String(s) => s.clone(),
            serde_json::Value::Number(n) => n.to_string(),
            serde_json::Value::Bool(b) => b.to_string(),
            serde_json::Value::Null => continue,
            _ => val.to_string(),
        };

        if !s.is_empty() {
            entries.push(s);
            if let (Some(ref mut w), Some(wc)) = (&mut weights, weight_column) {
                let weight_val = obj.get(wc).ok_or_else(|| {
                    anyhow::anyhow!(
                        "JSON object in '{}' missing weight column '{}'",
                        display_path,
                        wc
                    )
                })?;
                let weight = weight_val.as_f64().ok_or_else(|| {
                    anyhow::anyhow!(
                        "non-numeric weight {:?} in JSON '{}' for column '{}'",
                        weight_val,
                        display_path,
                        wc
                    )
                })?;
                if weight < 0.0 || !weight.is_finite() {
                    bail!(
                        "invalid weight in JSON '{}' for column '{}'",
                        display_path,
                        wc
                    );
                }
                w.push(weight);
            }
        }
    }

    Ok((entries, weights))
}

/// Load from Parquet using Arrow readers.
fn load_lookup_parquet(
    path: &Path,
    column: &str,
    weight_column: Option<&str>,
    display_path: &str,
) -> Result<(Vec<String>, Option<Vec<f64>>)> {
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    let file = std::fs::File::open(path).with_context(|| {
        format!(
            "failed to open Parquet lookup file '{}' (resolved to '{}')",
            display_path,
            path.display()
        )
    })?;

    let builder = ParquetRecordBatchReaderBuilder::try_new(file).with_context(|| {
        format!(
            "failed to read Parquet metadata from '{}'",
            display_path
        )
    })?;

    let reader = builder.build().with_context(|| {
        format!("failed to build Parquet reader for '{}'", display_path)
    })?;

    let mut entries = Vec::new();
    let mut weights: Option<Vec<f64>> = weight_column.map(|_| Vec::new());

    for batch_result in reader {
        let batch = batch_result.with_context(|| {
            format!("failed to read Parquet batch from '{}'", display_path)
        })?;

        let col_idx = batch.schema().index_of(column).map_err(|_| {
            anyhow::anyhow!(
                "column '{}' not found in Parquet '{}'; available: {:?}",
                column,
                display_path,
                batch
                    .schema()
                    .fields()
                    .iter()
                    .map(|f| f.name().as_str())
                    .collect::<Vec<_>>()
            )
        })?;

        let col = batch.column(col_idx);

        // Resolve weight column index (if needed) once per batch
        let weight_col_idx = if let Some(wc) = weight_column {
            Some(batch.schema().index_of(wc).map_err(|_| {
                anyhow::anyhow!(
                    "weight column '{}' not found in Parquet '{}'; available: {:?}",
                    wc,
                    display_path,
                    batch
                        .schema()
                        .fields()
                        .iter()
                        .map(|f| f.name().as_str())
                        .collect::<Vec<_>>()
                )
            })?)
        } else {
            None
        };

        // Process rows in lockstep: only keep weights for rows whose value is kept
        for i in 0..col.len() {
            if col.is_null(i) {
                continue;
            }
            let val = arrow::util::display::array_value_to_string(col, i)
                .unwrap_or_default();
            let trimmed = val.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }

            entries.push(trimmed);

            if let (Some(ref mut w), Some(wi)) = (&mut weights, weight_col_idx) {
                let weight_col = batch.column(wi);
                let weight = extract_single_weight(weight_col, i)?;
                w.push(weight);
            }
        }
    }

    Ok((entries, weights))
}

/// Extract a single weight value from an Arrow array at a given row index.
fn extract_single_weight(array: &dyn arrow::array::Array, idx: usize) -> Result<f64> {
    use arrow::array::{Float64Array, Int32Array, Int64Array};

    if array.is_null(idx) {
        return Ok(0.0);
    }

    let w = if let Some(f64_arr) = array.as_any().downcast_ref::<Float64Array>() {
        f64_arr.value(idx)
    } else if let Some(i64_arr) = array.as_any().downcast_ref::<Int64Array>() {
        i64_arr.value(idx) as f64
    } else if let Some(i32_arr) = array.as_any().downcast_ref::<Int32Array>() {
        i32_arr.value(idx) as f64
    } else {
        bail!(
            "weight column has unsupported type {:?} — expected numeric",
            array.data_type()
        );
    };

    if w < 0.0 || !w.is_finite() {
        bail!("invalid weight value: {}", w);
    }
    Ok(w)
}

/// Load WASM plugins from `--plugin` and `--plugin-dir` CLI arguments.
fn load_plugins(cli: &Cli) -> Result<()> {
    #[cfg(feature = "wasm-plugins")]
    {
        use crate::gen::wasm_plugin;
        use std::collections::HashSet;

        let mut seen_names = HashSet::new();

        // Load individual plugin files.
        for path_str in &cli.plugins {
            let path = std::path::Path::new(path_str);
            wasm_plugin::load_wasm_plugin(path, &mut seen_names)
                .with_context(|| format!("failed to load WASM plugin `{}`", path_str))?;
        }

        // Load all plugins from a directory.
        if let Some(ref dir_str) = cli.plugin_dir {
            let dir = std::path::Path::new(dir_str);
            wasm_plugin::load_wasm_plugins_from_dir(dir)
                .with_context(|| format!("failed to load plugins from `{}`", dir_str))?;
        }
    }

    #[cfg(not(feature = "wasm-plugins"))]
    {
        if !cli.plugins.is_empty() || cli.plugin_dir.is_some() {
            bail!(
                "--plugin and --plugin-dir require the `wasm-plugins` feature.\n\
                 Rebuild with: cargo install knit --features wasm-plugins"
            );
        }
    }

    Ok(())
}