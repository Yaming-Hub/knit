//! CLI handler for `knit model` subcommands.

use std::path::Path;

use anyhow::Result;
use colored::Colorize;

use crate::cli::commands::load_schema;
use crate::model::{is_structured_model, reader, writer};

/// Run `knit model convert` — convert between flat and structured formats.
pub fn run_convert(input: &str, output: &str) -> Result<()> {
    let input_path = Path::new(input);
    let output_path = Path::new(output);

    if is_structured_model(input_path) {
        // Structured → Flat
        println!("{}", "Converting structured model → flat schema...".bold());
        let model = reader::load_model_directory(input_path)?;
        let toml_str = toml::to_string_pretty(&model)?;
        std::fs::write(output_path, toml_str)?;
        println!("  Written to: {}", output.green());
    } else {
        // Flat → Structured
        println!("{}", "Converting flat schema → structured model...".bold());
        let model = load_schema(input)?;
        writer::write_model_directory(&model, output_path)?;
        println!("  Written to: {}", output.green());
    }

    Ok(())
}

/// Run `knit model info` — show summary of a model.
pub fn run_info(input: &str) -> Result<()> {
    let input_path = Path::new(input);

    let (model, format_name) = if is_structured_model(input_path) {
        (reader::load_model_directory(input_path)?, "structured directory")
    } else {
        (load_schema(input)?, "flat schema file")
    };

    println!("{}", "== Model Info ==".bold());
    println!("  Format:         {}", format_name);
    println!("  Name:           {}", model.name.cyan());
    if let Some(ref desc) = model.description {
        println!("  Description:    {}", desc);
    }
    println!("  Seed:           {}", model.seed);
    println!("  Locale:         {}", model.locale);
    println!("  Schema version: {}", model.schema_version);
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
