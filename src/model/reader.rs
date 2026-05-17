//! Reader for structured model directories → DataModel.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;
use tracing::info;

use crate::core::types::*;
use crate::model::model_root;

/// Load a structured model directory into a DataModel.
pub fn load_model_directory(path: &Path) -> Result<DataModel> {
    let root = model_root(path);
    info!(path = %root.display(), "loading structured model");

    // 1. Load root manifest
    let manifest_path = root.join("knit.toml");
    let manifest_str = std::fs::read_to_string(&manifest_path)
        .with_context(|| format!("reading {}", manifest_path.display()))?;
    let manifest: Manifest = toml::from_str(&manifest_str)
        .with_context(|| format!("parsing {}", manifest_path.display()))?;

    // 2. Load layout (optional)
    let layout_path = root.join("layout.toml");
    let layout: Option<LayoutFile> = if layout_path.is_file() {
        let s = std::fs::read_to_string(&layout_path)
            .with_context(|| format!("reading {}", layout_path.display()))?;
        Some(toml::from_str(&s).with_context(|| format!("parsing {}", layout_path.display()))?)
    } else {
        None
    };

    // 3. Load table files
    let tables_dir = root.join("tables");
    let mut entities = Vec::new();
    if tables_dir.is_dir() {
        let mut entries: Vec<_> = std::fs::read_dir(&tables_dir)
            .with_context(|| format!("reading {}", tables_dir.display()))?
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("toml"))
            .collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let table_str = std::fs::read_to_string(entry.path())
                .with_context(|| format!("reading {}", entry.path().display()))?;
            let table: TableFile = toml::from_str(&table_str)
                .with_context(|| format!("parsing {}", entry.path().display()))?;
            entities.push(table_to_entity(table, &layout));
        }
    }

    // 4. Load relationships (optional)
    let rels_path = root.join("relationships.toml");
    let (relationships, actor_relationships) = if rels_path.is_file() {
        let s = std::fs::read_to_string(&rels_path)
            .with_context(|| format!("reading {}", rels_path.display()))?;
        let rf: RelationshipsFile =
            toml::from_str(&s).with_context(|| format!("parsing {}", rels_path.display()))?;
        (
            rf.foreign_keys.unwrap_or_default(),
            rf.actor_graphs.unwrap_or_default(),
        )
    } else {
        (vec![], vec![])
    };

    // 5. Load correlations (optional)
    let corr_path = root.join("correlations.toml");
    let correlations = if corr_path.is_file() {
        let s = std::fs::read_to_string(&corr_path)
            .with_context(|| format!("reading {}", corr_path.display()))?;
        let cf: CorrelationsFile =
            toml::from_str(&s).with_context(|| format!("parsing {}", corr_path.display()))?;
        cf.correlations.unwrap_or_default()
    } else {
        vec![]
    };

    // 6. Load shared definitions (optional)
    let shared_path = root.join("shared.toml");
    let (custom_types, mixins, personas) = if shared_path.is_file() {
        let s = std::fs::read_to_string(&shared_path)
            .with_context(|| format!("reading {}", shared_path.display()))?;
        let sf: SharedFile =
            toml::from_str(&s).with_context(|| format!("parsing {}", shared_path.display()))?;
        (
            sf.types.unwrap_or_default(),
            sf.mixins.unwrap_or_default(),
            sf.personas.unwrap_or_default(),
        )
    } else {
        (vec![], vec![], vec![])
    };

    // 7. Companion files from layout
    let companion_files = layout
        .as_ref()
        .and_then(|l| l.companions.as_ref())
        .cloned()
        .unwrap_or_default();

    // 8. Noise profiles from layout or per-entity (handled inline)
    let noise_profiles = layout
        .as_ref()
        .and_then(|l| l.noise.as_ref())
        .cloned()
        .unwrap_or_default();

    let model = DataModel {
        name: manifest.model.name,
        description: manifest.model.description,
        seed: manifest.model.seed.unwrap_or(42),
        locale: manifest.model.locale.unwrap_or_else(|| "en_US".to_string()),
        timezone: manifest.model.timezone.unwrap_or_else(|| "UTC".to_string()),
        entities,
        relationships,
        noise_profiles,
        correlations,
        params: manifest.model.params.unwrap_or_default(),
        blueprint_version: manifest
            .blueprint_version
            .unwrap_or_else(|| "2.0".to_string()),
        personas,
        actor_relationships,
        custom_types,
        mixins,
        companion_files,
    };

    info!(
        entities = model.entities.len(),
        relationships = model.relationships.len(),
        "structured model loaded"
    );

    Ok(model)
}

