//! `knit inspect` — display summary information about a learn state file or blueprint file.
//!
//! This command reads a serialized [`LearnState`] file (.json) or a blueprint file (.toml)
//! and prints a human-readable summary. For state files: tables, row counts, columns,
//! cardinality estimates, and processing history. For blueprint files with `--actors`:
//! actor entities, personas, and actor relationships.

use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;
use crate::learn::streaming::state::LearnState;

use crate::cli::Cli;

/// Run the inspect command.
pub fn run(file_path: &str, show_columns: bool, show_actors: bool, cli: &Cli) -> Result<()> {
    let path = Path::new(file_path);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "toml" => run_schema(file_path, show_actors, cli),
        _ => run_state(file_path, show_columns, show_actors, cli),
    }
}

/// Inspect a blueprint file (.toml) for actor/persona/relationship summary.
fn run_schema(blueprint_path: &str, show_actors: bool, cli: &Cli) -> Result<()> {
    let model = super::load_blueprint(blueprint_path)
        .map_err(|e| anyhow::anyhow!("failed to parse schema: {}", e))?;

    if cli.json {
        print_schema_json(&model, show_actors);
    } else {
        print_schema_human(&model, show_actors);
    }
    Ok(())
}

fn print_schema_human(model: &crate::core::DataModel, show_actors: bool) {
    println!("{} — {}", "Schema Summary".bold(), model.name.cyan(),);
    if let Some(ref desc) = model.description {
        println!("  {}", desc.dimmed());
    }
    println!();

    // Entity overview
    println!("{}", "Entities:".bold());
    for entity in &model.entities {
        let field_count = entity.fields.len();
        let actor_cols: Vec<&str> = entity
            .fields
            .iter()
            .filter(|f| f.actor_column)
            .map(|f| f.name.as_str())
            .collect();
        let row_desc = if entity.activity_count.is_some() {
            match &entity.count {
                crate::core::types::CountSpec::Fixed(n) => format!("~{n} rows (activity-driven)"),
                _ => "dynamic rows (activity-driven)".to_string(),
            }
        } else {
            match &entity.count {
                crate::core::types::CountSpec::Fixed(n) => format!("{n} rows"),
                crate::core::types::CountSpec::Range { min, max } => format!("{min}–{max} rows"),
                _ => "dynamic rows".to_string(),
            }
        };

        if entity.actor {
            println!(
                "  {} {} — {} fields, {} {}",
                entity.name.green(),
                "(actor)".yellow(),
                field_count,
                row_desc,
                if let Some(ref pd) = entity.persona_distribution {
                    format!("[persona: {}]", pd)
                } else {
                    String::new()
                },
            );
        } else {
            println!(
                "  {} — {} fields, {}",
                entity.name.green(),
                field_count,
                row_desc,
            );
        }

        if !actor_cols.is_empty() {
            println!("    actor columns: {}", actor_cols.join(", ").yellow(),);
        }
    }

    if !show_actors {
        let has_behavioral = !model.personas.is_empty()
            || !model.actor_relationships.is_empty()
            || model.entities.iter().any(|e| e.actor);
        if has_behavioral {
            println!();
            println!(
                "  {} use {} to see actor/persona/relationship details",
                "hint:".dimmed(),
                "--actors".cyan(),
            );
        }
        return;
    }

    // Persona details
    if !model.personas.is_empty() {
        println!();
        println!("{}", "Personas:".bold());

        // Group personas by the entity that references them.
        // The compiler matches personas to entities by "{entity_name}_" prefix,
        // so we use the same rule here.
        let persona_groups: std::collections::BTreeMap<String, Vec<&crate::core::types::Persona>> = {
            let mut groups =
                std::collections::BTreeMap::<String, Vec<&crate::core::types::Persona>>::new();
            for entity in &model.entities {
                if let Some(ref pd) = entity.persona_distribution {
                    groups.entry(pd.clone()).or_default();
                }
            }
            for p in &model.personas {
                let mut placed = false;
                // Try prefix-based matching (learned schemas use "{entity}_" prefix)
                for entity in &model.entities {
                    if let Some(ref pd) = entity.persona_distribution {
                        let prefix = format!("{}_", entity.name);
                        if p.name.starts_with(&prefix) {
                            groups.entry(pd.clone()).or_default().push(p);
                            placed = true;
                            break;
                        }
                    }
                }
                // Fallback: put in first matching group (hand-authored schemas)
                if !placed {
                    if let Some(first) = groups.values_mut().next() {
                        first.push(p);
                    } else {
                        groups.entry("default".to_string()).or_default().push(p);
                    }
                }
            }
            groups
        };

        for (group, personas) in &persona_groups {
            println!("  {} ({} persona(s)):", group.cyan(), personas.len());
            for p in personas {
                let pct = (p.weight * 100.0).round() as u32;
                let trait_names: Vec<&str> = p.traits.keys().map(|k| k.as_str()).collect();
                println!(
                    "    {} — {}% weight, {} trait(s){}",
                    p.name.yellow(),
                    pct,
                    p.traits.len(),
                    if trait_names.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", trait_names.join(", "))
                    },
                );
            }
        }
    }

    // Actor relationships
    if !model.actor_relationships.is_empty() {
        println!();
        println!("{}", "Actor Relationships:".bold());
        for rel in &model.actor_relationships {
            let direction = if rel.from_entity == rel.to_entity {
                format!(
                    "{} ↔ {} (self-referential)",
                    rel.from_entity.green(),
                    rel.to_entity.green()
                )
            } else {
                format!("{} → {}", rel.from_entity.green(), rel.to_entity.green())
            };
            println!(
                "  {} — {} ({})",
                rel.name.yellow(),
                direction,
                rel.graph_type,
            );
            if !rel.params.is_empty() {
                let params_str: Vec<String> = rel
                    .params
                    .iter()
                    .map(|(k, v)| format!("{}={}", k, v))
                    .collect();
                println!("    params: {}", params_str.join(", "));
            }
        }
    }

    // Behavioral generator summary
    let behavioral_gens: Vec<(&str, &str, &str)> = model
        .entities
        .iter()
        .flat_map(|e| {
            e.fields.iter().filter_map(move |f| {
                let gen_type = f.generator.as_ref().and_then(|g| match g {
                    crate::core::types::GeneratorSpec::ActorRef { .. } => Some("actor_ref"),
                    crate::core::types::GeneratorSpec::ActorTemporal { .. } => Some("actor_temporal"),
                    crate::core::types::GeneratorSpec::PersonaField { .. } => Some("persona_field"),
                    crate::core::types::GeneratorSpec::RelationshipRef { .. } => {
                        Some("relationship_ref")
                    }
                    crate::core::types::GeneratorSpec::ThreadRef { .. } => Some("thread_ref"),
                    _ => None,
                });
                gen_type.map(|gt| (e.name.as_str(), f.name.as_str(), gt))
            })
        })
        .collect();

    if !behavioral_gens.is_empty() {
        println!();
        println!("{}", "Behavioral Generators:".bold());
        for (entity, field, gen_type) in &behavioral_gens {
            println!(
                "  {}.{} — {}",
                entity.green(),
                field.yellow(),
                gen_type.cyan(),
            );
        }
    }
}

