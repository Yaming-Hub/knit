//! Per-batch context passed to field generators.
//!
//! The [`GenContext`] is constructed by the generation engine once per batch
//! and handed to each [`FieldGenerator::generate`](crate::FieldGenerator::generate) call.

use arrow::array::ArrayRef;
use std::collections::HashMap;

/// Per-batch context available to generators during production.
///
/// Carries references to already-generated columns in the current batch,
/// partition metadata, and the entity name for diagnostics. Created by the
/// generation loop and passed immutably to each field generator.
///
/// # Interactions
///
/// - **Derived generators** (future PR) read `batch_columns` to compute
///   expressions referencing sibling fields.
/// - **Sequence generators** use `row_offset` + `partition_index` to produce
///   globally-unique, partition-aware identifiers.
pub struct GenContext<'a> {
    /// Other fields already generated in the current batch, keyed by field name.
    ///
    /// Only fields listed *before* the current field in topological order are
    /// present. Derived generators use this to evaluate expressions.
    pub batch_columns: &'a HashMap<String, ArrayRef>,
    /// Absolute row offset within the entity (cumulative across batches).
    ///
    /// Used by [`SequenceGenerator`](crate::generators::sequence::SequenceGenerator)
    /// to produce monotonically increasing IDs.
    pub row_offset: u64,
    /// Zero-based partition index assigned to this batch.
    pub partition_index: usize,
    /// Total number of partitions for this entity.
    pub partition_count: usize,
    /// Entity name, included in tracing spans and error messages.
    pub entity_name: &'a str,
}
