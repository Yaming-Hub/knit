//! CLI handler for `knit model` subcommands.

use std::path::Path;

use anyhow::Result;
use colored::Colorize;
use serde::Serialize;

use crate::cli::commands::load_blueprint;
use crate::core::{ActorRelationship, Correlation, Entity, Persona, Relationship};
use crate::model::{is_structured_model, reader, writer};

/// Wrapper for proper flat TOML serialization with [model] section.
#[derive(Serialize)]
struct FlatSchema {
    blueprint_version: String,
    model: FlatModelMeta,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    entities: Vec<Entity>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    relationships: Vec<Relationship>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    correlations: Vec<Correlation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    personas: Vec<Persona>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    actor_relationships: Vec<ActorRelationship>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    companion_files: Vec<String>,
}

#[derive(Serialize)]
struct FlatModelMeta {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    locale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timezone: Option<String>,
}

/// Run `knit model convert` — convert between flat and structured formats.
pub fn run_convert(input: &str, output: &str) -> Result<()> {
    let input_path = Path::new(input);
    let output_path = Path::new(output);

    if is_structured_model(input_path) {
        // Structured → Flat
        println!("{}", "Converting structured model → flat schema...".bold());
        let model = reader::load_model_directory(input_path)?;
        let flat = FlatSchema {
            blueprint_version: model.blueprint_version.clone(),
            model: FlatModelMeta {
                name: model.name.clone(),
                description: model.description.clone(),
                seed: if model.seed != 0 {
                    Some(model.seed)
                } else {
                    None
                },
                locale: if model.locale != "en" {
                    Some(model.locale.clone())
                } else {
                    None
                },
                timezone: if model.timezone != "UTC" {
                    Some(model.timezone.clone())
                } else {
                    None
                },
            },
            entities: model.entities.clone(),
            relationships: model.relationships.clone(),
            correlations: model.correlations.clone(),
            personas: model.personas.clone(),
            actor_relationships: model.actor_relationships.clone(),
            companion_files: model.companion_files.clone(),
        };
        let toml_str = toml::to_string_pretty(&flat)?;
        std::fs::write(output_path, toml_str)?;
        println!("  Written to: {}", output.green());
    } else {
        // Flat → Structured
        println!("{}", "Converting flat schema → structured model...".bold());
        let model = load_blueprint(input)?;
        writer::write_model_directory(&model, output_path)?;
        println!("  Written to: {}", output.green());
    }

    Ok(())
}

/// Run `knit model info` — show summary of a model.
pub fn run_info(input: &str) -> Result<()> {
    let input_path = Path::new(input);

    let (model, format_name) = if is_structured_model(input_path) {
        (
            reader::load_model_directory(input_path)?,
            "structured directory",
        )
    } else {
        (load_blueprint(input)?, "flat schema file")
    };

    println!("{}", "== Model Info ==".bold());
    println!("  Format:         {}", format_name);
    println!("  Name:           {}", model.name.cyan());
    if let Some(ref desc) = model.description {
        println!("  Description:    {}", desc);
    }
    println!("  Seed:           {}", model.seed);
    println!("  Locale:         {}", model.locale);
    println!("  Schema version: {}", model.blueprint_version);
    println!();

    println!("  {} entities:", model.entities.len());
    for entity in &model.entities {
        let count_str = match &entity.count {
            crate::core::CountSpec::Fixed(n) => format!("{} rows", n),
            crate::core::CountSpec::Range { min, max } => format!("{}-{} rows", min, max),
            _ => "dynamic".to_string(),
        };
        println!(
            "    {} — {} fields, {}",
            entity.name.green(),
            entity.fields.len(),
            count_str,
        );
    }

    if !model.relationships.is_empty() {
        println!("\n  {} relationships", model.relationships.len());
    }
    if !model.correlations.is_empty() {
        println!("  {} correlations", model.correlations.len());
    }
    if !model.personas.is_empty() {
        println!("  {} personas", model.personas.len());
    }
    if !model.companion_files.is_empty() {
        println!("  {} companion files", model.companion_files.len());
    }

    Ok(())
}

/// Run `knit model migrate` — upgrade a model to v2 structured format.
pub fn run_migrate(input: &str, output: Option<&str>) -> Result<()> {
    let input_path = Path::new(input);

    let mut model = if is_structured_model(input_path) {
        reader::load_model_directory(input_path)?
    } else {
        load_blueprint(input)?
    };

    // Early exit if already v2
    if model.blueprint_version == "2.0" && output.is_none() {
        println!("{}", "Model is already v2 — no migration needed.".green());
        return Ok(());
    }

    let old_version = model.blueprint_version.clone();
    model.migrate_to_v2();

    let out_path = match output {
        Some(p) => Path::new(p).to_path_buf(),
        None => {
            if is_structured_model(input_path) {
                // Refuse in-place overwrite without explicit --output
                anyhow::bail!(
                    "input is already a structured directory; specify --output explicitly"
                );
            }
            let stem = input_path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("model");
            input_path
                .parent()
                .unwrap_or(Path::new("."))
                .join(format!("{}_v2", stem))
        }
    };

    writer::write_model_directory(&model, &out_path)?;

    println!(
        "{}",
        format!(
            "Migrated {} → {} (v{} → v{})",
            input,
            out_path.display(),
            old_version,
            model.blueprint_version
        )
        .green()
    );

    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_migrate_v1_flat_to_v2_structured() {
        let dir = TempDir::new().unwrap();
        let input_file = dir.path().join("test.toml");
        std::fs::write(
            &input_file,
            r#"
blueprint_version = "1.0"
[model]
name = "migrate_test"
seed = 99

[[entities]]
name = "Users"
count = 50

[[entities.fields]]
name = "id"
data_type = "int"
"#,
        )
        .unwrap();

        let output_dir = dir.path().join("output_v2");
        run_migrate(
            input_file.to_str().unwrap(),
            Some(output_dir.to_str().unwrap()),
        )
        .unwrap();

        // Verify output is a v2 structured model
        assert!(output_dir.join("knit.toml").is_file());
        assert!(output_dir.join("tables").join("Users.toml").is_file());

        let loaded = reader::load_model_directory(&output_dir).unwrap();
        assert_eq!(loaded.blueprint_version, "2.0");
        assert_eq!(loaded.name, "migrate_test");
        assert_eq!(loaded.seed, 99);
        assert_eq!(loaded.entities.len(), 1);
        assert_eq!(loaded.entities[0].name, "Users");
    }
}