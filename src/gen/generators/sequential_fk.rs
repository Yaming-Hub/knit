//! Sequential (round-robin) foreign-key generator.
//!
//! Assigns parent keys in deterministic round-robin order based on the child
//! row's absolute position: `parent_index = (row_offset + batch_row) % parent_count`.
//!
//! This is fully deterministic regardless of partition count or thread scheduling.

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::gen::context::GenContext;
use crate::gen::traits::{FieldGenerator, KeyStore, StringKeyStore};

/// Round-robin FK generator for integer primary keys.
pub struct SequentialForeignKeyGenerator {
    key_store: Arc<dyn KeyStore>,
}

impl SequentialForeignKeyGenerator {
    /// Create a new sequential FK generator.
    pub fn new(key_store: Arc<dyn KeyStore>) -> Self {
        Self { key_store }
    }
}

impl FieldGenerator for SequentialForeignKeyGenerator {
    fn generate(&self, _rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let n = self.key_store.len() as u64;
        if n == 0 {
            tracing::warn!(
                entity = ctx.entity_name,
                "sequential FK: key store is empty — producing null column"
            );
            return Arc::new(Int64Array::from(vec![None; count]));
        }

        let values: Vec<Option<i64>> = (0..count)
            .map(|i| {
                let global_row = ctx.row_offset + i as u64;
                let idx = (global_row % n) as usize;
                self.key_store.get_by_index(idx)
            })
            .collect();

        Arc::new(Int64Array::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

/// Round-robin FK generator for string/UUID primary keys.
pub struct SequentialStringForeignKeyGenerator {
    key_store: Arc<dyn StringKeyStore>,
}

impl SequentialStringForeignKeyGenerator {
    /// Create a new sequential string FK generator.
    pub fn new(key_store: Arc<dyn StringKeyStore>) -> Self {
        Self { key_store }
    }
}

impl FieldGenerator for SequentialStringForeignKeyGenerator {
    fn generate(&self, _rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let n = self.key_store.len() as u64;
        if n == 0 {
            tracing::warn!(
                entity = ctx.entity_name,
                "sequential FK: string key store is empty — producing null column"
            );
            return Arc::new(StringArray::from(vec![None::<&str>; count]));
        }

        let values: Vec<Option<String>> = (0..count)
            .map(|i| {
                let global_row = ctx.row_offset + i as u64;
                let idx = (global_row % n) as usize;
                self.key_store.get_by_index(idx)
            })
            .collect();

        Arc::new(StringArray::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Utf8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::context::GenContext;
    use crate::gen::keystore::InMemoryKeyStore;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx_with_offset(offset: u64) -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, offset, 0, 1, "test")
    }

    #[test]
    fn sequential_round_robin() {
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=5 {
            store.insert(i);
        }
        let gen = SequentialForeignKeyGenerator::new(store);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 12, &make_ctx_with_offset(0));

        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        // Should cycle: 1,2,3,4,5,1,2,3,4,5,1,2
        for i in 0..12 {
            let expected = (i % 5) as i64 + 1;
            assert_eq!(int_arr.value(i), expected, "row {i}");
        }
    }

    #[test]
    fn sequential_respects_row_offset() {
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=5 {
            store.insert(i);
        }
        let gen = SequentialForeignKeyGenerator::new(store);
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        // Offset 3 means first row is global row 3 → index 3 → key 4
        let arr = gen.generate(&mut rng, 5, &make_ctx_with_offset(3));
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(int_arr.value(0), 4); // index 3
        assert_eq!(int_arr.value(1), 5); // index 4
        assert_eq!(int_arr.value(2), 1); // index 0 (wrap)
        assert_eq!(int_arr.value(3), 2); // index 1
        assert_eq!(int_arr.value(4), 3); // index 2
    }

    #[test]
    fn sequential_empty_store_nulls() {
        let store = Arc::new(InMemoryKeyStore::new());
        let gen = SequentialForeignKeyGenerator::new(store);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 5, &make_ctx_with_offset(0));
        assert_eq!(arr.null_count(), 5);
    }

    #[test]
    fn sequential_deterministic_across_seeds() {
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=10 {
            store.insert(i);
        }
        let gen = SequentialForeignKeyGenerator::new(store);

        let mut rng1 = ChaCha8Rng::seed_from_u64(1);
        let mut rng2 = ChaCha8Rng::seed_from_u64(999);

        let arr1 = gen.generate(&mut rng1, 10, &make_ctx_with_offset(0));
        let arr2 = gen.generate(&mut rng2, 10, &make_ctx_with_offset(0));

        let v1 = arr1.as_any().downcast_ref::<Int64Array>().unwrap();
        let v2 = arr2.as_any().downcast_ref::<Int64Array>().unwrap();
        // Sequential is deterministic regardless of RNG seed
        for i in 0..10 {
            assert_eq!(v1.value(i), v2.value(i));
        }
    }
}
