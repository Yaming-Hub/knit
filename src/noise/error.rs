//! Error types for the noise module.

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_column_not_found() {
        assert_eq!(
            NoiseError::ColumnNotFound("user_id".to_string()).to_string(),
            "column not found: user_id"
        );
    }

    #[test]
    fn display_invalid_config() {
        assert_eq!(
            NoiseError::InvalidConfig("probability must be <= 1.0".to_string()).to_string(),
            "invalid config: probability must be <= 1.0"
        );
    }

    #[test]
    fn from_arrow_error() {
        let expected = arrow::error::ArrowError::InvalidArgumentError("bad arrow".to_string())
            .to_string();
        let err = NoiseError::from(arrow::error::ArrowError::InvalidArgumentError(
            "bad arrow".to_string(),
        ));

        match err {
            NoiseError::Arrow(inner) => assert_eq!(inner.to_string(), expected),
            other => panic!("expected NoiseError::Arrow, got {other:?}"),
        }
    }
}
