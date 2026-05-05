//! `knit schema` — schema manipulation subcommands.
//!
//! Provides:
//! - `expand` — flatten an extends chain into a standalone schema
//! - `normalize` — reformat a schema to canonical style
//! - `diff` — compare two schemas and show differences
//! - `doc` — generate markdown documentation for a schema

use std::collections::BTreeSet;

use anyhow::{Context, Result};
use colored::Colorize;
use knit_core::{DataModel, Entity, Field};

use super::load_schema;

/// Run the `schema expand` command.
///
/// Loads a schema file (resolving any `extends` chain via `knit_schema`),
/// then serializes the fully resolved model back to TOML.
pub fn run_expand(path: &str, json: bool) -> Result<()> {
    let model = load_schema(path)
        .with_context(|| format!("failed to load schema `{}`", path))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&model)?);
    } else {
        let output = serialize_model_to_toml(&model)?;
        println!("{}", output);
    }
    Ok(())
}

/// Run the `schema normalize` command.
///
/// Parses a schema and re-serializes it in canonical TOML form with
/// sorted keys and consistent formatting.
pub fn run_normalize(path: &str, json: bool) -> Result<()> {
    let model = load_schema(path)
        .with_context(|| format!("failed to load schema `{}`", path))?;

    if json {
        println!("{}", serde_json::to_string_pretty(&model)?);
    } else {
        let output = serialize_model_to_toml(&model)?;
        println!("{}", output);
    }
    Ok(())
}

/// Run the `schema diff` command.
///
/// Parses two schema files and compares them entity-by-entity
/// and field-by-field, printing colored diff output.
pub fn run_diff(path_a: &str, path_b: &str) -> Result<()> {
    let model_a = load_schema(path_a)
        .with_context(|| format!("failed to load schema `{}`", path_a))?;
    let model_b = load_schema(path_b)
        .with_context(|| format!("failed to load schema `{}`", path_b))?;

    let diffs = compute_diff(&model_a, &model_b);

    if diffs.is_empty() {
        println!("{}", "schemas are identical".green());
    } else {
        println!(
            "{} {} and {}",
            "diff".bold(),
            path_a.cyan(),
            path_b.cyan()
        );
        println!();
        for entry in &diffs {
            print_diff_entry(entry);
        }
        println!(
            "\n{} change(s) found",
            diffs.len().to_string().yellow()
        );
    }
    Ok(())
}

/// Run the `schema doc` command.
///
/// Generates markdown documentation for the schema including entity descriptions,
/// field tables, relationships, and generator info.
pub fn run_doc(path: &str, output: Option<&str>) -> Result<()> {
    let model = load_schema(path)
        .with_context(|| format!("failed to load schema `{}`", path))?;

    let markdown = generate_schema_doc(&model);

    if let Some(out_path) = output {
        std::fs::write(out_path, &markdown)
            .with_context(|| format!("failed to write to `{}`", out_path))?;
        println!(
            "{} documentation written to {}",
            "✓".green().bold(),
            out_path.cyan()
        );
    } else {
        print!("{}", markdown);
    }
    Ok(())
}

