//! `knit validate` — parse a schema file and report errors.

use anyhow::{bail, Result};
use colored::Colorize;

use crate::Cli;
use super::{load_schema, validate_model};

/// Run the validate command.
///
/// Parses the schema file, runs semantic validation, and prints diagnostics.
/// In `--json` mode the output is a JSON array of error objects.
pub fn run(schema_path: &str, cli: &Cli) -> Result<()> {
    // Phase 1: parse
    let model = match load_schema(schema_path) {
        Ok(m) => m,
        Err(e) => {
            if cli.json {
                let obj = serde_json::json!({
                    "valid": false,
                    "errors": [{ "kind": "parse", "message": e.to_string() }],
                });
                println!("{}", serde_json::to_string_pretty(&obj)?);
            } else {
                eprintln!(
                    "{} {} {}",
                    "error".red().bold(),
                    "parse failure:".bold(),
                    e
                );
            }
            bail!("schema parsing failed");
        }
    };

    // Phase 2: semantic validation
    let errors = validate_model(&model);

    if cli.json {
        let error_objs: Vec<_> = errors
            .iter()
            .map(|e| serde_json::json!({ "kind": "validation", "message": e.to_string() }))
            .collect();
        let obj = serde_json::json!({
            "valid": errors.is_empty(),
            "schema": model.name,
            "entity_count": model.entities.len(),
            "errors": error_objs,
        });
        println!("{}", serde_json::to_string_pretty(&obj)?);
        if !errors.is_empty() {
            bail!("validation failed with {} error(s)", errors.len());
        }
        return Ok(());
    }

    // Human-readable output
    if errors.is_empty() {
        println!(
            "{} schema {} is valid ({} entities)",
            "✓".green().bold(),
            schema_path.cyan(),
            model.entities.len()
        );
        print_schema_summary(&model);
        Ok(())
    } else {
        eprintln!(
            "{} {} error(s) in {}",
            "✗".red().bold(),
            errors.len(),
            schema_path.cyan()
        );
        for (i, err) in errors.iter().enumerate() {
            eprintln!("  {} {}", format!("{}.", i + 1).dimmed(), err);
        }
        bail!("validation failed with {} error(s)", errors.len());
    }
}

/// Print a brief summary of the parsed schema.
fn print_schema_summary(model: &knit_core::DataModel) {
    println!("  {} {}", "name:".dimmed(), model.name);
    if let Some(desc) = &model.description {
        println!("  {} {}", "desc:".dimmed(), desc);
    }
    for entity in &model.entities {
        println!(
            "  {} {} ({} fields)",
            "entity:".dimmed(),
            entity.name.yellow(),
            entity.fields.len()
        );
    }
    if !model.relationships.is_empty() {
        println!(
            "  {} {}",
            "relationships:".dimmed(),
            model.relationships.len()
        );
    }
    if !model.noise_profiles.is_empty() {
        println!(
            "  {} {}",
            "noise profiles:".dimmed(),
            model.noise_profiles.len()
        );
    }
}
