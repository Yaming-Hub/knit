//! `knit init` — scaffold a new `.weave.toml` schema file.
//!
//! Creates a minimal, well-commented starter schema that the user can
//! populate with their own data model. The schema language is the single
//! source of truth for data definition — no domain-specific templates needed.

use std::fs;
use std::path::Path;

use anyhow::{bail, Result};
use colored::Colorize;

/// Run the `knit init` command.
///
/// Creates a minimal `.weave.toml` starter schema at the given path.
/// The generated file contains commented documentation showing available
/// options so the user can define their own data model.
pub fn run(output_path: &str) -> Result<()> {
    let dest = Path::new(output_path);

    if dest.exists() {
        bail!(
            "{} already exists. Remove it first or choose a different path.",
            output_path
        );
    }

    // Create parent directories if needed
    if let Some(parent) = dest.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let schema = generate_scaffold();
    fs::write(dest, &schema)?;

    println!(
        "{} Created {}",
        "✓".green().bold(),
        output_path.cyan()
    );
    println!();
    println!(
        "  Edit the schema to define your data model, then run:"
    );
    println!(
        "    {} to verify syntax",
        "knit validate".yellow()
    );
    println!(
        "    {} to preview the execution plan",
        "knit plan".yellow()
    );
    println!(
        "    {} to generate data",
        "knit generate".yellow()
    );

    Ok(())
}

/// Generate a minimal, well-documented scaffold schema.
fn generate_scaffold() -> String {
    r#"# Knit Schema — Data Model Definition
# See: https://github.com/Yaming-Hub/knit/blob/main/docs/weave-spec.md

schema_version = "1.0"

[model]
name = "my_dataset"
description = "Describe your data model here"
seed = 42
# locale = "en_US"
# timezone = "UTC"

# Define entities (tables). Each entity produces one output file.
# Specify generators for each field to control how data is produced.

[[entities]]
name = "example"
count = 1000

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1

[[entities.fields]]
name = "name"
data_type = "string"
[entities.fields.generator]
type = "pattern"
pattern = "item-[A-Z]{3}-[0-9]{4}"

[[entities.fields]]
name = "value"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "normal"
[entities.fields.generator.params]
mean = 100.0
std_dev = 25.0

[[entities.fields]]
name = "created_at"
data_type = "datetime"

# Add more entities and define relationships between them:
#
# [[entities]]
# name = "child_table"
# count = 5000
#
# [[entities.fields]]
# name = "parent_id"
# data_type = "int"
# [entities.fields.generator]
# type = "lookup"
# entity = "example"
# field = "id"
#
# [[relationships]]
# name = "child_to_parent"
# from_entity = "child_table"
# from_field = "parent_id"
# to_entity = "example"
# to_field = "id"
# kind = "many_to_one"

# Available generator types:
#   sequence        — incrementing integers (start, step)
#   uuid            — random UUIDs
#   pattern         — regex-like pattern strings
#   distribution    — statistical (normal, uniform, exponential, zipf, etc.)
#   one_of          — weighted random selection from choices
#   lookup          — reference to another entity's field (foreign key)
#   relative        — value relative to another field (e.g. end = start + offset)
#   business_hours  — timestamps constrained to working hours
#   derived         — computed from other fields (expressions)
#   conditional     — branching based on another field's value
#   composite       — template-based composition of sub-generators
#   constant        — fixed value for every row
#   unique          — wrap any generator with uniqueness enforcement
#   faker           — structured fake data (names, emails, etc.)
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_is_valid_toml() {
        let schema = generate_scaffold();
        // Verify it parses as valid TOML
        let _: toml::Value = toml::from_str(&schema).expect("scaffold should be valid TOML");
    }

    #[test]
    fn scaffold_contains_required_fields() {
        let schema = generate_scaffold();
        assert!(schema.contains("schema_version"));
        assert!(schema.contains("[model]"));
        assert!(schema.contains("[[entities]]"));
        assert!(schema.contains("[[entities.fields]]"));
    }

    #[test]
    fn run_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.weave.toml");
        let path_str = path.to_str().unwrap();
        run(path_str).unwrap();
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("schema_version"));
    }

    #[test]
    fn run_fails_if_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.toml");
        fs::write(&path, "exists").unwrap();
        let result = run(path.to_str().unwrap());
        assert!(result.is_err());
    }
}
