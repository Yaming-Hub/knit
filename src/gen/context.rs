//! Per-batch context passed to field generators.
//!
//! The [`GenContext`] is constructed by the generation engine once per batch
//! and handed to each [`FieldGenerator::generate`](crate::gen::FieldGenerator::generate) call.

use arrow::array::ArrayRef;
use std::collections::HashMap;

/// Per-batch context available to generators during production.
///
/// Carries references to already-generated columns in the current batch,
/// partition metadata, user-supplied parameters, and the entity name for
/// diagnostics. Created by the generation loop and passed immutably to each
/// field generator.
///
/// # Interactions
///
/// - **Derived generators** read `batch_columns` to compute expressions
///   referencing sibling fields, and `params` to resolve `${param.key}` refs.
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
    /// Used by [`SequenceGenerator`](crate::gen::generators::sequence::SequenceGenerator)
    /// to produce monotonically increasing IDs.
    pub row_offset: u64,
    /// Zero-based partition index assigned to this batch.
    pub partition_index: usize,
    /// Total number of partitions for this entity.
    pub partition_count: usize,
    /// Entity name, included in tracing spans and error messages.
    pub entity_name: &'a str,
    /// User-supplied parameters from `--param key=value` CLI flags.
    ///
    /// Derived generators resolve `${param.key}` placeholders from this map.
    /// Empty by default when no params are provided.
    pub params: &'a HashMap<String, String>,
}

/// A static empty params map used as default when no params are provided.
static EMPTY_PARAMS: std::sync::LazyLock<HashMap<String, String>> =
    std::sync::LazyLock::new(HashMap::new);

impl<'a> GenContext<'a> {
    /// Create a context with no user-supplied parameters (the common case).
    pub fn new(
        batch_columns: &'a HashMap<String, ArrayRef>,
        row_offset: u64,
        partition_index: usize,
        partition_count: usize,
        entity_name: &'a str,
    ) -> Self {
        Self {
            batch_columns,
            row_offset,
            partition_index,
            partition_count,
            entity_name,
            params: &EMPTY_PARAMS,
        }
    }

    /// Attach user-supplied parameters to this context.
    pub fn with_params(mut self, params: &'a HashMap<String, String>) -> Self {
        self.params = params;
        self
    }
}
