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
use crate::core::{DataModel, Entity, Field};

use super::load_schema;

/// Run the `schema expand` command.
///
/// Loads a schema file (resolving any `extends` chain via the schema module),
/// then serializes the fully resolved model back to TOML.
pub fn run_expand(path: &str, json: bool) -> Result<()> {
    let model = load_schema(path).with_context(|| format!("failed to load schema `{}`", path))?;

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
    let model = load_schema(path).with_context(|| format!("failed to load schema `{}`", path))?;

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
    let model_a =
        load_schema(path_a).with_context(|| format!("failed to load schema `{}`", path_a))?;
    let model_b =
        load_schema(path_b).with_context(|| format!("failed to load schema `{}`", path_b))?;

    let diffs = compute_diff(&model_a, &model_b);

    if diffs.is_empty() {
        println!("{}", "schemas are identical".green());
    } else {
        println!("{} {} and {}", "diff".bold(), path_a.cyan(), path_b.cyan());
        println!();
        for entry in &diffs {
            print_diff_entry(entry);
        }
        println!("\n{} change(s) found", diffs.len().to_string().yellow());
    }
    Ok(())
}

/// Run the `schema doc` command.
///
/// Generates markdown documentation for the schema including entity descriptions,
/// field tables, relationships, and generator info.
pub fn run_doc(path: &str, output: Option<&str>) -> Result<()> {
    let model = load_schema(path).with_context(|| format!("failed to load schema `{}`", path))?;

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
    doc.push_str(&format!(
        "| Relationships | {} |\n",
        model.relationships.len()
    ));
    if !model.noise_profiles.is_empty() {
        doc.push_str(&format!(
            "| Noise profiles | {} |\n",
            model.noise_profiles.len()
        ));
    }
    let actor_count = model.entities.iter().filter(|e| e.actor).count();
    if actor_count > 0 {
        doc.push_str(&format!("| Actor entities | {} |\n", actor_count));
    }
    if !model.personas.is_empty() {
        doc.push_str(&format!("| Personas | {} |\n", model.personas.len()));
    }
    if !model.actor_relationships.is_empty() {
        doc.push_str(&format!(
            "| Actor relationships | {} |\n",
            model.actor_relationships.len()
        ));
    }
    doc.push('\n');

    // Entities
    doc.push_str("## Entities\n\n");
    for entity in &model.entities {
        let actor_badge = if entity.actor { " 🎭" } else { "" };
        doc.push_str(&format!(
            "### {}{}\n\n",
            md_escape(&entity.name),
            actor_badge
        ));
        if let Some(desc) = &entity.description {
            doc.push_str(&format!("{}\n\n", desc));
        }
        doc.push_str(&format!(
            "**Rows:** {}\n\n",
            format_count_spec(&entity.count)
        ));
        if entity.actor {
            if let Some(pd) = &entity.persona_distribution {
                doc.push_str(&format!("**Persona distribution:** {}\n\n", md_escape(pd)));
            } else {
                doc.push_str("**Actor entity** (no persona distribution specified)\n\n");
            }
        }
        if let Some(ac) = &entity.activity_count {
            doc.push_str(&format!(
                "**Activity count:** actor\\_field=`{}`, trait=`{}`\n\n",
                ac.actor_field, ac.trait_name
            ));
        }

        if !entity.fields.is_empty() {
            doc.push_str("| Field | Type | Nullable | Generator |\n");
            doc.push_str("|---|---|---|---|\n");
            for field in &entity.fields {
                let nullable = match &field.nullable {
                    crate::core::NullSpec::Never => "no",
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

    // Personas
    if !model.personas.is_empty() {
        doc.push_str("## Personas\n\n");
        doc.push_str("| Name | Weight | Traits |\n|---|---|---|\n");
        for persona in &model.personas {
            let traits_str = if persona.traits.is_empty() {
                "—".to_string()
            } else {
                persona
                    .traits
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            let pct = persona.weight * 100.0;
            let pct_str = if (pct - pct.round()).abs() < 0.01 {
                format!("{:.0}%", pct)
            } else {
                format!("{:.1}%", pct)
            };
            doc.push_str(&format!(
                "| {} | {} | {} |\n",
                md_escape(&persona.name),
                pct_str,
                md_escape(&traits_str)
            ));
        }
        doc.push('\n');
    }

    // Actor Relationships
    if !model.actor_relationships.is_empty() {
        doc.push_str("## Actor Relationships\n\n");
        doc.push_str("| Name | From | To | Graph Type | Parameters |\n|---|---|---|---|---|\n");
        for ar in &model.actor_relationships {
            let mut parts: Vec<String> = ar
                .params
                .iter()
                .map(|(k, v)| format!("{}={}", k, v))
                .collect();
            if let Some(ref cc) = ar.community_count {
                parts.push(format!("community_count={}", format_count_spec(cc)));
            }
            if let Some(hd) = ar.hierarchy_depth {
                parts.push(format!("hierarchy_depth={}", hd));
            }
            let params_str = if parts.is_empty() {
                "—".to_string()
            } else {
                parts.join(", ")
            };
            doc.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                md_escape(&ar.name),
                md_escape(&ar.from_entity),
                md_escape(&ar.to_entity),
                ar.graph_type,
                md_escape(&params_str)
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

fn format_count_spec(count: &crate::core::CountSpec) -> String {
    match count {
        crate::core::CountSpec::Fixed(n) => n.to_string(),
        crate::core::CountSpec::Range { min, max } => format!("{} – {}", min, max),
        crate::core::CountSpec::Expression { expr } => format!("expr: {}", expr),
        crate::core::CountSpec::Distribution(spec) => format!("{}(…)", spec.kind),
    }
}

fn format_generator_spec(gen: &crate::core::GeneratorSpec) -> String {
    match gen {
        crate::core::GeneratorSpec::Distribution { spec } => format!("{}", spec.kind),
        crate::core::GeneratorSpec::Faker { method, .. } => format!("faker({})", method),
        crate::core::GeneratorSpec::Sequence { .. } => "sequence".to_string(),
        crate::core::GeneratorSpec::OneOf { choices, .. } => {
            format!("oneOf({} choices)", choices.len())
        }
        crate::core::GeneratorSpec::Pattern { pattern, .. } => format!("pattern({})", pattern),
        crate::core::GeneratorSpec::Derived { expr, .. } => format!("derived({})", expr),
        crate::core::GeneratorSpec::Conditional { .. } => "conditional".to_string(),
        crate::core::GeneratorSpec::Composite { .. } => "composite".to_string(),
        crate::core::GeneratorSpec::Lookup { entity, field } => {
            format!("lookup({}.{})", entity, field)
        }
        crate::core::GeneratorSpec::Constant { value } => format!("const({:?})", value),
        crate::core::GeneratorSpec::UuidGen { version } => format!("uuid(v{})", version),
        crate::core::GeneratorSpec::Unique { .. } => "unique(…)".to_string(),
        crate::core::GeneratorSpec::Relative { field, .. } => format!("relative({})", field),
        crate::core::GeneratorSpec::BusinessHours { .. } => "business_hours".to_string(),
        crate::core::GeneratorSpec::Dictionary {
            file, expansion, ..
        } => {
            format!("dictionary({}, {})", file, expansion)
        }
        crate::core::GeneratorSpec::ActorRef { entity } => format!("actor_ref({})", entity),
        crate::core::GeneratorSpec::ActorTemporal { trait_name, .. } => {
            format!("actor_temporal({})", trait_name)
        }
        crate::core::GeneratorSpec::RelationshipRef {
            relationship,
            source_field,
        } => {
            if let Some(src) = source_field {
                format!("relationship_ref({}, source={})", relationship, src)
            } else {
                format!("relationship_ref({})", relationship)
            }
        }
        crate::core::GeneratorSpec::PersonaField { trait_name } => {
            format!("persona_field({})", trait_name)
        }
        crate::core::GeneratorSpec::ThreadRef {
            reply_probability,
            max_depth,
            ..
        } => {
            format!(
                "thread_ref(p={:.0}%, depth={})",
                reply_probability * 100.0,
                max_depth
            )
        }
        crate::core::GeneratorSpec::Plugin { name, .. } => format!("plugin({})", name),
        crate::core::GeneratorSpec::ExternalLookup {
            source, column, format, ..
        } => {
            format!("external_lookup({}, {}, {})", source, column, format)
        }
        crate::core::GeneratorSpec::TimeSeries { components, .. } => {
            format!("time_series({} components)", components.len())
        }
        crate::core::GeneratorSpec::EventStream { components, .. } => {
            format!("event_stream({} components)", components.len())
        }
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
    FieldRemoved { entity: String, field: String },
    /// Field added to an entity.
    FieldAdded { entity: String, field: String },
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
        let ea = a
            .entities
            .iter()
            .find(|e| e.name == *name)
            .expect("entity present in intersection");
        let eb = b
            .entities
            .iter()
            .find(|e| e.name == *name)
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

    // Actor flag diff
    if a.actor != b.actor {
        diffs.push(DiffEntry::FieldChanged {
            entity: entity.clone(),
            field: "actor".to_string(),
            detail: format!("{} → {}", a.actor, b.actor),
        });
    }

    // Persona distribution diff
    if a.persona_distribution != b.persona_distribution {
        diffs.push(DiffEntry::FieldChanged {
            entity: entity.clone(),
            field: "persona_distribution".to_string(),
            detail: format!(
                "{:?} → {:?}",
                a.persona_distribution, b.persona_distribution
            ),
        });
    }

    // Activity count diff
    if a.activity_count != b.activity_count {
        diffs.push(DiffEntry::FieldChanged {
            entity: entity.clone(),
            field: "activity_count".to_string(),
            detail: format!("{:?} → {:?}", a.activity_count, b.activity_count),
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
        let fa = a
            .fields
            .iter()
            .find(|f| f.name == *name)
            .expect("field present in intersection");
        let fb = b
            .fields
            .iter()
            .find(|f| f.name == *name)
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
    if a.actor_column != b.actor_column {
        diffs.push(DiffEntry::FieldChanged {
            entity: entity.to_string(),
            field: a.name.clone(),
            detail: format!("actor_column: {} → {}", a.actor_column, b.actor_column),
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
            println!("  {} {}.{}", "-".red().bold(), entity.dimmed(), field.red());
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

    out.push_str(&format!(
        "schema_version = \"{}\"\n\n",
        model.schema_version
    ));

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
        if entity.actor {
            out.push_str("actor = true\n");
        }
        if let Some(pd) = &entity.persona_distribution {
            out.push_str(&format!("persona_distribution = \"{}\"\n", pd));
        }
        if let Some(ac) = &entity.activity_count {
            out.push_str(&format!(
                "activity_count = {{ actor_field = \"{}\", \"trait\" = \"{}\" }}\n",
                ac.actor_field, ac.trait_name
            ));
        }
        // Count
        match &entity.count {
            crate::core::CountSpec::Fixed(n) => {
                out.push_str(&format!("count = {}\n", n));
            }
            crate::core::CountSpec::Range { min, max } => {
                out.push_str(&format!("count = {{ min = {}, max = {} }}\n", min, max));
            }
            crate::core::CountSpec::Expression { expr } => {
                let escaped = expr.replace('\\', "\\\\").replace('"', "\\\"");
                out.push_str(&format!("count = {{ expr = \"{}\" }}\n", escaped));
            }
            crate::core::CountSpec::Distribution(dist) => {
                let dist_toml = toml::to_string(dist).unwrap_or_default();
                out.push_str(&format!(
                    "count = {{ distribution = {} }}\n",
                    dist_toml.trim()
                ));
            }
        }

        // Fields
        for field in &entity.fields {
            out.push_str(&format!(
                "\n[[entities.fields]]\nname = \"{}\"\n",
                field.name
            ));
            out.push_str(&format!("data_type = \"{:?}\"\n", field.data_type).to_lowercase());
            if let Some(pk) = field.primary_key {
                if pk {
                    out.push_str("primary_key = true\n");
                }
            }
            if let Some(prec) = field.precision {
                out.push_str(&format!("precision = {}\n", prec));
            }
            if field.actor_column {
                out.push_str("actor_column = true\n");
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

    // [[personas]]
    for persona in &model.personas {
        let val = toml::Value::try_from(persona.clone());
        if let Ok(val) = val {
            out.push_str("\n[[personas]]\n");
            out.push_str(&toml::to_string_pretty(&val)?);
        }
    }

    // [[actor_relationships]]
    for ar in &model.actor_relationships {
        let val = toml::Value::try_from(ar.clone());
        if let Ok(val) = val {
            out.push_str("\n[[actor_relationships]]\n");
            out.push_str(&toml::to_string_pretty(&val)?);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CountSpec, DataType, Field, NullSpec};

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
            personas: Vec::new(),
            actor_relationships: Vec::new(),
            custom_types: Vec::new(),
            mixins: Vec::new(),
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
            actor: false,
            persona_distribution: None,
            activity_count: None,
                mixin_refs: None,
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
            actor_column: false,
            fields: vec![],
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
                vec![
                    make_field("id", DataType::Int),
                    make_field("name", DataType::String),
                ],
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
        use crate::core::Relationship;
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
            kind: crate::core::RelationshipKind::OneToMany,
            foreign_key: Some("user_id".to_string()),
            cardinality: None,
            degree: None,

            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![],
        });
        let doc = generate_schema_doc(&model);
        assert!(doc.contains("## Relationships"));
        assert!(doc.contains("| orders_users |"));
        assert!(doc.contains("| user_id |") || doc.contains("user_id"));
    }

    #[test]
    fn diff_entity_actor_flag_changed() {
        let mut a_entity = make_entity("users", vec![make_field("id", DataType::Int)]);
        a_entity.actor = false;
        let mut b_entity = make_entity("users", vec![make_field("id", DataType::Int)]);
        b_entity.actor = true;
        let a = make_model("test", vec![a_entity]);
        let b = make_model("test", vec![b_entity]);
        let diffs = compute_diff(&a, &b);
        assert!(diffs.iter().any(|d| matches!(
            d,
            DiffEntry::FieldChanged { entity, field, detail }
                if entity == "users" && field == "actor" && detail.contains("true")
        )));
    }

    #[test]
    fn diff_field_actor_column_changed() {
        let mut a_field = make_field("user_id", DataType::Int);
        a_field.actor_column = false;
        let mut b_field = make_field("user_id", DataType::Int);
        b_field.actor_column = true;
        let a = make_model(
            "test",
            vec![make_entity(
                "events",
                vec![make_field("id", DataType::Int), a_field],
            )],
        );
        let b = make_model(
            "test",
            vec![make_entity(
                "events",
                vec![make_field("id", DataType::Int), b_field],
            )],
        );
        let diffs = compute_diff(&a, &b);
        assert!(diffs.iter().any(|d| matches!(
            d,
            DiffEntry::FieldChanged { entity, field, detail }
                if entity == "events" && field == "user_id" && detail.contains("actor_column")
        )));
    }

    #[test]
    fn doc_shows_actor_entity_badge() {
        let mut entity = make_entity("users", vec![make_field("id", DataType::Int)]);
        entity.actor = true;
        entity.persona_distribution = Some("power_mix".to_string());
        let model = make_model("test", vec![entity]);
        let doc = generate_schema_doc(&model);
        assert!(doc.contains("### users 🎭"), "should have actor badge");
        assert!(
            doc.contains("Actor entities | 1"),
            "overview should count actors"
        );
        assert!(doc.contains("**Persona distribution:** power_mix"));
    }

    #[test]
    fn doc_shows_personas_section() {
        use crate::core::Persona;
        let mut model = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        let mut traits = std::collections::BTreeMap::new();
        traits.insert("activity_rate".to_string(), crate::core::Value::Float(0.8));
        traits.insert(
            "peak_hours".to_string(),
            crate::core::Value::String("morning".to_string()),
        );
        model.personas.push(Persona {
            name: "power_user".to_string(),
            weight: 0.3,
            traits,
        });
        model.personas.push(Persona {
            name: "casual".to_string(),
            weight: 0.7,
            traits: std::collections::BTreeMap::new(),
        });
        let doc = generate_schema_doc(&model);
        assert!(doc.contains("## Personas"), "should have personas section");
        assert!(
            doc.contains("| Personas | 2 |"),
            "overview should count personas"
        );
        assert!(doc.contains("| power_user | 30% | activity_rate, peak_hours |"));
        assert!(doc.contains("| casual | 70% | — |"));
    }

    #[test]
    fn doc_shows_actor_relationships_section() {
        use crate::core::ActorRelationship;
        let mut entity = make_entity("users", vec![make_field("id", DataType::Int)]);
        entity.actor = true;
        let mut model = make_model("test", vec![entity]);
        let mut params = std::collections::BTreeMap::new();
        params.insert("avg_degree".to_string(), 5.0);
        model.actor_relationships.push(ActorRelationship {
            name: "email_network".to_string(),
            from_entity: "users".to_string(),
            to_entity: "users".to_string(),
            graph_type: Default::default(),
            params,
            community_count: Some(crate::core::CountSpec::Fixed(4)),
            hierarchy_depth: Some(3),
        });
        let doc = generate_schema_doc(&model);
        assert!(doc.contains("## Actor Relationships"));
        assert!(doc.contains("| Actor relationships | 1 |"));
        assert!(doc.contains("| email_network |"));
        assert!(doc.contains("scale_free"), "should use Display, not Debug");
        assert!(doc.contains("avg_degree=5"));
        assert!(doc.contains("community_count=4"));
        assert!(doc.contains("hierarchy_depth=3"));
    }

    #[test]
    fn doc_omits_behavioral_sections_when_empty() {
        let model = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        let doc = generate_schema_doc(&model);
        assert!(!doc.contains("## Personas"));
        assert!(!doc.contains("## Actor Relationships"));
        assert!(!doc.contains("Actor entities"));
        assert!(!doc.contains("| Personas |"));
        assert!(!doc.contains("| Actor relationships |"));
    }
}