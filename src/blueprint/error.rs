//! Error types for schema parsing and validation.

use thiserror::Error;

/// Errors that can occur when parsing or validating a Weave schema.
///
/// Wraps underlying TOML, JSON, and I/O errors as well as semantic validation
/// failures detected by [`validate()`](crate::blueprint::validate).
#[derive(Debug, Error)]
pub enum BlueprintError {
    /// The input is not valid TOML.
    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    /// The input is not valid JSON.
    #[error("JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),

    /// A file system operation failed (e.g. schema file not found).
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A semantic validation error (e.g. unknown entity reference, invalid
    /// distribution parameters, duplicate names).
    #[error("{path}: {message}")]
    Validation {
        /// Schema path where the error was found.
        path: String,
        /// Description of the validation failure.
        message: String,
    },

    /// Catch-all for schema issues not covered by other variants.
    #[error("Schema error: {0}")]
    Other(String),
}