/// Generate markdown documentation for a data model.
pub fn generate_schema_doc(model: &DataModel) -> String {
    let mut doc = String::new();

    // Title
    doc.push_str(&format!("# {}\n\n", md_escape(&model.name)));
    if let Some(desc) = &model.description {
        doc.push_str(&format!("{}\n\n", desc));
    }

    // Overview table
    doc.push_str("## Overview\n\n");
    doc.push_str("| Property | Value |\n|---|---|\n");
    doc.push_str(&format!("| Schema version | {} |\n", model.schema_version));
    doc.push_str(&format!("| Seed | {} |\n", model.seed));
    doc.push_str(&format!("| Locale | {} |\n", model.locale));
    doc.push_str(&format!("| Entities | {} |\n", model.entities.len()));
    doc.push_str(&format!("| Relationships | {} |\n", model.relationships.len()));
    if !model.noise_profiles.is_empty() {
        doc.push_str(&format!("| Noise profiles | {} |\n", model.noise_profiles.len()));
    }
    doc.push('\n');

    // Entities
    doc.push_str("## Entities\n\n");
    for entity in &model.entities {
        doc.push_str(&format!("### {}\n\n", md_escape(&entity.name)));
        if let Some(desc) = &entity.description {
            doc.push_str(&format!("{}\n\n", desc));
        }
        doc.push_str(&format!("**Rows:** {}\n\n", format_count_spec(&entity.count)));

        if !entity.fields.is_empty() {
            doc.push_str("| Field | Type | Nullable | Generator |\n");
            doc.push_str("|---|---|---|---|\n");
            for field in &entity.fields {
                let nullable = match &field.nullable {
                    knit_core::NullSpec::Never => "no",
                    _ => "yes",
                };
                let generator = field
                    .generator
                    .as_ref()
                    .map(|g| md_escape(&format_generator_spec(g)))
                    .unwrap_or_else(|| "—".to_string());
                doc.push_str(&format!(
                    "| {} | {} | {} | {} |\n",
                    md_escape(&field.name),
                    field.data_type,
                    nullable,
                    generator
                ));
            }
            doc.push('\n');
        }
    }

    // Relationships
    if !model.relationships.is_empty() {
        doc.push_str("## Relationships\n\n");
        doc.push_str("| Name | From | To | Kind | FK Column |\n");
        doc.push_str("|---|---|---|---|---|\n");
        for rel in &model.relationships {
            let default_fk = format!("{}_id", rel.to);
            let fk = rel.foreign_key.as_deref().unwrap_or(&default_fk);
            doc.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                md_escape(&rel.name),
                md_escape(&rel.from),
                md_escape(&rel.to),
                rel.kind,
                md_escape(fk)
            ));
        }
        doc.push('\n');
    }

    doc
}

/// Escape characters that break markdown table cells.
fn md_escape(s: &str) -> String {
    s.replace('|', "\\|").replace('\n', " ")
}

fn format_count_spec(count: &knit_core::CountSpec) -> String {
    match count {
        knit_core::CountSpec::Fixed(n) => n.to_string(),
        knit_core::CountSpec::Range { min, max } => format!("{} – {}", min, max),
        knit_core::CountSpec::Distribution(spec) => format!("{}(…)", spec.kind),
    }
}

fn format_generator_spec(gen: &knit_core::GeneratorSpec) -> String {
    match gen {
        knit_core::GeneratorSpec::Distribution { spec } => format!("{}", spec.kind),
        knit_core::GeneratorSpec::Faker { method, .. } => format!("faker({})", method),
        knit_core::GeneratorSpec::Sequence { .. } => "sequence".to_string(),
        knit_core::GeneratorSpec::OneOf { choices, .. } => {
            format!("oneOf({} choices)", choices.len())
        }
        knit_core::GeneratorSpec::Pattern { pattern, .. } => format!("pattern({})", pattern),
        knit_core::GeneratorSpec::Derived { expr, .. } => format!("derived({})", expr),
        knit_core::GeneratorSpec::Conditional { .. } => "conditional".to_string(),
        knit_core::GeneratorSpec::Composite { .. } => "composite".to_string(),
        knit_core::GeneratorSpec::Lookup { entity, field } => {
            format!("lookup({}.{})", entity, field)
        }
        knit_core::GeneratorSpec::Constant { value } => format!("const({:?})", value),
        knit_core::GeneratorSpec::UuidGen { version } => format!("uuid(v{})", version),
        knit_core::GeneratorSpec::Unique { .. } => "unique(…)".to_string(),
        knit_core::GeneratorSpec::Relative { field, .. } => format!("relative({})", field),
        knit_core::GeneratorSpec::BusinessHours { .. } => "business_hours".to_string(),
    }
}

/// A single diff entry describing one change between two schemas.
#[derive(Debug, PartialEq)]
pub enum DiffEntry {
    /// Entity present only in the first schema.
    EntityRemoved(String),
    /// Entity present only in the second schema.
    EntityAdded(String),
    /// Field removed from an entity.
    FieldRemoved {
        entity: String,
        field: String,
    },
    /// Field added to an entity.
    FieldAdded {
        entity: String,
        field: String,
    },
    /// Field changed between schemas.
    FieldChanged {
        entity: String,
        field: String,
        detail: String,
    },
    /// Top-level model property changed.
    PropertyChanged {
        key: String,
        old_val: String,
        new_val: String,
    },
    /// Entity count changed.
    EntityCountChanged {
        entity: String,
        old_val: String,
        new_val: String,
    },
}

