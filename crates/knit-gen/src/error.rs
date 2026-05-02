//! Error types for the generation engine.

/// Errors that can occur during data generation.
#[derive(Debug, thiserror::Error)]
pub enum GenError {
    /// A generation step failed.
    #[error("generation error: {0}")]
    Generation(String),

    /// An Arrow operation failed.
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// The generator plan variant is not yet supported.
    #[error("unsupported generator plan: {0}")]
    UnsupportedPlan(String),
}
