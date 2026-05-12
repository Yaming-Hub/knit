//! `knit scale` — multi-dimensional dataset scaling.

use std::path::Path;

use anyhow::{bail, Result};
use colored::Colorize;

use super::load_blueprint;
use crate::cli::Cli;
use crate::scale::{self, analyze, ScaleTargets};

/// Run the `knit scale` command.
pub fn run(
    blueprint_path: &str,
    output_dir: Option<&str>,
    analyze_only: bool,
    actors: Option<u64>,
    time: Option<&str>,
    dims: &[(String, u64)],
    count: Option<f64>,
    cadence: Option<&str>,
    density: &[(String, f64)],
    cli: &Cli,
) -> Result<()> {
    let _span = tracing::info_span!("scale", schema = %blueprint_path).entered();

    // Load and parse schema
    let mut model = load_blueprint(blueprint_path)?;

    // Apply CLI seed override
    if let Some(seed) = cli.seed {
        model.seed = seed;
    }
    for (key, value) in &cli.params {
        model
            .params
            .insert(key.clone(), crate::core::Value::String(value.clone()));
    }

    // Analyze dimensions (prefer persisted annotations when available)
    let (analysis, used_annotations) = analyze::analyze_or_from_annotations(&model);

    if analyze_only {
        if used_annotations && !cli.quiet {
            eprintln!(
                "  {} using persisted dimension annotations from blueprint",
                "✓".green().bold()
            );
        }
        print_analysis(&analysis, cli);
        return Ok(());
    }

    // Need at least one scaling target
    if actors.is_none() && time.is_none() && dims.is_empty() && count.is_none() && density.is_empty() {
        print_analysis(&analysis, cli);
        eprintln!();
        eprintln!(
            "{} specify at least one scaling target (--actors, --time, --dim, --count, --density)",
            "hint:".cyan().bold()
        );
        return Ok(());
    }

    // Parse and validate --cadence
    let cadence = if let Some(spec) = cadence {
        if time.is_none() {
            bail!("--cadence requires --time (cadence override only applies to time scaling)");
        }
        Some(parse_cadence(spec)?)
    } else {
        None
    };

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
        cadence,
        density: density.to_vec(),
    };

    // Compute plan
    let plan = scale::compute_plan(&analysis, &targets)?;

    if cli.dry_run {
        print_dry_run(&model, &analysis, &plan, cli);
        return Ok(());
    }

    // Apply plan to model
    scale::rewrite(&mut model, &plan);

    // Delegate to generate pipeline
    let schema_dir = Path::new(blueprint_path)
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
            let cadence_json = match t.cadence {
                Some(scale::Cadence::Days(d)) => serde_json::json!({"unit": "days", "value": d}),
                Some(scale::Cadence::Months(m)) => serde_json::json!({"unit": "months", "value": m}),
                None => serde_json::json!(null),
            };
            dims.push(serde_json::json!({
                "name": "time",
                "type": "built-in",
                "entity": t.entity_name,
                "field": t.partition_field,
                "partitions": t.partition_values.len(),
                "cadence": cadence_json,
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
        let cadence_str = match t.cadence {
            Some(scale::Cadence::Days(1)) => "daily".to_string(),
            Some(scale::Cadence::Days(7)) => "weekly".to_string(),
            Some(scale::Cadence::Days(d)) => format!("{}d", d),
            Some(scale::Cadence::Months(1)) => "monthly".to_string(),
            Some(scale::Cadence::Months(3)) => "quarterly".to_string(),
            Some(scale::Cadence::Months(m)) => format!("{}m", m),
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
        let confidence_hint = if t.cadence_confidence < 1.0 {
            format!(" (confidence: {:.0}%)", t.cadence_confidence * 100.0)
        } else {
            String::new()
        };
        println!(
            "  {:<16}{:<12}{:<20}{}",
            "time",
            "built-in",
            format!("{} partitions", t.partition_values.len()),
            format!(
                "{}.{}, cadence ≈ {}{}, {}",
                t.entity_name, t.partition_field, cadence_str, confidence_hint, range
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
    model: &crate::core::types::DataModel,
    analysis: &scale::ScalingAnalysis,
    plan: &scale::ScalingPlan,
    cli: &Cli,
) {
    let (entity_estimates, total_csv_bytes) =
        scale::estimate_output_size(model, analysis, plan);
    let total_json_bytes: u64 = entity_estimates.iter().map(|e| e.json_bytes).sum();
    let total_parquet_bytes = (total_csv_bytes as f64 * 0.4) as u64;

    // Pick the estimate for the selected format
    let (format_estimate, format_label) = match cli.format {
        crate::cli::Format::Csv => (total_csv_bytes, "csv"),
        crate::cli::Format::Json | crate::cli::Format::Jsonl => (total_json_bytes, "json"),
        crate::cli::Format::Parquet => (total_parquet_bytes, "parquet"),
        crate::cli::Format::Avro => ((total_csv_bytes as f64 * 0.5) as u64, "avro"),
        crate::cli::Format::ArrowIpc => ((total_csv_bytes as f64 * 0.6) as u64, "arrow"),
        crate::cli::Format::Sql => ((total_csv_bytes as f64 * 1.5) as u64, "sql"),
    };

    // Per-entity estimate getter for the selected format
    let entity_bytes = |e: &scale::EntitySizeEstimate| -> u64 {
        match cli.format {
            crate::cli::Format::Csv => e.csv_bytes,
            crate::cli::Format::Json | crate::cli::Format::Jsonl => e.json_bytes,
            crate::cli::Format::Parquet => (e.csv_bytes as f64 * 0.4) as u64,
            crate::cli::Format::Avro => (e.csv_bytes as f64 * 0.5) as u64,
            crate::cli::Format::ArrowIpc => (e.csv_bytes as f64 * 0.6) as u64,
            crate::cli::Format::Sql => (e.csv_bytes as f64 * 1.5) as u64,
        }
    };

    if cli.json {
        let entities: Vec<_> = analysis
            .entity_counts
            .iter()
            .map(|(name, &current)| {
                let scaled = plan.entity_overrides.get(name).copied().unwrap_or(current);
                let factor = scaled as f64 / current.max(1) as f64;
                let est = entity_estimates
                    .iter()
                    .find(|e| e.entity_name == *name)
                    .map(|e| entity_bytes(e))
                    .unwrap_or(0);
                serde_json::json!({
                    "entity": name,
                    "current": current,
                    "scaled": scaled,
                    "factor": format!("{:.1}×", factor),
                    "estimated_bytes": est,
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
            "estimated_size": {
                "csv_bytes": total_csv_bytes,
                "json_bytes": total_json_bytes,
                "parquet_bytes": total_parquet_bytes,
                "format": format_label,
                "format_bytes": format_estimate,
                "display": scale::format_bytes(format_estimate),
            },
        }));
        return;
    }

    println!();
    println!("{}", "═══ Scaling Plan (dry run) ═══".green().bold());
    println!();
    println!(
        "  {:<20}{:<12}{:<12}{:<10}{}",
        "Entity".bold(),
        "Current".bold(),
        "Scaled".bold(),
        "Factor".bold(),
        "Est. Size".bold(),
    );
    println!(
        "  {:<20}{:<12}{:<12}{:<10}{}",
        "──────", "───────", "──────", "──────", "─────────"
    );

    for (name, &current) in &analysis.entity_counts {
        let scaled = plan.entity_overrides.get(name).copied().unwrap_or(current);
        let factor = scaled as f64 / current.max(1) as f64;
        let est = entity_estimates
            .iter()
            .find(|e| e.entity_name == *name)
            .map(|e| scale::format_bytes(entity_bytes(e)))
            .unwrap_or_else(|| "—".to_string());
        println!(
            "  {:<20}{:<12}{:<12}{:<10}{}",
            name, current, scaled, format!("{:.1}×", factor), est
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
    println!(
        "  {} ~{} ({})",
        "Estimated output:".bold(),
        scale::format_bytes(format_estimate),
        format_label,
    );
    println!();
}

/// Parse a cadence spec (e.g. "7d", "1w", "14d", "1m", "3m") into a Cadence.
fn parse_cadence(spec: &str) -> Result<scale::Cadence> {
    let spec = spec.trim();
    if spec.is_empty() {
        bail!("empty cadence spec");
    }

    let (num_str, unit) = spec.split_at(spec.len() - 1);
    let num: u32 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid cadence number in '{spec}'"))?;

    if num == 0 {
        bail!("cadence must be at least 1");
    }

    match unit {
        "d" => Ok(scale::Cadence::Days(num)),
        "w" => Ok(scale::Cadence::Days(
            num.checked_mul(7)
                .ok_or_else(|| anyhow::anyhow!("cadence overflow: '{spec}' is too large"))?,
        )),
        "m" => Ok(scale::Cadence::Months(num)),
        _ => bail!(
            "unsupported cadence unit '{unit}' in '{spec}'; use d (days), w (weeks), or m (months)"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cadence_days() {
        assert_eq!(parse_cadence("7d").unwrap(), scale::Cadence::Days(7));
        assert_eq!(parse_cadence("1w").unwrap(), scale::Cadence::Days(7));
        assert_eq!(parse_cadence("2w").unwrap(), scale::Cadence::Days(14));
        assert_eq!(parse_cadence("30d").unwrap(), scale::Cadence::Days(30));
    }

    #[test]
    fn test_parse_cadence_months() {
        assert_eq!(parse_cadence("1m").unwrap(), scale::Cadence::Months(1));
        assert_eq!(parse_cadence("3m").unwrap(), scale::Cadence::Months(3));
        assert_eq!(parse_cadence("6m").unwrap(), scale::Cadence::Months(6));
    }

    #[test]
    fn test_parse_cadence_rejects_zero() {
        assert!(parse_cadence("0d").is_err());
        assert!(parse_cadence("0m").is_err());
    }

    #[test]
    fn test_parse_cadence_rejects_empty() {
        assert!(parse_cadence("").is_err());
    }

    #[test]
    fn test_parse_cadence_rejects_unknown_unit() {
        assert!(parse_cadence("5x").is_err());
    }
}