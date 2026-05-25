//! Subcommand implementations for the knit CLI.

pub mod blueprint;
pub mod enrich;
pub mod generate;
pub mod generators;
pub mod init;
pub mod inspect;
pub mod learn;
pub mod model;
pub mod plan;
pub mod scale;
pub mod tokenize;
pub mod validate;

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::blueprint::BlueprintError;
use crate::core::DataModel;
use crate::model as model_dir;

/// Load and parse a schema file or structured model directory.
///
/// Auto-detects format:
/// - Directory with `knit.toml` → structured model
/// - File with `.json` extension → JSON schema
/// - Otherwise → TOML flat schema
///
/// Emits a deprecation warning for v1 blueprints.
/// Returns the parsed [`DataModel`] or an error.
pub fn load_blueprint(path: &str) -> Result<DataModel, BlueprintError> {
    let p = Path::new(path);

    // Check for structured model directory
    if model_dir::is_structured_model(p) {
        let model = model_dir::reader::load_model_directory(p)
            .map_err(|e| BlueprintError::Other(e.to_string()))?;
        warn_v1_deprecation(&model, path);
        return Ok(model);
    }

    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let model = match ext.as_str() {
        "json" => crate::blueprint::parse_json_file(p)?,
        "toml" => {
            // Try TOML first; if it fails and content looks like JSON, try JSON
            match crate::blueprint::parse_toml_file(p) {
                Ok(m) => m,
                Err(_) => {
                    let content = std::fs::read_to_string(p)
                        .map_err(|e| BlueprintError::Other(e.to_string()))?;
                    if content.trim_start().starts_with('{') {
                        crate::blueprint::parse_json_file(p)?
                    } else {
                        // Re-parse to get original error
                        crate::blueprint::parse_toml_file(p)?
                    }
                }
            }
        }
        _ => crate::blueprint::parse_toml_file(p)?,
    };
    warn_v1_deprecation(&model, path);
    Ok(model)
}

/// Emit a deprecation warning if the loaded blueprint uses v1 format.
fn warn_v1_deprecation(model: &DataModel, path: &str) {
    let v = &model.blueprint_version;
    if v.is_empty() || v == "1" || v.starts_with("1.") {
        tracing::warn!(
            path = path,
            version = %v,
            "loading v1 blueprint; v1 format is deprecated — use `knit model migrate` to upgrade to v2"
        );
    }
}

/// Save a DataModel to disk, auto-detecting format from the path.
///
/// - If path is an existing structured model directory (contains `knit.toml`),
///   or has no extension, writes as structured directory.
/// - Otherwise writes as JSON.
///
/// This is the write counterpart to [`load_blueprint`].
pub fn save_blueprint(model: &DataModel, path: &str) -> Result<()> {
    let p = Path::new(path);

    // Normalize: if path points to knit.toml directly, use its parent as the model root
    let effective_path = if model_dir::is_structured_model(p) && p.is_file() {
        model_dir::model_root(p)
    } else {
        p
    };

    let use_structured = model_dir::is_structured_model(effective_path)
        || effective_path.extension().is_none()
        || effective_path.is_dir();

    if use_structured {
        // Clean stale table files before writing
        let tables_dir = effective_path.join("tables");
        if tables_dir.is_dir() {
            std::fs::remove_dir_all(&tables_dir).with_context(|| {
                format!(
                    "failed to clean stale tables directory: {}",
                    tables_dir.display()
                )
            })?;
        }
        model_dir::writer::write_model_directory(model, effective_path)
            .with_context(|| format!("failed to write structured model to {path}"))?;
    } else {
        let schema_text =
            serialize_model_to_json(model).context("failed to serialize blueprint to JSON")?;
        std::fs::write(path, &schema_text)
            .with_context(|| format!("failed to write blueprint to {path}"))?;
    }
    Ok(())
}

/// Serialize a DataModel to pretty-printed JSON.
pub(crate) fn serialize_model_to_json(model: &DataModel) -> Result<String> {
    use serde_json::json;

    let mut obj = serde_json::Map::new();
    obj.insert(
        "blueprint_version".to_string(),
        json!(model.blueprint_version),
    );

    // Model metadata — always serialize seed/locale/timezone to avoid round-trip loss
    let mut meta = serde_json::Map::new();
    meta.insert("name".to_string(), json!(model.name));
    if let Some(ref desc) = model.description {
        meta.insert("description".to_string(), json!(desc));
    }
    meta.insert("seed".to_string(), json!(model.seed));
    meta.insert("locale".to_string(), json!(model.locale));
    meta.insert("timezone".to_string(), json!(model.timezone));
    if !model.params.is_empty() {
        meta.insert("params".to_string(), serde_json::to_value(&model.params)?);
    }
    obj.insert("model".to_string(), serde_json::Value::Object(meta));

    if !model.entities.is_empty() {
        obj.insert(
            "entities".to_string(),
            serde_json::to_value(&model.entities)?,
        );
    }
    if !model.relationships.is_empty() {
        obj.insert(
            "relationships".to_string(),
            serde_json::to_value(&model.relationships)?,
        );
    }
    if !model.noise_profiles.is_empty() {
        obj.insert(
            "noise".to_string(),
            serde_json::to_value(&model.noise_profiles)?,
        );
    }
    if !model.correlations.is_empty() {
        obj.insert(
            "correlations".to_string(),
            serde_json::to_value(&model.correlations)?,
        );
    }
    if !model.personas.is_empty() {
        obj.insert(
            "personas".to_string(),
            serde_json::to_value(&model.personas)?,
        );
    }
    if !model.actor_relationships.is_empty() {
        obj.insert(
            "actor_relationships".to_string(),
            serde_json::to_value(&model.actor_relationships)?,
        );
    }
    if !model.custom_types.is_empty() {
        obj.insert(
            "types".to_string(),
            serde_json::to_value(&model.custom_types)?,
        );
    }
    if !model.mixins.is_empty() {
        obj.insert("mixins".to_string(), serde_json::to_value(&model.mixins)?);
    }
    if !model.companion_files.is_empty() {
        obj.insert(
            "companion_files".to_string(),
            serde_json::to_value(&model.companion_files)?,
        );
    }

    serde_json::to_string_pretty(&serde_json::Value::Object(obj))
        .context("failed to serialize to JSON")
}

/// Wrapper for proper flat TOML serialization of a DataModel.
#[derive(Serialize)]
pub(crate) struct FlatSchemaOutput {
    pub blueprint_version: String,
    pub model: FlatModelMeta,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub entities: Vec<crate::core::Entity>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub relationships: Vec<crate::core::Relationship>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "noise")]
    pub noise: Vec<crate::core::NoiseProfile>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub correlations: Vec<crate::core::Correlation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub personas: Vec<crate::core::Persona>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub actor_relationships: Vec<crate::core::ActorRelationship>,
    #[serde(skip_serializing_if = "Vec::is_empty", rename = "types")]
    pub types: Vec<crate::core::CustomType>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub mixins: Vec<crate::core::Mixin>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub companion_files: Vec<String>,
}

#[derive(Serialize)]
pub(crate) struct FlatModelMeta {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locale: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub params: std::collections::BTreeMap<String, crate::core::Value>,
}

/// Validate a parsed model and return collected errors.
pub fn validate_model(model: &DataModel) -> Vec<BlueprintError> {
    crate::blueprint::validate(model)
}
