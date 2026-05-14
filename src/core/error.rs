//! Validation errors for the core data model.

use crate::core::types::DistributionKind;
use thiserror::Error;

/// Errors detected during data model validation.
///
/// These represent structural or semantic issues in a Weave schema.
/// `ModelError` is the core error taxonomy for structural and semantic issues
/// in a Weave schema. Each variant includes
/// context (path, entity, or field name) for diagnostic messages.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum ModelError {
    /// A required field is absent from the schema definition.
    #[error("{path}: missing required field '{field}'")]
    MissingField {
        /// Schema path where the field was expected.
        path: String,
        /// Name of the missing field.
        field: String,
    },

    /// A reference (e.g. relationship target, lookup entity) points to a name
    /// that does not exist in the model.
    #[error("{path}: invalid reference to '{target}': {message}")]
    InvalidReference {
        /// Schema path containing the reference.
        path: String,
        /// Name of the unresolved target.
        target: String,
        /// Explanation of the resolution failure.
        message: String,
    },

    /// A distribution parameter is out of its valid range (e.g. negative std_dev).
    #[error("{distribution:?}.{param} = {value}: {message}")]
    InvalidDistributionParam {
        /// The distribution kind with the invalid parameter.
        distribution: DistributionKind,
        /// Name of the offending parameter.
        param: String,
        /// The invalid value.
        value: f64,
        /// Explanation of the constraint violation.
        message: String,
    },

    /// A probability value is not in the range `[0.0, 1.0]`.
    #[error("{path}: probability {value} outside [0.0, 1.0]")]
    InvalidProbability {
        /// Schema path of the field with the invalid probability.
        path: String,
        /// The out-of-range value.
        value: f64,
    },

    /// Two items within the same scope share an identical name.
    #[error("{scope}: duplicate name '{name}'")]
    DuplicateName {
        /// Scope in which the duplicate was found (e.g. "entities").
        scope: String,
        /// The duplicated name.
        name: String,
    },

    /// The correlation matrix for an entity is invalid (e.g. not symmetric,
    /// wrong dimensions, eigenvalues out of range).
    #[error("correlations[{entity}]: {message}")]
    InvalidCorrelationMatrix {
        /// Name of the entity with the invalid matrix.
        entity: String,
        /// Description of the matrix issue.
        message: String,
    },

    /// An entity's row count specification is invalid (e.g. zero, negative range).
    #[error("{entity}.count: {message}")]
    InvalidCount {
        /// Entity with the invalid count.
        entity: String,
        /// Description of the count issue.
        message: String,
    },

    /// Catch-all for validation issues not covered by other variants.
    #[error("{path}: {message}")]
    Other {
        /// Schema path of the issue.
        path: String,
        /// Description of the error.
        message: String,
    },
}