// ─── Intermediate serde types ────────────────────────────────────────

/// Root manifest (`knit.toml`).
#[derive(Debug, Deserialize)]
struct Manifest {
    blueprint_version: Option<String>,
    model: ManifestModel,
}

#[derive(Debug, Deserialize)]
struct ManifestModel {
    name: String,
    description: Option<String>,
    seed: Option<u64>,
    locale: Option<String>,
    timezone: Option<String>,
    params: Option<BTreeMap<String, Value>>,
}

/// Layout file (`layout.toml`).
#[derive(Debug, Deserialize)]
struct LayoutFile {
    #[serde(default)]
    folders: Option<Vec<FolderEntry>>,
    #[serde(default)]
    companions: Option<Vec<String>>,
    #[serde(default)]
    noise: Option<Vec<NoiseProfile>>,
}

#[derive(Debug, Deserialize)]
struct FolderEntry {
    table: String,
    path: Option<String>,
    #[serde(default, rename = "format")]
    _format: Option<String>,
    #[serde(default)]
    partition: Option<PartitionEntry>,
}

#[derive(Debug, Deserialize)]
struct PartitionEntry {
    by: String,
    #[serde(default)]
    values: Vec<String>,
    #[serde(default)]
    weights: Vec<f64>,
    #[serde(default)]
    counts: Vec<u64>,
}

/// Table file (`tables/*.toml`).
#[derive(Debug, Deserialize)]
struct TableFile {
    table: TableMeta,
    #[serde(default)]
    columns: Vec<Field>,
    #[serde(default)]
    constraints: Vec<Constraint>,
}

#[derive(Debug, Deserialize)]
struct TableMeta {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    count: CountSpec,
    #[serde(default)]
    actor: bool,
    #[serde(default)]
    persona_distribution: Option<String>,
    #[serde(default)]
    activity_count: Option<ActivityCount>,
    #[serde(default)]
    topology: Option<TopologySpec>,
    #[serde(default)]
    mixins: Option<Vec<String>>,
    #[serde(default)]
    stats: Option<TableStats>,
    #[serde(default)]
    scaling: Option<DimensionAnnotation>,
}
#[derive(Debug, Deserialize)]
struct RelationshipsFile {
    #[serde(default)]
    foreign_keys: Option<Vec<Relationship>>,
    #[serde(default)]
    actor_graphs: Option<Vec<ActorRelationship>>,
}

/// Correlations file (`correlations.toml`).
#[derive(Debug, Deserialize)]
struct CorrelationsFile {
    #[serde(default, alias = "intra_table", alias = "cross_table")]
    correlations: Option<Vec<Correlation>>,
}

/// Shared definitions file (`shared.toml`).
#[derive(Debug, Deserialize)]
struct SharedFile {
    #[serde(default)]
    types: Option<Vec<CustomType>>,
    #[serde(default)]
    mixins: Option<Vec<Mixin>>,
    #[serde(default)]
    personas: Option<Vec<Persona>>,
}

// ─── Conversion helpers ──────────────────────────────────────────────

