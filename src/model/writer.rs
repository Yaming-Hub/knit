//! Writer for DataModel → structured model directory.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::info;

use crate::core::types::*;

/// Write a DataModel to a structured model directory.
pub fn write_model_directory(model: &DataModel, output: &Path) -> Result<()> {
    info!(path = %output.display(), "writing structured model");

    // Create directory structure
    std::fs::create_dir_all(output)
        .with_context(|| format!("creating {}", output.display()))?;
    std::fs::create_dir_all(output.join("tables"))
        .with_context(|| "creating tables/")?;

    // 1. Write knit.toml
    let manifest = ManifestOut {
        blueprint_version: "2.0".to_string(),
        model: ManifestModelOut {
            name: model.name.clone(),
            description: model.description.clone(),
            seed: model.seed,
            locale: model.locale.clone(),
            timezone: model.timezone.clone(),
            params: if model.params.is_empty() { None } else { Some(model.params.clone()) },
        },
    };
    let manifest_str = toml::to_string_pretty(&manifest).context("serializing knit.toml")?;
    std::fs::write(output.join("knit.toml"), manifest_str)?;

    // 2. Write layout.toml (if any entities have output layout or companions exist)
    let folders: Vec<FolderOut> = model.entities.iter()
        .filter_map(|e| {
            e.output.as_ref().map(|o| FolderOut {
                table: e.name.clone(),
                path: o.path.clone(),
                partition: o.partition_by.as_ref().map(|by| PartitionOut {
                    by: by.clone(),
                    values: o.partition_values.iter().map(|pv| pv.value.clone()).collect(),
                    weights: o.partition_values.iter().map(|pv| pv.weight).collect(),
                }),
            })
        })
        .collect();

    if !folders.is_empty() || !model.companion_files.is_empty() || !model.noise_profiles.is_empty() {
        let layout = LayoutOut {
            folders: if folders.is_empty() { None } else { Some(folders) },
            companions: if model.companion_files.is_empty() { None } else { Some(model.companion_files.clone()) },
            noise: if model.noise_profiles.is_empty() { None } else { Some(model.noise_profiles.clone()) },
        };
        let layout_str = toml::to_string_pretty(&layout).context("serializing layout.toml")?;
        std::fs::write(output.join("layout.toml"), layout_str)?;
    }

    // 3. Write table files
    for entity in &model.entities {
        let table = TableOut {
            table: TableMetaOut {
                name: entity.name.clone(),
                description: entity.description.clone(),
                tags: if entity.tags.is_empty() { None } else { Some(entity.tags.clone()) },
                count: entity.count.clone(),
                actor: if entity.actor { Some(true) } else { None },
                persona_distribution: entity.persona_distribution.clone(),
                activity_count: entity.activity_count.clone(),
                topology: entity.topology.clone(),
                mixins: entity.mixin_refs.clone(),
                stats: entity.stats.clone(),
                scaling: entity.scaling.clone(),
            },
            columns: entity.fields.clone(),
            constraints: if entity.constraints.is_empty() { None } else { Some(entity.constraints.clone()) },
        };
        let table_str = toml::to_string_pretty(&table).context("serializing table")?;
        let filename = format!("{}.toml", entity.name);
        std::fs::write(output.join("tables").join(&filename), table_str)?;
    }

    // 4. Write relationships.toml (if any)
    if !model.relationships.is_empty() || !model.actor_relationships.is_empty() {
        let rels = RelationshipsOut {
            foreign_keys: if model.relationships.is_empty() { None } else { Some(model.relationships.clone()) },
            actor_graphs: if model.actor_relationships.is_empty() { None } else { Some(model.actor_relationships.clone()) },
        };
        let rels_str = toml::to_string_pretty(&rels).context("serializing relationships.toml")?;
        std::fs::write(output.join("relationships.toml"), rels_str)?;
    }

    // 5. Write correlations.toml (if any)
    if !model.correlations.is_empty() {
        let corr = CorrelationsOut {
            correlations: model.correlations.clone(),
        };
        let corr_str = toml::to_string_pretty(&corr).context("serializing correlations.toml")?;
        std::fs::write(output.join("correlations.toml"), corr_str)?;
    }

    // 6. Write shared.toml (if custom types, mixins, or personas exist)
    if !model.custom_types.is_empty() || !model.mixins.is_empty() || !model.personas.is_empty() {
        let shared = SharedOut {
            types: if model.custom_types.is_empty() { None } else { Some(model.custom_types.clone()) },
            mixins: if model.mixins.is_empty() { None } else { Some(model.mixins.clone()) },
            personas: if model.personas.is_empty() { None } else { Some(model.personas.clone()) },
        };
        let shared_str = toml::to_string_pretty(&shared).context("serializing shared.toml")?;
        std::fs::write(output.join("shared.toml"), shared_str)?;
    }

    info!(
        tables = model.entities.len(),
        "structured model written"
    );

    Ok(())
}