/// Compute differences between two data models.
pub fn compute_diff(a: &DataModel, b: &DataModel) -> Vec<DiffEntry> {
    let mut diffs = Vec::new();

    // Top-level property diffs
    if a.name != b.name {
        diffs.push(DiffEntry::PropertyChanged {
            key: "name".into(),
            old_val: a.name.clone(),
            new_val: b.name.clone(),
        });
    }
    if a.seed != b.seed {
        diffs.push(DiffEntry::PropertyChanged {
            key: "seed".into(),
            old_val: a.seed.to_string(),
            new_val: b.seed.to_string(),
        });
    }
    if a.locale != b.locale {
        diffs.push(DiffEntry::PropertyChanged {
            key: "locale".into(),
            old_val: a.locale.clone(),
            new_val: b.locale.clone(),
        });
    }

    // Collect entity names
    let names_a: BTreeSet<&str> = a.entities.iter().map(|e| e.name.as_str()).collect();
    let names_b: BTreeSet<&str> = b.entities.iter().map(|e| e.name.as_str()).collect();

    for name in names_a.difference(&names_b) {
        diffs.push(DiffEntry::EntityRemoved(name.to_string()));
    }
    for name in names_b.difference(&names_a) {
        diffs.push(DiffEntry::EntityAdded(name.to_string()));
    }

    // Compare common entities
    for name in names_a.intersection(&names_b) {
        let ea = a.entities.iter().find(|e| e.name == *name)
            .expect("entity present in intersection");
        let eb = b.entities.iter().find(|e| e.name == *name)
            .expect("entity present in intersection");
        diff_entity(&mut diffs, ea, eb);
    }

    diffs
}

/// Compare two entities and emit diffs.
fn diff_entity(diffs: &mut Vec<DiffEntry>, a: &Entity, b: &Entity) {
    let entity = &a.name;

    // Count diff
    let count_a = format!("{:?}", a.count);
    let count_b = format!("{:?}", b.count);
    if count_a != count_b {
        diffs.push(DiffEntry::EntityCountChanged {
            entity: entity.clone(),
            old_val: count_a,
            new_val: count_b,
        });
    }

    let fields_a: BTreeSet<&str> = a.fields.iter().map(|f| f.name.as_str()).collect();
    let fields_b: BTreeSet<&str> = b.fields.iter().map(|f| f.name.as_str()).collect();

    for name in fields_a.difference(&fields_b) {
        diffs.push(DiffEntry::FieldRemoved {
            entity: entity.clone(),
            field: name.to_string(),
        });
    }
    for name in fields_b.difference(&fields_a) {
        diffs.push(DiffEntry::FieldAdded {
            entity: entity.clone(),
            field: name.to_string(),
        });
    }

    for name in fields_a.intersection(&fields_b) {
        let fa = a.fields.iter().find(|f| f.name == *name)
            .expect("field present in intersection");
        let fb = b.fields.iter().find(|f| f.name == *name)
            .expect("field present in intersection");
        diff_field(diffs, entity, fa, fb);
    }
}

/// Compare two fields and emit diffs.
fn diff_field(diffs: &mut Vec<DiffEntry>, entity: &str, a: &Field, b: &Field) {
    if a.data_type != b.data_type {
        diffs.push(DiffEntry::FieldChanged {
            entity: entity.to_string(),
            field: a.name.clone(),
            detail: format!("type: {:?} → {:?}", a.data_type, b.data_type),
        });
    }
    if a.nullable != b.nullable {
        diffs.push(DiffEntry::FieldChanged {
            entity: entity.to_string(),
            field: a.name.clone(),
            detail: format!("nullable: {:?} → {:?}", a.nullable, b.nullable),
        });
    }
    let gen_a = format!("{:?}", a.generator);
    let gen_b = format!("{:?}", b.generator);
    if gen_a != gen_b {
        diffs.push(DiffEntry::FieldChanged {
            entity: entity.to_string(),
            field: a.name.clone(),
            detail: "generator changed".to_string(),
        });
    }
}

