//! Core traits for field generation and key storage.
//!
//! These traits define the extension points for `knit-gen`. Concrete
//! implementations live in the [`generators`](crate::generators) and
//! [`keystore`](crate::keystore) modules.

use arrow::array::ArrayRef;
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;

/// Generate a column of synthetic values as an Arrow array.
///
/// This is the primary extension point for `knit-gen`. Each field in an entity
/// is backed by one `FieldGenerator` instance, created by
/// [`create_generator`](crate::create_generator) from a
/// [`GeneratorPlan`](knit_plan::GeneratorPlan).
///
/// # Implementors
///
/// - [`DistributionGenerator`](crate::generators::distribution::DistributionGenerator)
/// - [`SequenceGenerator`](crate::generators::sequence::SequenceGenerator)
/// - [`ConstantGenerator`](crate::generators::constant::ConstantGenerator)
/// - [`UuidGenerator`](crate::generators::uuid_gen::UuidGenerator)
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
/// [`RngTree`](knit_plan::RngTree).
pub trait FieldGenerator: Send + Sync {
    /// Produce `count` values using the given RNG and generation context.
    ///
    /// The returned [`ArrayRef`] must have exactly `count` elements and match
    /// the type declared by [`output_type`](Self::output_type).
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef;

    /// The Arrow data type this generator produces.
    ///
    /// Used by [`assemble_batch`](crate::assemble_batch) to construct the
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
/// - [`InMemoryKeyStore`](crate::InMemoryKeyStore) — vec-backed, suitable for ≤10 M keys.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` because generation partitions run in
/// parallel via Rayon (future PR).
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
}
