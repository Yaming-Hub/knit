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
pub mod schema;
pub mod tokenize;
pub mod validate;

use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::core::DataModel;
use crate::model as model_dir;
use crate::schema::SchemaError;

/// Load and parse a schema file or structured model directory.
///
/// Auto-detects format:
/// - Directory with `knit.toml` → structured model
/// - File with `.json` extension → JSON schema
/// - Otherwise → TOML flat schema
///
/// Returns the parsed [`DataModel`] or an error.
pub fn load_schema(path: &str) -> Result<DataModel, SchemaError> {
    let p = Path::new(path);

    // Check for structured model directory
    if model_dir::is_structured_model(p) {
        return model_dir::reader::load_model_directory(p)
            .map_err(|e| SchemaError::Other(e.to_string()));
    }

    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "json" => crate::schema::parse_json_file(p),
        _ => crate::schema::parse_toml_file(p),
    }
}

/// Save a DataModel to disk, auto-detecting format from the path.
///
/// - If path is an existing structured model directory (contains `knit.toml`),
///   or has no extension, writes as structured directory.
/// - Otherwise writes as flat TOML.
///
/// This is the write counterpart to [`load_schema`].
pub fn save_schema(model: &DataModel, path: &str) -> Result<()> {
    let p = Path::new(path);

    let use_structured = model_dir::is_structured_model(p)
        || p.extension().is_none()
        || p.is_dir();

    if use_structured {
        // Clean stale table files before writing
        let tables_dir = p.join("tables");
        if tables_dir.is_dir() {
            std::fs::remove_dir_all(&tables_dir).with_context(|| {
                format!("failed to clean stale tables directory: {}", tables_dir.display())
            })?;
        }
        model_dir::writer::write_model_directory(model, p)
            .with_context(|| format!("failed to write structured model to {path}"))?;
    } else {
        let raw = FlatSchemaOutput {
            schema_version: model.schema_version.clone(),
            model: FlatModelMeta {
                name: model.name.clone(),
                description: model.description.clone(),
            },
            entities: model.entities.clone(),
            relationships: model.relationships.clone(),
            correlations: model.correlations.clone(),
            personas: model.personas.clone(),
            actor_relationships: model.actor_relationships.clone(),
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
struct FlatSchemaOutput {
    schema_version: String,
    model: FlatModelMeta,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    entities: Vec<crate::core::Entity>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    relationships: Vec<crate::core::Relationship>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    correlations: Vec<crate::core::Correlation>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    personas: Vec<crate::core::Persona>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    actor_relationships: Vec<crate::core::ActorRelationship>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    companion_files: Vec<String>,
}

#[derive(Serialize)]
struct FlatModelMeta {
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
}

/// Validate a parsed model and return collected errors.
pub fn validate_model(model: &DataModel) -> Vec<SchemaError> {
    crate::schema::validate(model)
}
