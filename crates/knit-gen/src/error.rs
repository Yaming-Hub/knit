//! Error types for the generation engine.
//!
//! All fallible operations in `knit-gen` return [`GenError`]. The error is
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

    /// The [`GeneratorPlan`](knit_plan::GeneratorPlan) variant is not yet implemented.
    #[error("unsupported generator plan: {0}")]
    UnsupportedPlan(String),
}
