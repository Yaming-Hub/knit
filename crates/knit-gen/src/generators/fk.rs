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
        let values: Vec<Option<i64>> = (0..count).map(|_| self.key_store.sample(rng)).collect();

        // If all values are None, the key store was empty — warn once.
        if values.iter().all(|v| v.is_none()) && count > 0 {
            tracing::warn!(
                entity = ctx.entity_name,
                "FK key store is empty — producing null column"
            );
        }

        Arc::new(Int64Array::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::GenContext;
    use crate::keystore::InMemoryKeyStore;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, 0, 0, 1, "test")
    }

    #[test]
    fn fk_samples_from_store() {
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=100 {
            store.insert(i);
        }
        let gen = ForeignKeyGenerator::new(store);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 50, &make_ctx());

        assert_eq!(arr.len(), 50);
        assert_eq!(arr.null_count(), 0);
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..50 {
            let v = int_arr.value(i);
            assert!((1..=100).contains(&v), "FK value {v} out of parent range");
        }
    }

    #[test]
    fn fk_empty_store_produces_nulls() {
        let store = Arc::new(InMemoryKeyStore::new());
        let gen = ForeignKeyGenerator::new(store);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 10, &make_ctx());

        assert_eq!(arr.len(), 10);
        assert_eq!(arr.null_count(), 10);
    }

    #[test]
    fn fk_output_type() {
        let store = Arc::new(InMemoryKeyStore::new());
        let gen = ForeignKeyGenerator::new(store);
        assert_eq!(gen.output_type(), DataType::Int64);
    }

    #[test]
    fn fk_deterministic_with_same_seed() {
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=50 {
            store.insert(i);
        }
        let gen = ForeignKeyGenerator::new(store);

        let mut rng1 = ChaCha8Rng::seed_from_u64(99);
        let arr1 = gen.generate(&mut rng1, 20, &make_ctx());
        let mut rng2 = ChaCha8Rng::seed_from_u64(99);
        let arr2 = gen.generate(&mut rng2, 20, &make_ctx());

        let v1 = arr1.as_any().downcast_ref::<Int64Array>().unwrap();
        let v2 = arr2.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..20 {
            assert_eq!(v1.value(i), v2.value(i));
        }
    }

    #[test]
    fn fk_single_key_always_sampled() {
        let store = Arc::new(InMemoryKeyStore::new());
        store.insert(42);
        let gen = ForeignKeyGenerator::new(store);
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let arr = gen.generate(&mut rng, 10, &make_ctx());

        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..10 {
            assert_eq!(int_arr.value(i), 42);
        }
    }

    #[test]
    fn fk_large_store_uniform_coverage() {
        // With enough samples from a store of size 10, all keys should appear
        // and frequencies should be roughly uniform (~100 each for 1000 draws)
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=10 {
            store.insert(i);
        }
        let gen = ForeignKeyGenerator::new(store);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 1000, &make_ctx());

        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        let mut counts = std::collections::HashMap::new();
        for i in 0..1000 {
            *counts.entry(int_arr.value(i)).or_insert(0u32) += 1;
        }
        assert_eq!(
            counts.len(),
            10,
            "all 10 keys should be sampled in 1000 draws"
        );
        // Each key expected ~100 times; allow 60-140 (generous but rejects severe bias)
        for (&key, &count) in &counts {
            assert!(
                count >= 60 && count <= 140,
                "key {key} sampled {count} times, expected ~100 (60-140 range)"
            );
        }
    }

    #[test]
    fn fk_count_zero() {
        let store = Arc::new(InMemoryKeyStore::new());
        store.insert(1);
        let gen = ForeignKeyGenerator::new(store);
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let arr = gen.generate(&mut rng, 0, &make_ctx());
        assert_eq!(arr.len(), 0);
    }

    #[test]
    fn fk_different_seeds_different_output() {
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=100 {
            store.insert(i);
        }
        let gen = ForeignKeyGenerator::new(store);

        let mut rng1 = ChaCha8Rng::seed_from_u64(1);
        let arr1 = gen.generate(&mut rng1, 50, &make_ctx());
        let mut rng2 = ChaCha8Rng::seed_from_u64(2);
        let arr2 = gen.generate(&mut rng2, 50, &make_ctx());

        let v1 = arr1.as_any().downcast_ref::<Int64Array>().unwrap();
        let v2 = arr2.as_any().downcast_ref::<Int64Array>().unwrap();
        let differs = (0..50).any(|i| v1.value(i) != v2.value(i));
        assert!(
            differs,
            "different seeds should produce different FK columns"
        );
    }

    #[test]
    fn fk_negative_keys_sampled_correctly() {
        let store = Arc::new(InMemoryKeyStore::new());
        store.insert(-100);
        store.insert(-50);
        store.insert(0);
        let gen = ForeignKeyGenerator::new(store);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 30, &make_ctx());

        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..30 {
            let v = int_arr.value(i);
            assert!(v == -100 || v == -50 || v == 0, "unexpected FK value: {v}");
        }
    }
}