// ─── Intermediate serde types for output ─────────────────────────────

#[derive(Serialize)]
struct ManifestOut {
    blueprint_version: String,
    model: ManifestModelOut,
}

#[derive(Serialize)]
struct ManifestModelOut {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    seed: u64,
    locale: String,
    timezone: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    params: Option<BTreeMap<String, Value>>,
}

#[derive(Serialize)]
struct LayoutOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    folders: Option<Vec<FolderOut>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    companions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    noise: Option<Vec<NoiseProfile>>,
}

#[derive(Serialize)]
struct FolderOut {
    table: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    partition: Option<PartitionOut>,
}

#[derive(Serialize)]
struct PartitionOut {
    by: String,
    values: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    weights: Vec<f64>,
}

#[derive(Serialize)]
struct TableOut {
    table: TableMetaOut,
    #[serde(rename = "columns")]
    columns: Vec<Field>,
    #[serde(skip_serializing_if = "Option::is_none")]
    constraints: Option<Vec<Constraint>>,
}

#[derive(Serialize)]
struct TableMetaOut {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tags: Option<Vec<String>>,
    count: CountSpec,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    persona_distribution: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    activity_count: Option<ActivityCount>,
    #[serde(skip_serializing_if = "Option::is_none")]
    topology: Option<TopologySpec>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mixins: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stats: Option<TableStats>,
    #[serde(skip_serializing_if = "Option::is_none")]
    scaling: Option<DimensionAnnotation>,
}

#[derive(Serialize)]
struct RelationshipsOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    foreign_keys: Option<Vec<Relationship>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor_graphs: Option<Vec<ActorRelationship>>,
}

#[derive(Serialize)]
struct CorrelationsOut {
    correlations: Vec<Correlation>,
}

#[derive(Serialize)]
struct SharedOut {
    #[serde(skip_serializing_if = "Option::is_none")]
    types: Option<Vec<CustomType>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mixins: Option<Vec<Mixin>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    personas: Option<Vec<Persona>>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_simple_model() -> DataModel {
        DataModel {
            name: "test_model".to_string(),
            description: Some("A test".to_string()),
            seed: 42,
            locale: "en_US".to_string(),
            timezone: "UTC".to_string(),
            entities: vec![Entity {
                name: "Users".to_string(),
                description: None,
                tags: vec![],
                count: CountSpec::Fixed(100),
                fields: vec![
                    Field {
                        name: "id".to_string(),
                        description: None,
                        data_type: DataType::Int,
                        generator: Some(GeneratorSpec::Sequence {
                            start: IntOrString::Int(1),
                            step: IntOrString::Int(1),
                            prefix: None,
                            values: None,
                            cycle: None,
                            jitter: None,
                        }),
                        nullable: NullSpec::Never,
                        primary_key: Some(true),
                        precision: None,
                        actor_column: false,
                        fields: vec![],
                        stats: None,
                        traits: None,
                    },
                    Field {
                        name: "name".to_string(),
                        description: None,
                        data_type: DataType::String,
                        generator: Some(GeneratorSpec::Faker {
                            method: "name".to_string(),
                            args: vec![],
                        }),
                        nullable: NullSpec::Never,
                        primary_key: None,
                        precision: None,
                        actor_column: false,
                        fields: vec![],
                        stats: None,
                        traits: None,
                    },
                ],
                constraints: vec![],
                topology: None,
                actor: false,
                persona_distribution: None,
                activity_count: None,
                mixin_refs: None,
                output: None,
                stats: None,
                scaling: None,
            }],
            relationships: vec![],
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            blueprint_version: "2.0".to_string(),
            personas: vec![],
            actor_relationships: vec![],
            custom_types: vec![],
            mixins: vec![],
            companion_files: vec![],
        }
    }

    #[test]
    fn test_write_and_read_roundtrip() {
        let model = make_simple_model();
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("my_model");

        write_model_directory(&model, &out).unwrap();

        // Verify files exist
        assert!(out.join("knit.toml").is_file());
        assert!(out.join("tables").join("Users.toml").is_file());

        // Read back
        let loaded = crate::model::reader::load_model_directory(&out).unwrap();
        assert_eq!(loaded.name, "test_model");
        assert_eq!(loaded.seed, 42);
        assert_eq!(loaded.entities.len(), 1);
        assert_eq!(loaded.entities[0].name, "Users");
        assert_eq!(loaded.entities[0].fields.len(), 2);
    }

    #[test]
    fn test_write_creates_directories() {
        let model = make_simple_model();
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("nested").join("model");

        write_model_directory(&model, &out).unwrap();
        assert!(out.join("knit.toml").is_file());
    }
}