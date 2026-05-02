//! Foreign-key generator — samples from a parent entity's [`KeyStore`].
//!
//! During topological execution the parent entity is generated first,
//! populating its [`KeyStore`] with primary-key values. The
//! [`ForeignKeyGenerator`] then draws from that store to produce a referentially
//! valid foreign-key column.

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;
use crate::traits::{FieldGenerator, KeyStore};

/// Generates a column of foreign-key values by sampling from a parent entity's
/// [`KeyStore`].
///
/// Constructed by the generation engine (not the generic factory) because it
/// requires a runtime reference to the parent's key store.
///
/// # Empty store safety
///
/// If the parent key store is empty (should not happen with correct
/// topological ordering) the generator produces an all-null Int64 column and
/// logs a warning.
pub struct ForeignKeyGenerator {
    /// Shared reference to the parent entity's key store.
    key_store: Arc<dyn KeyStore>,
}

impl ForeignKeyGenerator {
    /// Create a new foreign-key generator backed by the given key store.
    pub fn new(key_store: Arc<dyn KeyStore>) -> Self {
        Self { key_store }
    }
}

impl FieldGenerator for ForeignKeyGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        if self.key_store.is_empty() {
            tracing::warn!(
                entity = ctx.entity_name,
                "FK key store is empty — producing null column"
            );
            return Arc::new(Int64Array::from(vec![None; count]));
        }

        let values: Vec<i64> = (0..count)
            .map(|_| {
                self.key_store
                    .sample(rng)
                    .expect("key store reported non-empty but sample returned None")
            })
            .collect();
        Arc::new(Int64Array::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}