/// Print a coloured diff entry to stdout.
fn print_diff_entry(entry: &DiffEntry) {
    match entry {
        DiffEntry::EntityAdded(name) => {
            println!("{} entity {}", "+".green().bold(), name.green());
        }
        DiffEntry::EntityRemoved(name) => {
            println!("{} entity {}", "-".red().bold(), name.red());
        }
        DiffEntry::FieldAdded { entity, field } => {
            println!(
                "  {} {}.{}",
                "+".green().bold(),
                entity.dimmed(),
                field.green()
            );
        }
        DiffEntry::FieldRemoved { entity, field } => {
            println!(
                "  {} {}.{}",
                "-".red().bold(),
                entity.dimmed(),
                field.red()
            );
        }
        DiffEntry::FieldChanged {
            entity,
            field,
            detail,
        } => {
            println!(
                "  {} {}.{}: {}",
                "~".yellow().bold(),
                entity.dimmed(),
                field.yellow(),
                detail
            );
        }
        DiffEntry::PropertyChanged {
            key,
            old_val,
            new_val,
        } => {
            println!(
                "{} {}: {} → {}",
                "~".yellow().bold(),
                key.yellow(),
                old_val.red(),
                new_val.green()
            );
        }
        DiffEntry::EntityCountChanged {
            entity,
            old_val,
            new_val,
        } => {
            println!(
                "  {} {}.count: {} → {}",
                "~".yellow().bold(),
                entity.yellow(),
                old_val.red(),
                new_val.green()
            );
        }
    }
}

