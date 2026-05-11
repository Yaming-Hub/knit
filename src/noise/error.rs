//! Error types for the knit-noise crate.

use thiserror::Error;

/// Errors that can occur during noise injection.
///
/// Returned by [`Perturbator::perturb`](crate::noise::Perturbator::perturb) and
/// [`Pipeline::run`](crate::noise::Pipeline::run) when a perturbation step fails.
#[derive(Debug, Error)]
pub enum NoiseError {
    /// The target column index or name does not exist in the batch.
    #[error("column not found: {0}")]
    ColumnNotFound(String),

    /// An Arrow operation failed (e.g., type cast, builder error).
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// A configuration value is out of its valid range.
    #[error("invalid config: {0}")]
    InvalidConfig(String),

    /// A scope predicate expression failed to evaluate.
    #[error("scope error: {0}")]
    Scope(String),
}