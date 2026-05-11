//! Degree-weighted foreign-key generator.
//!
//! When a relationship specifies a `degree` distribution (e.g. Zipf), children
//! are assigned to parents non-uniformly: some parents receive
//! disproportionately more children than others.
//!
//! This module provides [`WeightedForeignKeyGenerator`] for integer-typed FKs
//! and [`WeightedStringForeignKeyGenerator`] for string/UUID-typed FKs.

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;
use rand_distr::{Distribution, Zipf};

use crate::gen::context::GenContext;
use crate::gen::traits::{FieldGenerator, KeyStore, StringKeyStore};
use crate::plan::DegreePlan;

/// Degree-weighted FK generator for integer primary keys.
///
/// Uses a Zipf distribution to sample parent indices, producing a power-law
/// distribution of children across parents.  Falls back to uniform sampling
/// for unsupported distribution kinds.
pub struct WeightedForeignKeyGenerator {
    key_store: Arc<dyn KeyStore>,
    degree: DegreePlan,
}

impl WeightedForeignKeyGenerator {
    /// Create a new weighted FK generator.
    pub fn new(key_store: Arc<dyn KeyStore>, degree: DegreePlan) -> Self {
        Self { key_store, degree }
    }

    /// Sample a parent index using the configured degree distribution.
    fn sample_index(&self, rng: &mut dyn RngCore, n: u64) -> usize {
        if n == 0 {
            return 0;
        }
        match self.degree.kind {
            crate::core::DistributionKind::Zipf => {
                let exponent = self
                    .degree
                    .params
                    .get("exponent")
                    .or_else(|| self.degree.params.get("s"))
                    .copied()
                    .unwrap_or(1.0);
                // Zipf::new(n, s) samples ranks in 1..=n.
                let zipf = Zipf::new(n, exponent).expect("valid Zipf params");
                let rank: f64 = zipf.sample(rng);
                // Convert 1-based rank to 0-based index, clamped.
                let idx = (rank as u64).saturating_sub(1).min(n - 1) as usize;
                idx
            }
            _ => {
                // For unsupported distributions, fall back to uniform.
                let threshold = u64::MAX - (u64::MAX % n);
                loop {
                    let r = rng.next_u64();
                    if r < threshold {
                        break (r % n) as usize;
                    }
                }
            }
        }
    }
}

