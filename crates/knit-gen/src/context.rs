//! Per-batch context passed to field generators.

use arrow::array::ArrayRef;
use std::collections::HashMap;

/// Context available to generators during batch production.
///
/// Carries references to already-generated columns in the current batch,
/// partition metadata, and the entity name for logging.
pub struct GenContext<'a> {
    /// Other fields already generated in the current batch (keyed by field name).
    pub batch_columns: &'a HashMap<String, ArrayRef>,
    /// Row offset within the entity (for sequence generation).
    pub row_offset: u64,
    /// Current partition index.
    pub partition_index: usize,
    /// Total partitions for this entity.
    pub partition_count: usize,
    /// Entity name (for logging / diagnostics).
    pub entity_name: &'a str,
}
