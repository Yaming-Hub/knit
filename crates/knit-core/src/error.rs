use crate::types::DistributionKind;
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum ModelError {
    #[error("{path}: missing required field '{field}'")]
    MissingField { path: String, field: String },

    #[error("{path}: invalid reference to '{target}': {message}")]
    InvalidReference {
        path: String,
        target: String,
        message: String,
    },

    #[error("{distribution:?}.{param} = {value}: {message}")]
    InvalidDistributionParam {
        distribution: DistributionKind,
        param: String,
        value: f64,
        message: String,
    },

    #[error("{path}: probability {value} outside [0.0, 1.0]")]
    InvalidProbability { path: String, value: f64 },

    #[error("{scope}: duplicate name '{name}'")]
    DuplicateName { scope: String, name: String },

    #[error("correlations[{entity}]: {message}")]
    InvalidCorrelationMatrix { entity: String, message: String },

    #[error("{entity}.count: {message}")]
    InvalidCount { entity: String, message: String },

    #[error("{path}: {message}")]
    Other { path: String, message: String },
}
