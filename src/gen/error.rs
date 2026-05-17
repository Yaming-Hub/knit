//! Error types for the generation engine.
//!
//! All fallible operations in [`gen`](crate::gen) return [`GenError`]. The error is
//! propagated up to the CLI or integration-test harness for reporting.

/// Errors that can occur during data generation.
///
/// Constructed internally by batch assembly, generator factories, and future
/// parallel execution logic. Surfaced to callers of the top-level generate API.
#[derive(Debug, thiserror::Error)]
pub enum GenError {
    /// A generation step failed (e.g. mismatched array lengths, missing context).
    #[error("generation error: {0}")]
    Generation(String),

    /// An Arrow kernel or schema operation failed.
    #[error("arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),

    /// The [`GeneratorPlan`](crate::plan::GeneratorPlan) variant is not yet implemented.
    #[error("unsupported generator plan: {0}")]
    UnsupportedPlan(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_error_display_formats_message() {
        let err = GenError::Generation("missing dependency".to_string());

        assert_eq!(err.to_string(), "generation error: missing dependency");
    }

    #[test]
    fn arrow_error_display_formats_message() {
        let err = GenError::from(arrow::error::ArrowError::InvalidArgumentError(
            "bad column".to_string(),
        ));
        let message = err.to_string();

        assert!(message.starts_with("arrow error: "));
        assert!(message.contains("bad column"));
    }

    #[test]
    fn unsupported_plan_display_formats_message() {
        let err = GenError::UnsupportedPlan("Plugin".to_string());

        assert_eq!(err.to_string(), "unsupported generator plan: Plugin");
    }
}