impl FieldGenerator for WeightedForeignKeyGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let n = self.key_store.len() as u64;
        if n == 0 {
            if count > 0 {
                tracing::warn!(
                    entity = ctx.entity_name,
                    "weighted FK key store is empty — producing null column"
                );
            }
            return Arc::new(Int64Array::from(vec![None::<i64>; count]));
        }

        let values: Vec<Option<i64>> = (0..count)
            .map(|_| {
                let idx = self.sample_index(rng, n);
                self.key_store.get_by_index(idx)
            })
            .collect();

        Arc::new(Int64Array::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

/// Degree-weighted FK generator for string/UUID primary keys.
pub struct WeightedStringForeignKeyGenerator {
    key_store: Arc<dyn StringKeyStore>,
    degree: DegreePlan,
}

impl WeightedStringForeignKeyGenerator {
    /// Create a new weighted string FK generator.
    pub fn new(key_store: Arc<dyn StringKeyStore>, degree: DegreePlan) -> Self {
        Self { key_store, degree }
    }

    /// Sample a parent index using the configured degree distribution.
    fn sample_index(&self, rng: &mut dyn RngCore, n: u64) -> usize {
        if n == 0 {
            return 0;
        }
        match self.degree.kind {
            crate::core::DistributionKind::Zipf => {
                let exponent = self
                    .degree
                    .params
                    .get("exponent")
                    .or_else(|| self.degree.params.get("s"))
                    .copied()
                    .unwrap_or(1.0);
                let zipf = Zipf::new(n, exponent).expect("valid Zipf params");
                let rank: f64 = zipf.sample(rng);
                let idx = (rank as u64).saturating_sub(1).min(n - 1) as usize;
                idx
            }
            _ => {
                let threshold = u64::MAX - (u64::MAX % n);
                loop {
                    let r = rng.next_u64();
                    if r < threshold {
                        break (r % n) as usize;
                    }
                }
            }
        }
    }
}

impl FieldGenerator for WeightedStringForeignKeyGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let n = self.key_store.len() as u64;
        if n == 0 {
            if count > 0 {
                tracing::warn!(
                    entity = ctx.entity_name,
                    "weighted string FK key store is empty — producing null column"
                );
            }
            return Arc::new(StringArray::from(vec![None::<&str>; count]));
        }

        let values: Vec<Option<String>> = (0..count)
            .map(|_| {
                let idx = self.sample_index(rng, n);
                self.key_store.get_by_index(idx)
            })
            .collect();

        Arc::new(StringArray::from(
            values
                .iter()
                .map(|v| v.as_deref())
                .collect::<Vec<Option<&str>>>(),
        ))
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
    use crate::gen::string_keystore::InMemoryStringKeyStore;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, 0, 0, 1, "test")
    }

    fn zipf_degree(exponent: f64, parent_count: u64) -> DegreePlan {
        DegreePlan {
            kind: crate::core::DistributionKind::Zipf,
            params: std::collections::BTreeMap::from([("exponent".into(), exponent)]),
            parent_count,
        }
    }

    #[test]
    fn weighted_fk_skews_toward_low_indices() {
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=100 {
            store.insert(i);
        }
        let gen = WeightedForeignKeyGenerator::new(store, zipf_degree(1.5, 100));
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 10_000, &make_ctx());
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // Count how many samples map to the first 10 parents (indices 0-9, keys 1-10)
        let top10_count = (0..10_000)
            .filter(|&i| int_arr.value(i) >= 1 && int_arr.value(i) <= 10)
            .count();

        // With Zipf(1.5), top 10% of parents should get >>10% of children.
        // For exponent=1.5 and n=100, the top-10 expected share is ~65%.
        assert!(
            top10_count > 4000,
            "expected top-10 parents to get >40% of children, got {top10_count}/10000"
        );
    }

    #[test]
    fn weighted_fk_deterministic() {
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=50 {
            store.insert(i);
        }
        let gen = WeightedForeignKeyGenerator::new(store, zipf_degree(1.2, 50));

        let mut rng1 = ChaCha8Rng::seed_from_u64(99);
        let arr1 = gen.generate(&mut rng1, 100, &make_ctx());
        let mut rng2 = ChaCha8Rng::seed_from_u64(99);
        let arr2 = gen.generate(&mut rng2, 100, &make_ctx());

        let v1 = arr1.as_any().downcast_ref::<Int64Array>().unwrap();
        let v2 = arr2.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..100 {
            assert_eq!(v1.value(i), v2.value(i));
        }
    }

    #[test]
    fn weighted_fk_empty_store_produces_nulls() {
        let store = Arc::new(InMemoryKeyStore::new());
        let gen = WeightedForeignKeyGenerator::new(store, zipf_degree(1.0, 0));
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 10, &make_ctx());
        assert_eq!(arr.null_count(), 10);
    }

    #[test]
    fn weighted_fk_all_values_valid() {
        let store = Arc::new(InMemoryKeyStore::new());
        for i in 1..=20 {
            store.insert(i);
        }
        let gen = WeightedForeignKeyGenerator::new(store, zipf_degree(1.0, 20));
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 500, &make_ctx());
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..500 {
            let v = int_arr.value(i);
            assert!((1..=20).contains(&v), "FK value {v} out of parent range");
        }
    }

    #[test]
    fn weighted_string_fk_skews() {
        let store = Arc::new(InMemoryStringKeyStore::new());
        for i in 1..=50 {
            store.insert(format!("user-{i:03}"));
        }
        let gen = WeightedStringForeignKeyGenerator::new(store, zipf_degree(1.5, 50));
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 5000, &make_ctx());
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();

        let mut counts: HashMap<&str, usize> = HashMap::new();
        for i in 0..5000 {
            *counts.entry(str_arr.value(i)).or_default() += 1;
        }

        // user-001 should be the most frequent (rank 1 in Zipf)
        let top = counts.get("user-001").copied().unwrap_or(0);
        assert!(
            top > 200,
            "user-001 should be most frequent, got {top}/5000"
        );
    }

    #[test]
    fn weighted_string_fk_empty_store() {
        let store = Arc::new(InMemoryStringKeyStore::new());
        let gen = WeightedStringForeignKeyGenerator::new(store, zipf_degree(1.0, 0));
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 5, &make_ctx());
        assert_eq!(arr.null_count(), 5);
    }

    #[test]
    fn weighted_fk_single_parent() {
        let store = Arc::new(InMemoryKeyStore::new());
        store.insert(42);
        let gen = WeightedForeignKeyGenerator::new(store, zipf_degree(1.5, 1));
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let arr = gen.generate(&mut rng, 10, &make_ctx());
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..10 {
            assert_eq!(int_arr.value(i), 42);
        }
    }
}