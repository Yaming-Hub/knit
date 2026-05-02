//! Core traits for field generation and key storage.

use arrow::array::ArrayRef;
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;

/// Generates a column of synthetic values as an Arrow array.
///
/// Implementations are expected to be deterministic for a given RNG state,
/// enabling reproducible data generation across runs.
pub trait FieldGenerator: Send + Sync {
    /// Produce `count` values using the given RNG and context.
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef;

    /// The Arrow data type this generator produces.
    fn output_type(&self) -> DataType;
}

/// Key store for foreign-key resolution.
///
/// Stores primary-key values so that foreign-key generators can sample
/// valid references during generation.
pub trait KeyStore: Send + Sync {
    /// Insert a primary-key value.
    fn insert(&self, key: i64);

    /// Sample a random key from the store.
    fn sample(&self, rng: &mut dyn RngCore) -> Option<i64>;

    /// Number of keys stored.
    fn len(&self) -> usize;

    /// Returns `true` if the store contains no keys.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
