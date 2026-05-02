//! Subcommand implementations for the knit CLI.

pub mod generate;
pub mod init;
pub mod learn;
pub mod plan;
pub mod schema;
pub mod validate;

use std::path::Path;

use knit_core::DataModel;
use knit_schema::SchemaError;

/// Load and parse a schema file, auto-detecting TOML vs JSON by extension.
///
/// Returns the parsed [`DataModel`] or an error.
pub fn load_schema(path: &str) -> Result<DataModel, SchemaError> {
    let p = Path::new(path);
    let ext = p
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    match ext.as_str() {
        "json" => knit_schema::parse_json_file(p),
        _ => knit_schema::parse_toml_file(p),
    }
}

/// Validate a parsed model and return collected errors.
pub fn validate_model(model: &DataModel) -> Vec<SchemaError> {
    knit_schema::validate(model)
}