/// Serialize a [`DataModel`] to a canonical TOML schema string.
///
/// Produces a hand-formatted TOML document that matches the expected
/// `.weave.toml` layout with `[model]`, `[[entities]]`, and `[[relationships]]`
/// sections.
fn serialize_model_to_toml(model: &DataModel) -> Result<String> {
    let mut out = String::new();

    out.push_str(&format!("schema_version = \"{}\"\n\n", model.schema_version));

    // [model]
    out.push_str("[model]\n");
    out.push_str(&format!("name = \"{}\"\n", model.name));
    if let Some(desc) = &model.description {
        out.push_str(&format!("description = \"{}\"\n", desc));
    }
    out.push_str(&format!("seed = {}\n", model.seed));
    out.push_str(&format!("locale = \"{}\"\n", model.locale));
    out.push_str(&format!("timezone = \"{}\"\n", model.timezone));

    // [[entities]]
    for entity in &model.entities {
        out.push_str(&format!("\n[[entities]]\nname = \"{}\"\n", entity.name));
        if let Some(desc) = &entity.description {
            out.push_str(&format!("description = \"{}\"\n", desc));
        }
        // Count
        match &entity.count {
            knit_core::CountSpec::Fixed(n) => {
                out.push_str(&format!("count = {}\n", n));
            }
            knit_core::CountSpec::Range { min, max } => {
                out.push_str(&format!(
                    "count = {{ min = {}, max = {} }}\n",
                    min, max
                ));
            }
            knit_core::CountSpec::Distribution(dist) => {
                let dist_toml = toml::to_string(dist).unwrap_or_default();
                out.push_str(&format!("count = {{ distribution = {} }}\n", dist_toml.trim()));
            }
        }

        // Fields
        for field in &entity.fields {
            out.push_str(&format!("\n[[entities.fields]]\nname = \"{}\"\n", field.name));
            out.push_str(&format!("data_type = \"{:?}\"\n", field.data_type).to_lowercase());
            if let Some(pk) = field.primary_key {
                if pk {
                    out.push_str("primary_key = true\n");
                }
            }
            if let Some(prec) = field.precision {
                out.push_str(&format!("precision = {}\n", prec));
            }
            // Serialize generator if present
            if let Some(gen) = &field.generator {
                let gen_val = toml::Value::try_from(gen);
                if let Ok(val) = gen_val {
                    let gen_str = toml::to_string_pretty(&val)?;
                    // Indent under [entities.fields.generator]
                    out.push_str("[entities.fields.generator]\n");
                    out.push_str(&gen_str);
                }
            }
        }
    }

    // [[relationships]]
    for rel in &model.relationships {
        let rel_val = toml::Value::try_from(rel.clone());
        if let Ok(val) = rel_val {
            out.push_str("\n[[relationships]]\n");
            out.push_str(&toml::to_string_pretty(&val)?);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use knit_core::{CountSpec, DataType, Field, NullSpec};

    fn make_model(name: &str, entities: Vec<Entity>) -> DataModel {
        DataModel {
            name: name.to_string(),
            description: None,
            seed: 42,
            locale: "en_US".to_string(),
            timezone: "UTC".to_string(),
            entities,
            relationships: vec![],
            noise_profiles: vec![],
            correlations: vec![],
            params: std::collections::BTreeMap::new(),
            schema_version: "1.0".to_string(),
        }
    }

    fn make_entity(name: &str, fields: Vec<Field>) -> Entity {
        Entity {
            name: name.to_string(),
            description: None,
            count: CountSpec::Fixed(100),
            fields,
            constraints: vec![],
            topology: None,
        }
    }

    fn make_field(name: &str, dt: DataType) -> Field {
        Field {
            name: name.to_string(),
            description: None,
            data_type: dt,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
        }
    }

    #[test]
    fn diff_identical_models() {
        let model = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        let diffs = compute_diff(&model, &model);
        assert!(diffs.is_empty());
    }

    #[test]
    fn diff_added_entity() {
        let a = make_model("test", vec![]);
        let b = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        let diffs = compute_diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(&diffs[0], DiffEntry::EntityAdded(n) if n == "users"));
    }

    #[test]
    fn diff_removed_entity() {
        let a = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        let b = make_model("test", vec![]);
        let diffs = compute_diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(&diffs[0], DiffEntry::EntityRemoved(n) if n == "users"));
    }

    #[test]
    fn diff_added_field() {
        let a = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        let b = make_model(
            "test",
            vec![make_entity(
                "users",
                vec![
                    make_field("id", DataType::Int),
                    make_field("name", DataType::String),
                ],
            )],
        );
        let diffs = compute_diff(&a, &b);
        assert_eq!(diffs.len(), 1);
        assert!(matches!(
            &diffs[0],
            DiffEntry::FieldAdded { entity, field } if entity == "users" && field == "name"
        ));
    }

    #[test]
    fn diff_changed_field_type() {
        let a = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        let b = make_model(
            "test",
            vec![make_entity(
                "users",
                vec![make_field("id", DataType::String)],
            )],
        );
        let diffs = compute_diff(&a, &b);
        assert!(diffs.iter().any(|d| matches!(
            d,
            DiffEntry::FieldChanged { entity, field, .. } if entity == "users" && field == "id"
        )));
    }

    #[test]
    fn diff_property_changed() {
        let a = make_model("alpha", vec![]);
        let b = make_model("beta", vec![]);
        let diffs = compute_diff(&a, &b);
        assert!(diffs.iter().any(|d| matches!(
            d,
            DiffEntry::PropertyChanged { key, .. } if key == "name"
        )));
    }

    #[test]
    fn expand_produces_toml() {
        let model = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        let output = serialize_model_to_toml(&model).unwrap();
        assert!(output.contains("[model]"));
        assert!(output.contains("name = \"test\""));
        assert!(output.contains("[[entities]]"));
        assert!(output.contains("name = \"users\""));
    }

    #[test]
    fn doc_contains_title_and_entities() {
        let model = make_model(
            "my_schema",
            vec![make_entity(
                "users",
                vec![make_field("id", DataType::Int), make_field("name", DataType::String)],
            )],
        );
        let doc = generate_schema_doc(&model);
        assert!(doc.contains("# my_schema"));
        assert!(doc.contains("### users"));
        assert!(doc.contains("| id | int | no | — |"));
        assert!(doc.contains("| name | string | no | — |"));
        assert!(doc.contains("| Entities | 1 |"));
    }

    #[test]
    fn doc_includes_relationships() {
        use knit_core::Relationship;
        let mut model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity("orders", vec![make_field("id", DataType::Int)]),
            ],
        );
        model.relationships.push(Relationship {
            name: "orders_users".to_string(),
            from: "orders".to_string(),
            to: "users".to_string(),
            kind: knit_core::RelationshipKind::OneToMany,
            foreign_key: Some("user_id".to_string()),
            cardinality: None,
        });
        let doc = generate_schema_doc(&model);
        assert!(doc.contains("## Relationships"));
        assert!(doc.contains("| orders_users |"));
        assert!(doc.contains("| user_id |") || doc.contains("user_id"));
    }
}
