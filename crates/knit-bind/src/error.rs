//! Error types for the knit-bind crate.

/// Errors that can occur during sink operations.
#[derive(Debug, thiserror::Error)]
pub enum BindError {
    /// An I/O error occurred while writing output.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// A Parquet encoding or writing error occurred.
    #[error("Parquet error: {0}")]
    Parquet(#[from] parquet::errors::ParquetError),

    /// An Arrow error occurred during type conversion or writing.
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// A JSON serialization error occurred.
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    /// A general bind error with a descriptive message.
    #[error("bind error: {0}")]
    Other(String),
}
