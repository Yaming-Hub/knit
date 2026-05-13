//! Clustered foreign-key generator.
//!
//! Children reference parents that are "nearby" in insertion order, creating
//! locality-based clustering. For child row `i` out of `N` children referencing
//! `M` parents, the proportional center is `(i * M) / N`. The generator samples
//! uniformly from a window of `cluster_size` parents centered on that position
//! (clamped to valid bounds).
//!
//! This produces a diagonal assignment pattern where consecutive children tend
//! to reference the same group of parents.

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::gen::context::GenContext;
use crate::gen::traits::{FieldGenerator, KeyStore, StringKeyStore};

/// Clustered FK generator for integer primary keys.
pub struct ClusteredForeignKeyGenerator {
    key_store: Arc<dyn KeyStore>,
    cluster_size: u64,
    total_child_rows: u64,
}

impl ClusteredForeignKeyGenerator {
    /// Create a new clustered FK generator.
    pub fn new(key_store: Arc<dyn KeyStore>, cluster_size: u64, total_child_rows: u64) -> Self {
        Self {
            key_store,
            cluster_size: cluster_size.max(1),
            total_child_rows: total_child_rows.max(1),
        }
    }

    /// Sample a parent index within the cluster window for the given global row.
    fn sample_index(&self, rng: &mut dyn RngCore, global_row: u64, parent_count: u64) -> usize {
        if parent_count == 0 {
            return 0;
        }
        // Map child position proportionally into parent range:
        // center = (global_row * parent_count) / total_child_rows
        let center =
            (global_row as u128 * parent_count as u128 / self.total_child_rows as u128) as i64;

        let half = (self.cluster_size / 2) as i64;
        let lo = (center - half).max(0) as u64;
        let hi = ((center + half) as u64).min(parent_count - 1);
        let window = hi - lo + 1;

        // Uniform sample within window
        let threshold = u64::MAX - (u64::MAX % window);
        let offset = loop {
            let r = rng.next_u64();
            if r < threshold {
                break r % window;
            }
        };
        (lo + offset) as usize
    }
}

impl FieldGenerator for ClusteredForeignKeyGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let n = self.key_store.len() as u64;
        if n == 0 {
            tracing::warn!(
                entity = ctx.entity_name,
                "clustered FK: key store is empty — producing null column"
            );
            return Arc::new(Int64Array::from(vec![None; count]));
        }

        let values: Vec<Option<i64>> = (0..count)
            .map(|i| {
                let global_row = ctx.row_offset + i as u64;
                let idx = self.sample_index(rng, global_row, n);
                self.key_store.get_by_index(idx)
            })
            .collect();

        Arc::new(Int64Array::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

/// Clustered FK generator for string/UUID primary keys.
pub struct ClusteredStringForeignKeyGenerator {
    key_store: Arc<dyn StringKeyStore>,
    cluster_size: u64,
    total_child_rows: u64,
}

impl ClusteredStringForeignKeyGenerator {
    /// Create a new clustered string FK generator.
    pub fn new(
        key_store: Arc<dyn StringKeyStore>,
        cluster_size: u64,
        total_child_rows: u64,
    ) -> Self {
        Self {
            key_store,
            cluster_size: cluster_size.max(1),
            total_child_rows: total_child_rows.max(1),
        }
    }

    fn sample_index(&self, rng: &mut dyn RngCore, global_row: u64, parent_count: u64) -> usize {
        if parent_count == 0 {
            return 0;
        }
        let center =
            (global_row as u128 * parent_count as u128 / self.total_child_rows as u128) as i64;

        let half = (self.cluster_size / 2) as i64;
        let lo = (center - half).max(0) as u64;
        let hi = ((center + half) as u64).min(parent_count - 1);
        let window = hi - lo + 1;

        let threshold = u64::MAX - (u64::MAX % window);
        let offset = loop {
            let r = rng.next_u64();
            if r < threshold {
                break r % window;
            }
        };
        (lo + offset) as usize
    }
}

impl FieldGenerator for ClusteredStringForeignKeyGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let n = self.key_store.len() as u64;
        if n == 0 {
            tracing::warn!(
                entity = ctx.entity_name,
                "clustered FK: string key store is empty — producing null column"
            );
            return Arc::new(StringArray::from(vec![None::<&str>; count]));
        }

        let values: Vec<Option<String>> = (0..count)
            .map(|i| {
                let global_row = ctx.row_offset + i as u64;
                let idx = self.sample_index(rng, global_row, n);
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
    fn clustered_produces_valid_fks() {
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=100 {
            store.insert(i);
        }
        let gen = ClusteredForeignKeyGenerator::new(store, 10, 50);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 50, &make_ctx_with_offset(0));

        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..50 {
            let v = int_arr.value(i);
            assert!((1..=100).contains(&v), "FK value {v} out of parent range");
        }
    }

    #[test]
    fn clustered_shows_locality() {
        // With 100 parents and cluster_size=10, early children should reference
        // early parents and late children should reference late parents.
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=100 {
            store.insert(i);
        }
        let gen = ClusteredForeignKeyGenerator::new(store, 10, 100);
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        // Generate 100 child rows
        let arr = gen.generate(&mut rng, 100, &make_ctx_with_offset(0));
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // First 10 children should mostly reference low-index parents
        let early_avg: f64 = (0..10).map(|i| int_arr.value(i) as f64).sum::<f64>() / 10.0;
        // Last 10 children should mostly reference high-index parents
        let late_avg: f64 = (90..100).map(|i| int_arr.value(i) as f64).sum::<f64>() / 10.0;

        assert!(
            early_avg < late_avg,
            "early children (avg={early_avg:.1}) should reference lower parents than late children (avg={late_avg:.1})"
        );
    }

    #[test]
    fn clustered_empty_store_nulls() {
        let store = Arc::new(InMemoryKeyStore::new());
        let gen = ClusteredForeignKeyGenerator::new(store, 10, 5);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 5, &make_ctx_with_offset(0));
        assert_eq!(arr.null_count(), 5);
    }

    #[test]
    fn clustered_single_parent() {
        let store = Arc::new(InMemoryKeyStore::new());
        store.insert(42);
        let gen = ClusteredForeignKeyGenerator::new(store, 10, 10);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 10, &make_ctx_with_offset(0));

        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..10 {
            assert_eq!(int_arr.value(i), 42);
        }
    }

    #[test]
    fn clustered_deterministic_same_seed() {
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=50 {
            store.insert(i);
        }
        let gen = ClusteredForeignKeyGenerator::new(store, 20, 30);

        let mut rng1 = ChaCha8Rng::seed_from_u64(99);
        let arr1 = gen.generate(&mut rng1, 30, &make_ctx_with_offset(0));
        let mut rng2 = ChaCha8Rng::seed_from_u64(99);
        let arr2 = gen.generate(&mut rng2, 30, &make_ctx_with_offset(0));

        let v1 = arr1.as_any().downcast_ref::<Int64Array>().unwrap();
        let v2 = arr2.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..30 {
            assert_eq!(v1.value(i), v2.value(i));
        }
    }
}
