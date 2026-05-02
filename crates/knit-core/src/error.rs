//! Validation errors for the core data model.

use crate::types::DistributionKind;
use thiserror::Error;

/// Errors detected during data model validation.
///
/// These represent structural or semantic issues in a Weave schema.
/// `ModelError` is the core error taxonomy; `knit-schema` wraps these
/// inside its own `SchemaError::Validation` variant. Each variant includes
/// context (path, entity, or field name) for diagnostic messages.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ModelError {
    /// A required field is absent from the schema definition.
    #[error("{path}: missing required field '{field}'")]
    MissingField { path: String, field: String },

    /// A reference (e.g. relationship target, lookup entity) points to a name
    /// that does not exist in the model.
    #[error("{path}: invalid reference to '{target}': {message}")]
    InvalidReference {
        path: String,
        target: String,
        message: String,
    },

    /// A distribution parameter is out of its valid range (e.g. negative std_dev).
    #[error("{distribution:?}.{param} = {value}: {message}")]
    InvalidDistributionParam {
        distribution: DistributionKind,
        param: String,
        value: f64,
        message: String,
    },

    /// A probability value is not in the range `[0.0, 1.0]`.
    #[error("{path}: probability {value} outside [0.0, 1.0]")]
    InvalidProbability { path: String, value: f64 },

    /// Two items within the same scope share an identical name.
    #[error("{scope}: duplicate name '{name}'")]
    DuplicateName { scope: String, name: String },

    /// The correlation matrix for an entity is invalid (e.g. not symmetric,
    /// wrong dimensions, eigenvalues out of range).
    #[error("correlations[{entity}]: {message}")]
    InvalidCorrelationMatrix { entity: String, message: String },

    /// An entity's row count specification is invalid (e.g. zero, negative range).
    #[error("{entity}.count: {message}")]
    InvalidCount { entity: String, message: String },

    /// Catch-all for validation issues not covered by other variants.
    #[error("{path}: {message}")]
    Other { path: String, message: String },
}