/// Convert a TableFile into an Entity, applying layout info.
fn table_to_entity(table: TableFile, layout: &Option<LayoutFile>) -> Entity {
    let folder = layout.as_ref().and_then(|l| {
        l.folders
            .as_ref()
            .and_then(|fs| fs.iter().find(|f| f.table == table.table.name))
    });

    let output = folder.map(|f| {
        let partition_values = f
            .partition
            .as_ref()
            .map(|p| {
                p.values
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        let weight = if let Some(&w) = p.weights.get(i) {
                            w
                        } else if let Some(&count) = p.counts.get(i) {
                            let total: u64 = p.counts.iter().sum();
                            if total > 0 {
                                count as f64 / total as f64
                            } else {
                                1.0 / p.values.len() as f64
                            }
                        } else {
                            1.0 / p.values.len().max(1) as f64
                        };
                        PartitionValue {
                            value: v.clone(),
                            weight,
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();

        OutputLayout {
            path: f.path.clone(),
            source_format: None,
            partition_by: f.partition.as_ref().map(|p| p.by.clone()),
            partition_values,
        }
    });

    Entity {
        name: table.table.name,
        description: table.table.description,
        tags: table.table.tags,
        count: table.table.count,
        fields: table.columns,
        constraints: table.constraints,
        topology: table.table.topology,
        actor: table.table.actor,
        persona_distribution: table.table.persona_distribution,
        activity_count: table.table.activity_count,
        mixin_refs: table.table.mixins,
        output,
        stats: table.table.stats,
        scaling: table.table.scaling,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_load_minimal_model() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        // Create knit.toml
        fs::write(
            root.join("knit.toml"),
            r#"
blueprint_version = "2.0"

[model]
name = "test_model"
seed = 123
"#,
        )
        .unwrap();

        // Create tables directory
        fs::create_dir(root.join("tables")).unwrap();
        fs::write(
            root.join("tables").join("Users.toml"),
            r#"
[table]
name = "Users"
count = 100

[[columns]]
name = "id"
data_type = "int"

[columns.generator]
type = "sequence"
start = 1
step = 1

[[columns]]
name = "email"
data_type = "string"
"#,
        )
        .unwrap();

        let model = load_model_directory(root).unwrap();
        assert_eq!(model.name, "test_model");
        assert_eq!(model.seed, 123);
        assert_eq!(model.entities.len(), 1);
        assert_eq!(model.entities[0].name, "Users");
        assert_eq!(model.entities[0].fields.len(), 2);
    }

    #[test]
    fn test_load_with_relationships() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        fs::write(
            root.join("knit.toml"),
            r#"
[model]
name = "reltest"
"#,
        )
        .unwrap();

        fs::create_dir(root.join("tables")).unwrap();
        fs::write(
            root.join("tables").join("Orders.toml"),
            r#"
[table]
name = "Orders"
count = 50

[[columns]]
name = "user_id"
data_type = "int"
"#,
        )
        .unwrap();

        fs::write(
            root.join("relationships.toml"),
            r#"
[[foreign_keys]]
name = "orders_to_users"
from = "Orders.user_id"
to = "Users.id"
kind = "many_to_one"
"#,
        )
        .unwrap();

        let model = load_model_directory(root).unwrap();
        assert_eq!(model.relationships.len(), 1);
        assert_eq!(model.relationships[0].name, "orders_to_users");
    }

    #[test]
    fn test_load_without_optional_files() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();

        fs::write(
            root.join("knit.toml"),
            r#"
[model]
name = "minimal"
"#,
        )
        .unwrap();
        fs::create_dir(root.join("tables")).unwrap();
        fs::write(
            root.join("tables").join("Users.toml"),
            r#"
[table]
name = "Users"
count = 1

[[columns]]
name = "id"
data_type = "int"
"#,
        )
        .unwrap();

        let model = load_model_directory(root).unwrap();

        assert_eq!(model.seed, 42);
        assert_eq!(model.locale, "en_US");
        assert_eq!(model.timezone, "UTC");
        assert!(model.relationships.is_empty());
        assert!(model.noise_profiles.is_empty());
        assert!(model.correlations.is_empty());
        assert!(model.custom_types.is_empty());
        assert!(model.mixins.is_empty());
        assert!(model.personas.is_empty());
        assert!(model.actor_relationships.is_empty());
        assert!(model.companion_files.is_empty());
    }

    #[test]
    fn test_load_writer_roundtrip() {
        let model = DataModel {
            name: "roundtrip".to_string(),
            description: Some("reader roundtrip".to_string()),
            seed: 7,
            locale: "en_GB".to_string(),
            timezone: "Europe/London".to_string(),
            entities: vec![Entity {
                name: "Users".to_string(),
                description: Some("people".to_string()),
                tags: vec!["demo".to_string()],
                count: CountSpec::Fixed(2),
                fields: vec![Field {
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
                }],
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
        };
        let dir = TempDir::new().unwrap();
        let out = dir.path().join("written_model");

        crate::model::writer::write_model_directory(&model, &out).unwrap();
        let loaded = load_model_directory(&out).unwrap();

        assert_eq!(loaded, model);
    }
}
