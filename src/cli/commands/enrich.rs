//! CLI handler for `knit enrich`.

use std::path::Path;

use anyhow::Result;
use colored::Colorize;

use crate::cli::commands::{load_blueprint, save_blueprint};
use crate::cli::Cli;
use crate::enrich::{enrich, EnrichConfig};

/// Run the enrich subcommand.
#[allow(clippy::too_many_arguments)] // CLI handlers mirror subcommand flags directly.
pub fn run(
    blueprint_path: &str,
    ref_path: &str,
    output: Option<&str>,
    entity: Option<&str>,
    min_confidence: f64,
    max_rows: Option<usize>,
    dry_run: bool,
    interactive: bool,
    _cli: &Cli,
) -> Result<()> {
    // Load model
    let mut model = load_blueprint(blueprint_path)?;

    let config = EnrichConfig {
        min_confidence,
        max_rows: max_rows.or(Some(100_000)),
        dry_run,
        entity_filter: entity.map(|s| s.to_string()),
        interactive,
    };

    let result = enrich(&mut model, Path::new(ref_path), &config)?;

    // Print summary
    println!("{}", "== Enrichment Summary ==".bold());
    println!("  Reference columns:  {}", result.ref_columns);
    println!("  Mapped columns:     {}", result.mapped_columns);
    println!("  Unmapped columns:   {}", result.unmapped_columns);
    println!();

    if !result.mappings.is_empty() {
        println!("{}", "  Column Mappings:".bold());
        for m in &result.mappings {
            let conf_str = format!("{:.0}%", m.confidence * 100.0);
            let type_str = if m.type_compatible { "✓" } else { "⚠" };
            println!(
                "    {} → {} ({} {})",
                m.ref_col_name.cyan(),
                m.target_field.green(),
                conf_str,
                type_str
            );
        }
        println!();
    }

    if dry_run {
        println!("{}", "  (dry-run: no changes written)".yellow());
        return Ok(());
    }

    println!("  Fields enriched:    {}", result.enriched_fields);
    println!("  Fields skipped:     {}", result.skipped_fields);

    // Print quality report
    if let Some(ref report) = result.quality_report {
        println!();
        println!("{}", report);
    }

    // Write updated schema (format-aware: preserves structured directory if input was structured)
    let out_path = output.unwrap_or(blueprint_path);
    save_blueprint(&model, out_path)?;
    println!("  Written to: {}", out_path.green());

    Ok(())
}