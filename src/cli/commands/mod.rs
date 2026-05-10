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

/// Validate a parsed model and return collected errors.
pub fn validate_model(model: &DataModel) -> Vec<SchemaError> {
    crate::schema::validate(model)
}
