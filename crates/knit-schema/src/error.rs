use thiserror::Error;

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("TOML parse error: {0}")]
    TomlParse(#[from] toml::de::Error),

    #[error("JSON parse error: {0}")]
    JsonParse(#[from] serde_json::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("{path}: {message}")]
    Validation { path: String, message: String },

    #[error("Schema error: {0}")]
    Other(String),
}
