//! `knit blueprint` — schema manipulation subcommands.
//!
//! Provides:
//! - `expand` — flatten an extends chain into a standalone schema
//! - `normalize` — reformat a blueprint to canonical style
//! - `diff` — compare two schemas and show differences
//! - `doc` — generate markdown documentation for a schema

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use colored::Colorize;
use crate::core::{DataModel, Entity, Field};

use super::{load_blueprint, validate_model};

/// Run the `schema expand` command.
///
/// Loads a blueprint file (resolving any `extends` chain via the schema module),
/// then serializes the fully resolved model back to TOML.
pub fn run_expand(path: &str, json: bool) -> Result<()> {
    let model = load_blueprint(path).with_context(|| format!("failed to load schema `{}`", path))?;

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
    let model = load_blueprint(path).with_context(|| format!("failed to load schema `{}`", path))?;

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
/// Parses two blueprint files and compares them entity-by-entity
/// and field-by-field, printing colored diff output.
pub fn run_diff(path_a: &str, path_b: &str) -> Result<()> {
    let model_a =
        load_blueprint(path_a).with_context(|| format!("failed to load schema `{}`", path_a))?;
    let model_b =
        load_blueprint(path_b).with_context(|| format!("failed to load schema `{}`", path_b))?;

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
    let model = load_blueprint(path).with_context(|| format!("failed to load schema `{}`", path))?;

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
    doc.push_str(&format!("| Schema version | {} |\n", model.blueprint_version));
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
        crate::core::GeneratorSpec::Relative { anchor, .. } => format!("relative({})", anchor),
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

/// Escape a string for use inside TOML double-quoted strings.
fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

/// Serialize a [`DataModel`] to a canonical TOML schema string.
///
/// Produces a hand-formatted TOML document that matches the expected
/// `.knit.toml` layout with `[model]`, `[[entities]]`, and `[[relationships]]`
/// sections.
fn serialize_model_to_toml(model: &DataModel) -> Result<String> {
    let mut out = String::new();

    out.push_str(&format!(
        "blueprint_version = \"{}\"\n\n",
        model.blueprint_version
    ));

    // [model]
    out.push_str("[model]\n");
    out.push_str(&format!("name = \"{}\"\n", model.name));
    if let Some(desc) = &model.description {
        out.push_str(&format!("description = \"{}\"\n", toml_escape(desc)));
    }
    out.push_str(&format!("seed = {}\n", model.seed));
    out.push_str(&format!("locale = \"{}\"\n", model.locale));
    out.push_str(&format!("timezone = \"{}\"\n", model.timezone));

    // [[entities]]
    for entity in &model.entities {
        out.push_str(&format!("\n[[entities]]\nname = \"{}\"\n", entity.name));
        if let Some(desc) = &entity.description {
            out.push_str(&format!("description = \"{}\"\n", toml_escape(desc)));
        }
        if !entity.tags.is_empty() {
            let tags_str: Vec<String> = entity.tags.iter().map(|t| format!("\"{}\"", toml_escape(t))).collect();
            out.push_str(&format!("tags = [{}]\n", tags_str.join(", ")));
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
            if let Some(desc) = &field.description {
                out.push_str(&format!("description = \"{}\"\n", toml_escape(desc)));
            }
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

// ── Blueprint stats ──────────────────────────────────────────────────

/// Collected statistics about a blueprint's structure and complexity.
#[derive(Debug, serde::Serialize)]
pub struct BlueprintStats {
    pub entities: usize,
    pub total_fields: usize,
    pub relationships: usize,
    pub correlations: usize,
    pub noise_profiles: usize,
    pub personas: usize,
    pub actor_relationships: usize,
    pub estimated_rows: u64,
    pub generator_usage: BTreeMap<String, usize>,
    pub data_type_usage: BTreeMap<String, usize>,
    pub entity_details: Vec<EntityStats>,
    pub scaling_annotated: usize,
}

/// Per-entity statistics.
#[derive(Debug, serde::Serialize)]
pub struct EntityStats {
    pub name: String,
    pub fields: usize,
    pub estimated_rows: u64,
    pub constraints: usize,
    pub is_actor: bool,
    pub has_scaling: bool,
    pub has_topology: bool,
    pub nullable_fields: usize,
}

/// Compute blueprint statistics.
pub fn compute_stats(model: &DataModel) -> BlueprintStats {
    let mut generator_usage: BTreeMap<String, usize> = BTreeMap::new();
    let mut data_type_usage: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_fields = 0usize;
    let mut estimated_rows = 0u64;
    let mut entity_details = Vec::new();
    let mut scaling_annotated = 0usize;

    for entity in &model.entities {
        let fields = count_fields_recursive(&entity.fields);
        total_fields += fields;

        let entity_rows = crate::plan::compiler::resolve_count_estimate(&entity.count);
        estimated_rows += entity_rows;

        let nullable_fields = count_nullable(&entity.fields);

        if entity.scaling.is_some() {
            scaling_annotated += 1;
        }

        entity_details.push(EntityStats {
            name: entity.name.clone(),
            fields,
            estimated_rows: entity_rows,
            constraints: entity.constraints.len(),
            is_actor: entity.actor,
            has_scaling: entity.scaling.is_some(),
            has_topology: entity.topology.is_some(),
            nullable_fields,
        });

        for field in &entity.fields {
            collect_generator_usage(field, &mut generator_usage);
            collect_data_type_usage(field, &mut data_type_usage);
        }
    }

    BlueprintStats {
        entities: model.entities.len(),
        total_fields,
        relationships: model.relationships.len(),
        correlations: model.correlations.len(),
        noise_profiles: model.noise_profiles.len(),
        personas: model.personas.len(),
        actor_relationships: model.actor_relationships.len(),
        estimated_rows,
        generator_usage,
        data_type_usage,
        entity_details,
        scaling_annotated,
    }
}

fn count_fields_recursive(fields: &[Field]) -> usize {
    fields
        .iter()
        .map(|f| 1 + count_fields_recursive(&f.fields))
        .sum()
}

fn count_nullable(fields: &[Field]) -> usize {
    fields
        .iter()
        .map(|f| {
            let this = if matches!(f.nullable, crate::core::NullSpec::Never) {
                0
            } else {
                1
            };
            this + count_nullable(&f.fields)
        })
        .sum()
}

fn collect_generator_usage(field: &Field, usage: &mut BTreeMap<String, usize>) {
    if let Some(ref gen) = field.generator {
        *usage.entry(gen.type_name().to_string()).or_insert(0) += 1;
    }
    for sub in &field.fields {
        collect_generator_usage(sub, usage);
    }
}

fn collect_data_type_usage(field: &Field, usage: &mut BTreeMap<String, usize>) {
    *usage
        .entry(field.data_type.to_string())
        .or_insert(0) += 1;
    for sub in &field.fields {
        collect_data_type_usage(sub, usage);
    }
}

/// Run `knit blueprint stats`.
pub fn run_stats(path: &str, json: bool) -> Result<()> {
    let model =
        load_blueprint(path).with_context(|| format!("failed to load blueprint `{}`", path))?;
    let stats = compute_stats(&model);

    if json {
        println!("{}", serde_json::to_string_pretty(&stats)?);
        return Ok(());
    }

    // ── Header ──────────────────────────────────────────────────────
    println!(
        "{} {}",
        "Blueprint:".bold(),
        model.name.cyan()
    );
    if let Some(ref desc) = model.description {
        println!("  {}", desc.dimmed());
    }
    println!();

    // ── Overview table ──────────────────────────────────────────────
    println!("{}", "Overview".bold().underline());
    println!("  {} {}", "Entities:".dimmed(), stats.entities);
    println!("  {} {}", "Fields:".dimmed(), stats.total_fields);
    println!(
        "  {} ~{}",
        "Estimated rows:".dimmed(),
        format_count(stats.estimated_rows)
    );
    println!(
        "  {} {}",
        "Relationships:".dimmed(),
        stats.relationships
    );
    if stats.correlations > 0 {
        println!("  {} {}", "Correlations:".dimmed(), stats.correlations);
    }
    if stats.noise_profiles > 0 {
        println!(
            "  {} {}",
            "Noise profiles:".dimmed(),
            stats.noise_profiles
        );
    }
    if stats.personas > 0 {
        println!("  {} {}", "Personas:".dimmed(), stats.personas);
    }
    if stats.scaling_annotated > 0 {
        println!(
            "  {} {}/{}",
            "Scaling annotations:".dimmed(),
            stats.scaling_annotated,
            stats.entities
        );
    }
    println!();

    // ── Entity breakdown ────────────────────────────────────────────
    println!("{}", "Entities".bold().underline());
    for e in &stats.entity_details {
        let mut flags = Vec::new();
        if e.is_actor {
            flags.push("actor".to_string());
        }
        if e.has_topology {
            flags.push("topology".to_string());
        }
        if e.has_scaling {
            flags.push("scaled".to_string());
        }
        let flag_str = if flags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", flags.join(", "))
        };

        println!(
            "  {} — {} fields, ~{} rows, {} constraints{}",
            e.name.cyan(),
            e.fields,
            format_count(e.estimated_rows),
            e.constraints,
            flag_str.dimmed()
        );
    }
    println!();

    // ── Generator distribution ──────────────────────────────────────
    if !stats.generator_usage.is_empty() {
        println!("{}", "Generators".bold().underline());
        let mut sorted: Vec<_> = stats.generator_usage.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        for (gen, count) in sorted {
            let pct = (*count as f64 / stats.total_fields as f64) * 100.0;
            println!("  {:15} {:>4} ({:.0}%)", gen, count, pct);
        }
        println!();
    }

    // ── Data type distribution ──────────────────────────────────────
    if !stats.data_type_usage.is_empty() {
        println!("{}", "Data Types".bold().underline());
        let mut sorted: Vec<_> = stats.data_type_usage.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        for (dt, count) in sorted {
            println!("  {:15} {:>4}", dt, count);
        }
        println!();
    }

    Ok(())
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

// ── Blueprint Merge ─────────────────────────────────────────────────

/// Result of merging two blueprints.
#[derive(Debug, serde::Serialize)]
pub struct MergeReport {
    /// Entities added from overlay (not present in base).
    pub entities_added: Vec<String>,
    /// Entities that existed in both (base version kept, overlay fields appended).
    pub entities_merged: Vec<String>,
    /// Relationships added from overlay.
    pub relationships_added: usize,
    /// Noise profiles added from overlay.
    pub noise_profiles_added: usize,
    /// Correlations added from overlay.
    pub correlations_added: usize,
    /// Personas added from overlay.
    pub personas_added: usize,
    /// Warnings (e.g. duplicate field names in merged entities).
    pub warnings: Vec<String>,
}

/// Merge two DataModels. The base model is used as the foundation; the overlay
/// adds new entities, appends fields to shared entities, and unions collection
/// fields (relationships, noise, correlations, personas, etc.).
pub fn merge_models(base: &DataModel, overlay: &DataModel) -> (DataModel, MergeReport) {
    let mut result = base.clone();
    let mut report = MergeReport {
        entities_added: Vec::new(),
        entities_merged: Vec::new(),
        relationships_added: 0,
        noise_profiles_added: 0,
        correlations_added: 0,
        personas_added: 0,
        warnings: Vec::new(),
    };

    for overlay_entity in &overlay.entities {
        if let Some(base_entity) = result.entities.iter_mut().find(|e| e.name == overlay_entity.name)
        {
            // Warn about entity-level metadata differences (base version is kept).
            if overlay_entity.count != base_entity.count {
                report.warnings.push(format!(
                    "entity `{}`: count differs, keeping base version",
                    overlay_entity.name
                ));
            }
            if overlay_entity.actor != base_entity.actor {
                report.warnings.push(format!(
                    "entity `{}`: actor flag differs, keeping base version",
                    overlay_entity.name
                ));
            }
            if overlay_entity.topology != base_entity.topology {
                report.warnings.push(format!(
                    "entity `{}`: topology differs, keeping base version",
                    overlay_entity.name
                ));
            }

            // Merge fields: append new fields from overlay that don't exist in base.
            let existing_fields: BTreeSet<String> =
                base_entity.fields.iter().map(|f| f.name.clone()).collect();
            let mut added = 0usize;
            for field in &overlay_entity.fields {
                if existing_fields.contains(&field.name) {
                    report.warnings.push(format!(
                        "entity `{}`: field `{}` exists in both, keeping base version",
                        overlay_entity.name, field.name
                    ));
                } else {
                    base_entity.fields.push(field.clone());
                    added += 1;
                }
            }
            // Merge constraints from overlay.
            let existing_constraints: BTreeSet<String> = base_entity
                .constraints
                .iter()
                .map(|c| format!("{:?}", c))
                .collect();
            for constraint in &overlay_entity.constraints {
                let key = format!("{:?}", constraint);
                if !existing_constraints.contains(&key) {
                    base_entity.constraints.push(constraint.clone());
                }
            }
            if added > 0 {
                report.entities_merged.push(overlay_entity.name.clone());
            }
        } else {
            // New entity — add to result.
            result.entities.push(overlay_entity.clone());
            report.entities_added.push(overlay_entity.name.clone());
        }
    }

    // Merge relationships (dedup by name).
    let existing_rels: BTreeSet<String> = result
        .relationships
        .iter()
        .map(|r| r.name.clone())
        .collect();
    for rel in &overlay.relationships {
        if existing_rels.contains(&rel.name) {
            // Same name exists — check if semantics differ.
            let base_rel = result.relationships.iter().find(|r| r.name == rel.name);
            if let Some(br) = base_rel {
                if br.from != rel.from || br.to != rel.to || br.foreign_key != rel.foreign_key {
                    report.warnings.push(format!(
                        "relationship `{}`: exists in both with different endpoints, keeping base version",
                        rel.name
                    ));
                }
            }
        } else {
            result.relationships.push(rel.clone());
            report.relationships_added += 1;
        }
    }

    // Merge noise profiles (dedup by name).
    let existing_noise: BTreeSet<String> =
        result.noise_profiles.iter().map(|n| n.name.clone()).collect();
    for np in &overlay.noise_profiles {
        if !existing_noise.contains(&np.name) {
            result.noise_profiles.push(np.clone());
            report.noise_profiles_added += 1;
        }
    }

    // Merge correlations (dedup by field pair).
    let existing_corr: BTreeSet<String> = result
        .correlations
        .iter()
        .map(|c| format!("{:?}", c))
        .collect();
    for corr in &overlay.correlations {
        let key = format!("{:?}", corr);
        if !existing_corr.contains(&key) {
            result.correlations.push(corr.clone());
            report.correlations_added += 1;
        }
    }

    // Merge personas (dedup by name).
    let existing_personas: BTreeSet<String> =
        result.personas.iter().map(|p| p.name.clone()).collect();
    for persona in &overlay.personas {
        if !existing_personas.contains(&persona.name) {
            result.personas.push(persona.clone());
            report.personas_added += 1;
        }
    }

    // Merge actor_relationships (dedup by debug repr).
    let existing_ar: BTreeSet<String> = result
        .actor_relationships
        .iter()
        .map(|a| format!("{:?}", a))
        .collect();
    for ar in &overlay.actor_relationships {
        let key = format!("{:?}", ar);
        if !existing_ar.contains(&key) {
            result.actor_relationships.push(ar.clone());
        }
    }

    // Merge custom_types (dedup by name).
    let existing_types: BTreeSet<String> =
        result.custom_types.iter().map(|t| t.name.clone()).collect();
    for ct in &overlay.custom_types {
        if !existing_types.contains(&ct.name) {
            result.custom_types.push(ct.clone());
        }
    }

    // Merge mixins (dedup by name).
    let existing_mixins: BTreeSet<String> =
        result.mixins.iter().map(|m| m.name.clone()).collect();
    for mixin in &overlay.mixins {
        if !existing_mixins.contains(&mixin.name) {
            result.mixins.push(mixin.clone());
        }
    }

    // Merge params (overlay wins on conflict).
    for (k, v) in &overlay.params {
        if result.params.contains_key(k) {
            report.warnings.push(format!(
                "param `{}` exists in both, overlay value used",
                k
            ));
        }
        result.params.insert(k.clone(), v.clone());
    }

    // Merge companion_files (dedup).
    let existing_companions: BTreeSet<String> =
        result.companion_files.iter().cloned().collect();
    for cf in &overlay.companion_files {
        if !existing_companions.contains(cf) {
            result.companion_files.push(cf.clone());
        }
    }

    (result, report)
}

/// Run `knit blueprint merge`.
pub fn run_merge(base_path: &str, overlay_path: &str, output: Option<&str>, json: bool) -> Result<()> {
    let base = load_blueprint(base_path)
        .with_context(|| format!("failed to load base blueprint `{}`", base_path))?;
    let overlay = load_blueprint(overlay_path)
        .with_context(|| format!("failed to load overlay blueprint `{}`", overlay_path))?;

    let (merged, report) = merge_models(&base, &overlay);

    // Serialize merged model in canonical blueprint format.
    let toml_output = serialize_model_to_toml(&merged)
        .context("failed to serialize merged blueprint")?;

    // Write or print.
    if let Some(out_path) = output {
        std::fs::write(out_path, &toml_output)
            .with_context(|| format!("failed to write `{}`", out_path))?;
        if !json {
            println!("{} {}", "Wrote:".green(), out_path);
        }
    } else if !json {
        println!("{}", toml_output);
    }

    // Report.
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if output.is_some() {
        println!();
        println!("{}", "Merge Report".bold().underline());
        if !report.entities_added.is_empty() {
            println!(
                "  {} {} ({})",
                "Entities added:".dimmed(),
                report.entities_added.len(),
                report.entities_added.join(", ")
            );
        }
        if !report.entities_merged.is_empty() {
            println!(
                "  {} {} ({})",
                "Entities merged:".dimmed(),
                report.entities_merged.len(),
                report.entities_merged.join(", ")
            );
        }
        if report.relationships_added > 0 {
            println!(
                "  {} {}",
                "Relationships added:".dimmed(),
                report.relationships_added
            );
        }
        if report.noise_profiles_added > 0 {
            println!(
                "  {} {}",
                "Noise profiles added:".dimmed(),
                report.noise_profiles_added
            );
        }
        if report.correlations_added > 0 {
            println!(
                "  {} {}",
                "Correlations added:".dimmed(),
                report.correlations_added
            );
        }
        if report.personas_added > 0 {
            println!(
                "  {} {}",
                "Personas added:".dimmed(),
                report.personas_added
            );
        }
        for w in &report.warnings {
            println!("  {} {}", "⚠".yellow(), w);
        }
    }

    Ok(())
}

// ── Blueprint Graph ─────────────────────────────────────────────────

/// A node in the entity dependency graph.
#[derive(Debug, serde::Serialize)]
struct GraphNode {
    name: String,
    fields: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    actor: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    count: Option<u64>,
}

/// An edge in the entity dependency graph.
#[derive(Debug, serde::Serialize)]
struct GraphEdge {
    from: String,
    to: String,
    kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    foreign_key: Option<String>,
    name: String,
}

/// Full graph output.
#[derive(Debug, serde::Serialize)]
struct GraphOutput {
    nodes: Vec<GraphNode>,
    edges: Vec<GraphEdge>,
}

/// Build the graph representation from a DataModel.
pub fn build_graph(model: &DataModel) -> GraphOutput {
    let nodes: Vec<GraphNode> = model
        .entities
        .iter()
        .map(|e| {
            let count = match &e.count {
                crate::core::CountSpec::Fixed(n) => Some(*n),
                crate::core::CountSpec::Range { min, max } => Some((min + max) / 2),
                _ => None,
            };
            GraphNode {
                name: e.name.clone(),
                fields: e.fields.len(),
                actor: e.actor,
                count,
            }
        })
        .collect();

    let mut edges: Vec<GraphEdge> = model
        .relationships
        .iter()
        .map(|r| GraphEdge {
            from: r.from.clone(),
            to: r.to.clone(),
            kind: r.kind.to_string(),
            foreign_key: r.foreign_key.clone(),
            name: r.name.clone(),
        })
        .collect();

    // Include actor_relationships as edges with "actor_*" kind prefix.
    for ar in &model.actor_relationships {
        edges.push(GraphEdge {
            from: ar.from_entity.clone(),
            to: ar.to_entity.clone(),
            kind: format!("actor_{:?}", ar.graph_type).to_lowercase(),
            foreign_key: None,
            name: ar.name.clone(),
        });
    }

    GraphOutput { nodes, edges }
}

/// Escape a string for use in DOT quoted strings and record labels.
fn dot_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('{', "\\{")
        .replace('}', "\\}")
        .replace('|', "\\|")
        .replace('<', "\\<")
        .replace('>', "\\>")
}

/// Render a graph as DOT (GraphViz) format.
fn render_dot(model: &DataModel, graph: &GraphOutput) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "digraph \"{}\" {{\n  rankdir=LR;\n  node [shape=record, style=filled, fillcolor=\"#f0f0f0\"];\n\n",
        dot_escape(&model.name)
    ));

    // Nodes.
    for node in &graph.nodes {
        let escaped = dot_escape(&node.name);
        let mut label = escaped.clone();
        label.push_str(&format!(" | {} fields", node.fields));
        if let Some(count) = node.count {
            label.push_str(&format!(" | ~{} rows", count));
        }
        let color = if node.actor {
            "\"#d4edda\""
        } else {
            "\"#f0f0f0\""
        };
        out.push_str(&format!(
            "  \"{}\" [label=\"{{{}}}\", fillcolor={}];\n",
            escaped, label, color
        ));
    }

    out.push('\n');

    // Edges.
    for edge in &graph.edges {
        let label = if edge.from == edge.to {
            format!("{} (self)", edge.kind)
        } else {
            edge.kind.clone()
        };
        let fk_label = edge
            .foreign_key
            .as_deref()
            .map(|fk| format!("\\n({})", dot_escape(fk)))
            .unwrap_or_default();
        let style = if edge.from == edge.to {
            ", style=dashed"
        } else {
            ""
        };
        out.push_str(&format!(
            "  \"{}\" -> \"{}\" [label=\"{}{}\"{}];\n",
            dot_escape(&edge.from), dot_escape(&edge.to), label, fk_label, style
        ));
    }

    out.push_str("}\n");
    out
}

/// Render a graph as Mermaid ERD format.
/// Output can be embedded in GitHub Markdown as ```mermaid ... ``` blocks.
fn render_mermaid(model: &DataModel, _graph: &GraphOutput) -> String {
    let mut out = String::new();
    out.push_str("erDiagram\n");

    // Pre-compute effective FK columns per entity for FK annotation.
    let mut effective_fks: BTreeSet<(String, String)> = BTreeSet::new();
    for rel in &model.relationships {
        let fk_col = rel
            .foreign_key
            .clone()
            .unwrap_or_else(|| format!("{}_id", rel.to));
        effective_fks.insert((rel.from.clone(), fk_col));
    }

    // Entity blocks with fields.
    for entity in &model.entities {
        out.push_str(&format!(
            "    {} {{\n",
            mermaid_ident(&entity.name)
        ));
        for field in &entity.fields {
            let type_str = mermaid_type(&field.data_type);
            let is_pk = field.primary_key == Some(true);
            let is_fk = effective_fks.contains(&(entity.name.clone(), field.name.clone()));
            let key = match (is_pk, is_fk) {
                (true, true) => " PK,FK",
                (true, false) => " PK",
                (false, true) => " FK",
                (false, false) => "",
            };
            out.push_str(&format!(
                "        {} {}{}\n",
                type_str,
                mermaid_ident(&field.name),
                key,
            ));
        }
        out.push_str("    }\n");
    }

    out.push('\n');

    // Relationships as Mermaid ERD links.
    // `from` = child (FK side), `to` = parent (PK side).
    for rel in &model.relationships {
        let child = mermaid_ident(&rel.from);
        let parent = mermaid_ident(&rel.to);
        let fk_label = rel
            .foreign_key
            .clone()
            .unwrap_or_else(|| format!("{}_id", rel.to));
        // Mermaid reads: left <cardinality> right.
        // For OneToMany: parent has one, child has many → parent ||--o{ child.
        let cardinality = mermaid_cardinality_directed(&rel.kind);
        out.push_str(&format!(
            "    {} {} {} : \"{}\"\n",
            parent, cardinality, child, fk_label
        ));
    }

    // Actor relationships.
    for ar in &model.actor_relationships {
        let from = mermaid_ident(&ar.from_entity);
        let to = mermaid_ident(&ar.to_entity);
        out.push_str(&format!(
            "    {} ||--o{{ {} : \"{}\"\n",
            from, to, ar.name
        ));
    }

    out
}

/// Map DataType to a Mermaid-friendly type string.
fn mermaid_type(dt: &crate::core::DataType) -> &'static str {
    use crate::core::DataType;
    match dt {
        DataType::Bool => "bool",
        DataType::Int | DataType::Int32 => "int",
        DataType::Float => "float",
        DataType::String => "string",
        DataType::Uuid => "uuid",
        DataType::Date => "date",
        DataType::Time => "time",
        DataType::Datetime | DataType::DatetimeUs | DataType::Datetimetz => "datetime",
        DataType::Duration => "duration",
        DataType::Bytes => "bytes",
        DataType::Array => "array",
        DataType::Map | DataType::Object => "json",
        DataType::Custom(_) => "custom",
    }
}

