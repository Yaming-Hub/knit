//! `knit init` — scaffold a new `schema.weave.toml` schema file.
//!
//! Creates a minimal, well-commented starter schema that the user can
//! populate with their own data model. Optionally uses a domain template
//! (ecommerce, financial, hr, iot, logs) to provide a full working example.

use std::fs;
use std::path::Path;

use anyhow::{bail, Result};
use colored::Colorize;

/// A template definition with its schema content and optional sidecar files.
struct Template {
    name: &'static str,
    description: &'static str,
    schema: &'static str,
    /// Sidecar files to write alongside the schema (relative path, content).
    sidecars: &'static [(&'static str, &'static str)],
}

/// Available templates.
const TEMPLATES: &[Template] = &[
    Template {
        name: "ecommerce",
        description: "E-commerce platform (users, products, orders, reviews)",
        schema: include_str!("../../../../examples/ecommerce.weave.toml"),
        sidecars: &[("products.dict.txt", include_str!("../../../../examples/products.dict.txt"))],
    },
    Template {
        name: "financial",
        description: "Financial transactions (accounts, transfers, audit)",
        schema: include_str!("../../../../examples/financial.weave.toml"),
        sidecars: &[],
    },
    Template {
        name: "hr",
        description: "HR organization (departments, employees, payroll)",
        schema: include_str!("../../../../examples/hr_org.weave.toml"),
        sidecars: &[],
    },
    Template {
        name: "iot",
        description: "IoT sensor readings (devices, measurements, alerts)",
        schema: include_str!("../../../../examples/iot_sensors.weave.toml"),
        sidecars: &[],
    },
    Template {
        name: "logs",
        description: "Server access logs (servers, endpoints, requests)",
        schema: include_str!("../../../../examples/server_logs.weave.toml"),
        sidecars: &[],
    },
];

/// Run the `knit init` command.
///
/// Creates a starter schema at the given path. If `--template` is provided,
/// uses a domain-specific template; otherwise generates a minimal scaffold.
pub fn run(output_path: &str, template: Option<&str>, list_templates: bool) -> Result<()> {
    if list_templates {
        println!("{}", "Available templates:".bold());
        println!();
        for t in TEMPLATES {
            println!("  {:12} {}", t.name.cyan(), t.description);
        }
        println!();
        println!("Usage: {} {}", "knit init --template".yellow(), "<name>".yellow());
        return Ok(());
    }

    // Resolve template content before touching the filesystem
    let (schema, sidecars, template_name) = match template {
        Some(name) => {
            let entry = TEMPLATES.iter().find(|t| t.name == name);
            match entry {
                Some(t) => (t.schema.to_string(), t.sidecars, Some(name)),
                None => {
                    let available: Vec<&str> = TEMPLATES.iter().map(|t| t.name).collect();
                    bail!(
                        "unknown template '{}'; available: {}",
                        name,
                        available.join(", ")
                    );
                }
            }
        }
        None => (generate_scaffold(), &[] as &[(&str, &str)], None),
    };

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

    fs::write(dest, &schema)?;

    // Write sidecar files alongside the schema
    let schema_dir = dest.parent().unwrap_or(Path::new("."));
    for (sidecar_path, content) in sidecars {
        let sidecar_dest = schema_dir.join(sidecar_path);
        fs::write(&sidecar_dest, content)?;
    }

    println!(
        "{} Created {}{}",
        "✓".green().bold(),
        output_path.cyan(),
        template_name
            .map(|t| format!(" (from '{}' template)", t))
            .unwrap_or_default()
    );
    if !sidecars.is_empty() {
        for (sidecar_path, _) in sidecars {
            println!("  {} {}", "+".green(), sidecar_path);
        }
    }
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
        run(path_str, None, false).unwrap();
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("schema_version"));
    }

    #[test]
    fn run_with_template() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ecom.weave.toml");
        let path_str = path.to_str().unwrap();
        run(path_str, Some("ecommerce"), false).unwrap();
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("ecommerce"));
    }

    #[test]
    fn run_unknown_template_fails() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.weave.toml");
        let result = run(path.to_str().unwrap(), Some("nonexistent"), false);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("unknown template 'nonexistent'"));
        assert!(err.contains("ecommerce"));
    }

    #[test]
    fn run_fails_if_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.toml");
        fs::write(&path, "exists").unwrap();
        let result = run(path.to_str().unwrap(), None, false);
        assert!(result.is_err());
    }

    #[test]
    fn list_templates_succeeds() {
        let result = run("ignored.toml", None, true);
        assert!(result.is_ok());
    }

    #[test]
    fn all_templates_are_valid_toml() {
        for t in TEMPLATES {
            let _: toml::Value =
                toml::from_str(t.schema).unwrap_or_else(|e| panic!("template '{}' invalid: {}", t.name, e));
        }
    }

    #[test]
    fn ecommerce_template_includes_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("ecom.weave.toml");
        run(path.to_str().unwrap(), Some("ecommerce"), false).unwrap();
        // Sidecar file should be written alongside schema
        assert!(dir.path().join("products.dict.txt").exists());
    }
}
