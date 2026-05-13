//! Subcommand implementations for the knit CLI.

pub mod enrich;
pub mod generate;
pub mod generators;
pub mod init;
pub mod inspect;
pub mod learn;
pub mod model;
pub mod plan;
pub mod scale;
pub mod blueprint;
pub mod tokenize;
pub mod validate;

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::core::DataModel;
use crate::model as model_dir;
use crate::blueprint::BlueprintError;

/// Load and parse a schema file or structured model directory.
///
/// Auto-detects format:
/// - Directory with `knit.toml` → structured model
/// - File with `.json` extension → JSON schema
/// - Otherwise → TOML flat schema
///
/// Returns the parsed [`DataModel`] or an error.
pub fn load_blueprint(path: &str) -> Result<DataModel, BlueprintError> {
    let p = Path::new(path);

    // Check for structured model directory
    if model_dir::is_structured_model(p) {
        return model_dir::reader::load_model_directory(p)
            .map_err(|e| BlueprintError::Other(e.to_string()));
    }

    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "json" => crate::blueprint::parse_json_file(p),
        _ => crate::blueprint::parse_toml_file(p),
    }
}

/// Save a DataModel to disk, auto-detecting format from the path.
///
/// - If path is an existing structured model directory (contains `knit.toml`),
///   or has no extension, writes as structured directory.
/// - Otherwise writes as flat TOML.
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
                format!("failed to clean stale tables directory: {}", tables_dir.display())
            })?;
        }
        model_dir::writer::write_model_directory(model, effective_path)
            .with_context(|| format!("failed to write structured model to {path}"))?;
    } else {
        let raw = FlatSchemaOutput {
            blueprint_version: model.blueprint_version.clone(),
            model: FlatModelMeta {
                name: model.name.clone(),
                description: model.description.clone(),
                seed: if model.seed != 0 { Some(model.seed) } else { None },
                locale: if model.locale != "en" { Some(model.locale.clone()) } else { None },
                timezone: if model.timezone != "UTC" {
                    Some(model.timezone.clone())
                } else {
                    None
                },
                params: model.params.clone(),
            },
            entities: model.entities.clone(),
            relationships: model.relationships.clone(),
            noise: model.noise_profiles.clone(),
            correlations: model.correlations.clone(),
            personas: model.personas.clone(),
            actor_relationships: model.actor_relationships.clone(),
            types: model.custom_types.clone(),
            mixins: model.mixins.clone(),
            companion_files: model.companion_files.clone(),
        };
        let schema_text =
            toml::to_string_pretty(&raw).context("failed to serialize schema to TOML")?;
        std::fs::write(path, &schema_text)
            .with_context(|| format!("failed to write schema to {path}"))?;
    }
    Ok(())
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