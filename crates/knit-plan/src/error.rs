//! Error types for the execution planner.

/// Errors that can occur during execution plan compilation.
///
/// These indicate issues in the schema's relationship graph that prevent
/// the planner from producing a valid generation order.
#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    /// A dependency cycle exists that the planner cannot break with deferred refs.
    /// This should not occur in practice since Tarjan's SCC + deferral handles all cycles.
    #[error("dependency cycle cannot be resolved: {entities:?}")]
    UnresolvableCycle {
        /// Entity names involved in the unbreakable cycle.
        entities: Vec<String>,
    },

    /// A relationship references an entity that doesn't exist in the model.
    #[error("unknown entity in relationship: {name}")]
    UnknownEntity {
        /// Name of the missing entity.
        name: String,
    },

    /// Catch-all for unexpected planning failures.
    #[error("planning error: {0}")]
    Other(String),
}
