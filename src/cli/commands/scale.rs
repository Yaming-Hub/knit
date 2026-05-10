//! `knit scale` — multi-dimensional dataset scaling.

use std::path::Path;

use anyhow::{bail, Result};
use colored::Colorize;

use super::load_schema;
use crate::cli::Cli;
use crate::scale::{self, analyze, ScaleTargets};

/// Run the `knit scale` command.
pub fn run(
    schema_path: &str,
    output_dir: Option<&str>,
    analyze_only: bool,
    actors: Option<u64>,
    time: Option<&str>,
    dims: &[(String, u64)],
    count: Option<f64>,
    cadence: Option<&str>,
    cli: &Cli,
) -> Result<()> {
    let _span = tracing::info_span!("scale", schema = %schema_path).entered();

    // Load and parse schema
    let mut model = load_schema(schema_path)?;

    // Apply CLI seed override
    if let Some(seed) = cli.seed {
        model.seed = seed;
    }
    for (key, value) in &cli.params {
        model
            .params
            .insert(key.clone(), crate::core::Value::String(value.clone()));
    }

    // Analyze dimensions
    let analysis = analyze::analyze(&model);

    if analyze_only {
        print_analysis(&analysis, cli);
        return Ok(());
    }

    // Need at least one scaling target
    if actors.is_none() && time.is_none() && dims.is_empty() && count.is_none() {
        print_analysis(&analysis, cli);
        eprintln!();
        eprintln!(
            "{} specify at least one scaling target (--actors, --time, --dim, --count)",
            "hint:".cyan().bold()
        );
        return Ok(());
    }

    // Need output directory for generation
    let output = output_dir.ok_or_else(|| {
        anyhow::anyhow!("--output is required when generating scaled data")
    })?;

    // Build scaling targets
    let targets = ScaleTargets {
        actors,
        time: time.map(String::from),
        dims: dims.to_vec(),
        count,
    };

    // Compute plan
    let plan = scale::compute_plan(&analysis, &targets)?;

    if cli.dry_run {
        print_dry_run(&analysis, &plan, cli);
        return Ok(());
    }

    // Apply plan to model
    scale::rewrite(&mut model, &plan);

    // Delegate to generate pipeline
    let schema_dir = Path::new(schema_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    super::generate::run_from_model(model, schema_dir, output, &[], cli)
}

/// Display the scaling analysis.
fn print_analysis(analysis: &scale::ScalingAnalysis, cli: &Cli) {
    if cli.json {
        let mut dims = Vec::new();
        if let Some(ref a) = analysis.actor {
            dims.push(serde_json::json!({
                "name": "actors",
                "type": "built-in",
                "entity": a.entity_name,
                "current": a.current_count,
                "confidence": a.confidence,
                "dependents": a.dependents.iter().map(|(n, r)| {
                    serde_json::json!({"entity": n, "ratio": r})
                }).collect::<Vec<_>>(),
            }));
        }
        if let Some(ref t) = analysis.time {
            dims.push(serde_json::json!({
                "name": "time",
                "type": "built-in",
                "entity": t.entity_name,
                "field": t.partition_field,
                "partitions": t.partition_values.len(),
                "cadence_days": t.cadence_days,
                "cadence_confidence": t.cadence_confidence,
            }));
        }
        for c in &analysis.custom {
            dims.push(serde_json::json!({
                "name": c.field_name,
                "type": "custom",
                "entity": c.entity_name,
                "current_values": c.current_values.len(),
                "values": c.current_values.iter().map(|(v, w)| {
                    serde_json::json!({"value": v, "weight": w})
                }).collect::<Vec<_>>(),
                "is_condition_key": c.is_condition_key,
            }));
        }
        let total: u64 = analysis.entity_counts.values().sum();
        dims.push(serde_json::json!({
            "name": "rows",
            "type": "built-in",
            "current": total,
        }));
        println!("{}", serde_json::json!({"dimensions": dims}));
        return;
    }

    println!();
    println!("{}", "═══ Scaling Analysis ═══".green().bold());
    println!();
    println!(
        "  {:<16}{:<12}{:<20}{}",
        "Dimension".bold(),
        "Type".bold(),
        "Current".bold(),
        "Description".bold()
    );
    println!(
        "  {:<16}{:<12}{:<20}{}",
        "─────────", "────", "───────", "───────────"
    );

    if let Some(ref a) = analysis.actor {
        let deps_str = a
            .dependents
            .iter()
            .map(|(n, _)| n.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        println!(
            "  {:<16}{:<12}{:<20}{}",
            "actors",
            "built-in",
            format!("{} entities", a.current_count),
            format!(
                "{} (actor entity, dependents: {})",
                a.entity_name, deps_str
            )
        );
    }

    if let Some(ref t) = analysis.time {
        let cadence_str = match t.cadence_days {
            Some(1) => "daily".to_string(),
            Some(7) => "weekly".to_string(),
            Some(30) | Some(31) => "monthly".to_string(),
            Some(d) => format!("{}d", d),
            None => "unknown".to_string(),
        };
        let range = if t.partition_values.len() >= 2 {
            format!(
                "{} .. {}",
                t.partition_values.first().unwrap(),
                t.partition_values.last().unwrap()
            )
        } else {
            t.partition_values
                .first()
                .cloned()
                .unwrap_or_default()
        };
        println!(
            "  {:<16}{:<12}{:<20}{}",
            "time",
            "built-in",
            format!("{} partitions", t.partition_values.len()),
            format!(
                "{}.{}, cadence ≈ {}, {}",
                t.entity_name, t.partition_field, cadence_str, range
            )
        );
    }

    for c in &analysis.custom {
        let values_str = c
            .current_values
            .iter()
            .take(5)
            .map(|(v, _)| v.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let suffix = if c.current_values.len() > 5 {
            format!(", +{} more", c.current_values.len() - 5)
        } else {
            String::new()
        };
        println!(
            "  {:<16}{:<12}{:<20}{}",
            c.field_name,
            "custom",
            format!("{} values", c.current_values.len()),
            format!(
                "{}.{} one_of: [{}{}]",
                c.entity_name, c.field_name, values_str, suffix
            )
        );
    }

    let total: u64 = analysis.entity_counts.values().sum();
    println!(
        "  {:<16}{:<12}{:<20}{}",
        "rows",
        "built-in",
        format!("{} total", total),
        "Uniform row scaling (--count)"
    );

    // Suggestions
    println!();
    println!("  {}:", "Suggested commands".dimmed());
    if let Some(ref _a) = analysis.actor {
        println!(
            "    knit scale <SCHEMA> -o out/ --actors 100",
        );
    }
    if analysis.time.is_some() {
        println!(
            "    knit scale <SCHEMA> -o out/ --time 52w",
        );
    }
    for c in &analysis.custom {
        println!(
            "    knit scale <SCHEMA> -o out/ --dim {}=10",
            c.field_name.to_lowercase()
        );
    }
    println!();
}

/// Display dry-run scaling plan.
fn print_dry_run(
    analysis: &scale::ScalingAnalysis,
    plan: &scale::ScalingPlan,
    cli: &Cli,
) {
    if cli.json {
        let entities: Vec<_> = analysis
            .entity_counts
            .iter()
            .map(|(name, &current)| {
                let scaled = plan.entity_overrides.get(name).copied().unwrap_or(current);
                let factor = scaled as f64 / current.max(1) as f64;
                serde_json::json!({
                    "entity": name,
                    "current": current,
                    "scaled": scaled,
                    "factor": format!("{:.1}×", factor),
                })
            })
            .collect();
        println!("{}", serde_json::json!({
            "event": "dry_run",
            "entities": entities,
            "partitions": plan.new_partitions.as_ref().map(|np| np.values.len()),
            "dim_overrides": plan.dim_overrides.iter().map(|d| {
                serde_json::json!({
                    "field": d.field_name,
                    "new_count": d.new_values.len(),
                })
            }).collect::<Vec<_>>(),
        }));
        return;
    }

    println!();
    println!("{}", "═══ Scaling Plan (dry run) ═══".green().bold());
    println!();
    println!(
        "  {:<20}{:<12}{:<12}{}",
        "Entity".bold(),
        "Current".bold(),
        "Scaled".bold(),
        "Factor".bold()
    );
    println!(
        "  {:<20}{:<12}{:<12}{}",
        "──────", "───────", "──────", "──────"
    );

    for (name, &current) in &analysis.entity_counts {
        let scaled = plan.entity_overrides.get(name).copied().unwrap_or(current);
        let factor = scaled as f64 / current.max(1) as f64;
        println!(
            "  {:<20}{:<12}{:<12}{:.1}×",
            name, current, scaled, factor
        );
    }

    if let Some(ref np) = plan.new_partitions {
        let old_count = analysis
            .time
            .as_ref()
            .map(|t| t.partition_values.len())
            .unwrap_or(0);
        println!();
        println!(
            "  Partitions: {} → {} ({} .. {})",
            old_count,
            np.values.len(),
            np.values.first().map(|v| v.value.as_str()).unwrap_or("?"),
            np.values.last().map(|v| v.value.as_str()).unwrap_or("?"),
        );
    }

    for d in &plan.dim_overrides {
        let old_count = analysis
            .custom
            .iter()
            .find(|c| c.field_name == d.field_name)
            .map(|c| c.current_values.len())
            .unwrap_or(0);
        println!(
            "  {}: {} → {}",
            d.field_name, old_count, d.new_values.len()
        );
    }
    println!();
}
