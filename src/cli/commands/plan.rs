//! `knit plan` — display the execution plan without generating data.

use anyhow::{bail, Result};
use colored::Colorize;
use std::collections::HashSet;

use super::{load_schema, validate_model};
use crate::cli::Cli;

/// Run the plan command.
///
/// Parses the schema, validates it, compiles an execution plan, and prints a
/// human-readable (or JSON) summary of the planned generation pipeline.
pub fn run(schema_path: &str, cli: &Cli) -> Result<()> {
    // Load and validate
    let mut model = load_schema(schema_path)
        .map_err(|e| anyhow::anyhow!("failed to parse schema `{}`: {}", schema_path, e))?;

    // Apply --count override so the plan reflects it
    if let Some(ref count_str) = cli.count {
        super::generate::apply_count_override(&mut model, count_str)?;
    }

    let errors = validate_model(&model);
    if !errors.is_empty() {
        for err in &errors {
            eprintln!("{} {}", "error:".red().bold(), err);
        }
        bail!("schema has {} validation error(s)", errors.len());
    }

    // Compile execution plan
    let plan = crate::plan::compile(&model)
        .map_err(|e| anyhow::anyhow!("plan compilation failed: {}", e))?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }

    // Human-readable plan display
    let behavioral = BehavioralSummary::from_model(&model);
    print_plan(&plan, &behavioral);
    Ok(())
}

/// Set of actor entity names for badge display in plan output.
pub(crate) struct BehavioralSummary {
    pub actor_entities: HashSet<String>,
}

impl BehavioralSummary {
    /// Build from a data model.
    pub fn from_model(model: &crate::core::DataModel) -> Self {
        Self {
            actor_entities: model
                .entities
                .iter()
                .filter(|e| e.actor)
                .map(|e| e.name.clone())
                .collect(),
        }
    }
}

/// Print a formatted execution plan to stdout.
pub(crate) fn print_plan(plan: &crate::plan::ExecutionPlan, behavioral: &BehavioralSummary) {
    let meta = &plan.metadata;

    println!("{}", "═══ Execution Plan ═══".bold());
    println!("  {} {}", "schema:".dimmed(), meta.schema_name.cyan());
    println!(
        "  {} {} entities, {} phases, {} partitions",
        "scope:".dimmed(),
        meta.total_entities.to_string().yellow(),
        meta.total_phases.to_string().yellow(),
        meta.total_partitions.to_string().yellow(),
    );
    println!(
        "  {} ~{} rows, ~{}",
        "estimated:".dimmed(),
        format_count(meta.estimated_total_rows),
        format_bytes(meta.estimated_total_bytes),
    );
    if meta.has_cycles {
        println!(
            "  {} yes ({} deferred refs)",
            "cycles:".dimmed(),
            meta.deferred_ref_count,
        );
    }
    println!(
        "  {} {}",
        "global seed:".dimmed(),
        plan.rng_tree.global_seed,
    );
    let has_behavioral =
        meta.actor_entity_count > 0 || meta.persona_count > 0 || meta.actor_relationship_count > 0;
    if has_behavioral {
        println!(
            "  {} {} actor(s), {} persona(s), {} actor relationship(s)",
            "behavioral:".dimmed(),
            meta.actor_entity_count,
            meta.persona_count,
            meta.actor_relationship_count,
        );
    }
    if !plan.actor_pool.pools.is_empty() {
        let total_actors: u64 = plan.actor_pool.pools.iter().map(|p| p.actor_count).sum();
        println!(
            "  {} {} pool(s), {} total actors, {} graph plan(s)",
            "actor pool:".dimmed(),
            plan.actor_pool.pools.len(),
            total_actors,
            plan.actor_pool.graph_plans.len(),
        );
    }
    println!();

    // Phase breakdown
    for (i, phase) in plan.phases.iter().enumerate() {
        println!("{}", format!("── Phase {} ──", i).bold());
        for ep in &phase.entity_plans {
            let badge = if behavioral.actor_entities.contains(&ep.entity_name) {
                " 🎭"
            } else {
                ""
            };
            println!(
                "  {} {}{} ({} rows, {} partitions, {} fields, ~{})",
                "▸".green(),
                ep.entity_name.yellow(),
                badge,
                format_count(ep.estimated_row_count),
                ep.partitions.len(),
                ep.field_plans.len(),
                format_bytes(ep.estimated_byte_size),
            );
            // Show generator assignments
            for fp in &ep.field_plans {
                let gen_label = generator_label(&fp.generator_plan);
                println!(
                    "    {} {} → {}",
                    "·".dimmed(),
                    fp.field_name.white(),
                    gen_label.dimmed(),
                );
            }
        }
        for dr in &phase.deferred_refs {
            println!(
                "  {} {}.{} → {}.{}",
                "⟳".yellow(),
                dr.from_entity,
                dr.from_field,
                dr.to_entity,
                dr.to_field,
            );
        }
        println!();
    }

    // RNG tree summary
    println!("{}", "── RNG Tree ──".bold());
    println!(
        "  {} {}",
        "global seed:".dimmed(),
        plan.rng_tree.global_seed
    );
    for (name, node) in &plan.rng_tree.entity_nodes {
        println!(
            "  {} {} (seed {}, {} fields)",
            "▸".green(),
            name.yellow(),
            node.entity_seed,
            node.field_seeds.len(),
        );
    }
}

