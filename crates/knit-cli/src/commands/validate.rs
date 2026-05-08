//! `knit validate` — parse a schema file and report errors.

use std::path::Path;

use anyhow::{bail, Result};
use colored::Colorize;

use super::{load_schema, validate_model};
use crate::Cli;

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
                eprintln!("{} {} {}", "error".red().bold(), "parse failure:".bold(), e);
            }
            bail!("schema parsing failed");
        }
    };

    // Phase 2: semantic validation
    let errors = validate_model(&model);

    // Phase 3: file-system validation (dictionary files, etc.)
    let schema_dir = Path::new(schema_path)
        .parent()
        .unwrap_or_else(|| Path::new("."));
    let fs_warnings = validate_dictionary_files(&model, schema_dir);

    if cli.json {
        let mut error_objs: Vec<_> = errors
            .iter()
            .map(|e| serde_json::json!({ "kind": "validation", "message": e.to_string() }))
            .collect();
        for w in &fs_warnings {
            error_objs.push(serde_json::json!({ "kind": "file", "message": w }));
        }
        let obj = serde_json::json!({
            "valid": errors.is_empty() && fs_warnings.is_empty(),
            "schema": model.name,
            "entity_count": model.entities.len(),
            "errors": error_objs,
        });
        println!("{}", serde_json::to_string_pretty(&obj)?);
        if !errors.is_empty() || !fs_warnings.is_empty() {
            bail!(
                "validation failed with {} error(s)",
                errors.len() + fs_warnings.len()
            );
        }
        return Ok(());
    }

    // Human-readable output
    let total_errors = errors.len() + fs_warnings.len();
    if total_errors == 0 {
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
            total_errors,
            schema_path.cyan()
        );
        for (i, err) in errors.iter().enumerate() {
            eprintln!("  {} {}", format!("{}.", i + 1).dimmed(), err);
        }
        for (i, warn) in fs_warnings.iter().enumerate() {
            eprintln!(
                "  {} {}",
                format!("{}.", errors.len() + i + 1).dimmed(),
                warn
            );
        }
        bail!("validation failed with {} error(s)", total_errors);
    }
}

/// Check that dictionary files referenced in generators exist on disk.
fn validate_dictionary_files(model: &knit_core::DataModel, schema_dir: &Path) -> Vec<String> {
    let mut warnings = Vec::new();
    for entity in &model.entities {
        for field in &entity.fields {
            if let Some(gen) = &field.generator {
                collect_dictionary_file_errors(
                    gen,
                    schema_dir,
                    &format!("entities.{}.fields.{}", entity.name, field.name),
                    &mut warnings,
                );
            }
        }
    }
    warnings
}

/// Recursively walk generator specs to find Dictionary generators and check files.
fn collect_dictionary_file_errors(
    gen: &knit_core::GeneratorSpec,
    schema_dir: &Path,
    path: &str,
    errors: &mut Vec<String>,
) {
    match gen {
        knit_core::GeneratorSpec::Dictionary { file, .. } => {
            if file.is_empty() {
                return; // Already caught by semantic validation
            }
            // Same rules as generate.rs: reject absolute paths and path traversal
            if Path::new(file).is_absolute() {
                errors.push(format!(
                    "{}: dictionary file path must be relative to schema directory, got absolute path: '{}'",
                    path, file
                ));
                return;
            }
            if file.contains("..") {
                errors.push(format!(
                    "{}: dictionary file path must not contain '..': '{}'",
                    path, file
                ));
                return;
            }
            let dict_path = schema_dir.join(file);
            if !dict_path.exists() {
                errors.push(format!(
                    "{}: dictionary file '{}' not found (resolved to '{}')",
                    path,
                    file,
                    dict_path.display()
                ));
            } else {
                // Check that file has at least one non-empty line (matching generate.rs parsing)
                if let Ok(content) = std::fs::read_to_string(&dict_path) {
                    let usable_entries = content
                        .lines()
                        .filter(|line| !line.trim().is_empty())
                        .count();
                    if usable_entries == 0 {
                        errors.push(format!(
                            "{}: dictionary file '{}' contains no usable entries (all lines empty/whitespace)",
                            path, file
                        ));
                    }
                }
            }
        }
        knit_core::GeneratorSpec::Unique { inner, .. } => {
            collect_dictionary_file_errors(inner, schema_dir, path, errors);
        }
        knit_core::GeneratorSpec::Conditional {
            branches, default, ..
        } => {
            for (i, branch) in branches.iter().enumerate() {
                collect_dictionary_file_errors(
                    &branch.generator,
                    schema_dir,
                    &format!("{}.branches[{}]", path, i),
                    errors,
                );
            }
            if let Some(def) = default {
                collect_dictionary_file_errors(
                    def,
                    schema_dir,
                    &format!("{}.default", path),
                    errors,
                );
            }
        }
        knit_core::GeneratorSpec::Composite { generators, .. } => {
            for (key, sub_gen) in generators {
                collect_dictionary_file_errors(
                    sub_gen,
                    schema_dir,
                    &format!("{}.generators.{}", path, key),
                    errors,
                );
            }
        }
        _ => {}
    }
}

/// Print a brief summary of the parsed schema.
fn print_schema_summary(model: &knit_core::DataModel) {
    println!("  {} {}", "name:".dimmed(), model.name);
    if let Some(desc) = &model.description {
        println!("  {} {}", "desc:".dimmed(), desc);
    }
    for entity in &model.entities {
        let badge = if entity.actor { " 🎭" } else { "" };
        println!(
            "  {} {}{} ({} fields)",
            "entity:".dimmed(),
            entity.name.yellow(),
            badge,
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
    if !model.personas.is_empty() {
        println!("  {} {}", "personas:".dimmed(), model.personas.len());
    }
    if !model.actor_relationships.is_empty() {
        println!(
            "  {} {}",
            "actor relationships:".dimmed(),
            model.actor_relationships.len()
        );
    }
}