fn print_schema_json(model: &crate::core::DataModel, show_actors: bool) {
    let entities: Vec<serde_json::Value> = model
        .entities
        .iter()
        .map(|e| {
            let actor_cols: Vec<&str> = e
                .fields
                .iter()
                .filter(|f| f.actor_column)
                .map(|f| f.name.as_str())
                .collect();
            serde_json::json!({
                "name": e.name,
                "fields": e.fields.len(),
                "actor": e.actor,
                "persona_distribution": e.persona_distribution,
                "actor_columns": actor_cols,
            })
        })
        .collect();

    let mut result = serde_json::json!({
        "name": model.name,
        "description": model.description,
        "entities": entities,
    });

    if show_actors {
        let personas: Vec<serde_json::Value> = model
            .personas
            .iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "weight": p.weight,
                    "traits": p.traits.keys().collect::<Vec<_>>(),
                })
            })
            .collect();

        let relationships: Vec<serde_json::Value> = model
            .actor_relationships
            .iter()
            .map(|r| {
                serde_json::json!({
                    "name": r.name,
                    "from_entity": r.from_entity,
                    "to_entity": r.to_entity,
                    "graph_type": r.graph_type.to_string(),
                    "params": r.params,
                })
            })
            .collect();

        let behavioral_gens: Vec<serde_json::Value> = model
            .entities
            .iter()
            .flat_map(|e| {
                e.fields.iter().filter_map(move |f| {
                    let gen_type = f.generator.as_ref().and_then(|g| match g {
                        crate::core::types::GeneratorSpec::ActorRef { .. } => Some("actor_ref"),
                        crate::core::types::GeneratorSpec::ActorTemporal { .. } => {
                            Some("actor_temporal")
                        }
                        crate::core::types::GeneratorSpec::PersonaField { .. } => {
                            Some("persona_field")
                        }
                        crate::core::types::GeneratorSpec::RelationshipRef { .. } => {
                            Some("relationship_ref")
                        }
                        crate::core::types::GeneratorSpec::ThreadRef { .. } => Some("thread_ref"),
                        _ => None,
                    });
                    gen_type.map(|gt| {
                        serde_json::json!({
                            "entity": e.name,
                            "field": f.name,
                            "generator": gt,
                        })
                    })
                })
            })
            .collect();

        result["personas"] = serde_json::Value::Array(personas);
        result["actor_relationships"] = serde_json::Value::Array(relationships);
        result["behavioral_generators"] = serde_json::Value::Array(behavioral_gens);
    }

    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}

