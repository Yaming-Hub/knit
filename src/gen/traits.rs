//! Core traits for field generation and key storage.
//!
//! These traits define the extension points for `knit-gen`. Concrete
//! implementations live in the [`generators`](crate::gen::generators) and
//! [`keystore`](crate::gen::keystore) modules.

use arrow::array::ArrayRef;
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::gen::context::GenContext;

/// Generate a column of synthetic values as an Arrow array.
///
/// This is the primary extension point for `knit-gen`. Each field in an entity
/// is backed by one `FieldGenerator` instance, created by
/// [`create_generator`](crate::gen::create_generator) from a
/// [`GeneratorPlan`](crate::plan::GeneratorPlan).
///
/// # Implementors
///
/// - [`DistributionGenerator`](crate::gen::generators::distribution::DistributionGenerator)
/// - [`SequenceGenerator`](crate::gen::generators::sequence::SequenceGenerator)
/// - [`ConstantGenerator`](crate::gen::generators::constant::ConstantGenerator)
/// - [`UuidGenerator`](crate::gen::generators::uuid_gen::UuidGenerator)
///
/// # Determinism
///
/// Implementations must be deterministic for a given RNG state so that the
/// same seed reproduces identical datasets across runs.
///
/// # Callers
///
/// The batch-assembly loop in the generation engine calls [`generate`](Self::generate)
/// once per field per batch, passing the per-field RNG derived from the
/// [`RngTree`](crate::plan::RngTree).
pub trait FieldGenerator: Send + Sync {
    /// Produce `count` values using the given RNG and generation context.
    ///
    /// The returned [`ArrayRef`] must have exactly `count` elements and match
    /// the type declared by [`output_type`](Self::output_type).
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef;

    /// The Arrow data type this generator produces.
    ///
    /// Used by [`assemble_batch`](crate::gen::assemble_batch) to construct the
    /// [`RecordBatch`](arrow::record_batch::RecordBatch) schema.
    fn output_type(&self) -> DataType;
}

/// Thread-safe key store for foreign-key resolution.
///
/// During generation, primary-key generators insert keys via [`insert`](Self::insert).
/// Foreign-key generators in downstream entities then call [`sample`](Self::sample) to
/// obtain valid references, ensuring referential integrity.
///
/// # Implementors
///
/// - [`InMemoryKeyStore`](crate::gen::InMemoryKeyStore) — vec-backed, suitable for ≤10 M keys.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` because generation partitions run in
/// parallel via Rayon.
pub trait KeyStore: Send + Sync {
    /// Insert a primary-key value into the store.
    fn insert(&self, key: i64);

    /// Sample a random key uniformly from the store.
    ///
    /// Returns `None` if the store is empty (no parent rows generated yet).
    fn sample(&self, rng: &mut dyn RngCore) -> Option<i64>;

    /// Return the number of keys currently stored.
    fn len(&self) -> usize;

    /// Returns `true` if the store contains no keys.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a key by its insertion index (0-based).
    ///
    /// Returns `None` if the index is out of bounds. Used by actor-aware FK
    /// generation to map a weighted actor index to the actual PK value.
    ///
    /// Default implementation falls back to `sample` (ignoring the index),
    /// which is only correct for full in-memory stores that preserve insertion order.
    fn get_by_index(&self, _index: usize) -> Option<i64> {
        None
    }
}

/// Thread-safe key store for string/UUID foreign-key resolution.
///
/// Parallel to [`KeyStore`] but stores `String` values for UUID and
/// string-typed primary keys. Used by [`StringForeignKeyGenerator`](crate::gen::generators::string_fk::StringForeignKeyGenerator).
pub trait StringKeyStore: Send + Sync {
    /// Insert a primary-key value into the store.
    fn insert(&self, key: String);

    /// Sample a random key uniformly from the store.
    ///
    /// Returns `None` if the store is empty (no parent rows generated yet).
    /// Clones the sampled value to avoid holding a lock across generation.
    fn sample(&self, rng: &mut dyn RngCore) -> Option<String>;

    /// Return the number of keys currently stored.
    fn len(&self) -> usize;

    /// Returns `true` if the store contains no keys.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Get a key by its insertion index (0-based).
    ///
    /// Returns `None` if the index is out of bounds. Used by degree-weighted FK
    /// generation to map a Zipf rank to a specific parent key.
    fn get_by_index(&self, _index: usize) -> Option<String> {
        None
    }
}