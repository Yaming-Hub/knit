//! `knit plan` — display the execution plan without generating data.

use anyhow::{bail, Result};
use colored::Colorize;

use crate::Cli;
use super::{load_schema, validate_model};

/// Run the plan command.
///
/// Parses the schema, validates it, compiles an execution plan, and prints a
/// human-readable (or JSON) summary of the planned generation pipeline.
pub fn run(schema_path: &str, cli: &Cli) -> Result<()> {
    // Load and validate
    let model = load_schema(schema_path).map_err(|e| {
        anyhow::anyhow!("failed to parse schema `{}`: {}", schema_path, e)
    })?;

    let errors = validate_model(&model);
    if !errors.is_empty() {
        for err in &errors {
            eprintln!("{} {}", "error:".red().bold(), err);
        }
        bail!("schema has {} validation error(s)", errors.len());
    }

    // Compile execution plan
    let plan = knit_plan::compile(&model).map_err(|e| {
        anyhow::anyhow!("plan compilation failed: {}", e)
    })?;

    if cli.json {
        println!("{}", serde_json::to_string_pretty(&plan)?);
        return Ok(());
    }

    // Human-readable plan display
    print_plan(&plan);
    Ok(())
}

/// Print a formatted execution plan to stdout.
fn print_plan(plan: &knit_plan::ExecutionPlan) {
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
    println!();

    // Phase breakdown
    for (i, phase) in plan.phases.iter().enumerate() {
        println!("{}", format!("── Phase {} ──", i).bold());
        for ep in &phase.entity_plans {
            println!(
                "  {} {} ({} rows, {} partitions, {} fields, ~{})",
                "▸".green(),
                ep.entity_name.yellow(),
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
    println!("  {} {}", "global seed:".dimmed(), plan.rng_tree.global_seed);
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
fn generator_label(gp: &knit_plan::GeneratorPlan) -> String {
    match gp {
        knit_plan::GeneratorPlan::Distribution { kind, .. } => format!("dist({:?})", kind),
        knit_plan::GeneratorPlan::Faker { category, locale } => {
            format!("faker({}, {})", category, locale)
        }
        knit_plan::GeneratorPlan::Sequence { start, step } => {
            format!("seq(start={}, step={})", start, step)
        }
        knit_plan::GeneratorPlan::OneOf { choices, .. } => {
            format!("oneOf({} choices)", choices.len())
        }
        knit_plan::GeneratorPlan::Derived { expr, .. } => format!("derived({})", expr),
        knit_plan::GeneratorPlan::Constant(v) => format!("const({:?})", v),
        knit_plan::GeneratorPlan::Composite { .. } => "composite".to_string(),
        knit_plan::GeneratorPlan::ForeignKey { target_entity, .. } => {
            format!("fk(→{})", target_entity)
        }
        knit_plan::GeneratorPlan::Uuid => "uuid()".to_string(),
        knit_plan::GeneratorPlan::Pattern { pattern, .. } => {
            format!("pattern({})", pattern)
        }
        knit_plan::GeneratorPlan::Temporal { kind, .. } => {
            format!("temporal({:?})", kind)
        }
        knit_plan::GeneratorPlan::Correlated { target_field, .. } => {
            format!("correlated({})", target_field)
        }
        knit_plan::GeneratorPlan::Topology { model, .. } => {
            format!("topology({:?})", model)
        }
        knit_plan::GeneratorPlan::Unique { inner, max_retries } => {
            format!("unique({}, retries={})", generator_label(inner), max_retries)
        }
        knit_plan::GeneratorPlan::Conditional {
            field, branches, ..
        } => {
            format!("conditional({}, {} branches)", field, branches.len())
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