/// Inspect a learn state file (.json).
fn run_state(state_path: &str, show_columns: bool, show_actors: bool, cli: &Cli) -> Result<()> {
    let path = Path::new(state_path);
    let state = LearnState::load(path)
        .map_err(|e| anyhow::anyhow!("{}", e))?
        .with_context(|| format!("state file not found: {}", state_path))?;

    if show_actors && !cli.json {
        eprintln!(
            "  {} behavioral data is stored in the blueprint file, not the state file",
            "note:".yellow(),
        );
        eprintln!(
            "  {} run {} on a .knit.toml file to see actor details",
            "hint:".dimmed(),
            "knit inspect schema.knit.toml --actors".cyan(),
        );
        eprintln!();
    }

    if cli.json {
        print_json(&state, show_columns);
    } else {
        print_human(&state, show_columns);
    }

    Ok(())
}

fn print_json(state: &LearnState, show_columns: bool) {
    let tables: Vec<serde_json::Value> = state
        .tables
        .values()
        .map(|t| {
            let mut obj = serde_json::json!({
                "name": t.name,
                "rows": t.row_count,
                "columns": t.columns.len(),
            });
            if show_columns {
                let cols: Vec<serde_json::Value> = t
                    .columns
                    .iter()
                    .map(|c| {
                        serde_json::json!({
                            "name": c.name,
                            "type": column_type_str(c.data_type),
                            "count": c.count,
                            "null_count": c.null_count,
                            "null_rate": round2(c.null_rate()),
                            "cardinality": c.estimated_cardinality().round() as u64,
                            "top_values": c.top_k.top_items().into_iter().take(5)
                                .map(|(v, n)| serde_json::json!({"value": v, "count": n}))
                                .collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                obj["column_details"] = serde_json::Value::Array(cols);
            }
            obj
        })
        .collect();

    let recent_chunks: Vec<serde_json::Value> = state
        .chunks
        .iter()
        .rev()
        .take(10)
        .map(|c| {
            serde_json::json!({
                "source": c.source,
                "row_count": c.row_count,
                "processed_at": c.processed_at,
            })
        })
        .collect();

    let summary = serde_json::json!({
        "format_version": state.format_version,
        "algorithm_version": state.algorithm_version,
        "seed": state.seed,
        "total_rows": state.total_rows,
        "tables": tables,
        "chunks_processed": state.chunks.len(),
        "recent_chunks": recent_chunks,
        "relationships_detected": state.relationship_evidence.len(),
        "correlations_tracked": state.correlations.len(),
    });

    println!("{}", serde_json::to_string_pretty(&summary).unwrap());
}

fn print_human(state: &LearnState, show_columns: bool) {
    println!(
        "{} (format v{}, algorithm v{}, seed {})",
        "State Summary".bold(),
        state.format_version,
        state.algorithm_version,
        state.seed,
    );
    println!();

    // Overall stats
    println!(
        "  {} rows across {} table(s), {} chunk(s) processed",
        format_count(state.total_rows).cyan(),
        state.tables.len(),
        state.chunks.len(),
    );
    if !state.relationship_evidence.is_empty() {
        println!(
            "  {} FK candidate(s) detected",
            state.relationship_evidence.len(),
        );
    }
    if !state.correlations.is_empty() {
        println!("  {} correlation pair(s) tracked", state.correlations.len(),);
    }
    println!();

    // Per-table summary
    println!("{}", "Tables:".bold());
    for table in state.tables.values() {
        println!(
            "  {} — {} rows, {} columns",
            table.name.green(),
            format_count(table.row_count),
            table.columns.len(),
        );

        if show_columns {
            for col in &table.columns {
                let cardinality = col.estimated_cardinality().round() as u64;
                let null_pct = col.null_rate() * 100.0;
                println!(
                    "    {} ({:?}) — {} values, ~{} distinct, {:.1}% null",
                    col.name.yellow(),
                    col.data_type,
                    format_count(col.count),
                    format_count(cardinality),
                    null_pct,
                );

                // Show top values
                let top: Vec<_> = col.top_k.top_items().into_iter().take(5).collect();
                if !top.is_empty() {
                    let top_str: Vec<String> = top
                        .iter()
                        .map(|(v, n)| {
                            let display = if v.chars().count() > 20 {
                                let truncated: String = v.chars().take(19).collect();
                                format!("{}…", truncated)
                            } else {
                                v.clone()
                            };
                            format!("\"{}\" ({})", display, n)
                        })
                        .collect();
                    println!("      top: {}", top_str.join(", "));
                }

                // Show numeric stats if present
                if let Some(ref num) = col.numeric {
                    let mean = num.mean();
                    let std = num.std_dev();
                    println!(
                        "      range: [{:.4}, {:.4}], mean: {:.4}, std: {:.4}",
                        num.min(),
                        num.max(),
                        mean,
                        std,
                    );
                }
            }
        }
    }

    // Chunk history (last 10)
    if !state.chunks.is_empty() {
        println!();
        println!("{}", "Recent chunks:".bold());
        let start = state.chunks.len().saturating_sub(10);
        for chunk in &state.chunks[start..] {
            let ts = format_timestamp(chunk.processed_at);
            println!(
                "  {} — {} rows ({})",
                chunk.source,
                format_count(chunk.row_count),
                ts,
            );
        }
        if start > 0 {
            println!("  ... and {} earlier chunk(s)", start);
        }
    }
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn format_timestamp(unix_secs: u64) -> String {
    // Simple relative-time display
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if now == 0 || unix_secs == 0 {
        return "unknown".to_string();
    }
    let diff = now.saturating_sub(unix_secs);
    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        format!("{}m ago", diff / 60)
    } else if diff < 86400 {
        format!("{}h ago", diff / 3600)
    } else {
        format!("{}d ago", diff / 86400)
    }
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

fn column_type_str(dt: crate::learn::streaming::state::ColumnDataType) -> &'static str {
    use crate::learn::streaming::state::ColumnDataType;
    match dt {
        ColumnDataType::Integer => "integer",
        ColumnDataType::Float => "float",
        ColumnDataType::String => "string",
        ColumnDataType::Temporal => "temporal",
        ColumnDataType::Boolean => "boolean",
        ColumnDataType::Other => "other",
    }
}