/// Map RelationshipKind to Mermaid ERD cardinality notation.
/// Returns notation for parent → child direction (parent on left).
fn mermaid_cardinality_directed(kind: &crate::core::RelationshipKind) -> &'static str {
    use crate::core::RelationshipKind;
    match kind {
        RelationshipKind::OneToOne => "||--||",
        RelationshipKind::OneToMany => "||--o{",
        // ManyToOne: from has many → to; but we render parent(to) left, child(from) right,
        // so parent has one, child has many (same visual as OneToMany).
        RelationshipKind::ManyToOne => "||--o{",
        RelationshipKind::ManyToMany => "}o--o{",
    }
}

/// Format a name as a valid Mermaid identifier.
/// If the name is purely alphanumeric/underscore, use as-is.
/// Otherwise, wrap in double quotes to preserve the original name.
fn mermaid_ident(name: &str) -> String {
    if name
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_')
        && !name.is_empty()
    {
        name.to_string()
    } else {
        // Mermaid supports quoted identifiers
        format!("\"{}\"", name.replace('"', "'"))
    }
}

/// Run `knit blueprint graph`.
pub fn run_graph(path: &str, format: &str) -> Result<()> {
    let model = load_blueprint(path)
        .with_context(|| format!("failed to load blueprint `{}`", path))?;
    let graph = build_graph(&model);

    match format {
        "dot" | "graphviz" => {
            print!("{}", render_dot(&model, &graph));
        }
        "mermaid" => {
            print!("{}", render_mermaid(&model, &graph));
        }
        "json" => {
            println!("{}", serde_json::to_string_pretty(&graph)?);
        }
        other => {
            anyhow::bail!("unsupported graph format `{}` (use `dot`, `mermaid`, or `json`)", other);
        }
    }

    Ok(())
}

// ── Blueprint Lint ──────────────────────────────────────────────────

/// Severity level for lint findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LintSeverity {
    Warning,
    Info,
}

/// A single lint finding.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LintFinding {
    pub severity: LintSeverity,
    pub entity: Option<String>,
    pub field: Option<String>,
    pub message: String,
}

/// Run all lint checks on a DataModel.
pub fn lint_model(model: &DataModel) -> Vec<LintFinding> {
    let mut findings = Vec::new();
    let entity_names: BTreeSet<&str> = model.entities.iter().map(|e| e.name.as_str()).collect();

    // Collect all entities referenced by relationships (both sides).
    let mut referenced: BTreeSet<&str> = BTreeSet::new();
    for rel in &model.relationships {
        referenced.insert(&rel.from);
        referenced.insert(&rel.to);
    }

    for entity in &model.entities {
        // 1. Entity with no fields.
        if entity.fields.is_empty() {
            findings.push(LintFinding {
                severity: LintSeverity::Warning,
                entity: Some(entity.name.clone()),
                field: None,
                message: "entity has no fields".into(),
            });
        }

        // 2. Missing description.
        if entity.description.is_none() {
            findings.push(LintFinding {
                severity: LintSeverity::Info,
                entity: Some(entity.name.clone()),
                field: None,
                message: "entity has no description".into(),
            });
        }

        // 3. Orphan entity (not referenced by any relationship and not an actor).
        if model.entities.len() > 1
            && !referenced.contains(entity.name.as_str())
            && !entity.actor
        {
            findings.push(LintFinding {
                severity: LintSeverity::Info,
                entity: Some(entity.name.clone()),
                field: None,
                message: "entity is not referenced by any relationship".into(),
            });
        }

        // 4. Duplicate field names.
        let mut seen_fields: BTreeSet<&str> = BTreeSet::new();
        for field in &entity.fields {
            if !seen_fields.insert(&field.name) {
                findings.push(LintFinding {
                    severity: LintSeverity::Warning,
                    entity: Some(entity.name.clone()),
                    field: Some(field.name.clone()),
                    message: "duplicate field name".into(),
                });
            }
        }

        // 5. Duplicate field names check already done above.
    }

    // 6. Dangling relationship references.
    for rel in &model.relationships {
        if !entity_names.contains(rel.from.as_str()) {
            findings.push(LintFinding {
                severity: LintSeverity::Warning,
                entity: None,
                field: None,
                message: format!(
                    "relationship `{}`: `from` entity `{}` does not exist",
                    rel.name, rel.from
                ),
            });
        }
        if !entity_names.contains(rel.to.as_str()) {
            findings.push(LintFinding {
                severity: LintSeverity::Warning,
                entity: None,
                field: None,
                message: format!(
                    "relationship `{}`: `to` entity `{}` does not exist",
                    rel.name, rel.to
                ),
            });
        }
    }

    // 7. Self-referential relationships without acyclic flag.
    for rel in &model.relationships {
        if rel.from == rel.to && rel.acyclic != Some(true) {
            findings.push(LintFinding {
                severity: LintSeverity::Info,
                entity: Some(rel.from.clone()),
                field: None,
                message: format!(
                    "self-referential relationship `{}` has no `acyclic = true` flag",
                    rel.name
                ),
            });
        }
    }

    // 8. Noise profiles targeting non-existent entities.
    for np in &model.noise_profiles {
        if !np.entity.is_empty() && !entity_names.contains(np.entity.as_str()) {
            findings.push(LintFinding {
                severity: LintSeverity::Warning,
                entity: None,
                field: None,
                message: format!(
                    "noise profile `{}` targets non-existent entity `{}`",
                    np.name, np.entity
                ),
            });
        }
    }

    findings
}

/// Run `knit blueprint lint`.
pub fn run_lint(path: &str, json: bool) -> Result<()> {
    let model =
        load_blueprint(path).with_context(|| format!("failed to load blueprint `{}`", path))?;
    let findings = lint_model(&model);

    if json {
        println!("{}", serde_json::to_string_pretty(&findings)?);
        return Ok(());
    }

    if findings.is_empty() {
        println!("{}", "No issues found.".green());
        return Ok(());
    }

    let warnings = findings
        .iter()
        .filter(|f| f.severity == LintSeverity::Warning)
        .count();
    let infos = findings
        .iter()
        .filter(|f| f.severity == LintSeverity::Info)
        .count();

    for finding in &findings {
        let icon = match finding.severity {
            LintSeverity::Warning => "⚠".yellow(),
            LintSeverity::Info => "ℹ".cyan(),
        };
        let location = match (&finding.entity, &finding.field) {
            (Some(e), Some(f)) => format!("{}.{}", e, f),
            (Some(e), None) => e.clone(),
            _ => "model".into(),
        };
        println!("  {} {} — {}", icon, location.bold(), finding.message);
    }

    println!();
    println!(
        "  {} warning(s), {} info(s)",
        warnings.to_string().yellow(),
        infos.to_string().cyan()
    );

    Ok(())
}

// ---------------------------------------------------------------------------
// Subset
// ---------------------------------------------------------------------------

/// Compute the subset of a model containing only the specified entities and
/// their transitive dependencies (parent entities reachable via relationships).
pub fn subset_model(model: &DataModel, roots: &[String], include_deps: bool) -> DataModel {
    let entity_names: BTreeSet<String> = model.entities.iter().map(|e| e.name.clone()).collect();

    // Start with requested roots (filter to those that actually exist).
    let mut selected: BTreeSet<String> = roots
        .iter()
        .filter(|r| entity_names.contains(r.as_str()))
        .cloned()
        .collect();

    // Transitively include parent entities reachable via relationships.
    if include_deps {
        let mut frontier: Vec<String> = selected.iter().cloned().collect();
        while let Some(name) = frontier.pop() {
            for rel in &model.relationships {
                // If this entity is the child (`from`), the parent (`to`) is a dependency.
                if rel.from == name && !selected.contains(&rel.to) {
                    selected.insert(rel.to.clone());
                    frontier.push(rel.to.clone());
                }
            }
        }
    }

    // Filter entities.
    let entities: Vec<Entity> = model
        .entities
        .iter()
        .filter(|e| selected.contains(&e.name))
        .cloned()
        .collect();

    // Filter relationships: keep only those where both sides are in the subset.
    let relationships = model
        .relationships
        .iter()
        .filter(|r| selected.contains(&r.from) && selected.contains(&r.to))
        .cloned()
        .collect();

    // Filter correlations: keep only those referencing entities in the subset.
    let correlations = model
        .correlations
        .iter()
        .filter(|c| selected.contains(&c.entity))
        .cloned()
        .collect();

    // Filter noise profiles: keep those whose entity is in the subset.
    let noise_profiles = model
        .noise_profiles
        .iter()
        .filter(|np| !np.entity.is_empty() && selected.contains(&np.entity))
        .cloned()
        .collect();

    // Filter actor relationships: keep those where both actors are in the subset.
    let actor_relationships = model
        .actor_relationships
        .iter()
        .filter(|ar| selected.contains(&ar.from_entity) && selected.contains(&ar.to_entity))
        .cloned()
        .collect();

    DataModel {
        name: model.name.clone(),
        description: model.description.clone(),
        seed: model.seed,
        locale: model.locale.clone(),
        timezone: model.timezone.clone(),
        entities,
        relationships,
        noise_profiles,
        correlations,
        params: model.params.clone(),
        blueprint_version: model.blueprint_version.clone(),
        personas: model.personas.clone(),
        actor_relationships,
        custom_types: model.custom_types.clone(),
        mixins: model.mixins.clone(),
        companion_files: model.companion_files.clone(),
    }
}