/// Short human label for a generator plan variant.
fn generator_label(gp: &crate::plan::GeneratorPlan) -> String {
    match gp {
        crate::plan::GeneratorPlan::Distribution { kind, .. } => format!("dist({:?})", kind),
        crate::plan::GeneratorPlan::Faker {
            category, locale, ..
        } => {
            format!("faker({}, {})", category, locale)
        }
        crate::plan::GeneratorPlan::Sequence { start, step } => {
            format!("seq(start={}, step={})", start, step)
        }
        crate::plan::GeneratorPlan::OneOf { choices, .. } => {
            format!("oneOf({} choices)", choices.len())
        }
        crate::plan::GeneratorPlan::Derived { expr, .. } => format!("derived({})", expr),
        crate::plan::GeneratorPlan::Constant(v) => format!("const({:?})", v),
        crate::plan::GeneratorPlan::Composite { .. } => "composite".to_string(),
        crate::plan::GeneratorPlan::ForeignKey { target_entity, .. } => {
            format!("fk(→{})", target_entity)
        }
        crate::plan::GeneratorPlan::Uuid => "uuid()".to_string(),
        crate::plan::GeneratorPlan::Pattern { pattern, .. } => {
            format!("pattern({})", pattern)
        }
        crate::plan::GeneratorPlan::Temporal { kind, .. } => {
            format!("temporal({:?})", kind)
        }
        crate::plan::GeneratorPlan::Correlated { target_field, .. } => {
            format!("correlated({})", target_field)
        }
        crate::plan::GeneratorPlan::Topology { model, .. } => {
            format!("topology({:?})", model)
        }
        crate::plan::GeneratorPlan::Unique { inner, max_retries } => {
            format!(
                "unique({}, retries={})",
                generator_label(inner),
                max_retries
            )
        }
        crate::plan::GeneratorPlan::Conditional {
            field, branches, ..
        } => {
            format!("conditional({}, {} branches)", field, branches.len())
        }
        crate::plan::GeneratorPlan::Dictionary {
            entries, expansion, ..
        } => {
            format!("dictionary({} entries, {})", entries.len(), expansion)
        }
        crate::plan::GeneratorPlan::GraphTarget {
            graph_name,
            source_field,
            target_entity,
            ..
        } => {
            format!(
                "graph_fk({}→{}, src={})",
                graph_name, target_entity, source_field
            )
        }
        crate::plan::GeneratorPlan::PersonaField {
            trait_name,
            actor_entity,
            ..
        } => {
            format!("persona({}.{})", actor_entity, trait_name)
        }
        crate::plan::GeneratorPlan::ActorTemporal {
            trait_name,
            actor_entity,
            ..
        } => {
            format!("actor_temporal({}.{})", actor_entity, trait_name)
        }
        crate::plan::GeneratorPlan::ThreadRef {
            reply_probability,
            max_depth,
            ..
        } => {
            format!(
                "thread_ref(p={:.0}%, depth={})",
                reply_probability * 100.0,
                max_depth
            )
        }
        crate::plan::GeneratorPlan::Plugin { name, .. } => {
            format!("plugin({})", name)
        }
    }
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
