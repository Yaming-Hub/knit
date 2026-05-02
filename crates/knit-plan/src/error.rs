//! Plan errors.

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error("dependency cycle cannot be resolved: {entities:?}")]
    UnresolvableCycle { entities: Vec<String> },

    #[error("unknown entity in relationship: {name}")]
    UnknownEntity { name: String },

    #[error("planning error: {0}")]
    Other(String),
}