/// Run the `blueprint subset` command.
pub fn run_subset(
    path: &str,
    entities: &[String],
    include_deps: bool,
    output: Option<&str>,
    json: bool,
) -> Result<()> {
    let model =
        load_blueprint(path).with_context(|| format!("failed to load blueprint `{}`", path))?;

    if entities.is_empty() {
        anyhow::bail!("at least one --entity must be specified");
    }

    // Warn about requested entities not found in the model.
    let existing: BTreeSet<&str> = model.entities.iter().map(|e| e.name.as_str()).collect();
    for name in entities {
        if !existing.contains(name.as_str()) {
            eprintln!(
                "{} entity `{}` not found in blueprint (available: {})",
                "warning:".yellow(),
                name,
                existing.iter().copied().collect::<Vec<_>>().join(", ")
            );
        }
    }

    let subset = subset_model(&model, entities, include_deps);

    if subset.entities.is_empty() {
        anyhow::bail!(
            "no matching entities found; available: {}",
            existing.iter().copied().collect::<Vec<_>>().join(", ")
        );
    }

    let output_str = if json {
        serde_json::to_string_pretty(&subset)?
    } else {
        serialize_model_to_toml(&subset)?
    };

    if let Some(out_path) = output {
        std::fs::write(out_path, &output_str)
            .with_context(|| format!("failed to write `{}`", out_path))?;
        eprintln!(
            "{} wrote subset ({} entities) to {}",
            "✓".green(),
            subset.entities.len(),
            out_path
        );
    } else {
        println!("{}", output_str);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Rename
// ---------------------------------------------------------------------------

/// Apply entity and field renames to a model, updating all cross-references.
///
/// `entity_renames` maps old entity name → new entity name.
/// `field_renames` maps (entity_name, old_field_name) → new_field_name.
/// Returns the modified model and a count of references updated.
pub fn rename_in_model(
    model: &DataModel,
    entity_renames: &BTreeMap<String, String>,
    field_renames: &BTreeMap<(String, String), String>,
) -> (DataModel, usize) {
    let mut m = model.clone();
    let mut updates = 0usize;

    // Helper: rename an entity name string if it matches.
    let rename_entity = |name: &mut String, count: &mut usize| {
        if let Some(new) = entity_renames.get(name.as_str()) {
            *name = new.clone();
            *count += 1;
        }
    };

    // Helper: rename a field name if there's a matching rename for this entity.
    let rename_field =
        |orig_entity: &str, name: &mut String, count: &mut usize| {
            if let Some(new) = field_renames.get(&(orig_entity.to_string(), name.clone())) {
                *name = new.clone();
                *count += 1;
            }
        };

    // Collect original entity names before renaming.
    let orig_entity_names: Vec<String> = m.entities.iter().map(|e| e.name.clone()).collect();

    // Rename entity names and their fields.
    for (i, ent) in m.entities.iter_mut().enumerate() {
        rename_entity(&mut ent.name, &mut updates);
        let orig = &orig_entity_names[i];
        rename_fields_recursive(&mut ent.fields, orig, field_renames, &mut updates);
    }

    // Rename references in relationships.
    // Preserve implicit FK semantics: if renaming `to` and no explicit FK is set,
    // lock in the original implicit FK name.
    for rel in &mut m.relationships {
        if entity_renames.contains_key(&rel.to) && rel.foreign_key.is_none() {
            rel.foreign_key = Some(format!("{}_id", rel.to));
            updates += 1;
        }
        rename_entity(&mut rel.from, &mut updates);
        rename_entity(&mut rel.to, &mut updates);
    }

    // Rename references in correlations.
    for corr in &mut m.correlations {
        let orig_entity = corr.entity.clone();
        rename_entity(&mut corr.entity, &mut updates);
        for f in &mut corr.fields {
            rename_field(&orig_entity, f, &mut updates);
        }
        if let Some(ref mut dep) = corr.dependent {
            rename_field(&orig_entity, dep, &mut updates);
        }
        if let Some(ref mut given) = corr.given {
            rename_field(&orig_entity, given, &mut updates);
        }
    }

    // Rename references in noise profiles.
    for np in &mut m.noise_profiles {
        let orig_entity = np.entity.clone();
        rename_entity(&mut np.entity, &mut updates);
        for f in &mut np.fields {
            rename_field(&orig_entity, f, &mut updates);
        }
    }

    // Rename references in actor relationships.
    for ar in &mut m.actor_relationships {
        rename_entity(&mut ar.from_entity, &mut updates);
        rename_entity(&mut ar.to_entity, &mut updates);
    }

    // Rename entity/field references inside generators.
    for (i, ent) in m.entities.iter_mut().enumerate() {
        let orig = &orig_entity_names[i];
        for field in &mut ent.fields {
            rename_generator_refs(field, orig, entity_renames, field_renames, &mut updates);
        }
    }

    (m, updates)
}

/// Recursively rename fields in a field list.
fn rename_fields_recursive(
    fields: &mut [Field],
    entity_name: &str,
    field_renames: &BTreeMap<(String, String), String>,
    updates: &mut usize,
) {
    for field in fields.iter_mut() {
        if let Some(new) = field_renames.get(&(entity_name.to_string(), field.name.clone())) {
            field.name = new.clone();
            *updates += 1;
        }
        rename_fields_recursive(&mut field.fields, entity_name, field_renames, updates);
    }
}

/// Rename entity/field references inside generator specs.
fn rename_generator_refs(
    field: &mut Field,
    owner_entity: &str,
    entity_renames: &BTreeMap<String, String>,
    field_renames: &BTreeMap<(String, String), String>,
    updates: &mut usize,
) {
    if let Some(ref mut gen) = field.generator {
        rename_generator_spec(gen, owner_entity, entity_renames, field_renames, updates);
    }
    for child in &mut field.fields {
        rename_generator_refs(child, owner_entity, entity_renames, field_renames, updates);
    }
}

/// Rename references within a single GeneratorSpec.
fn rename_generator_spec(
    gen: &mut crate::core::GeneratorSpec,
    owner_entity: &str,
    entity_renames: &BTreeMap<String, String>,
    field_renames: &BTreeMap<(String, String), String>,
    updates: &mut usize,
) {
    use crate::core::GeneratorSpec;

    let rename_field =
        |entity: &str, name: &mut String, count: &mut usize| {
            if let Some(new) = field_renames.get(&(entity.to_string(), name.clone())) {
                *name = new.clone();
                *count += 1;
            }
        };

    match gen {
        GeneratorSpec::Lookup { entity, field } => {
            let orig_entity = entity.clone();
            if let Some(new) = entity_renames.get(entity.as_str()) {
                *entity = new.clone();
                *updates += 1;
            }
            rename_field(&orig_entity, field, updates);
        }
        GeneratorSpec::ActorRef { entity } => {
            if let Some(new) = entity_renames.get(entity.as_str()) {
                *entity = new.clone();
                *updates += 1;
            }
        }
        GeneratorSpec::ActorTemporal { temporal_after, .. } => {
            if let Some(ref mut ta) = temporal_after {
                let orig = ta.entity.clone();
                if let Some(new) = entity_renames.get(ta.entity.as_str()) {
                    ta.entity = new.clone();
                    *updates += 1;
                }
                rename_field(&orig, &mut ta.field, updates);
                rename_field(owner_entity, &mut ta.fk, updates);
            }
        }
        GeneratorSpec::Relative { anchor, .. } => {
            rename_field(owner_entity, anchor, updates);
        }
        GeneratorSpec::RelationshipRef { source_field, .. } => {
            if let Some(ref mut sf) = source_field {
                rename_field(owner_entity, sf, updates);
            }
        }
        GeneratorSpec::Unique { inner, .. } => {
            rename_generator_spec(inner, owner_entity, entity_renames, field_renames, updates);
        }
        GeneratorSpec::Composite { generators, .. } => {
            for sub in generators.values_mut() {
                rename_generator_spec(sub, owner_entity, entity_renames, field_renames, updates);
            }
        }
        _ => {}
    }
}

/// Parse a rename spec of the form `Old=New`.
fn parse_rename_spec(spec: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = spec.splitn(2, '=').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        anyhow::bail!("invalid rename spec `{}`: expected `Old=New`", spec);
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

/// Parse a field rename spec of the form `Entity.Old=New`.
fn parse_field_rename_spec(spec: &str) -> Result<(String, String, String)> {
    let parts: Vec<&str> = spec.splitn(2, '=').collect();
    if parts.len() != 2 || parts[0].is_empty() || parts[1].is_empty() {
        anyhow::bail!(
            "invalid field rename spec `{}`: expected `Entity.OldField=NewField`",
            spec
        );
    }
    let dot_parts: Vec<&str> = parts[0].splitn(2, '.').collect();
    if dot_parts.len() != 2 || dot_parts[0].is_empty() || dot_parts[1].is_empty() {
        anyhow::bail!(
            "invalid field rename spec `{}`: expected `Entity.OldField=NewField`",
            spec
        );
    }
    Ok((
        dot_parts[0].to_string(),
        dot_parts[1].to_string(),
        parts[1].to_string(),
    ))
}

/// Run the `blueprint rename` command.
pub fn run_rename(
    path: &str,
    entity_specs: &[String],
    field_specs: &[String],
    output: Option<&str>,
    json: bool,
) -> Result<()> {
    let model =
        load_blueprint(path).with_context(|| format!("failed to load blueprint `{}`", path))?;

    if entity_specs.is_empty() && field_specs.is_empty() {
        anyhow::bail!("at least one --entity or --field rename must be specified");
    }

    let mut entity_renames = BTreeMap::new();
    for spec in entity_specs {
        let (old, new) = parse_rename_spec(spec)?;
        // Verify the old entity exists.
        if !model.entities.iter().any(|e| e.name == old) {
            anyhow::bail!(
                "entity `{}` not found in blueprint",
                old
            );
        }
        // Check for collision: new name must not conflict with an existing
        // entity that isn't itself being renamed away.
        let conflicts = model.entities.iter().any(|e| {
            e.name == new && !entity_specs.iter().any(|s| s.starts_with(&format!("{}=", e.name)))
        });
        if conflicts {
            anyhow::bail!(
                "cannot rename `{}` to `{}`: entity `{}` already exists",
                old, new, new
            );
        }
        entity_renames.insert(old, new);
    }

    let mut field_renames: BTreeMap<(String, String), String> = BTreeMap::new();
    for spec in field_specs {
        let (entity, old, new) = parse_field_rename_spec(spec)?;
        // Verify entity and field exist.
        let ent = model.entities.iter().find(|e| e.name == entity);
        match ent {
            None => anyhow::bail!("entity `{}` not found in blueprint", entity),
            Some(e) => {
                if !e.fields.iter().any(|f| f.name == old) {
                    anyhow::bail!("field `{}` not found in entity `{}`", old, entity);
                }
                // Check for field name collision.
                if e.fields.iter().any(|f| f.name == new) {
                    anyhow::bail!(
                        "cannot rename `{}.{}` to `{}`: field `{}` already exists in `{}`",
                        entity, old, new, new, entity
                    );
                }
            }
        }
        field_renames.insert((entity, old), new);
    }

    let (renamed, update_count) = rename_in_model(&model, &entity_renames, &field_renames);

    let output_str = if json {
        serde_json::to_string_pretty(&renamed)?
    } else {
        serialize_model_to_toml(&renamed)?
    };

    if let Some(out_path) = output {
        std::fs::write(out_path, &output_str)
            .with_context(|| format!("failed to write `{}`", out_path))?;
        eprintln!(
            "{} renamed ({} references updated), wrote to {}",
            "✓".green(),
            update_count,
            out_path
        );
    } else {
        println!("{}", output_str);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Export SQL
// ---------------------------------------------------------------------------

/// SQL dialect for DDL generation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SqlDialect {
    /// Standard SQL / PostgreSQL.
    Postgres,
    /// MySQL / MariaDB.
    Mysql,
    /// SQLite.
    Sqlite,
}

impl SqlDialect {
    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "postgres" | "postgresql" | "pg" => Ok(SqlDialect::Postgres),
            "mysql" | "mariadb" => Ok(SqlDialect::Mysql),
            "sqlite" => Ok(SqlDialect::Sqlite),
            _ => anyhow::bail!(
                "unknown SQL dialect `{}`; supported: postgres, mysql, sqlite",
                s
            ),
        }
    }
}

/// Map a knit DataType to a SQL column type for the given dialect.
fn sql_type(dt: &crate::core::DataType, dialect: SqlDialect) -> &'static str {
    use crate::core::DataType;
    match (dt, dialect) {
        (DataType::Bool, _) => "BOOLEAN",
        (DataType::Int, SqlDialect::Mysql) => "BIGINT",
        (DataType::Int, _) => "BIGINT",
        (DataType::Int32, _) => "INTEGER",
        (DataType::Float, SqlDialect::Mysql) => "DOUBLE",
        (DataType::Float, _) => "DOUBLE PRECISION",
        (DataType::String, SqlDialect::Mysql) => "VARCHAR(255)",
        (DataType::String, SqlDialect::Sqlite) => "TEXT",
        (DataType::String, SqlDialect::Postgres) => "TEXT",
        (DataType::Uuid, SqlDialect::Postgres) => "UUID",
        (DataType::Uuid, _) => "CHAR(36)",
        (DataType::Date, _) => "DATE",
        (DataType::Time, _) => "TIME",
        (DataType::Datetime, _) | (DataType::DatetimeUs, _) => "TIMESTAMP",
        (DataType::Datetimetz, SqlDialect::Postgres) => "TIMESTAMPTZ",
        (DataType::Datetimetz, _) => "TIMESTAMP",
        (DataType::Duration, SqlDialect::Postgres) => "INTERVAL",
        (DataType::Duration, _) => "VARCHAR(64)",
        (DataType::Bytes, SqlDialect::Postgres) => "BYTEA",
        (DataType::Bytes, _) => "BLOB",
        (DataType::Array, SqlDialect::Postgres) => "JSONB",
        (DataType::Array, _) => "JSON",
        (DataType::Map, SqlDialect::Postgres) => "JSONB",
        (DataType::Map, _) => "JSON",
        (DataType::Object, SqlDialect::Postgres) => "JSONB",
        (DataType::Object, _) => "JSON",
        (DataType::Custom(_), _) => "TEXT",
    }
}

/// Generate SQL DDL (CREATE TABLE statements) from a data model.
pub fn export_sql(model: &DataModel, dialect: SqlDialect, include_fks: bool) -> String {
    let mut out = String::new();

    // Header comment.
    out.push_str(&format!("-- Generated by knit from model: {}\n", model.name));
    out.push_str(&format!("-- Dialect: {:?}\n\n", dialect));

    // Build a map of entity → primary key field name for FK references.
    let pk_map: BTreeMap<&str, &str> = model
        .entities
        .iter()
        .filter_map(|e| {
            e.fields
                .iter()
                .find(|f| f.primary_key == Some(true))
                .map(|f| (e.name.as_str(), f.name.as_str()))
        })
        .collect();

    // Build FK info from relationships.
    struct FkInfo {
        fk_col: String,
        to_entity: String,
        to_pk: String,
    }

    let mut fk_map: BTreeMap<String, Vec<FkInfo>> = BTreeMap::new();
    if include_fks {
        for rel in &model.relationships {
            let fk_col = rel
                .foreign_key
                .clone()
                .unwrap_or_else(|| format!("{}_id", rel.to));
            let to_pk = pk_map
                .get(rel.to.as_str())
                .map(|s| s.to_string())
                .unwrap_or_else(|| "id".to_string());
            fk_map
                .entry(rel.from.clone())
                .or_default()
                .push(FkInfo {
                    fk_col,
                    to_entity: rel.to.clone(),
                    to_pk,
                });
        }
    }

    // Collect existing field names per entity to detect implicit FK columns.
    let entity_field_names: BTreeMap<&str, BTreeSet<&str>> = model
        .entities
        .iter()
        .map(|e| {
            let names = e.fields.iter().map(|f| f.name.as_str()).collect();
            (e.name.as_str(), names)
        })
        .collect();

    // Phase 1: CREATE TABLE statements (no inline FK constraints).
    for entity in &model.entities {
        out.push_str(&format!(
            "CREATE TABLE {} (\n",
            sql_quote_ident(&entity.name, dialect)
        ));

        let mut columns: Vec<String> = Vec::new();

        for field in &entity.fields {
            // Skip nested object fields — flatten them.
            if field.data_type == crate::core::DataType::Object && !field.fields.is_empty() {
                flatten_fields(&field.fields, &field.name, dialect, &mut columns);
                continue;
            }

            let col_type = sql_type(&field.data_type, dialect);
            let mut col_def = format!(
                "    {} {}",
                sql_quote_ident(&field.name, dialect),
                col_type
            );
            if field.primary_key == Some(true) {
                col_def.push_str(" PRIMARY KEY");
                // SQLite needs explicit NOT NULL on PKs (unlike PG/MySQL).
                if dialect == SqlDialect::Sqlite {
                    col_def.push_str(" NOT NULL");
                }
            }
            let is_nullable = !matches!(field.nullable, crate::core::NullSpec::Never);
            if !is_nullable && field.primary_key != Some(true) {
                col_def.push_str(" NOT NULL");
            }
            columns.push(col_def);
        }

        // Synthesize implicit FK columns not present in entity.fields.
        if let Some(fks) = fk_map.get(&entity.name) {
            let existing = entity_field_names.get(entity.name.as_str());
            for fk in fks {
                let has_col = existing.map_or(false, |s| s.contains(fk.fk_col.as_str()));
                if !has_col {
                    // Infer type from referenced PK — look up the target entity.
                    let ref_type = model
                        .entities
                        .iter()
                        .find(|e| e.name == fk.to_entity)
                        .and_then(|e| e.fields.iter().find(|f| f.name == fk.to_pk))
                        .map(|f| sql_type(&f.data_type, dialect))
                        .unwrap_or("BIGINT");
                    columns.push(format!(
                        "    {} {}",
                        sql_quote_ident(&fk.fk_col, dialect),
                        ref_type
                    ));
                }
            }
        }

        out.push_str(&columns.join(",\n"));
        out.push_str("\n);\n\n");
    }

    // Phase 2: ALTER TABLE ... ADD FOREIGN KEY (avoids ordering issues).
    if include_fks {
        let mut has_fks = false;
        for entity in &model.entities {
            if let Some(fks) = fk_map.get(&entity.name) {
                for fk in fks {
                    if !has_fks {
                        out.push_str("-- Foreign key constraints\n");
                        has_fks = true;
                    }
                    out.push_str(&format!(
                        "ALTER TABLE {} ADD FOREIGN KEY ({}) REFERENCES {} ({});\n",
                        sql_quote_ident(&entity.name, dialect),
                        sql_quote_ident(&fk.fk_col, dialect),
                        sql_quote_ident(&fk.to_entity, dialect),
                        sql_quote_ident(&fk.to_pk, dialect),
                    ));
                }
            }
        }
        if has_fks {
            out.push('\n');
        }
    }

    out
}

/// Flatten nested fields into SQL columns with dotted prefix.
fn flatten_fields(
    fields: &[Field],
    prefix: &str,
    dialect: SqlDialect,
    columns: &mut Vec<String>,
) {
    for field in fields {
        let col_name = format!("{}_{}", prefix, field.name);
        if field.data_type == crate::core::DataType::Object && !field.fields.is_empty() {
            flatten_fields(&field.fields, &col_name, dialect, columns);
        } else {
            let col_type = sql_type(&field.data_type, dialect);
            let mut col_def = format!("    {} {}", sql_quote_ident(&col_name, dialect), col_type);
            let is_nullable = !matches!(field.nullable, crate::core::NullSpec::Never);
            if !is_nullable {
                col_def.push_str(" NOT NULL");
            }
            columns.push(col_def);
        }
    }
}

/// Quote a SQL identifier based on dialect, escaping embedded quote characters.
fn sql_quote_ident(name: &str, dialect: SqlDialect) -> String {
    match dialect {
        SqlDialect::Mysql => format!("`{}`", name.replace('`', "``")),
        _ => format!("\"{}\"", name.replace('"', "\"\"")),
    }
}

/// Run the `blueprint export` command.
pub fn run_export(
    path: &str,
    format: &str,
    dialect: &str,
    include_fks: bool,
    output: Option<&str>,
) -> Result<()> {
    let model =
        load_blueprint(path).with_context(|| format!("failed to load blueprint `{}`", path))?;

    let output_str = match format.to_lowercase().as_str() {
        "sql" | "ddl" => {
            let d = SqlDialect::from_str(dialect)?;
            export_sql(&model, d, include_fks)
        }
        _ => anyhow::bail!(
            "unsupported export format `{}`; supported: sql",
            format
        ),
    };

    if let Some(out_path) = output {
        std::fs::write(out_path, &output_str)
            .with_context(|| format!("failed to write `{}`", out_path))?;
        eprintln!(
            "{} exported {} entities to {}",
            "✓".green(),
            model.entities.len(),
            out_path
        );
    } else {
        println!("{}", output_str);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Scaffold
// ---------------------------------------------------------------------------

/// Parse an entity spec: `Name:field1:type1,field2:type2,...` or `Name:count:field1:type1,...`.
///
/// Returns `(entity_name, count, fields)`.
fn parse_entity_spec(spec: &str) -> Result<(String, u64, Vec<Field>)> {
    let parts: Vec<&str> = spec.splitn(2, ':').collect();
    if parts.len() < 2 || parts[0].is_empty() {
        anyhow::bail!(
            "invalid entity spec `{}`: expected `Name:field1:type1,field2:type2,...`",
            spec
        );
    }

    let entity_name = parts[0].to_string();
    let field_str = parts[1];

    // Try parsing first token as a count (e.g. "1000:id:int,name:string").
    let (count, fields_part) = {
        let tokens: Vec<&str> = field_str.splitn(2, ':').collect();
        if tokens.len() == 2 {
            if let Ok(n) = tokens[0].parse::<u64>() {
                (n, tokens[1])
            } else {
                (1000, field_str)
            }
        } else {
            (1000, field_str)
        }
    };

    let mut fields = Vec::new();
    // Fields are comma-separated pairs: `name:type` or just `name` (defaults to string).
    for pair in fields_part.split(',') {
        let pair = pair.trim();
        if pair.is_empty() {
            continue;
        }
        let ft: Vec<&str> = pair.splitn(2, ':').collect();
        let field_name = ft[0].trim();
        let dt_str = if ft.len() > 1 { ft[1].trim() } else { "string" };
        let data_type = parse_scaffold_type(dt_str)?;

        let is_pk = field_name == "id"
            || field_name.ends_with("_id") && fields.is_empty();

        fields.push(Field {
            name: field_name.to_string(),
            description: None,
            data_type,
            generator: None,
            nullable: crate::core::NullSpec::Never,
            primary_key: if is_pk && field_name == "id" {
                Some(true)
            } else {
                None
            },
            precision: None,
            actor_column: false,
            fields: vec![],
            stats: None,
            traits: None,
        });
    }

    if fields.is_empty() {
        anyhow::bail!(
            "entity `{}` has no fields; expected `Name:field1:type1,field2:type2,...`",
            entity_name
        );
    }

    Ok((entity_name, count, fields))
}

/// Parse a simple type string into a DataType.
fn parse_scaffold_type(s: &str) -> Result<crate::core::DataType> {
    use crate::core::DataType;
    match s.to_lowercase().as_str() {
        "int" | "integer" | "bigint" | "i64" => Ok(DataType::Int),
        "int32" | "i32" => Ok(DataType::Int32),
        "float" | "double" | "f64" | "decimal" | "number" => Ok(DataType::Float),
        "string" | "str" | "text" | "varchar" => Ok(DataType::String),
        "bool" | "boolean" => Ok(DataType::Bool),
        "uuid" => Ok(DataType::Uuid),
        "date" => Ok(DataType::Date),
        "time" => Ok(DataType::Time),
        "datetime" | "timestamp" => Ok(DataType::Datetime),
        "datetimetz" | "timestamptz" => Ok(DataType::Datetimetz),
        "bytes" | "binary" | "blob" => Ok(DataType::Bytes),
        "array" | "list" => Ok(DataType::Array),
        "map" | "object" | "json" => Ok(DataType::Map),
        _ => anyhow::bail!(
            "unknown type `{}`; supported: int, float, string, bool, uuid, date, time, datetime, bytes, array, map",
            s
        ),
    }
}

/// Parse a relationship spec: `From.fk_col=To.pk_col` or `From=To` (implicit FK).
fn parse_rel_spec(spec: &str) -> Result<(String, Option<String>, String, Option<String>)> {
    let sides: Vec<&str> = spec.splitn(2, '=').collect();
    if sides.len() != 2 || sides[0].is_empty() || sides[1].is_empty() {
        anyhow::bail!(
            "invalid relationship spec `{}`: expected `From.fk=To.pk` or `From=To`",
            spec
        );
    }

    let (from_entity, from_field) = if let Some(dot) = sides[0].find('.') {
        (
            sides[0][..dot].to_string(),
            Some(sides[0][dot + 1..].to_string()),
        )
    } else {
        (sides[0].to_string(), None)
    };

    let (to_entity, to_field) = if let Some(dot) = sides[1].find('.') {
        (
            sides[1][..dot].to_string(),
            Some(sides[1][dot + 1..].to_string()),
        )
    } else {
        (sides[1].to_string(), None)
    };

    Ok((from_entity, from_field, to_entity, to_field))
}

/// Build a scaffold DataModel from parsed specs.
pub fn scaffold_model(
    name: &str,
    entity_specs: &[String],
    rel_specs: &[String],
) -> Result<DataModel> {
    let mut entities = Vec::new();

    for spec in entity_specs {
        let (ent_name, count, fields) = parse_entity_spec(spec)?;
        entities.push(Entity {
            name: ent_name,
            description: None,
            tags: Vec::new(),
            count: crate::core::CountSpec::Fixed(count),
            fields,
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
            output: None,
            stats: None,
            scaling: None,
        });
    }

    let mut relationships = Vec::new();
    for spec in rel_specs {
        let (from_entity, from_field, to_entity, to_field) = parse_rel_spec(spec)?;

        // Validate: if to_field is specified, it must be the PK of the target entity.
        // The Relationship type resolves FKs against the target PK, so a non-PK
        // target field would produce incorrect semantics.
        if let Some(ref tf) = to_field {
            let target = entities.iter().find(|e| e.name == to_entity);
            if let Some(target_ent) = target {
                let is_pk = target_ent
                    .fields
                    .iter()
                    .any(|f| f.name == *tf && f.primary_key == Some(true));
                if !is_pk {
                    anyhow::bail!(
                        "relationship target field `{}.{}` is not a primary key; \
                         FK relationships resolve against the target entity's PK",
                        to_entity,
                        tf
                    );
                }
            }
        }
        let rel_name = format!("{}_{}", from_entity, to_entity).to_lowercase();
        let foreign_key = from_field;
        relationships.push(crate::core::Relationship {
            name: rel_name,
            from: from_entity,
            to: to_entity,
            kind: crate::core::RelationshipKind::OneToMany,
            foreign_key,
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });
    }

    Ok(DataModel {
        name: name.to_string(),
        description: None,
        seed: 42,
        locale: "en_US".to_string(),
        timezone: "UTC".to_string(),
        entities,
        relationships,
        noise_profiles: Vec::new(),
        correlations: Vec::new(),
        params: BTreeMap::new(),
        blueprint_version: "1.0".to_string(),
        personas: Vec::new(),
        actor_relationships: Vec::new(),
        custom_types: Vec::new(),
        mixins: Vec::new(),
        companion_files: Vec::new(),
    })
}

/// Run the `blueprint scaffold` command.
pub fn run_scaffold(
    name: &str,
    entity_specs: &[String],
    rel_specs: &[String],
    output: Option<&str>,
    json: bool,
) -> Result<()> {
    if entity_specs.is_empty() {
        anyhow::bail!("at least one --entity must be specified");
    }

    let model = scaffold_model(name, entity_specs, rel_specs)?;

    // Validate the scaffolded model to catch issues like duplicate entities,
    // relationships referencing unknown entities, missing FK fields, etc.
    let errors = validate_model(&model);
    if !errors.is_empty() {
        for err in &errors {
            eprintln!("{} {}", "warning:".yellow().bold(), err);
        }
        eprintln!(
            "{} scaffold produced {} warning(s); review the output",
            "⚠".yellow(),
            errors.len()
        );
    }

    let output_str = if json {
        serde_json::to_string_pretty(&model)?
    } else {
        serialize_model_to_toml(&model)?
    };

    if let Some(out_path) = output {
        std::fs::write(out_path, &output_str)
            .with_context(|| format!("failed to write `{}`", out_path))?;
        eprintln!(
            "{} scaffolded {} entities, {} relationships to {}",
            "✓".green(),
            model.entities.len(),
            model.relationships.len(),
            out_path
        );
    } else {
        println!("{}", output_str);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Blueprint import — parse SQL DDL into a DataModel
// ---------------------------------------------------------------------------

/// Map a SQL type string to a knit DataType.
fn sql_type_to_data_type(sql: &str) -> crate::core::DataType {
    use crate::core::DataType;
    let upper = sql.to_uppercase();
    // Strip parenthesized length/precision, e.g. VARCHAR(255) → VARCHAR
    let base = if let Some(paren) = upper.find('(') {
        upper[..paren].trim()
    } else {
        upper.trim()
    };
    match base {
        "BOOLEAN" | "BOOL" => DataType::Bool,
        "BIGINT" | "INT8" | "BIGSERIAL" | "SERIAL8" => DataType::Int,
        "INTEGER" | "INT" | "INT4" | "SMALLINT" | "INT2" | "TINYINT" | "SERIAL" | "MEDIUMINT" => {
            DataType::Int32
        }
        "DOUBLE" | "FLOAT" | "REAL" | "FLOAT4" | "FLOAT8" | "DECIMAL" | "NUMERIC"
        | "MONEY" => DataType::Float,
        "DOUBLE PRECISION" => DataType::Float,
        "TEXT" | "VARCHAR" | "CHAR" | "CHARACTER" | "NVARCHAR" | "CLOB" | "LONGTEXT"
        | "MEDIUMTEXT" | "TINYTEXT" | "CHARACTER VARYING" | "NCHAR" => DataType::String,
        "UUID" => DataType::Uuid,
        "DATE" => DataType::Date,
        "TIME" => DataType::Time,
        "TIMESTAMP" | "DATETIME" | "DATETIME2" | "SMALLDATETIME" => DataType::Datetime,
        "TIMESTAMPTZ" | "TIMESTAMP WITH TIME ZONE" => DataType::Datetimetz,
        "INTERVAL" => DataType::Duration,
        "BYTEA" | "BLOB" | "BINARY" | "VARBINARY" | "LONGBLOB" | "MEDIUMBLOB" | "TINYBLOB"
        | "IMAGE" => DataType::Bytes,
        "JSON" | "JSONB" => DataType::Map,
        _ => {
            // Check compound types with space
            if upper.starts_with("DOUBLE PRECISION") {
                DataType::Float
            } else if upper.starts_with("CHARACTER VARYING") || upper.starts_with("NATIONAL ") {
                DataType::String
            } else if upper.starts_with("TIMESTAMP") && upper.contains("WITH TIME ZONE") {
                DataType::Datetimetz
            } else if upper.starts_with("TIMESTAMP") {
                DataType::Datetime
            } else {
                DataType::String // fallback
            }
        }
    }
}

/// Parse SQL DDL text into a DataModel.
///
/// Supports:
/// - CREATE TABLE statements (with column definitions)
/// - PRIMARY KEY constraints (inline and table-level)
/// - NOT NULL constraints
/// - ALTER TABLE ... ADD FOREIGN KEY references
/// - Inline REFERENCES on column definitions
pub fn import_sql(sql: &str, model_name: &str) -> Result<DataModel> {
    let mut entities: Vec<Entity> = Vec::new();
    let mut relationships: Vec<crate::core::Relationship> = Vec::new();

    // Normalize input: collapse whitespace, remove comments.
    let cleaned = remove_sql_comments(sql);

    // Split into statements on semicolons.
    let statements: Vec<&str> = cleaned.split(';').collect();

    for stmt in &statements {
        let trimmed = stmt.trim();
        if trimmed.is_empty() {
            continue;
        }
        let upper = trimmed.to_uppercase();

        if upper.starts_with("CREATE TABLE") {
            parse_create_table(trimmed, &mut entities, &mut relationships)?;
        } else if upper.starts_with("ALTER TABLE") && upper.contains("FOREIGN KEY") {
            parse_alter_table_fk(trimmed, &mut relationships)?;
        }
        // Skip other statements (INSERT, DROP, etc.)
    }

    if entities.is_empty() {
        anyhow::bail!("no CREATE TABLE statements found in input");
    }

    Ok(DataModel {
        name: model_name.to_string(),
        description: None,
        seed: 42,
        locale: "en_US".to_string(),
        timezone: "UTC".to_string(),
        entities,
        relationships,
        noise_profiles: Vec::new(),
        correlations: Vec::new(),
        params: BTreeMap::new(),
        blueprint_version: "1.0".to_string(),
        personas: Vec::new(),
        actor_relationships: Vec::new(),
        custom_types: Vec::new(),
        mixins: Vec::new(),
        companion_files: Vec::new(),
    })
}

/// Remove SQL line comments (--) and block comments (/* ... */), respecting string literals.
fn remove_sql_comments(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    let mut in_string = false;
    let mut string_char = '\'';
    while let Some(ch) = chars.next() {
        if in_string {
            out.push(ch);
            if ch == string_char {
                // Check for escaped quote (doubled)
                if chars.peek() == Some(&string_char) {
                    out.push(chars.next().unwrap());
                } else {
                    in_string = false;
                }
            }
        } else if ch == '\'' || ch == '"' {
            in_string = true;
            string_char = ch;
            out.push(ch);
        } else if ch == '-' && chars.peek() == Some(&'-') {
            // Skip to end of line.
            for c in chars.by_ref() {
                if c == '\n' {
                    out.push('\n');
                    break;
                }
            }
        } else if ch == '/' && chars.peek() == Some(&'*') {
            // Block comment — skip until */
            chars.next(); // consume *
            let mut depth = 1;
            while depth > 0 {
                match chars.next() {
                    Some('*') if chars.peek() == Some(&'/') => {
                        chars.next();
                        depth -= 1;
                    }
                    Some('/') if chars.peek() == Some(&'*') => {
                        chars.next();
                        depth += 1;
                    }
                    None => break,
                    _ => {}
                }
            }
            out.push(' ');
        } else {
            out.push(ch);
        }
    }
    out
}

/// Strip SQL quoting from an identifier (double quotes or backticks).
fn unquote_ident(s: &str) -> String {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"'))
        || (s.starts_with('`') && s.ends_with('`'))
        || (s.starts_with('[') && s.ends_with(']'))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Parse a CREATE TABLE statement into an entity and any inline FK relationships.
fn parse_create_table(
    stmt: &str,
    entities: &mut Vec<Entity>,
    relationships: &mut Vec<crate::core::Relationship>,
) -> Result<()> {
    // Extract table name: CREATE TABLE [IF NOT EXISTS] <name> (...)
    let upper = stmt.to_uppercase();
    let table_start = if upper.contains("IF NOT EXISTS") {
        upper.find("IF NOT EXISTS").unwrap() + "IF NOT EXISTS".len()
    } else {
        upper.find("TABLE").unwrap() + "TABLE".len()
    };

    let rest = stmt[table_start..].trim();
    let paren_pos = rest
        .find('(')
        .ok_or_else(|| anyhow::anyhow!("CREATE TABLE missing parenthesized column list"))?;
    let table_name = unquote_ident(&rest[..paren_pos]);

    // Extract the content between the outermost parentheses.
    let body = extract_paren_body(&rest[paren_pos..])?;

    // Split body into column/constraint definitions, respecting nested parens.
    let defs = split_column_defs(&body);

    let mut fields: Vec<Field> = Vec::new();
    let mut table_pk_cols: Vec<String> = Vec::new();

    for def in &defs {
        let trimmed = def.trim();
        if trimmed.is_empty() {
            continue;
        }
        let upper_def = trimmed.to_uppercase();

        // Table-level PRIMARY KEY constraint
        if upper_def.starts_with("PRIMARY KEY") {
            if let Some(cols) = extract_paren_list(trimmed) {
                table_pk_cols = cols.iter().map(|c| unquote_ident(c)).collect();
            }
            continue;
        }

        // Named CONSTRAINT — could be PK or FK
        if upper_def.starts_with("CONSTRAINT") {
            if upper_def.contains("PRIMARY KEY") {
                // CONSTRAINT pk_name PRIMARY KEY (cols)
                let pk_idx = upper_def.find("PRIMARY KEY").unwrap();
                let after_pk = &trimmed[pk_idx + "PRIMARY KEY".len()..];
                if let Some(cols) = extract_paren_list(after_pk) {
                    table_pk_cols = cols.iter().map(|c| unquote_ident(c)).collect();
                }
            } else if let Some(fk) = parse_inline_fk_constraint(trimmed, &table_name) {
                relationships.push(fk);
            }
            continue;
        }

        // Table-level FOREIGN KEY constraint
        if upper_def.starts_with("FOREIGN KEY") {
            if let Some(fk) = parse_inline_fk_constraint(trimmed, &table_name) {
                relationships.push(fk);
            }
            continue;
        }

        // Table-level UNIQUE, CHECK, INDEX — skip
        if upper_def.starts_with("UNIQUE")
            || upper_def.starts_with("CHECK")
            || upper_def.starts_with("INDEX")
            || upper_def.starts_with("KEY ")
        {
            continue;
        }

        // Column definition: name type [constraints...]
        if let Some(field) = parse_column_def(trimmed, &table_name, relationships) {
            fields.push(field);
        }
    }

    // Apply table-level PK — only first column for composite PKs (knit supports single PK).
    if table_pk_cols.len() > 1 {
        eprintln!(
            "{} table `{}` has composite primary key ({}); only `{}` will be marked as PK",
            "warning:".yellow().bold(),
            table_name,
            table_pk_cols.join(", "),
            table_pk_cols[0]
        );
    }
    if let Some(first_pk) = table_pk_cols.first() {
        if let Some(f) = fields.iter_mut().find(|f| f.name.eq_ignore_ascii_case(first_pk)) {
            f.primary_key = Some(true);
        }
    }

    entities.push(Entity {
        name: table_name,
        description: None,
        count: crate::core::CountSpec::Fixed(1000),
        fields,
        constraints: vec![],
        topology: None,
        actor: false,
        persona_distribution: None,
        activity_count: None,
        mixin_refs: None,
        output: None,
        stats: None,
        scaling: None,
        tags: Vec::new(),
    });

    Ok(())
}

/// Extract the body between matching outermost parentheses.
fn extract_paren_body(s: &str) -> Result<String> {
    let start = s
        .find('(')
        .ok_or_else(|| anyhow::anyhow!("expected '('"))?;
    let mut depth = 0;
    let mut end = start;
    for (i, ch) in s[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    if depth != 0 {
        anyhow::bail!("unmatched parentheses in CREATE TABLE");
    }
    Ok(s[start + 1..end].to_string())
}

/// Split comma-separated column definitions, respecting nested parentheses and string literals.
fn split_column_defs(body: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut in_string = false;
    let mut string_char = '\'';
    let mut chars = body.chars().peekable();
    while let Some(ch) = chars.next() {
        if in_string {
            current.push(ch);
            if ch == string_char {
                if chars.peek() == Some(&string_char) {
                    current.push(chars.next().unwrap());
                } else {
                    in_string = false;
                }
            }
        } else {
            match ch {
                '\'' | '"' => {
                    in_string = true;
                    string_char = ch;
                    current.push(ch);
                }
                '(' => {
                    depth += 1;
                    current.push(ch);
                }
                ')' => {
                    depth -= 1;
                    current.push(ch);
                }
                ',' if depth == 0 => {
                    result.push(current.trim().to_string());
                    current.clear();
                }
                _ => current.push(ch),
            }
        }
    }
    let last = current.trim().to_string();
    if !last.is_empty() {
        result.push(last);
    }
    result
}

/// Extract a parenthesized comma-separated list: `(a, b, c)` → `["a", "b", "c"]`.
/// Finds the matching close paren for the first open paren.
fn extract_paren_list(s: &str) -> Option<Vec<String>> {
    let start = s.find('(')?;
    let mut depth = 0;
    let mut end = start;
    for (i, ch) in s[start..].char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
            _ => {}
        }
    }
    if depth != 0 || end <= start {
        return None;
    }
    let inner = &s[start + 1..end];
    Some(
        inner
            .split(',')
            .map(|p| p.trim().to_string())
            .filter(|p| !p.is_empty())
            .collect(),
    )
}

/// Parse a single column definition like `name VARCHAR(255) NOT NULL PRIMARY KEY`.
fn parse_column_def(
    def: &str,
    table_name: &str,
    relationships: &mut Vec<crate::core::Relationship>,
) -> Option<Field> {
    // Tokenize: first token is name, second is type (may have parens), rest is constraints.
    let tokens = tokenize_column_def(def);
    if tokens.len() < 2 {
        return None;
    }

    let col_name = unquote_ident(&tokens[0]);
    let col_type_raw = &tokens[1];

    // Check for compound types like "DOUBLE PRECISION", "CHARACTER VARYING"
    let (data_type, constraint_start) = {
        let upper1 = tokens[1].to_uppercase();
        if tokens.len() > 2 {
            let upper2 = tokens[2].to_uppercase();
            let compound = format!("{} {}", upper1, upper2);
            if matches!(
                compound.as_str(),
                "DOUBLE PRECISION"
                    | "CHARACTER VARYING"
                    | "TIME ZONE"
                    | "TIMESTAMP WITH"
                    | "NATIONAL CHAR"
                    | "NATIONAL VARCHAR"
            ) {
                // Check for three-word types like "TIMESTAMP WITH TIMEZONE"
                if upper1 == "TIMESTAMP" && upper2 == "WITH" && tokens.len() > 3 {
                    let upper3 = tokens[3].to_uppercase();
                    if upper3 == "TIME" && tokens.len() > 4 && tokens[4].to_uppercase() == "ZONE" {
                        (sql_type_to_data_type("TIMESTAMP WITH TIME ZONE"), 5)
                    } else {
                        (sql_type_to_data_type(&compound), 3)
                    }
                } else {
                    (sql_type_to_data_type(&compound), 3)
                }
            } else {
                (sql_type_to_data_type(col_type_raw), 2)
            }
        } else {
            (sql_type_to_data_type(col_type_raw), 2)
        }
    };

    let mut primary_key = None;
    let mut nullable = crate::core::NullSpec::Never; // default NOT NULL; flip if no NOT NULL
    let mut has_not_null = false;

    // Scan constraint tokens
    let constraint_str = tokens[constraint_start..].join(" ").to_uppercase();
    if constraint_str.contains("PRIMARY KEY") {
        primary_key = Some(true);
        has_not_null = true;
    }
    if constraint_str.contains("NOT NULL") {
        has_not_null = true;
    }
    if !has_not_null {
        nullable = crate::core::NullSpec::Probability(0.05);
    }

    // Check for inline REFERENCES — use original tokens to preserve case.
    let original_constraint_str = tokens[constraint_start..].join(" ");
    if constraint_str.contains("REFERENCES") {
        if let Some(fk) = parse_inline_references(&original_constraint_str, table_name, &col_name) {
            relationships.push(fk);
        }
    }

    Some(Field {
        name: col_name,
        description: None,
        data_type,
        generator: None,
        nullable,
        primary_key,
        precision: None,
        actor_column: false,
        fields: vec![],
        stats: None,
        traits: None,
    })
}

/// Tokenize a column definition, keeping parenthesized groups attached to their preceding token.
fn tokenize_column_def(def: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut depth = 0;
    let mut in_quote = false;
    let mut quote_char = '"';

    for ch in def.chars() {
        match ch {
            '"' | '`' | '[' if depth == 0 && !in_quote => {
                in_quote = true;
                quote_char = if ch == '[' { ']' } else { ch };
                current.push(ch);
            }
            c if c == quote_char && in_quote => {
                in_quote = false;
                current.push(ch);
            }
            '(' if !in_quote => {
                depth += 1;
                current.push(ch);
            }
            ')' if !in_quote => {
                depth -= 1;
                current.push(ch);
            }
            ' ' | '\t' | '\n' if depth == 0 && !in_quote => {
                if !current.is_empty() {
                    tokens.push(current.clone());
                    current.clear();
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// Parse inline `REFERENCES target(col)` from constraint text (original case).
fn parse_inline_references(
    constraint_text: &str,
    from_entity: &str,
    fk_col: &str,
) -> Option<crate::core::Relationship> {
    let upper = constraint_text.to_uppercase();
    let idx = upper.find("REFERENCES")?;
    let rest = constraint_text[idx + "REFERENCES".len()..].trim();
    // Extract target table name and optional column
    let to_entity = if let Some(paren) = rest.find('(') {
        unquote_ident(rest[..paren].trim())
    } else {
        unquote_ident(rest.split_whitespace().next()?)
    };

    let rel_name = format!("{}_{}_{}", from_entity, fk_col, to_entity).to_lowercase();
    Some(crate::core::Relationship {
        name: rel_name,
        from: from_entity.to_string(),
        to: to_entity,
        kind: crate::core::RelationshipKind::OneToMany,
        foreign_key: Some(fk_col.to_string()),
        cardinality: None,
        degree: None,
        selection: None,
        nullable: None,
        acyclic: None,
        root_probability: None,
        max_depth: None,
        properties: Vec::new(),
    })
}

/// Parse a table-level FOREIGN KEY constraint or CONSTRAINT ... FOREIGN KEY.
fn parse_inline_fk_constraint(
    def: &str,
    table_name: &str,
) -> Option<crate::core::Relationship> {
    let upper = def.to_uppercase();
    // Find FOREIGN KEY (...) REFERENCES ... (...)
    let fk_idx = upper.find("FOREIGN KEY")?;
    let after_fk = &def[fk_idx + "FOREIGN KEY".len()..];
    let fk_cols = extract_paren_list(after_fk)?;
    if fk_cols.is_empty() {
        return None;
    }
    let fk_col = unquote_ident(&fk_cols[0]);

    // Find REFERENCES
    let ref_upper = after_fk.to_uppercase();
    let ref_idx = ref_upper.find("REFERENCES")?;
    let after_ref = after_fk[ref_idx + "REFERENCES".len()..].trim();

    let to_entity = if let Some(paren) = after_ref.find('(') {
        unquote_ident(&after_ref[..paren])
    } else {
        unquote_ident(after_ref.split_whitespace().next()?)
    };

    let rel_name = format!("{}_{}_{}", table_name, fk_col, to_entity).to_lowercase();
    Some(crate::core::Relationship {
        name: rel_name,
        from: table_name.to_string(),
        to: to_entity,
        kind: crate::core::RelationshipKind::OneToMany,
        foreign_key: Some(fk_col),
        cardinality: None,
        degree: None,
        selection: None,
        nullable: None,
        acyclic: None,
        root_probability: None,
        max_depth: None,
        properties: Vec::new(),
    })
}

/// Parse ALTER TABLE ... ADD FOREIGN KEY (...) REFERENCES ... (...).
fn parse_alter_table_fk(
    stmt: &str,
    relationships: &mut Vec<crate::core::Relationship>,
) -> Result<()> {
    let upper = stmt.to_uppercase();
    // Extract table name: ALTER TABLE <name> ...
    let table_start = upper.find("TABLE").unwrap() + "TABLE".len();
    let rest = stmt[table_start..].trim();

    // Skip optional "ONLY"
    let rest = if rest.to_uppercase().starts_with("ONLY ") {
        rest["ONLY ".len()..].trim()
    } else {
        rest
    };

    // Table name is next token (before ADD/FOREIGN)
    let name_end = rest
        .find(|c: char| c.is_whitespace())
        .unwrap_or(rest.len());
    let table_name = unquote_ident(&rest[..name_end]);

    if let Some(fk) = parse_inline_fk_constraint(stmt, &table_name) {
        relationships.push(fk);
    }

    Ok(())
}

/// Run the `blueprint import` command.
pub fn run_import(
    file: &str,
    name: Option<&str>,
    output: Option<&str>,
    json: bool,
) -> Result<()> {
    let sql_text = std::fs::read_to_string(file)
        .with_context(|| format!("failed to read `{}`", file))?;

    let model_name = name.unwrap_or_else(|| {
        std::path::Path::new(file)
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("imported")
    });

    let model = import_sql(&sql_text, model_name)?;

    // Validate and show warnings.
    let errors = validate_model(&model);
    if !errors.is_empty() {
        for err in &errors {
            eprintln!("{} {}", "warning:".yellow().bold(), err);
        }
    }

    let output_str = if json {
        serde_json::to_string_pretty(&model)?
    } else {
        serialize_model_to_toml(&model)?
    };

    if let Some(out_path) = output {
        std::fs::write(out_path, &output_str)
            .with_context(|| format!("failed to write `{}`", out_path))?;
        eprintln!(
            "{} imported {} entities, {} relationships from {}",
            "✓".green(),
            model.entities.len(),
            model.relationships.len(),
            file
        );
    } else {
        println!("{}", output_str);
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Blueprint update — programmatic CLI-driven model modifications
// ---------------------------------------------------------------------------

/// A single update operation to apply to a DataModel.
#[derive(Debug)]
enum UpdateOp {
    /// Set entity row count: `Entity=N`
    SetCount { entity: String, count: u64 },
    /// Set entity description: `Entity=text`
    SetEntityDesc { entity: String, desc: String },
    /// Set field description: `Entity.field=text`
    SetFieldDesc {
        entity: String,
        field: String,
        desc: String,
    },
    /// Add tags to an entity: `Entity=tag1,tag2`
    AddTags {
        entity: String,
        tags: Vec<String>,
    },
    /// Remove tags from an entity: `Entity=tag1,tag2`
    RemoveTags {
        entity: String,
        tags: Vec<String>,
    },
    /// Set model seed: `N`
    SetSeed { seed: u64 },
    /// Set model locale: `en_US`
    SetLocale { locale: String },
}

/// Parse a `--count Entity=N` spec.
fn parse_count_spec(spec: &str) -> Result<UpdateOp> {
    let (entity, val) = split_kv(spec, "count")?;
    let count: u64 = val
        .parse()
        .with_context(|| format!("invalid count `{}` for entity `{}`", val, entity))?;
    Ok(UpdateOp::SetCount { entity, count })
}

/// Parse a `--describe Entity=text` or `--describe Entity.field=text` spec.
fn parse_describe_spec(spec: &str) -> Result<UpdateOp> {
    let (target, desc) = split_kv(spec, "describe")?;
    if let Some(dot) = target.find('.') {
        let entity = target[..dot].to_string();
        let field = target[dot + 1..].to_string();
        if entity.is_empty() || field.is_empty() {
            anyhow::bail!("invalid describe target `{}`", target);
        }
        Ok(UpdateOp::SetFieldDesc {
            entity,
            field,
            desc: desc.to_string(),
        })
    } else {
        Ok(UpdateOp::SetEntityDesc {
            entity: target,
            desc: desc.to_string(),
        })
    }
}

/// Parse a `--tag Entity=tag1,tag2` spec.
fn parse_tag_spec(spec: &str) -> Result<UpdateOp> {
    let (entity, val) = split_kv(spec, "tag")?;
    let tags: Vec<String> = val.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if tags.is_empty() {
        anyhow::bail!("no tags specified for entity `{}`", entity);
    }
    Ok(UpdateOp::AddTags { entity, tags })
}

/// Parse a `--untag Entity=tag1,tag2` spec.
fn parse_untag_spec(spec: &str) -> Result<UpdateOp> {
    let (entity, val) = split_kv(spec, "untag")?;
    let tags: Vec<String> = val.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if tags.is_empty() {
        anyhow::bail!("no tags specified for entity `{}`", entity);
    }
    Ok(UpdateOp::RemoveTags { entity, tags })
}

/// Split a `key=value` spec, returning `(key, value)` as owned strings.
fn split_kv(spec: &str, flag_name: &str) -> Result<(String, String)> {
    let eq_pos = spec.find('=').ok_or_else(|| {
        anyhow::anyhow!(
            "invalid --{} spec `{}`: expected `key=value` format",
            flag_name,
            spec
        )
    })?;
    let key = spec[..eq_pos].trim().to_string();
    let val = spec[eq_pos + 1..].trim().to_string();
    if key.is_empty() {
        anyhow::bail!("empty key in --{} spec `{}`", flag_name, spec);
    }
    if val.is_empty() {
        anyhow::bail!("empty value in --{} spec `{}`", flag_name, spec);
    }
    Ok((key, val))
}

/// Apply a list of update operations to a DataModel.
pub fn update_model(model: &mut DataModel, ops: &[UpdateOp]) -> Result<Vec<String>> {
    let mut changes = Vec::new();

    for op in ops {
        match op {
            UpdateOp::SetCount { entity, count } => {
                let ent = model
                    .entities
                    .iter_mut()
                    .find(|e| e.name == *entity)
                    .ok_or_else(|| anyhow::anyhow!("entity `{}` not found", entity))?;
                let old = match &ent.count {
                    crate::core::CountSpec::Fixed(n) => format!("{}", n),
                    other => format!("{:?}", other),
                };
                ent.count = crate::core::CountSpec::Fixed(*count);
                changes.push(format!("{}.count: {} → {}", entity, old, count));
            }
            UpdateOp::SetEntityDesc { entity, desc } => {
                let ent = model
                    .entities
                    .iter_mut()
                    .find(|e| e.name == *entity)
                    .ok_or_else(|| anyhow::anyhow!("entity `{}` not found", entity))?;
                ent.description = Some(desc.clone());
                changes.push(format!("{}.description = \"{}\"", entity, desc));
            }
            UpdateOp::SetFieldDesc {
                entity,
                field,
                desc,
            } => {
                let ent = model
                    .entities
                    .iter_mut()
                    .find(|e| e.name == *entity)
                    .ok_or_else(|| anyhow::anyhow!("entity `{}` not found", entity))?;
                let fld = ent
                    .fields
                    .iter_mut()
                    .find(|f| f.name == *field)
                    .ok_or_else(|| {
                        anyhow::anyhow!("field `{}` not found in entity `{}`", field, entity)
                    })?;
                fld.description = Some(desc.clone());
                changes.push(format!("{}.{}.description = \"{}\"", entity, field, desc));
            }
            UpdateOp::AddTags { entity, tags } => {
                let ent = model
                    .entities
                    .iter_mut()
                    .find(|e| e.name == *entity)
                    .ok_or_else(|| anyhow::anyhow!("entity `{}` not found", entity))?;
                for tag in tags {
                    if !ent.tags.contains(tag) {
                        ent.tags.push(tag.clone());
                    }
                }
                changes.push(format!(
                    "{}.tags += [{}]",
                    entity,
                    tags.join(", ")
                ));
            }
            UpdateOp::RemoveTags { entity, tags } => {
                let ent = model
                    .entities
                    .iter_mut()
                    .find(|e| e.name == *entity)
                    .ok_or_else(|| anyhow::anyhow!("entity `{}` not found", entity))?;
                ent.tags.retain(|t| !tags.contains(t));
                changes.push(format!(
                    "{}.tags -= [{}]",
                    entity,
                    tags.join(", ")
                ));
            }
            UpdateOp::SetSeed { seed } => {
                let old = model.seed;
                model.seed = *seed;
                changes.push(format!("seed: {} → {}", old, seed));
            }
            UpdateOp::SetLocale { locale } => {
                let old = model.locale.clone();
                model.locale = locale.clone();
                changes.push(format!("locale: {} → {}", old, locale));
            }
        }
    }

    Ok(changes)
}

/// Run the `blueprint update` command.
pub fn run_update(
    file: &str,
    counts: &[String],
    describes: &[String],
    tags: &[String],
    untags: &[String],
    seed: Option<u64>,
    locale: Option<&str>,
    output: Option<&str>,
    json: bool,
) -> Result<()> {
    let mut model = load_blueprint(file)
        .with_context(|| format!("failed to load `{}`", file))?;

    // Parse all update operations.
    let mut ops: Vec<UpdateOp> = Vec::new();
    for spec in counts {
        ops.push(parse_count_spec(spec)?);
    }
    for spec in describes {
        ops.push(parse_describe_spec(spec)?);
    }
    for spec in tags {
        ops.push(parse_tag_spec(spec)?);
    }
    for spec in untags {
        ops.push(parse_untag_spec(spec)?);
    }
    if let Some(s) = seed {
        ops.push(UpdateOp::SetSeed { seed: s });
    }
    if let Some(loc) = locale {
        ops.push(UpdateOp::SetLocale {
            locale: loc.to_string(),
        });
    }

    if ops.is_empty() {
        anyhow::bail!("no update operations specified");
    }

    let changes = update_model(&mut model, &ops)?;

    // Always serialize as TOML for file output (--json only controls summary).
    let output_str = serialize_model_to_toml(&model)?;

    let out_path = output.unwrap_or(file);
    std::fs::write(out_path, &output_str)
        .with_context(|| format!("failed to write `{}`", out_path))?;

    if json {
        let summary = serde_json::json!({
            "changes": changes,
            "output": out_path,
        });
        println!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        for change in &changes {
            eprintln!("  {} {}", "▸".green(), change);
        }
        eprintln!(
            "{} applied {} update(s) to {}",
            "✓".green(),
            changes.len(),
            out_path
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Blueprint validate — check generated data against blueprint schema
// ---------------------------------------------------------------------------

use std::collections::HashSet;

/// Severity level for validation findings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Severity {
    Error,
    Warning,
}

/// A single validation finding.
#[derive(Debug)]
struct Finding {
    severity: Severity,
    entity: String,
    message: String,
}

impl Finding {
    fn error(entity: &str, msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Error,
            entity: entity.to_string(),
            message: msg.into(),
        }
    }
    fn warning(entity: &str, msg: impl Into<String>) -> Self {
        Self {
            severity: Severity::Warning,
            entity: entity.to_string(),
            message: msg.into(),
        }
    }
}

/// Map a knit DataType to the expected Arrow DataType(s).
fn expected_arrow_types(dt: &crate::core::DataType) -> Vec<arrow::datatypes::DataType> {
    use arrow::datatypes::DataType as A;
    use crate::core::DataType as K;
    match dt {
        K::Bool => vec![A::Boolean],
        K::Int => vec![A::Int64, A::Int32, A::Int16, A::Int8, A::UInt64, A::UInt32, A::UInt16, A::UInt8],
        K::Int32 => vec![A::Int32, A::Int16, A::Int8, A::UInt32, A::UInt16, A::UInt8],
        K::Float => vec![A::Float64, A::Float32],
        K::String => vec![A::Utf8, A::LargeUtf8],
        K::Uuid => vec![A::Utf8, A::LargeUtf8],
        K::Date => vec![A::Date32, A::Date64, A::Utf8, A::LargeUtf8],
        K::Time => vec![
            A::Time64(arrow::datatypes::TimeUnit::Microsecond),
            A::Time64(arrow::datatypes::TimeUnit::Nanosecond),
            A::Time32(arrow::datatypes::TimeUnit::Second),
            A::Time32(arrow::datatypes::TimeUnit::Millisecond),
            A::Utf8,
            A::LargeUtf8,
        ],
        K::Datetime | K::DatetimeUs | K::Datetimetz => vec![
            A::Timestamp(arrow::datatypes::TimeUnit::Microsecond, None),
            A::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None),
            A::Timestamp(arrow::datatypes::TimeUnit::Nanosecond, None),
            A::Timestamp(arrow::datatypes::TimeUnit::Second, None),
            A::Date64,
            A::Utf8,
            A::LargeUtf8,
        ],
        K::Duration => vec![
            A::Duration(arrow::datatypes::TimeUnit::Microsecond),
            A::Duration(arrow::datatypes::TimeUnit::Millisecond),
            A::Duration(arrow::datatypes::TimeUnit::Nanosecond),
            A::Duration(arrow::datatypes::TimeUnit::Second),
            A::Int64,
        ],
        K::Bytes => vec![A::Binary, A::LargeBinary],
        K::Array | K::Map | K::Object => vec![A::Utf8, A::LargeUtf8], // serialized as JSON strings
        K::Custom(_) => vec![], // skip type checking for custom types
    }
}

/// Check if an Arrow DataType is compatible with the expected types.
/// For Timestamp with timezone info, strip the tz for comparison.
fn arrow_type_compatible(
    actual: &arrow::datatypes::DataType,
    expected: &[arrow::datatypes::DataType],
) -> bool {
    if expected.is_empty() {
        return true; // custom types — no check
    }
    for exp in expected {
        if actual == exp {
            return true;
        }
        // Timestamp with timezone matches Timestamp without
        if let (
            arrow::datatypes::DataType::Timestamp(unit_a, _),
            arrow::datatypes::DataType::Timestamp(unit_e, _),
        ) = (actual, exp)
        {
            if unit_a == unit_e {
                return true;
            }
        }
    }
    false
}

/// Find a data file for the given entity name in the data directory.
fn find_entity_file(data_dir: &std::path::Path, entity_name: &str) -> Option<std::path::PathBuf> {
    let extensions = ["parquet", "csv", "json", "jsonl"];
    for ext in &extensions {
        let path = data_dir.join(format!("{}.{}", entity_name, ext));
        if path.exists() {
            return Some(path);
        }
    }
    // Try case-insensitive match
    if let Ok(entries) = std::fs::read_dir(data_dir) {
        let lower = entity_name.to_lowercase();
        for entry in entries.flatten() {
            let fname = entry.file_name().to_string_lossy().to_string();
            let stem = fname.rsplit_once('.').map(|(s, _)| s).unwrap_or(&fname);
            if stem.to_lowercase() == lower {
                return Some(entry.path());
            }
        }
    }
    None
}

/// Read a data file into Arrow RecordBatches using the learn ingest reader.
fn read_data_file(path: &std::path::Path) -> Result<Vec<arrow::record_batch::RecordBatch>> {
    crate::learn::ingest::read_auto(path)
        .map_err(|e| anyhow::anyhow!("failed to read `{}`: {}", path.display(), e))
}

/// Validate generated data files against a blueprint model.
pub fn validate_data(
    model: &DataModel,
    data_dir: &std::path::Path,
    filter_entities: &[String],
) -> Result<Vec<Finding>> {
    let mut findings = Vec::new();

    // Determine which entities to check.
    let entities: Vec<&Entity> = if filter_entities.is_empty() {
        model.entities.iter().collect()
    } else {
        let mut out = Vec::new();
        for name in filter_entities {
            if let Some(ent) = model.entities.iter().find(|e| e.name == *name) {
                out.push(ent);
            } else {
                findings.push(Finding::error(name, "entity not found in blueprint"));
            }
        }
        out
    };

    // entity name -> (schema, row_count, pk_values)
    let mut entity_data: BTreeMap<
        String,
        (
            arrow::datatypes::Schema,
            usize,
            Option<HashSet<String>>,
        ),
    > = BTreeMap::new();

    for entity in &entities {
        let file = match find_entity_file(data_dir, &entity.name) {
            Some(f) => f,
            None => {
                findings.push(Finding::error(&entity.name, "data file not found"));
                continue;
            }
        };

        let batches = match read_data_file(&file) {
            Ok(b) => b,
            Err(e) => {
                findings.push(Finding::error(
                    &entity.name,
                    format!("cannot read data file: {}", e),
                ));
                continue;
            }
        };

        if batches.is_empty() {
            findings.push(Finding::warning(&entity.name, "data file is empty"));
            entity_data.insert(
                entity.name.clone(),
                (arrow::datatypes::Schema::empty(), 0, None),
            );
            continue;
        }

        let schema = batches[0].schema();
        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();

        // --- Column presence ---
        let data_cols: HashSet<String> = schema
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();

        for field in &entity.fields {
            if !data_cols.contains(&field.name) {
                findings.push(Finding::error(
                    &entity.name,
                    format!("missing column `{}`", field.name),
                ));
            }
        }

        // Extra columns (warning only) — include implicit FK columns
        let mut expected_cols: HashSet<String> =
            entity.fields.iter().map(|f| f.name.clone()).collect();
        for rel in &model.relationships {
            if rel.from == entity.name {
                let fk = rel
                    .foreign_key
                    .clone()
                    .unwrap_or_else(|| format!("{}_id", rel.to));
                expected_cols.insert(fk);
            }
        }
        // Also include edge property columns from relationships
        for rel in &model.relationships {
            if rel.from == entity.name {
                for prop in &rel.properties {
                    expected_cols.insert(prop.name.clone());
                }
            }
        }
        for col in &data_cols {
            if !expected_cols.contains(col) {
                findings.push(Finding::warning(
                    &entity.name,
                    format!("unexpected column `{}`", col),
                ));
            }
        }

        // --- Data type checks ---
        for field in &entity.fields {
            if let Some(arrow_field) = schema.field_with_name(&field.name).ok() {
                let expected = expected_arrow_types(&field.data_type);
                if !arrow_type_compatible(arrow_field.data_type(), &expected) {
                    findings.push(Finding::error(
                        &entity.name,
                        format!(
                            "column `{}`: expected {:?}, got {:?}",
                            field.name, field.data_type, arrow_field.data_type()
                        ),
                    ));
                }
            }
        }

        // --- Null checks ---
        for field in &entity.fields {
            if matches!(field.nullable, crate::core::NullSpec::Never) {
                // Check for nulls in non-nullable columns
                let mut null_count = 0usize;
                for batch in &batches {
                    if let Some(col_idx) = schema.index_of(&field.name).ok() {
                        let col = batch.column(col_idx);
                        null_count += col.null_count();
                    }
                }
                if null_count > 0 {
                    findings.push(Finding::error(
                        &entity.name,
                        format!(
                            "column `{}`: {} null(s) but nullable = Never",
                            field.name, null_count
                        ),
                    ));
                }
            }
        }

        // --- PK uniqueness ---
        let mut pk_values: Option<HashSet<String>> = None;
        for field in &entity.fields {
            if field.primary_key == Some(true) {
                let mut seen = HashSet::new();
                let mut dup_count = 0usize;
                for batch in &batches {
                    if let Some(col_idx) = schema.index_of(&field.name).ok() {
                        let col = batch.column(col_idx);
                        for i in 0..col.len() {
                            use arrow::array::Array;
                            if col.is_null(i) {
                                findings.push(Finding::error(
                                    &entity.name,
                                    format!("PK column `{}` contains null", field.name),
                                ));
                                break;
                            }
                            let val = format!(
                                "{}",
                                arrow::util::display::ArrayFormatter::try_new(
                                    col.as_ref(),
                                    &arrow::util::display::FormatOptions::default()
                                )
                                .map(|f| f.value(i).to_string())
                                .unwrap_or_default()
                            );
                            if !seen.insert(val) {
                                dup_count += 1;
                            }
                        }
                    }
                }
                if dup_count > 0 {
                    findings.push(Finding::error(
                        &entity.name,
                        format!(
                            "PK column `{}`: {} duplicate value(s) out of {} rows",
                            field.name, dup_count, total_rows
                        ),
                    ));
                }
                pk_values = Some(seen);
                break; // only one PK
            }
        }

        // --- Row count ---
        match &entity.count {
            crate::core::CountSpec::Fixed(expected) => {
                if total_rows != *expected as usize {
                    findings.push(Finding::warning(
                        &entity.name,
                        format!(
                            "row count: expected {}, got {}",
                            expected, total_rows
                        ),
                    ));
                }
            }
            crate::core::CountSpec::Range { min, max } => {
                if total_rows < *min as usize || total_rows > *max as usize {
                    findings.push(Finding::warning(
                        &entity.name,
                        format!(
                            "row count {} outside expected range [{}, {}]",
                            total_rows, min, max
                        ),
                    ));
                }
            }
            _ => {} // Expression/Distribution — skip
        }

        entity_data.insert(
            entity.name.clone(),
            (schema.as_ref().clone(), total_rows, pk_values),
        );
    }

    // --- FK referential integrity ---
    for rel in &model.relationships {
        // Skip if child entity is filtered out
        if !filter_entities.is_empty()
            && !filter_entities.iter().any(|e| e == &rel.from)
        {
            continue;
        }

        let fk_col = rel
            .foreign_key
            .clone()
            .unwrap_or_else(|| format!("{}_id", rel.to));

        // Get parent PK values
        // Load parent PK values — may need on-demand loading if parent was filtered out
        let parent_pks = if let Some((_, _, pk_opt)) = entity_data.get(&rel.to) {
            match pk_opt {
                Some(pks) => pks.clone(),
                None => {
                    findings.push(Finding::warning(
                        &rel.from,
                        format!(
                            "FK `{}` -> `{}`: parent entity has no PK to check against",
                            fk_col, rel.to
                        ),
                    ));
                    continue;
                }
            }
        } else {
            // Parent not loaded yet (filtered out) — load on demand
            let parent_entity = model.entities.iter().find(|e| e.name == rel.to);
            let pk_field = parent_entity.and_then(|e| {
                e.fields.iter().find(|f| f.primary_key == Some(true))
            });
            match (find_entity_file(data_dir, &rel.to), pk_field) {
                (Some(file), Some(pk)) => {
                    match read_data_file(&file) {
                        Ok(batches) if !batches.is_empty() => {
                            let schema = batches[0].schema();
                            let mut pks = HashSet::new();
                            if let Ok(col_idx) = schema.index_of(&pk.name) {
                                for batch in &batches {
                                    let col = batch.column(col_idx);
                                    for i in 0..col.len() {
                                        use arrow::array::Array;
                                        if !col.is_null(i) {
                                            let val = arrow::util::display::ArrayFormatter::try_new(
                                                col.as_ref(),
                                                &arrow::util::display::FormatOptions::default(),
                                            )
                                            .map(|f| f.value(i).to_string())
                                            .unwrap_or_default();
                                            pks.insert(val);
                                        }
                                    }
                                }
                            }
                            pks
                        }
                        _ => continue,
                    }
                }
                _ => continue,
            }
        };

        // Read child FK column
        let child_file = match find_entity_file(data_dir, &rel.from) {
            Some(f) => f,
            None => continue,
        };
        let child_batches = match read_data_file(&child_file) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if child_batches.is_empty() {
            continue;
        }

        let child_schema = child_batches[0].schema();
        let fk_idx = match child_schema.index_of(&fk_col) {
            Ok(i) => i,
            Err(_) => {
                findings.push(Finding::error(
                    &rel.from,
                    format!(
                        "FK column `{}` not found (relationship `{}` -> `{}`)",
                        fk_col, rel.from, rel.to
                    ),
                ));
                continue;
            }
        };

        let mut orphan_count = 0usize;
        for batch in &child_batches {
            let col = batch.column(fk_idx);
            for i in 0..col.len() {
                use arrow::array::Array;
                if col.is_null(i) {
                    continue; // null FKs are ok if nullable
                }
                let val = arrow::util::display::ArrayFormatter::try_new(
                    col.as_ref(),
                    &arrow::util::display::FormatOptions::default(),
                )
                .map(|f| f.value(i).to_string())
                .unwrap_or_default();
                if !parent_pks.contains(&val) {
                    orphan_count += 1;
                }
            }
        }
        if orphan_count > 0 {
            findings.push(Finding::error(
                &rel.from,
                format!(
                    "FK `{}` -> `{}`: {} orphan row(s) reference non-existent parent keys",
                    fk_col, rel.to, orphan_count
                ),
            ));
        }
    }

    Ok(findings)
}

/// Run the `blueprint validate` command.
pub fn run_validate(
    file: &str,
    data: &str,
    entities: &[String],
    strict: bool,
    json: bool,
) -> Result<()> {
    let model = load_blueprint(file)
        .with_context(|| format!("failed to load `{}`", file))?;

    let data_dir = std::path::Path::new(data);
    if !data_dir.is_dir() {
        anyhow::bail!("`{}` is not a directory", data);
    }

    let findings = validate_data(&model, data_dir, entities)?;

    let errors = findings.iter().filter(|f| f.severity == Severity::Error).count();
    let warnings = findings
        .iter()
        .filter(|f| f.severity == Severity::Warning)
        .count();

    if json {
        let items: Vec<serde_json::Value> = findings
            .iter()
            .map(|f| {
                serde_json::json!({
                    "severity": if f.severity == Severity::Error { "error" } else { "warning" },
                    "entity": f.entity,
                    "message": f.message,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "errors": errors,
                "warnings": warnings,
                "findings": items,
            }))?
        );
    } else {
        for f in &findings {
            let (icon, label) = match f.severity {
                Severity::Error => ("✗".red(), "ERROR".red()),
                Severity::Warning => ("⚠".yellow(), "WARN ".yellow()),
            };
            eprintln!("  {} [{}] {}: {}", icon, label, f.entity, f.message);
        }
        if findings.is_empty() {
            eprintln!(
                "{} all checks passed for {}",
                "✓".green(),
                file
            );
        } else {
            eprintln!(
                "\n{} {} error(s), {} warning(s)",
                if errors > 0 { "✗".red() } else { "✓".green() },
                errors,
                warnings
            );
        }
    }

    if errors > 0 || (strict && warnings > 0) {
        anyhow::bail!(
            "validation failed: {} error(s), {} warning(s)",
            errors,
            warnings
        );
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Blueprint derive — create variant blueprints with scale/overrides
// ---------------------------------------------------------------------------

/// Parse a scale factor spec like `10x`, `0.5x`, `2.5x`.
fn parse_scale_factor(spec: &str) -> Result<f64> {
    let trimmed = spec.trim().trim_end_matches('x').trim_end_matches('X');
    let factor: f64 = trimmed
        .parse()
        .with_context(|| format!("invalid scale factor `{}`: expected format like `10x`", spec))?;
    if factor <= 0.0 {
        anyhow::bail!("scale factor must be positive, got `{}`", spec);
    }
    Ok(factor)
}

/// Apply derive operations to a cloned DataModel.
pub fn derive_model(
    model: &DataModel,
    scale: Option<f64>,
    count_overrides: &[(String, u64)],
    excludes: &[String],
    seed: Option<u64>,
    locale: Option<&str>,
    variant: Option<&str>,
) -> Result<(DataModel, Vec<String>)> {
    let mut derived = model.clone();
    let mut changes = Vec::new();

    // Apply variant name to description
    if let Some(v) = variant {
        let base_desc = derived
            .description
            .as_deref()
            .unwrap_or(&derived.name);
        derived.description = Some(format!("{} [variant: {}]", base_desc, v));
        changes.push(format!("variant = \"{}\"", v));
    }

    // Apply seed override
    if let Some(s) = seed {
        let old = derived.seed;
        derived.seed = s;
        changes.push(format!("seed: {} → {}", old, s));
    }

    // Apply locale override
    if let Some(loc) = locale {
        let old = derived.locale.clone();
        derived.locale = loc.to_string();
        changes.push(format!("locale: {} → {}", old, loc));
    }

    // Build exclusion set
    let exclude_set: HashSet<&str> = excludes.iter().map(|s| s.as_str()).collect();
    for excl in excludes {
        if !derived.entities.iter().any(|e| e.name == *excl) {
            anyhow::bail!("excluded entity `{}` not found in blueprint", excl);
        }
    }

    // Apply scale factor to all non-excluded entities
    if let Some(factor) = scale {
        for entity in &mut derived.entities {
            if exclude_set.contains(entity.name.as_str()) {
                continue;
            }
            match &entity.count {
                crate::core::CountSpec::Fixed(n) => {
                    let new_count = ((*n as f64) * factor).round().max(1.0) as u64;
                    let old = *n;
                    entity.count = crate::core::CountSpec::Fixed(new_count);
                    changes.push(format!("{}.count: {} → {} (×{})", entity.name, old, new_count, factor));
                }
                crate::core::CountSpec::Range { min, max } => {
                    let old_min = *min;
                    let old_max = *max;
                    let new_min = ((old_min as f64) * factor).round().max(1.0) as u64;
                    let new_max = ((old_max as f64) * factor).round().max(new_min as f64) as u64;
                    entity.count = crate::core::CountSpec::Range {
                        min: new_min,
                        max: new_max,
                    };
                    changes.push(format!(
                        "{}.count: [{}, {}] → [{}, {}] (×{})",
                        entity.name, old_min, old_max, new_min, new_max, factor
                    ));
                }
                _ => {
                    // Expression/Distribution counts — skip with warning
                    changes.push(format!(
                        "{}.count: skipped (expression/distribution)",
                        entity.name
                    ));
                }
            }
        }
    }

    // Apply explicit count overrides (after scale, so they take priority)
    for (entity_name, count) in count_overrides {
        let entity = derived
            .entities
            .iter_mut()
            .find(|e| e.name == *entity_name)
            .ok_or_else(|| anyhow::anyhow!("entity `{}` not found in blueprint", entity_name))?;
        let old = match &entity.count {
            crate::core::CountSpec::Fixed(n) => format!("{}", n),
            other => format!("{:?}", other),
        };
        entity.count = crate::core::CountSpec::Fixed(*count);
        changes.push(format!("{}.count: {} → {} (override)", entity_name, old, count));
    }

    Ok((derived, changes))
}

/// Run the `blueprint derive` command.
pub fn run_derive(
    file: &str,
    scale: Option<&str>,
    counts: &[String],
    seed: Option<u64>,
    locale: Option<&str>,
    variant: Option<&str>,
    excludes: &[String],
    output: Option<&str>,
    json: bool,
) -> Result<()> {
    let model = load_blueprint(file)
        .with_context(|| format!("failed to load `{}`", file))?;

    let scale_factor = match scale {
        Some(s) => Some(parse_scale_factor(s)?),
        None => None,
    };

    // Parse count overrides
    let mut count_overrides = Vec::new();
    for spec in counts {
        let (entity, val) = split_kv(spec, "count")?;
        let count: u64 = val
            .parse()
            .with_context(|| format!("invalid count `{}` for entity `{}`", val, entity))?;
        count_overrides.push((entity, count));
    }

    if scale_factor.is_none() && count_overrides.is_empty() && seed.is_none()
        && locale.is_none() && variant.is_none()
    {
        anyhow::bail!("no derive operations specified (use --scale, --count, --seed, --locale, or --variant)");
    }

    let (derived, changes) = derive_model(
        &model,
        scale_factor,
        &count_overrides,
        excludes,
        seed,
        locale,
        variant,
    )?;

    // Serialize output — always TOML for file, JSON for --json stdout
    let output_str = serialize_model_to_toml(&derived)?;

    if let Some(out_path) = output {
        std::fs::write(out_path, &output_str)
            .with_context(|| format!("failed to write `{}`", out_path))?;
    } else {
        print!("{}", output_str);
    }

    if json {
        let summary = serde_json::json!({
            "changes": changes,
            "output": output.unwrap_or("stdout"),
        });
        eprintln!("{}", serde_json::to_string_pretty(&summary)?);
    } else {
        for change in &changes {
            eprintln!("  {} {}", "▸".green(), change);
        }
        eprintln!(
            "{} derived variant with {} change(s)",
            "✓".green(),
            changes.len()
        );
    }

    Ok(())
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
            blueprint_version: "1.0".to_string(),
            personas: Vec::new(),
            actor_relationships: Vec::new(),
            custom_types: Vec::new(),
            mixins: Vec::new(),
        companion_files: Vec::new(),
        }
    }

    fn make_entity(name: &str, fields: Vec<Field>) -> Entity {
        Entity {
            name: name.to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(100),
            fields,
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
                mixin_refs: None,
        output: None,
        stats: None,
            scaling: None,
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
                stats: None,
                traits: None,
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

    // ── Stats tests ─────────────────────────────────────────────────

    #[test]
    fn stats_empty_model() {
        let model = make_model("empty", vec![]);
        let stats = compute_stats(&model);
        assert_eq!(stats.entities, 0);
        assert_eq!(stats.total_fields, 0);
        assert_eq!(stats.estimated_rows, 0);
        assert!(stats.generator_usage.is_empty());
        assert!(stats.data_type_usage.is_empty());
        assert!(stats.entity_details.is_empty());
    }

    #[test]
    fn stats_counts_fields_and_rows() {
        let model = make_model(
            "test",
            vec![
                make_entity(
                    "users",
                    vec![
                        make_field("id", DataType::Int),
                        make_field("name", DataType::String),
                    ],
                ),
                make_entity("orders", vec![make_field("oid", DataType::Int)]),
            ],
        );
        let stats = compute_stats(&model);
        assert_eq!(stats.entities, 2);
        assert_eq!(stats.total_fields, 3);
        // Each entity has CountSpec::Fixed(100) from make_entity
        assert_eq!(stats.estimated_rows, 200);
        assert_eq!(stats.entity_details.len(), 2);
        assert_eq!(stats.entity_details[0].name, "users");
        assert_eq!(stats.entity_details[0].fields, 2);
        assert_eq!(stats.entity_details[1].name, "orders");
        assert_eq!(stats.entity_details[1].fields, 1);
    }

    #[test]
    fn stats_tracks_generator_usage() {
        use crate::core::GeneratorSpec;

        let mut f1 = make_field("id", DataType::Int);
        f1.generator = Some(GeneratorSpec::Sequence {
            start: crate::core::types::IntOrString::Int(1),
            step: crate::core::types::IntOrString::Int(1),
            prefix: None,
            values: None,
            cycle: None,
            jitter: None,
        });
        let mut f2 = make_field("code", DataType::String);
        f2.generator = Some(GeneratorSpec::Pattern {
            pattern: "###-???".into(),
        });
        let mut f3 = make_field("seq2", DataType::Int);
        f3.generator = Some(GeneratorSpec::Sequence {
            start: crate::core::types::IntOrString::Int(100),
            step: crate::core::types::IntOrString::Int(5),
            prefix: None,
            values: None,
            cycle: None,
            jitter: None,
        });

        let model = make_model("test", vec![make_entity("t", vec![f1, f2, f3])]);
        let stats = compute_stats(&model);

        assert_eq!(stats.generator_usage.get("sequence"), Some(&2));
        assert_eq!(stats.generator_usage.get("pattern"), Some(&1));
        assert_eq!(stats.generator_usage.len(), 2);
    }

    #[test]
    fn stats_tracks_data_type_usage() {
        let model = make_model(
            "test",
            vec![make_entity(
                "t",
                vec![
                    make_field("a", DataType::Int),
                    make_field("b", DataType::Int),
                    make_field("c", DataType::String),
                ],
            )],
        );
        let stats = compute_stats(&model);
        assert_eq!(stats.data_type_usage.get("int"), Some(&2));
        assert_eq!(stats.data_type_usage.get("string"), Some(&1));
    }

    #[test]
    fn stats_nullable_field_count() {
        let mut nf = make_field("opt", DataType::String);
        nf.nullable = NullSpec::Probability(0.5);
        let model = make_model(
            "test",
            vec![make_entity(
                "t",
                vec![make_field("id", DataType::Int), nf],
            )],
        );
        let stats = compute_stats(&model);
        assert_eq!(stats.entity_details[0].nullable_fields, 1);
    }

    #[test]
    fn stats_nullable_nested_fields() {
        let mut child = make_field("inner", DataType::String);
        child.nullable = NullSpec::Probability(0.3);
        let mut parent = make_field("obj", DataType::String);
        parent.fields = vec![child, make_field("solid", DataType::Int)];
        let model = make_model("test", vec![make_entity("t", vec![parent])]);
        let stats = compute_stats(&model);
        // parent itself is Never, but one nested child is nullable
        assert_eq!(stats.entity_details[0].nullable_fields, 1);
    }

    #[test]
    fn stats_scaling_annotated_count() {
        let mut e = make_entity("scaled", vec![make_field("id", DataType::Int)]);
        e.scaling = Some(crate::core::DimensionAnnotation {
            actor: None,
            time: None,
            custom: Vec::new(),
        });
        let model = make_model("test", vec![e]);
        let stats = compute_stats(&model);
        assert_eq!(stats.scaling_annotated, 1);
    }

    #[test]
    fn stats_json_round_trip() {
        let model = make_model(
            "test",
            vec![make_entity("t", vec![make_field("id", DataType::Int)])],
        );
        let stats = compute_stats(&model);
        let json = serde_json::to_string(&stats).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["entities"], 1);
        assert_eq!(parsed["total_fields"], 1);
    }

    // ── Merge tests ─────────────────────────────────────────────────

    #[test]
    fn merge_adds_new_entity() {
        let base = make_model("base", vec![make_entity("users", vec![make_field("id", DataType::Int)])]);
        let overlay = make_model("overlay", vec![make_entity("orders", vec![make_field("oid", DataType::Int)])]);
        let (merged, report) = merge_models(&base, &overlay);
        assert_eq!(merged.entities.len(), 2);
        assert_eq!(report.entities_added, vec!["orders"]);
        assert!(report.entities_merged.is_empty());
    }

    #[test]
    fn merge_appends_new_fields_to_existing_entity() {
        let base = make_model("base", vec![make_entity("users", vec![make_field("id", DataType::Int)])]);
        let overlay = make_model("overlay", vec![make_entity("users", vec![
            make_field("id", DataType::Int),
            make_field("email", DataType::String),
        ])]);
        let (merged, report) = merge_models(&base, &overlay);
        assert_eq!(merged.entities.len(), 1);
        assert_eq!(merged.entities[0].fields.len(), 2);
        assert_eq!(merged.entities[0].fields[1].name, "email");
        assert_eq!(report.entities_merged, vec!["users"]);
        // Warning about duplicate "id" field
        assert!(report.warnings.iter().any(|w| w.contains("field `id`")));
    }

    #[test]
    fn merge_deduplicates_relationships() {
        use crate::core::types::{Relationship, RelationshipKind};
        let mut base = make_model("base", vec![
            make_entity("users", vec![make_field("id", DataType::Int)]),
            make_entity("orders", vec![make_field("id", DataType::Int)]),
        ]);
        base.relationships.push(Relationship {
            name: "orders_users".into(),
            from: "orders".into(),
            to: "users".into(),
            kind: RelationshipKind::OneToMany,
            foreign_key: Some("user_id".into()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });
        let mut overlay = make_model("overlay", vec![]);
        overlay.relationships.push(Relationship {
            name: "orders_users".into(),
            from: "orders".into(),
            to: "users".into(),
            kind: RelationshipKind::OneToMany,
            foreign_key: Some("user_id".into()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });
        overlay.relationships.push(Relationship {
            name: "orders_products".into(),
            from: "orders".into(),
            to: "products".into(),
            kind: RelationshipKind::OneToMany,
            foreign_key: None,
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });
        let (merged, report) = merge_models(&base, &overlay);
        assert_eq!(merged.relationships.len(), 2);
        assert_eq!(report.relationships_added, 1);
    }

    #[test]
    fn merge_params_overlay_wins() {
        let mut base = make_model("base", vec![]);
        base.params.insert("scale".into(), crate::core::types::Value::Int(10));
        let mut overlay = make_model("overlay", vec![]);
        overlay.params.insert("scale".into(), crate::core::types::Value::Int(100));
        overlay.params.insert("new_param".into(), crate::core::types::Value::Int(5));
        let (merged, report) = merge_models(&base, &overlay);
        assert_eq!(merged.params.get("scale"), Some(&crate::core::types::Value::Int(100)));
        assert_eq!(merged.params.get("new_param"), Some(&crate::core::types::Value::Int(5)));
        assert!(report.warnings.iter().any(|w| w.contains("param `scale`")));
    }

    #[test]
    fn merge_empty_overlay_is_identity() {
        let base = make_model("base", vec![
            make_entity("users", vec![make_field("id", DataType::Int)]),
        ]);
        let overlay = make_model("overlay", vec![]);
        let (merged, report) = merge_models(&base, &overlay);
        assert_eq!(merged.entities.len(), 1);
        assert!(report.entities_added.is_empty());
        assert!(report.entities_merged.is_empty());
        assert_eq!(report.relationships_added, 0);
        assert!(report.warnings.is_empty());
    }

    #[test]
    fn merge_report_json_serializable() {
        let base = make_model("base", vec![make_entity("a", vec![make_field("x", DataType::Int)])]);
        let overlay = make_model("overlay", vec![make_entity("b", vec![make_field("y", DataType::String)])]);
        let (_, report) = merge_models(&base, &overlay);
        let json = serde_json::to_string(&report).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["entities_added"][0], "b");
    }

    // ── Graph tests ─────────────────────────────────────────────────

    #[test]
    fn graph_empty_model() {
        let model = make_model("empty", vec![]);
        let graph = build_graph(&model);
        assert!(graph.nodes.is_empty());
        assert!(graph.edges.is_empty());
    }

    #[test]
    fn graph_nodes_from_entities() {
        let model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int), make_field("name", DataType::String)]),
                make_entity("orders", vec![make_field("oid", DataType::Int)]),
            ],
        );
        let graph = build_graph(&model);
        assert_eq!(graph.nodes.len(), 2);
        assert_eq!(graph.nodes[0].name, "users");
        assert_eq!(graph.nodes[0].fields, 2);
        assert_eq!(graph.nodes[0].count, Some(100)); // Fixed(100) from make_entity
        assert_eq!(graph.nodes[1].name, "orders");
        assert_eq!(graph.nodes[1].fields, 1);
    }

    #[test]
    fn graph_edges_from_relationships() {
        use crate::core::types::{Relationship, RelationshipKind};
        let mut model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity("orders", vec![make_field("id", DataType::Int)]),
            ],
        );
        model.relationships.push(Relationship {
            name: "orders_users".into(),
            from: "orders".into(),
            to: "users".into(),
            kind: RelationshipKind::OneToMany,
            foreign_key: Some("user_id".into()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });
        let graph = build_graph(&model);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].from, "orders");
        assert_eq!(graph.edges[0].to, "users");
        assert_eq!(graph.edges[0].kind, "one_to_many");
        assert_eq!(graph.edges[0].foreign_key, Some("user_id".into()));
    }

    #[test]
    fn graph_dot_output_contains_structure() {
        use crate::core::types::{Relationship, RelationshipKind};
        let mut model = make_model(
            "myschema",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity("orders", vec![make_field("oid", DataType::Int)]),
            ],
        );
        model.relationships.push(Relationship {
            name: "fk_orders_users".into(),
            from: "orders".into(),
            to: "users".into(),
            kind: RelationshipKind::OneToMany,
            foreign_key: Some("user_id".into()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });
        let graph = build_graph(&model);
        let dot = render_dot(&model, &graph);
        assert!(dot.contains("digraph \"myschema\""));
        assert!(dot.contains("\"users\""));
        assert!(dot.contains("\"orders\""));
        assert!(dot.contains("\"orders\" -> \"users\""));
        assert!(dot.contains("(user_id)"));
    }

    #[test]
    fn graph_self_referential_dashed() {
        use crate::core::types::{Relationship, RelationshipKind};
        let mut model = make_model(
            "test",
            vec![make_entity("categories", vec![make_field("id", DataType::Int)])],
        );
        model.relationships.push(Relationship {
            name: "self_ref".into(),
            from: "categories".into(),
            to: "categories".into(),
            kind: RelationshipKind::OneToMany,
            foreign_key: Some("parent_id".into()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });
        let graph = build_graph(&model);
        let dot = render_dot(&model, &graph);
        assert!(dot.contains("style=dashed"));
        assert!(dot.contains("(self)"));
    }

    #[test]
    fn graph_json_output() {
        let model = make_model(
            "test",
            vec![make_entity("t", vec![make_field("id", DataType::Int)])],
        );
        let graph = build_graph(&model);
        let json = serde_json::to_string(&graph).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["nodes"][0]["name"], "t");
        assert_eq!(parsed["nodes"][0]["fields"], 1);
    }

    #[test]
    fn graph_actor_node_flag() {
        let mut model = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        model.entities[0].actor = true;
        let graph = build_graph(&model);
        assert!(graph.nodes[0].actor);
        let dot = render_dot(&model, &graph);
        assert!(dot.contains("#d4edda")); // actor node color
    }

    #[test]
    fn graph_includes_actor_relationships() {
        use crate::core::types::ActorRelationship;
        let mut model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity("teams", vec![make_field("id", DataType::Int)]),
            ],
        );
        model.actor_relationships.push(ActorRelationship {
            name: "collab_network".into(),
            from_entity: "users".into(),
            to_entity: "teams".into(),
            graph_type: Default::default(),
            params: std::collections::BTreeMap::new(),
            community_count: None,
            hierarchy_depth: None,
        });
        let graph = build_graph(&model);
        assert_eq!(graph.edges.len(), 1);
        assert_eq!(graph.edges[0].name, "collab_network");
        assert!(graph.edges[0].kind.starts_with("actor_"));
        assert!(graph.edges[0].foreign_key.is_none());
    }

    #[test]
    fn graph_dot_escapes_special_chars() {
        let model = make_model(
            "my|schema",
            vec![make_entity("user\"data", vec![make_field("id", DataType::Int)])],
        );
        let graph = build_graph(&model);
        let dot = render_dot(&model, &graph);
        assert!(dot.contains("my\\|schema"));
        assert!(dot.contains("user\\\"data"));
        // Should not contain unescaped metacharacters in DOT strings
        assert!(!dot.contains("\"my|schema\""));
    }

    #[test]
    fn graph_mermaid_basic() {
        let model = make_model(
            "test",
            vec![
                make_entity(
                    "users",
                    vec![
                        {
                            let mut f = make_field("id", DataType::Int);
                            f.primary_key = Some(true);
                            f
                        },
                        make_field("name", DataType::String),
                    ],
                ),
                make_entity(
                    "orders",
                    vec![
                        {
                            let mut f = make_field("id", DataType::Int);
                            f.primary_key = Some(true);
                            f
                        },
                        make_field("user_id", DataType::Int),
                    ],
                ),
            ],
        );
        let graph = build_graph(&model);
        let mermaid = render_mermaid(&model, &graph);
        assert!(mermaid.starts_with("erDiagram\n"));
        assert!(mermaid.contains("users {"));
        assert!(mermaid.contains("orders {"));
        assert!(mermaid.contains("int id PK"));
        assert!(mermaid.contains("string name"));
    }

    #[test]
    fn graph_mermaid_with_relationships() {
        let mut model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity("orders", vec![make_field("user_id", DataType::Int)]),
            ],
        );
        model.relationships.push(make_relationship("r1", "orders", "users"));
        model.relationships[0].foreign_key = Some("user_id".into());
        let graph = build_graph(&model);
        let mermaid = render_mermaid(&model, &graph);
        // Should contain relationship line with cardinality
        assert!(mermaid.contains("users ||--o{ orders"));
        assert!(mermaid.contains("user_id"));
    }

    #[test]
    fn graph_mermaid_fk_annotation() {
        let mut model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity(
                    "orders",
                    vec![
                        make_field("id", DataType::Int),
                        make_field("user_id", DataType::Int),
                    ],
                ),
            ],
        );
        model.relationships.push(make_relationship("r1", "orders", "users"));
        model.relationships[0].foreign_key = Some("user_id".into());
        let graph = build_graph(&model);
        let mermaid = render_mermaid(&model, &graph);
        // user_id field should be annotated as FK
        assert!(mermaid.contains("int user_id FK"));
    }

    #[test]
    fn graph_mermaid_implicit_fk() {
        // Implicit FK: no foreign_key set, defaults to "{to}_id"
        let mut model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity(
                    "orders",
                    vec![
                        make_field("id", DataType::Int),
                        make_field("users_id", DataType::Int),
                    ],
                ),
            ],
        );
        model.relationships.push(make_relationship("r1", "orders", "users"));
        // foreign_key is None → defaults to "users_id"
        let graph = build_graph(&model);
        let mermaid = render_mermaid(&model, &graph);
        // Implicit FK field should still be annotated
        assert!(mermaid.contains("int users_id FK"));
        // Relationship label should show the effective FK
        assert!(mermaid.contains("users_id"));
    }

    #[test]
    fn graph_mermaid_special_chars() {
        let model = make_model(
            "test",
            vec![make_entity("user data", vec![make_field("first name", DataType::String)])],
        );
        let graph = build_graph(&model);
        let mermaid = render_mermaid(&model, &graph);
        // Names with spaces get quoted
        assert!(mermaid.contains("\"user data\" {"));
        assert!(mermaid.contains("\"first name\""));
    }

    // ── Lint tests ──────────────────────────────────────────────────

    #[test]
    fn lint_clean_model() {
        let mut model = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        model.entities[0].description = Some("User table".into());
        let findings = lint_model(&model);
        // Single entity: no orphan check, has description
        assert!(findings.is_empty(), "unexpected findings: {:?}", findings);
    }

    #[test]
    fn lint_empty_entity() {
        let model = make_model("test", vec![make_entity("empty", vec![])]);
        let findings = lint_model(&model);
        assert!(findings.iter().any(|f| f.message.contains("no fields")));
    }

    #[test]
    fn lint_missing_description() {
        let model = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        let findings = lint_model(&model);
        assert!(findings.iter().any(|f| f.message.contains("no description")));
    }

    #[test]
    fn lint_orphan_entity() {
        let model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity("logs", vec![make_field("id", DataType::Int)]),
            ],
        );
        // Both entities are orphans (no relationships), not actors
        let findings = lint_model(&model);
        let orphan_findings: Vec<_> = findings
            .iter()
            .filter(|f| f.message.contains("not referenced"))
            .collect();
        assert_eq!(orphan_findings.len(), 2);
    }

    #[test]
    fn lint_actor_not_orphan() {
        let mut model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity("logs", vec![make_field("id", DataType::Int)]),
            ],
        );
        model.entities[0].actor = true;
        let findings = lint_model(&model);
        // users (actor) should not be flagged as orphan, but logs should
        let orphans: Vec<_> = findings
            .iter()
            .filter(|f| f.message.contains("not referenced"))
            .collect();
        assert_eq!(orphans.len(), 1);
        assert_eq!(orphans[0].entity, Some("logs".into()));
    }

    #[test]
    fn lint_dangling_relationship() {
        use crate::core::types::{Relationship, RelationshipKind};
        let mut model = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        model.relationships.push(Relationship {
            name: "bad_rel".into(),
            from: "orders".into(),
            to: "products".into(),
            kind: RelationshipKind::OneToMany,
            foreign_key: None,
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });
        let findings = lint_model(&model);
        assert!(findings.iter().any(|f| f.message.contains("`orders` does not exist")));
        assert!(findings.iter().any(|f| f.message.contains("`products` does not exist")));
    }

    #[test]
    fn lint_self_ref_without_acyclic() {
        use crate::core::types::{Relationship, RelationshipKind};
        let mut model = make_model(
            "test",
            vec![make_entity("categories", vec![make_field("id", DataType::Int)])],
        );
        model.relationships.push(Relationship {
            name: "self_ref".into(),
            from: "categories".into(),
            to: "categories".into(),
            kind: RelationshipKind::OneToMany,
            foreign_key: Some("parent_id".into()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });
        let findings = lint_model(&model);
        assert!(findings.iter().any(|f| f.message.contains("acyclic")));
    }

    #[test]
    fn lint_json_output() {
        let model = make_model("test", vec![make_entity("t", vec![])]);
        let findings = lint_model(&model);
        let json = serde_json::to_string(&findings).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(parsed.as_array().unwrap().len() > 0);
        assert!(parsed[0]["severity"].is_string());
        assert!(parsed[0]["message"].is_string());
    }

    // -----------------------------------------------------------------------
    // Subset tests
    // -----------------------------------------------------------------------

    use crate::core::types::{Relationship, RelationshipKind};

    fn make_relationship(name: &str, from: &str, to: &str) -> Relationship {
        Relationship {
            name: name.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            kind: RelationshipKind::OneToMany,
            foreign_key: None,
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        }
    }

    #[test]
    fn subset_single_entity_no_deps() {
        let model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity("orders", vec![make_field("id", DataType::Int)]),
            ],
        );
        let subset = subset_model(&model, &["orders".into()], false);
        assert_eq!(subset.entities.len(), 1);
        assert_eq!(subset.entities[0].name, "orders");
    }

    #[test]
    fn subset_includes_transitive_deps() {
        let mut model = make_model(
            "test",
            vec![
                make_entity("regions", vec![make_field("id", DataType::Int)]),
                make_entity("stores", vec![make_field("id", DataType::Int)]),
                make_entity("orders", vec![make_field("id", DataType::Int)]),
            ],
        );
        model.relationships = vec![
            make_relationship("orders_stores", "orders", "stores"),
            make_relationship("stores_regions", "stores", "regions"),
        ];
        let subset = subset_model(&model, &["orders".into()], true);
        assert_eq!(subset.entities.len(), 3);
        assert_eq!(subset.relationships.len(), 2);
    }

    #[test]
    fn subset_filters_unrelated_relationships() {
        let mut model = make_model(
            "test",
            vec![
                make_entity("a", vec![make_field("id", DataType::Int)]),
                make_entity("b", vec![make_field("id", DataType::Int)]),
                make_entity("c", vec![make_field("id", DataType::Int)]),
            ],
        );
        model.relationships = vec![
            make_relationship("a_b", "a", "b"),
            make_relationship("c_b", "c", "b"),
        ];
        // Select only "a" with deps -> pulls in "b", but "c" excluded.
        let subset = subset_model(&model, &["a".into()], true);
        assert_eq!(subset.entities.len(), 2); // a, b
        assert_eq!(subset.relationships.len(), 1); // a_b only
        assert_eq!(subset.relationships[0].name, "a_b");
    }

    #[test]
    fn subset_filters_correlations() {
        let mut model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("age", DataType::Int)]),
                make_entity("orders", vec![make_field("amount", DataType::Float)]),
            ],
        );
        model.correlations = vec![
            crate::core::Correlation {
                entity: "users".into(),
                correlation_type: None,
                fields: vec!["age".into()],
                matrix: vec![],
                conditional: vec![],
                copula: None,
                dependent: None,
                given: None,
                distributions: vec![],
                default: None,
            },
            crate::core::Correlation {
                entity: "orders".into(),
                correlation_type: None,
                fields: vec!["amount".into()],
                matrix: vec![],
                conditional: vec![],
                copula: None,
                dependent: None,
                given: None,
                distributions: vec![],
                default: None,
            },
        ];
        let subset = subset_model(&model, &["users".into()], false);
        assert_eq!(subset.correlations.len(), 1);
        assert_eq!(subset.correlations[0].entity, "users");
    }

    #[test]
    fn subset_unknown_entity_ignored() {
        let model = make_model(
            "test",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        let subset = subset_model(&model, &["nonexistent".into()], true);
        assert_eq!(subset.entities.len(), 0);
    }

    #[test]
    fn subset_preserves_model_metadata() {
        let mut model = make_model(
            "my_model",
            vec![make_entity("users", vec![make_field("id", DataType::Int)])],
        );
        model.description = Some("A test model".into());
        model.locale = "de_DE".into();
        let subset = subset_model(&model, &["users".into()], true);
        assert_eq!(subset.name, "my_model");
        assert_eq!(subset.description.as_deref(), Some("A test model"));
        assert_eq!(subset.locale, "de_DE");
    }

    // -----------------------------------------------------------------------
    // Rename tests
    // -----------------------------------------------------------------------

    #[test]
    fn rename_entity_basic() {
        let mut model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity("orders", vec![make_field("id", DataType::Int)]),
            ],
        );
        model.relationships = vec![make_relationship("orders_users", "orders", "users")];

        let entity_renames = BTreeMap::from([("users".to_string(), "customers".to_string())]);
        let (renamed, updates) = rename_in_model(&model, &entity_renames, &BTreeMap::new());

        assert_eq!(renamed.entities[0].name, "customers");
        assert_eq!(renamed.relationships[0].to, "customers");
        assert!(updates >= 2); // entity name + relationship ref
    }

    #[test]
    fn rename_field_basic() {
        let model = make_model(
            "test",
            vec![make_entity(
                "users",
                vec![
                    make_field("id", DataType::Int),
                    make_field("user_name", DataType::String),
                ],
            )],
        );

        let field_renames = BTreeMap::from([(
            ("users".to_string(), "user_name".to_string()),
            "username".to_string(),
        )]);
        let (renamed, updates) = rename_in_model(&model, &BTreeMap::new(), &field_renames);

        assert_eq!(renamed.entities[0].fields[1].name, "username");
        assert!(updates >= 1);
    }

    #[test]
    fn rename_entity_updates_correlations() {
        let mut model = make_model(
            "test",
            vec![make_entity(
                "users",
                vec![make_field("age", DataType::Int)],
            )],
        );
        model.correlations = vec![crate::core::Correlation {
            entity: "users".into(),
            correlation_type: None,
            fields: vec!["age".into()],
            matrix: vec![],
            conditional: vec![],
            copula: None,
            dependent: None,
            given: None,
            distributions: vec![],
            default: None,
        }];

        let entity_renames = BTreeMap::from([("users".to_string(), "people".to_string())]);
        let (renamed, _) = rename_in_model(&model, &entity_renames, &BTreeMap::new());

        assert_eq!(renamed.correlations[0].entity, "people");
    }

    #[test]
    fn rename_entity_updates_noise_profiles() {
        let mut model = make_model(
            "test",
            vec![make_entity(
                "users",
                vec![make_field("name", DataType::String)],
            )],
        );
        model.noise_profiles = vec![crate::core::NoiseProfile {
            name: "typo".into(),
            entity: "users".into(),
            fields: vec!["name".into()],
            null_rate: 0.0,
            typo_rate: 0.1,
            outlier_rate: 0.0,
            swap_rate: 0.0,
            duplicate_rate: 0.0,
            truncate_rate: 0.0,
            fk_violate_rate: 0.0,
            temporal_spike_rate: 0.0,
            missing_field_rate: 0.0,
            scope: None,
        }];

        let entity_renames = BTreeMap::from([("users".to_string(), "people".to_string())]);
        let (renamed, _) = rename_in_model(&model, &entity_renames, &BTreeMap::new());

        assert_eq!(renamed.noise_profiles[0].entity, "people");
    }

    #[test]
    fn rename_entity_updates_lookup_generator() {
        let model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity("orders", vec![{
                    let mut f = make_field("user_name", DataType::String);
                    f.generator = Some(crate::core::GeneratorSpec::Lookup {
                        entity: "users".into(),
                        field: "name".into(),
                    });
                    f
                }]),
            ],
        );

        let entity_renames = BTreeMap::from([("users".to_string(), "customers".to_string())]);
        let (renamed, updates) = rename_in_model(&model, &entity_renames, &BTreeMap::new());

        match &renamed.entities[1].fields[0].generator {
            Some(crate::core::GeneratorSpec::Lookup { entity, .. }) => {
                assert_eq!(entity, "customers");
            }
            _ => panic!("expected Lookup generator"),
        }
        assert!(updates >= 2); // entity name + lookup ref
    }

    #[test]
    fn rename_parse_specs() {
        let (old, new) = parse_rename_spec("Users=Customers").unwrap();
        assert_eq!(old, "Users");
        assert_eq!(new, "Customers");

        let (entity, old_f, new_f) = parse_field_rename_spec("Users.name=full_name").unwrap();
        assert_eq!(entity, "Users");
        assert_eq!(old_f, "name");
        assert_eq!(new_f, "full_name");

        assert!(parse_rename_spec("noequals").is_err());
        assert!(parse_field_rename_spec("nodot=new").is_err());
    }

    #[test]
    fn rename_entity_preserves_implicit_fk() {
        let mut model = make_model(
            "test",
            vec![
                make_entity("users", vec![make_field("id", DataType::Int)]),
                make_entity("orders", vec![make_field("id", DataType::Int)]),
            ],
        );
        // Relationship with implicit FK (no explicit foreign_key).
        model.relationships = vec![make_relationship("orders_users", "orders", "users")];
        assert!(model.relationships[0].foreign_key.is_none());

        let entity_renames = BTreeMap::from([("users".to_string(), "customers".to_string())]);
        let (renamed, _) = rename_in_model(&model, &entity_renames, &BTreeMap::new());

        // After rename, FK should be explicitly set to preserve the original "users_id".
        assert_eq!(renamed.relationships[0].to, "customers");
        assert_eq!(
            renamed.relationships[0].foreign_key.as_deref(),
            Some("users_id")
        );
    }

    #[test]
    fn rename_entity_updates_relative_anchor() {
        let model = make_model(
            "test",
            vec![make_entity(
                "events",
                vec![
                    make_field("start_time", DataType::Datetime),
                    {
                        let mut f = make_field("end_time", DataType::Datetime);
                        f.generator = Some(crate::core::GeneratorSpec::Relative {
                            anchor: "start_time".into(),
                            offset: crate::core::types::RelativeOffset::Simple(crate::core::Value::Float(3600.0)),
                        });
                        f
                    },
                ],
            )],
        );

        let field_renames = BTreeMap::from([(
            ("events".to_string(), "start_time".to_string()),
            "begin_time".to_string(),
        )]);
        let (renamed, _) = rename_in_model(&model, &BTreeMap::new(), &field_renames);

        // The Relative anchor should also be renamed.
        match &renamed.entities[0].fields[1].generator {
            Some(crate::core::GeneratorSpec::Relative { anchor, .. }) => {
                assert_eq!(anchor, "begin_time");
            }
            _ => panic!("expected Relative generator"),
        }
    }

    // -----------------------------------------------------------------------
    // Export SQL tests
    // -----------------------------------------------------------------------

    #[test]
    fn export_sql_basic_table() {
        let model = make_model(
            "test",
            vec![make_entity(
                "users",
                vec![
                    {
                        let mut f = make_field("id", DataType::Int);
                        f.primary_key = Some(true);
                        f
                    },
                    make_field("name", DataType::String),
                    make_field("active", DataType::Bool),
                ],
            )],
        );

        let sql = export_sql(&model, SqlDialect::Postgres, false);
        assert!(sql.contains("CREATE TABLE \"users\""));
        assert!(sql.contains("\"id\" BIGINT PRIMARY KEY"));
        assert!(sql.contains("\"name\" TEXT NOT NULL"));
        assert!(sql.contains("\"active\" BOOLEAN NOT NULL"));
    }

    #[test]
    fn export_sql_with_foreign_keys() {
        let mut model = make_model(
            "test",
            vec![
                make_entity("users", vec![{
                    let mut f = make_field("id", DataType::Int);
                    f.primary_key = Some(true);
                    f
                }]),
                make_entity("orders", vec![
                    {
                        let mut f = make_field("id", DataType::Int);
                        f.primary_key = Some(true);
                        f
                    },
                    make_field("user_id", DataType::Int),
                ]),
            ],
        );
        model.relationships = vec![make_relationship("orders_users", "orders", "users")];

        let sql = export_sql(&model, SqlDialect::Postgres, true);
        assert!(sql.contains("ALTER TABLE \"orders\" ADD FOREIGN KEY (\"users_id\") REFERENCES \"users\" (\"id\")"));
    }

    #[test]
    fn export_sql_mysql_dialect() {
        let model = make_model(
            "test",
            vec![make_entity(
                "items",
                vec![
                    make_field("id", DataType::Int),
                    make_field("name", DataType::String),
                ],
            )],
        );

        let sql = export_sql(&model, SqlDialect::Mysql, false);
        assert!(sql.contains("CREATE TABLE `items`"));
        assert!(sql.contains("`name` VARCHAR(255)"));
    }

    #[test]
    fn export_sql_nullable_field() {
        let model = make_model(
            "test",
            vec![make_entity(
                "users",
                vec![{
                    let mut f = make_field("bio", DataType::String);
                    f.nullable = NullSpec::Probability(0.3);
                    f
                }],
            )],
        );

        let sql = export_sql(&model, SqlDialect::Postgres, false);
        // Nullable field should NOT have "NOT NULL".
        assert!(sql.contains("\"bio\" TEXT"));
        assert!(!sql.contains("\"bio\" TEXT NOT NULL"));
    }

    #[test]
    fn export_sql_type_mapping() {
        let model = make_model(
            "test",
            vec![make_entity(
                "events",
                vec![
                    make_field("id", DataType::Uuid),
                    make_field("created_at", DataType::Datetime),
                    make_field("updated_at", DataType::Datetimetz),
                    make_field("data", DataType::Bytes),
                    make_field("payload", DataType::Map),
                ],
            )],
        );

        let pg = export_sql(&model, SqlDialect::Postgres, false);
        assert!(pg.contains("UUID"));
        assert!(pg.contains("TIMESTAMP"));
        assert!(pg.contains("TIMESTAMPTZ"));
        assert!(pg.contains("BYTEA"));
        assert!(pg.contains("JSONB"));

        let sl = export_sql(&model, SqlDialect::Sqlite, false);
        assert!(sl.contains("CHAR(36)")); // UUID → CHAR(36)
        assert!(sl.contains("BLOB"));     // Bytes → BLOB
    }

    #[test]
    fn export_sql_dialect_parsing() {
        assert!(SqlDialect::from_str("postgres").is_ok());
        assert!(SqlDialect::from_str("pg").is_ok());
        assert!(SqlDialect::from_str("mysql").is_ok());
        assert!(SqlDialect::from_str("sqlite").is_ok());
        assert!(SqlDialect::from_str("oracle").is_err());
    }

    // -----------------------------------------------------------------------
    // Scaffold tests
    // -----------------------------------------------------------------------

    #[test]
    fn scaffold_basic_entity() {
        let model = scaffold_model(
            "test",
            &["Users:id:int,name:string,email:string".into()],
            &[],
        )
        .unwrap();

        assert_eq!(model.entities.len(), 1);
        assert_eq!(model.entities[0].name, "Users");
        assert_eq!(model.entities[0].fields.len(), 3);
        assert_eq!(model.entities[0].fields[0].name, "id");
        assert_eq!(model.entities[0].fields[0].data_type, DataType::Int);
        assert_eq!(model.entities[0].fields[0].primary_key, Some(true));
        assert_eq!(model.entities[0].fields[1].data_type, DataType::String);
    }

    #[test]
    fn scaffold_with_count() {
        let model = scaffold_model(
            "test",
            &["Products:5000:id:int,name:string,price:float".into()],
            &[],
        )
        .unwrap();

        assert_eq!(model.entities[0].count, CountSpec::Fixed(5000));
        assert_eq!(model.entities[0].fields.len(), 3);
    }

    #[test]
    fn scaffold_with_relationship() {
        let model = scaffold_model(
            "test",
            &[
                "Users:id:int,name:string".into(),
                "Orders:id:int,amount:float".into(),
            ],
            &["Orders.user_id=Users.id".into()],
        )
        .unwrap();

        assert_eq!(model.relationships.len(), 1);
        assert_eq!(model.relationships[0].from, "Orders");
        assert_eq!(model.relationships[0].to, "Users");
        assert_eq!(
            model.relationships[0].foreign_key.as_deref(),
            Some("user_id")
        );
    }

    #[test]
    fn scaffold_implicit_relationship() {
        let model = scaffold_model(
            "test",
            &[
                "Users:id:int".into(),
                "Orders:id:int".into(),
            ],
            &["Orders=Users".into()],
        )
        .unwrap();

        assert_eq!(model.relationships[0].from, "Orders");
        assert_eq!(model.relationships[0].to, "Users");
        assert!(model.relationships[0].foreign_key.is_none());
    }

    #[test]
    fn scaffold_type_aliases() {
        let model = scaffold_model(
            "test",
            &["T:a:integer,b:varchar,c:boolean,d:timestamp,e:blob".into()],
            &[],
        )
        .unwrap();

        assert_eq!(model.entities[0].fields[0].data_type, DataType::Int);
        assert_eq!(model.entities[0].fields[1].data_type, DataType::String);
        assert_eq!(model.entities[0].fields[2].data_type, DataType::Bool);
        assert_eq!(model.entities[0].fields[3].data_type, DataType::Datetime);
        assert_eq!(model.entities[0].fields[4].data_type, DataType::Bytes);
    }

    #[test]
    fn scaffold_default_type_is_string() {
        let model = scaffold_model("test", &["T:name,email".into()], &[]).unwrap();

        assert_eq!(model.entities[0].fields[0].data_type, DataType::String);
        assert_eq!(model.entities[0].fields[1].data_type, DataType::String);
    }

    #[test]
    fn scaffold_invalid_specs() {
        assert!(scaffold_model("test", &["".into()], &[]).is_err());
        assert!(scaffold_model("test", &["NoFields:".into()], &[]).is_err());
        assert!(parse_rel_spec("noequals").is_err());
    }

    #[test]
    fn scaffold_rejects_non_pk_target() {
        let err = scaffold_model(
            "test",
            &[
                "Users:id:int,email:string".into(),
                "Orders:id:int,user_email:string".into(),
            ],
            &["Orders.user_email=Users.email".into()],
        );
        assert!(err.is_err());
        let msg = err.unwrap_err().to_string();
        assert!(msg.contains("not a primary key"));
    }

    #[test]
    fn scaffold_generates_valid_toml() {
        let model = scaffold_model(
            "ecommerce",
            &[
                "Users:id:int,name:string".into(),
                "Orders:id:int,total:float".into(),
            ],
            &["Orders.user_id=Users.id".into()],
        )
        .unwrap();

        let toml = serialize_model_to_toml(&model).unwrap();
        assert!(toml.contains("name = \"ecommerce\""));
        assert!(toml.contains("[[entities]]"));
        assert!(toml.contains("[[relationships]]"));
    }

    // -----------------------------------------------------------------------
    // Import tests
    // -----------------------------------------------------------------------

    #[test]
    fn import_basic_table() {
        let sql = r#"
            CREATE TABLE users (
                id BIGINT PRIMARY KEY,
                name VARCHAR(255) NOT NULL,
                email TEXT
            );
        "#;
        let model = import_sql(sql, "test").unwrap();
        assert_eq!(model.entities.len(), 1);
        assert_eq!(model.entities[0].name, "users");
        assert_eq!(model.entities[0].fields.len(), 3);
        assert_eq!(model.entities[0].fields[0].name, "id");
        assert_eq!(model.entities[0].fields[0].data_type, DataType::Int);
        assert_eq!(model.entities[0].fields[0].primary_key, Some(true));
        assert_eq!(model.entities[0].fields[1].data_type, DataType::String);
        // email has no NOT NULL → should be nullable
        assert!(matches!(
            model.entities[0].fields[2].nullable,
            NullSpec::Probability(_)
        ));
    }

    #[test]
    fn import_with_foreign_keys() {
        let sql = r#"
            CREATE TABLE users (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL
            );
            CREATE TABLE orders (
                id INTEGER PRIMARY KEY,
                user_id INTEGER NOT NULL,
                total DOUBLE PRECISION
            );
            ALTER TABLE orders ADD FOREIGN KEY (user_id) REFERENCES users (id);
        "#;
        let model = import_sql(sql, "test").unwrap();
        assert_eq!(model.entities.len(), 2);
        assert_eq!(model.relationships.len(), 1);
        assert_eq!(model.relationships[0].from, "orders");
        assert_eq!(model.relationships[0].to, "users");
        assert_eq!(
            model.relationships[0].foreign_key.as_deref(),
            Some("user_id")
        );
    }

    #[test]
    fn import_inline_references() {
        let sql = r#"
            CREATE TABLE departments (
                id SERIAL PRIMARY KEY,
                name TEXT NOT NULL
            );
            CREATE TABLE employees (
                id SERIAL PRIMARY KEY,
                dept_id INTEGER REFERENCES departments(id),
                name TEXT NOT NULL
            );
        "#;
        let model = import_sql(sql, "test").unwrap();
        assert_eq!(model.relationships.len(), 1);
        assert_eq!(model.relationships[0].from, "employees");
        assert_eq!(model.relationships[0].to, "departments");
    }

    #[test]
    fn import_table_level_pk() {
        let sql = r#"
            CREATE TABLE items (
                item_id BIGINT NOT NULL,
                name VARCHAR(100),
                PRIMARY KEY (item_id)
            );
        "#;
        let model = import_sql(sql, "test").unwrap();
        assert_eq!(model.entities[0].fields[0].primary_key, Some(true));
    }

    #[test]
    fn import_type_mapping() {
        let sql = r#"
            CREATE TABLE types_test (
                a BOOLEAN,
                b BIGINT,
                c INTEGER,
                d DOUBLE PRECISION,
                e TEXT,
                f UUID,
                g DATE,
                h TIME,
                i TIMESTAMP,
                j TIMESTAMPTZ,
                k INTERVAL,
                l BYTEA,
                m JSONB
            );
        "#;
        let model = import_sql(sql, "test").unwrap();
        let fields = &model.entities[0].fields;
        assert_eq!(fields[0].data_type, DataType::Bool);
        assert_eq!(fields[1].data_type, DataType::Int);
        assert_eq!(fields[2].data_type, DataType::Int32);
        assert_eq!(fields[3].data_type, DataType::Float);
        assert_eq!(fields[4].data_type, DataType::String);
        assert_eq!(fields[5].data_type, DataType::Uuid);
        assert_eq!(fields[6].data_type, DataType::Date);
        assert_eq!(fields[7].data_type, DataType::Time);
        assert_eq!(fields[8].data_type, DataType::Datetime);
        assert_eq!(fields[9].data_type, DataType::Datetimetz);
        assert_eq!(fields[10].data_type, DataType::Duration);
        assert_eq!(fields[11].data_type, DataType::Bytes);
        assert_eq!(fields[12].data_type, DataType::Map);
    }

    #[test]
    fn import_mysql_syntax() {
        let sql = r#"
            CREATE TABLE `products` (
                `id` INT AUTO_INCREMENT PRIMARY KEY,
                `name` VARCHAR(255) NOT NULL,
                `price` DECIMAL(10,2)
            );
        "#;
        let model = import_sql(sql, "test").unwrap();
        assert_eq!(model.entities[0].name, "products");
        assert_eq!(model.entities[0].fields[0].primary_key, Some(true));
    }

    #[test]
    fn import_comments_stripped() {
        let sql = r#"
            -- This is a users table
            CREATE TABLE users (
                id BIGINT PRIMARY KEY, /* the primary key */
                name TEXT NOT NULL
            );
        "#;
        let model = import_sql(sql, "test").unwrap();
        assert_eq!(model.entities[0].fields.len(), 2);
    }

    #[test]
    fn import_empty_sql_fails() {
        assert!(import_sql("", "test").is_err());
        assert!(import_sql("-- just a comment", "test").is_err());
    }

    #[test]
    fn import_table_level_fk() {
        let sql = r#"
            CREATE TABLE parents (id INTEGER PRIMARY KEY);
            CREATE TABLE children (
                id INTEGER PRIMARY KEY,
                parent_id INTEGER NOT NULL,
                FOREIGN KEY (parent_id) REFERENCES parents (id)
            );
        "#;
        let model = import_sql(sql, "test").unwrap();
        assert_eq!(model.relationships.len(), 1);
        assert_eq!(model.relationships[0].from, "children");
        assert_eq!(model.relationships[0].to, "parents");
    }

    #[test]
    fn import_roundtrip_with_export() {
        // Build a model, export to SQL, then import back and check entities match.
        let original = scaffold_model(
            "roundtrip",
            &[
                "Users:id:int,name:string,email:string".into(),
                "Orders:id:int,total:float".into(),
            ],
            &["Orders.user_id=Users.id".into()],
        )
        .unwrap();

        let sql = export_sql(&original, SqlDialect::Postgres, true);
        let imported = import_sql(&sql, "roundtrip").unwrap();

        assert_eq!(imported.entities.len(), 2);
        assert_eq!(imported.relationships.len(), 1);
        // Entity names should match (scaffold uses title case)
        let entity_names: Vec<&str> = imported.entities.iter().map(|e| e.name.as_str()).collect();
        assert!(entity_names.contains(&"Users"));
        assert!(entity_names.contains(&"Orders"));
    }

    #[test]
    fn import_named_constraint_pk() {
        let sql = r#"
            CREATE TABLE accounts (
                account_id BIGINT NOT NULL,
                name TEXT NOT NULL,
                CONSTRAINT pk_accounts PRIMARY KEY (account_id)
            );
        "#;
        let model = import_sql(sql, "test").unwrap();
        assert_eq!(model.entities[0].fields[0].primary_key, Some(true));
    }

    #[test]
    fn import_string_literal_with_comma() {
        let sql = r#"
            CREATE TABLE items (
                id INTEGER PRIMARY KEY,
                note TEXT DEFAULT 'a,b' NOT NULL
            );
        "#;
        let model = import_sql(sql, "test").unwrap();
        assert_eq!(model.entities[0].fields.len(), 2);
    }

    #[test]
    fn import_multiple_fks_unique_names() {
        let sql = r#"
            CREATE TABLE users (id INTEGER PRIMARY KEY);
            CREATE TABLE tasks (
                id INTEGER PRIMARY KEY,
                created_by INTEGER NOT NULL,
                approved_by INTEGER NOT NULL
            );
            ALTER TABLE tasks ADD FOREIGN KEY (created_by) REFERENCES users (id);
            ALTER TABLE tasks ADD FOREIGN KEY (approved_by) REFERENCES users (id);
        "#;
        let model = import_sql(sql, "test").unwrap();
        assert_eq!(model.relationships.len(), 2);
        // Names should be unique
        assert_ne!(model.relationships[0].name, model.relationships[1].name);
    }

    // -----------------------------------------------------------------------
    // Update tests
    // -----------------------------------------------------------------------

    #[test]
    fn update_set_count() {
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        let ops = vec![UpdateOp::SetCount {
            entity: "Users".into(),
            count: 5000,
        }];
        let changes = update_model(&mut model, &ops).unwrap();
        assert_eq!(model.entities[0].count, CountSpec::Fixed(5000));
        assert_eq!(changes.len(), 1);
        assert!(changes[0].contains("5000"));
    }

    #[test]
    fn update_set_entity_description() {
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        let ops = vec![UpdateOp::SetEntityDesc {
            entity: "Users".into(),
            desc: "User accounts".into(),
        }];
        update_model(&mut model, &ops).unwrap();
        assert_eq!(
            model.entities[0].description.as_deref(),
            Some("User accounts")
        );
    }

    #[test]
    fn update_set_field_description() {
        let mut model = make_model(
            "test",
            vec![make_entity(
                "Users",
                vec![make_field("email", DataType::String)],
            )],
        );
        let ops = vec![UpdateOp::SetFieldDesc {
            entity: "Users".into(),
            field: "email".into(),
            desc: "Primary email address".into(),
        }];
        update_model(&mut model, &ops).unwrap();
        assert_eq!(
            model.entities[0].fields[0].description.as_deref(),
            Some("Primary email address")
        );
    }

    #[test]
    fn update_add_tags() {
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        let ops = vec![UpdateOp::AddTags {
            entity: "Users".into(),
            tags: vec!["pii".into(), "core".into()],
        }];
        update_model(&mut model, &ops).unwrap();
        assert_eq!(model.entities[0].tags, vec!["pii", "core"]);
    }

    #[test]
    fn update_add_tags_no_duplicates() {
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        model.entities[0].tags = vec!["core".into()];
        let ops = vec![UpdateOp::AddTags {
            entity: "Users".into(),
            tags: vec!["core".into(), "pii".into()],
        }];
        update_model(&mut model, &ops).unwrap();
        assert_eq!(model.entities[0].tags, vec!["core", "pii"]);
    }

    #[test]
    fn update_remove_tags() {
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        model.entities[0].tags = vec!["pii".into(), "core".into(), "test".into()];
        let ops = vec![UpdateOp::RemoveTags {
            entity: "Users".into(),
            tags: vec!["pii".into(), "test".into()],
        }];
        update_model(&mut model, &ops).unwrap();
        assert_eq!(model.entities[0].tags, vec!["core"]);
    }

    #[test]
    fn update_set_seed() {
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        let ops = vec![UpdateOp::SetSeed { seed: 12345 }];
        update_model(&mut model, &ops).unwrap();
        assert_eq!(model.seed, 12345);
    }

    #[test]
    fn update_set_locale() {
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        let ops = vec![UpdateOp::SetLocale {
            locale: "de_DE".into(),
        }];
        update_model(&mut model, &ops).unwrap();
        assert_eq!(model.locale, "de_DE");
    }

    #[test]
    fn update_unknown_entity_fails() {
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        let ops = vec![UpdateOp::SetCount {
            entity: "NonExistent".into(),
            count: 100,
        }];
        assert!(update_model(&mut model, &ops).is_err());
    }

    #[test]
    fn update_multiple_ops() {
        let mut model = make_model(
            "test",
            vec![make_entity(
                "Users",
                vec![
                    make_field("id", DataType::Int),
                    make_field("name", DataType::String),
                ],
            )],
        );
        let ops = vec![
            UpdateOp::SetCount {
                entity: "Users".into(),
                count: 5000,
            },
            UpdateOp::SetEntityDesc {
                entity: "Users".into(),
                desc: "User table".into(),
            },
            UpdateOp::AddTags {
                entity: "Users".into(),
                tags: vec!["core".into()],
            },
            UpdateOp::SetSeed { seed: 99 },
        ];
        let changes = update_model(&mut model, &ops).unwrap();
        assert_eq!(changes.len(), 4);
        assert_eq!(model.entities[0].count, CountSpec::Fixed(5000));
        assert_eq!(
            model.entities[0].description.as_deref(),
            Some("User table")
        );
        assert_eq!(model.entities[0].tags, vec!["core"]);
        assert_eq!(model.seed, 99);
    }

    #[test]
    fn update_parse_count_spec() {
        let op = parse_count_spec("Users=5000").unwrap();
        matches!(op, UpdateOp::SetCount { entity, count } if entity == "Users" && count == 5000);
        assert!(parse_count_spec("bad").is_err());
        assert!(parse_count_spec("Users=abc").is_err());
    }

    #[test]
    fn update_parse_describe_spec() {
        let op = parse_describe_spec("Users=A user table").unwrap();
        matches!(op, UpdateOp::SetEntityDesc { entity, desc } if entity == "Users" && desc == "A user table");

        let op2 = parse_describe_spec("Users.email=Primary email").unwrap();
        matches!(op2, UpdateOp::SetFieldDesc { entity, field, desc }
            if entity == "Users" && field == "email" && desc == "Primary email");
    }

    #[test]
    fn update_parse_tag_spec() {
        let op = parse_tag_spec("Users=pii,core").unwrap();
        matches!(op, UpdateOp::AddTags { entity, tags }
            if entity == "Users" && tags == vec!["pii", "core"]);
    }

    // -----------------------------------------------------------------------
    // Validate tests
    // -----------------------------------------------------------------------

    /// Helper: write a CSV file from column names and string rows.
    fn write_test_csv(dir: &std::path::Path, name: &str, headers: &[&str], rows: &[Vec<&str>]) {
        let path = dir.join(format!("{}.csv", name));
        let mut wtr = csv::Writer::from_path(&path).unwrap();
        wtr.write_record(headers).unwrap();
        for row in rows {
            wtr.write_record(row).unwrap();
        }
        wtr.flush().unwrap();
    }

    #[test]
    fn validate_all_pass() {
        let dir = tempfile::tempdir().unwrap();
        let model = make_model(
            "test",
            vec![make_entity(
                "Users",
                vec![
                    {
                        let mut f = make_field("id", DataType::Int);
                        f.primary_key = Some(true);
                        f
                    },
                    make_field("name", DataType::String),
                ],
            )],
        );
        write_test_csv(
            dir.path(),
            "Users",
            &["id", "name"],
            &[
                vec!["1", "Alice"],
                vec!["2", "Bob"],
                vec!["3", "Carol"],
            ],
        );
        let findings = validate_data(&model, dir.path(), &[]).unwrap();
        let errors: Vec<_> = findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
    }

    #[test]
    fn validate_missing_column() {
        let dir = tempfile::tempdir().unwrap();
        let model = make_model(
            "test",
            vec![make_entity(
                "Users",
                vec![
                    make_field("id", DataType::Int),
                    make_field("email", DataType::String),
                ],
            )],
        );
        // CSV only has "id" column, missing "email"
        write_test_csv(dir.path(), "Users", &["id"], &[vec!["1"]]);
        let findings = validate_data(&model, dir.path(), &[]).unwrap();
        assert!(findings
            .iter()
            .any(|f| f.severity == Severity::Error && f.message.contains("missing column `email`")));
    }

    #[test]
    fn validate_extra_column_warning() {
        let dir = tempfile::tempdir().unwrap();
        let model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        write_test_csv(
            dir.path(),
            "Users",
            &["id", "extra_col"],
            &[vec!["1", "foo"]],
        );
        let findings = validate_data(&model, dir.path(), &[]).unwrap();
        assert!(findings
            .iter()
            .any(|f| f.severity == Severity::Warning && f.message.contains("unexpected column")));
    }

    #[test]
    fn validate_missing_data_file() {
        let dir = tempfile::tempdir().unwrap();
        let model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        // No file written
        let findings = validate_data(&model, dir.path(), &[]).unwrap();
        assert!(findings
            .iter()
            .any(|f| f.severity == Severity::Error && f.message.contains("data file not found")));
    }

    #[test]
    fn validate_row_count_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        model.entities[0].count = CountSpec::Fixed(5);
        write_test_csv(dir.path(), "Users", &["id"], &[vec!["1"], vec!["2"]]);
        let findings = validate_data(&model, dir.path(), &[]).unwrap();
        assert!(findings
            .iter()
            .any(|f| f.severity == Severity::Warning && f.message.contains("row count")));
    }

    #[test]
    fn validate_fk_referential_integrity() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = make_model(
            "test",
            vec![
                make_entity(
                    "Users",
                    vec![{
                        let mut f = make_field("id", DataType::Int);
                        f.primary_key = Some(true);
                        f
                    }],
                ),
                make_entity(
                    "Orders",
                    vec![
                        make_field("id", DataType::Int),
                        make_field("user_id", DataType::Int),
                    ],
                ),
            ],
        );
        model.relationships.push(crate::core::Relationship {
            name: "orders_users".into(),
            from: "Orders".into(),
            to: "Users".into(),
            kind: crate::core::RelationshipKind::ManyToOne,
            foreign_key: Some("user_id".into()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });

        write_test_csv(dir.path(), "Users", &["id"], &[vec!["1"], vec!["2"]]);
        // Order references user_id=99, which doesn't exist
        write_test_csv(
            dir.path(),
            "Orders",
            &["id", "user_id"],
            &[vec!["1", "1"], vec!["2", "99"]],
        );

        let findings = validate_data(&model, dir.path(), &[]).unwrap();
        assert!(findings.iter().any(|f| {
            f.severity == Severity::Error && f.message.contains("orphan")
        }));
    }

    #[test]
    fn validate_fk_all_valid() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = make_model(
            "test",
            vec![
                make_entity(
                    "Users",
                    vec![{
                        let mut f = make_field("id", DataType::Int);
                        f.primary_key = Some(true);
                        f
                    }],
                ),
                make_entity(
                    "Orders",
                    vec![
                        make_field("id", DataType::Int),
                        make_field("user_id", DataType::Int),
                    ],
                ),
            ],
        );
        model.relationships.push(crate::core::Relationship {
            name: "orders_users".into(),
            from: "Orders".into(),
            to: "Users".into(),
            kind: crate::core::RelationshipKind::ManyToOne,
            foreign_key: Some("user_id".into()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });

        write_test_csv(dir.path(), "Users", &["id"], &[vec!["1"], vec!["2"]]);
        write_test_csv(
            dir.path(),
            "Orders",
            &["id", "user_id"],
            &[vec!["1", "1"], vec!["2", "2"]],
        );

        let findings = validate_data(&model, dir.path(), &[]).unwrap();
        let errors: Vec<_> = findings
            .iter()
            .filter(|f| f.severity == Severity::Error)
            .collect();
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
    }

    #[test]
    fn validate_entity_filter() {
        let dir = tempfile::tempdir().unwrap();
        let model = make_model(
            "test",
            vec![
                make_entity("Users", vec![make_field("id", DataType::Int)]),
                make_entity("Orders", vec![make_field("id", DataType::Int)]),
            ],
        );
        // Only write Users data — Orders should not be checked
        write_test_csv(dir.path(), "Users", &["id"], &[vec!["1"]]);
        let findings = validate_data(
            &model,
            dir.path(),
            &["Users".to_string()],
        )
        .unwrap();
        // No error for missing Orders file since it's filtered out
        assert!(!findings
            .iter()
            .any(|f| f.entity == "Orders"));
    }

    #[test]
    fn validate_fk_with_child_only_filter() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = make_model(
            "test",
            vec![
                make_entity(
                    "Users",
                    vec![{
                        let mut f = make_field("id", DataType::Int);
                        f.primary_key = Some(true);
                        f
                    }],
                ),
                make_entity(
                    "Orders",
                    vec![
                        make_field("id", DataType::Int),
                        make_field("user_id", DataType::Int),
                    ],
                ),
            ],
        );
        model.relationships.push(crate::core::Relationship {
            name: "orders_users".into(),
            from: "Orders".into(),
            to: "Users".into(),
            kind: crate::core::RelationshipKind::ManyToOne,
            foreign_key: Some("user_id".into()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });

        write_test_csv(dir.path(), "Users", &["id"], &[vec!["1"], vec!["2"]]);
        write_test_csv(
            dir.path(),
            "Orders",
            &["id", "user_id"],
            &[vec!["1", "1"], vec!["2", "99"]],
        );

        // Filter to Orders only — should still detect FK orphans
        let findings = validate_data(
            &model,
            dir.path(),
            &["Orders".to_string()],
        )
        .unwrap();
        assert!(findings.iter().any(|f| {
            f.severity == Severity::Error && f.message.contains("orphan")
        }));
    }

    #[test]
    fn validate_implicit_fk_not_flagged_as_extra() {
        let dir = tempfile::tempdir().unwrap();
        let mut model = make_model(
            "test",
            vec![
                make_entity(
                    "Users",
                    vec![{
                        let mut f = make_field("id", DataType::Int);
                        f.primary_key = Some(true);
                        f
                    }],
                ),
                make_entity(
                    "Orders",
                    vec![make_field("id", DataType::Int)],
                ),
            ],
        );
        // Relationship with implicit FK (Users_id)
        model.relationships.push(crate::core::Relationship {
            name: "orders_users".into(),
            from: "Orders".into(),
            to: "Users".into(),
            kind: crate::core::RelationshipKind::ManyToOne,
            foreign_key: None, // implicit: Users_id
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: Vec::new(),
        });

        // Data has the implicit FK column
        write_test_csv(dir.path(), "Users", &["id"], &[vec!["1"]]);
        write_test_csv(
            dir.path(),
            "Orders",
            &["id", "Users_id"],
            &[vec!["1", "1"]],
        );

        let findings = validate_data(&model, dir.path(), &[]).unwrap();
        // Users_id should NOT be flagged as unexpected
        assert!(!findings.iter().any(|f| {
            f.message.contains("unexpected column `Users_id`")
        }));
    }

    // -----------------------------------------------------------------------
    // Derive tests
    // -----------------------------------------------------------------------

    #[test]
    fn derive_scale_factor() {
        let model = make_model(
            "test",
            vec![
                make_entity("Users", vec![make_field("id", DataType::Int)]),
                make_entity("Orders", vec![make_field("id", DataType::Int)]),
            ],
        );
        let (derived, changes) = derive_model(&model, Some(10.0), &[], &[], None, None, None).unwrap();
        assert_eq!(derived.entities[0].count, CountSpec::Fixed(1000));
        assert_eq!(derived.entities[1].count, CountSpec::Fixed(1000));
        assert_eq!(changes.len(), 2);
    }

    #[test]
    fn derive_scale_fractional() {
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        model.entities[0].count = CountSpec::Fixed(100);
        let (derived, _) = derive_model(&model, Some(0.1), &[], &[], None, None, None).unwrap();
        assert_eq!(derived.entities[0].count, CountSpec::Fixed(10));
    }

    #[test]
    fn derive_scale_minimum_one() {
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        model.entities[0].count = CountSpec::Fixed(1);
        let (derived, _) = derive_model(&model, Some(0.001), &[], &[], None, None, None).unwrap();
        assert_eq!(derived.entities[0].count, CountSpec::Fixed(1));
    }

    #[test]
    fn derive_scale_range() {
        let mut model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        model.entities[0].count = CountSpec::Range { min: 10, max: 100 };
        let (derived, _) = derive_model(&model, Some(5.0), &[], &[], None, None, None).unwrap();
        assert_eq!(
            derived.entities[0].count,
            CountSpec::Range { min: 50, max: 500 }
        );
    }

    #[test]
    fn derive_count_override() {
        let model = make_model(
            "test",
            vec![
                make_entity("Users", vec![make_field("id", DataType::Int)]),
                make_entity("Orders", vec![make_field("id", DataType::Int)]),
            ],
        );
        let overrides = vec![("Users".to_string(), 5000u64)];
        let (derived, _) = derive_model(&model, None, &overrides, &[], None, None, None).unwrap();
        assert_eq!(derived.entities[0].count, CountSpec::Fixed(5000));
        // Orders unchanged
        assert_eq!(derived.entities[1].count, CountSpec::Fixed(100));
    }

    #[test]
    fn derive_count_override_after_scale() {
        let model = make_model(
            "test",
            vec![
                make_entity("Users", vec![make_field("id", DataType::Int)]),
                make_entity("Orders", vec![make_field("id", DataType::Int)]),
            ],
        );
        // Scale 10x then override Users to exactly 50
        let overrides = vec![("Users".to_string(), 50u64)];
        let (derived, _) = derive_model(&model, Some(10.0), &overrides, &[], None, None, None).unwrap();
        assert_eq!(derived.entities[0].count, CountSpec::Fixed(50)); // override wins
        assert_eq!(derived.entities[1].count, CountSpec::Fixed(1000)); // scaled 100*10
    }

    #[test]
    fn derive_exclude_entities() {
        let model = make_model(
            "test",
            vec![
                make_entity("Users", vec![make_field("id", DataType::Int)]),
                make_entity("Config", vec![make_field("id", DataType::Int)]),
            ],
        );
        let (derived, _) =
            derive_model(&model, Some(10.0), &[], &["Config".to_string()], None, None, None).unwrap();
        assert_eq!(derived.entities[0].count, CountSpec::Fixed(1000)); // scaled 100*10
        assert_eq!(derived.entities[1].count, CountSpec::Fixed(100)); // excluded, unchanged
    }

    #[test]
    fn derive_seed_and_locale() {
        let model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        let (derived, changes) =
            derive_model(&model, None, &[], &[], Some(42), Some("ja_JP"), None).unwrap();
        assert_eq!(derived.seed, 42);
        assert_eq!(derived.locale, "ja_JP");
        assert_eq!(changes.len(), 2);
    }

    #[test]
    fn derive_variant_name() {
        let model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        let (derived, _) =
            derive_model(&model, None, &[], &[], None, None, Some("small-test")).unwrap();
        assert!(derived.description.unwrap().contains("small-test"));
    }

    #[test]
    fn derive_unknown_entity_fails() {
        let model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        let overrides = vec![("NonExistent".to_string(), 100u64)];
        assert!(derive_model(&model, None, &overrides, &[], None, None, None).is_err());
    }

    #[test]
    fn derive_unknown_exclude_fails() {
        let model = make_model(
            "test",
            vec![make_entity("Users", vec![make_field("id", DataType::Int)])],
        );
        assert!(
            derive_model(&model, Some(2.0), &[], &["Bad".to_string()], None, None, None).is_err()
        );
    }

    #[test]
    fn derive_parse_scale_factor() {
        assert!((parse_scale_factor("10x").unwrap() - 10.0).abs() < f64::EPSILON);
        assert!((parse_scale_factor("0.5X").unwrap() - 0.5).abs() < f64::EPSILON);
        assert!((parse_scale_factor("2.5x").unwrap() - 2.5).abs() < f64::EPSILON);
        assert!(parse_scale_factor("0x").is_err());
        assert!(parse_scale_factor("-1x").is_err());
        assert!(parse_scale_factor("abc").is_err());
    }
}
