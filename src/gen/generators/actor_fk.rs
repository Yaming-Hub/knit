//! Actor-aware foreign-key generator — persona-weighted sampling.
//!
//! When a FK field has `actor_column = true` and the target entity has an
//! actor pool, this generator replaces uniform FK sampling with activity-weighted
//! actor selection. Actors with higher `activity_rate` traits are sampled more
//! frequently, producing realistic behavioral skew.

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::gen::actor_pool::ActorPool;
use crate::gen::context::GenContext;
use crate::gen::traits::{FieldGenerator, KeyStore};

/// Generates FK values by sampling actors weighted by persona activity rate.
///
/// Falls back to the underlying key store if the actor pool doesn't have
/// the target entity or has a count mismatch with the key store.
pub struct ActorForeignKeyGenerator {
    /// The actor pool for weighted sampling.
    actor_pool: Arc<ActorPool>,
    /// Target actor entity name.
    target_entity: String,
    /// Fallback key store for when actor pool isn't compatible.
    key_store: Arc<dyn KeyStore>,
}

impl ActorForeignKeyGenerator {
    /// Create a new actor-aware FK generator.
    ///
    /// The generator will sample from the actor pool weighted by activity rate,
    /// then map the actor index to the actual PK value in the key store.
    pub fn new(
        actor_pool: Arc<ActorPool>,
        target_entity: String,
        key_store: Arc<dyn KeyStore>,
    ) -> Self {
        Self {
            actor_pool,
            target_entity,
            key_store,
        }
    }
}

impl FieldGenerator for ActorForeignKeyGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let pool_count = self
            .actor_pool
            .actor_count(&self.target_entity)
            .unwrap_or(0);
        let store_len = self.key_store.len();

        // Validate: actor pool count must match key store size for index→PK mapping.
        if pool_count == 0 || store_len == 0 || pool_count as usize != store_len {
            if pool_count as usize != store_len && store_len > 0 {
                tracing::warn!(
                    entity = ctx.entity_name,
                    target = %self.target_entity,
                    pool_count = pool_count,
                    store_len = store_len,
                    "actor pool size != key store size — falling back to uniform FK"
                );
            }
            let values: Vec<Option<i64>> = (0..count).map(|_| self.key_store.sample(rng)).collect();
            return Arc::new(Int64Array::from(values));
        }

        // Weighted actor sampling → map index to actual PK value via key store
        let values: Vec<Option<i64>> = (0..count)
            .map(|_| {
                if let Some(actor_idx) = self.actor_pool.sample_actor(&self.target_entity, rng) {
                    self.key_store.get_by_index(actor_idx)
                } else {
                    self.key_store.sample(rng)
                }
            })
            .collect();

        Arc::new(Int64Array::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::context::GenContext;
    use crate::gen::keystore::InMemoryKeyStore;
    use crate::core::Value;
    use crate::plan::{ActorEntityPool, ActorPoolPlan, PersonaWeight};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::BTreeMap;

    fn make_test_pool() -> ActorPool {
        // 2 personas: "heavy" (weight 0.2, activity_rate 100) and "light" (weight 0.8, activity_rate 1)
        let plan = ActorPoolPlan {
            pools: vec![ActorEntityPool {
                entity_name: "users".to_string(),
                actor_count: 100,
                persona_weights: vec![
                    PersonaWeight {
                        name: "heavy".to_string(),
                        weight: 0.2,
                        traits: {
                            let mut m = BTreeMap::new();
                            m.insert("activity_rate".to_string(), Value::Float(100.0));
                            m
                        },
                    },
                    PersonaWeight {
                        name: "light".to_string(),
                        weight: 0.8,
                        traits: {
                            let mut m = BTreeMap::new();
                            m.insert("activity_rate".to_string(), Value::Float(1.0));
                            m
                        },
                    },
                ],
            }],
            graph_plans: vec![],
        };
        ActorPool::from_plan(&plan, 42)
    }

    fn make_key_store(n: usize) -> Arc<InMemoryKeyStore> {
        let ks = InMemoryKeyStore::new();
        for i in 1..=(n as i64) {
            ks.insert(i);
        }
        Arc::new(ks)
    }

    #[test]
    fn actor_fk_produces_weighted_distribution() {
        let pool = Arc::new(make_test_pool());
        let ks = make_key_store(100);
        let gen = ActorForeignKeyGenerator::new(pool, "users".to_string(), ks);

        let mut rng = ChaCha8Rng::seed_from_u64(123);
        let cols = std::collections::HashMap::new();
        let params = std::collections::HashMap::new();
        let ctx = GenContext {
            batch_columns: &cols,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "posts",
            params: &params,
        };

        let arr = gen.generate(&mut rng, 10_000, &ctx);
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // With weighted sampling, some actors (the ~20 "heavy" ones with
        // activity_rate=100) should dominate. Measure concentration: the top 20
        // most-sampled PKs should account for much more than 20% of samples.
        let mut counts = std::collections::HashMap::new();
        for i in 0..10_000 {
            *counts.entry(int_arr.value(i)).or_insert(0u32) += 1;
        }
        let mut sorted_counts: Vec<u32> = counts.values().copied().collect();
        sorted_counts.sort_unstable_by(|a, b| b.cmp(a));

        // Top 20 actors by sample count should have >> 20% of all samples
        let top20_total: u32 = sorted_counts.iter().take(20).sum();
        let top20_frac = top20_total as f64 / 10_000.0;

        // Under uniform distribution, top 20 of 100 would get ~20%.
        // With heavy weighting (100 vs 1), top 20 heavy actors should get ~96%.
        assert!(
            top20_frac > 0.70,
            "expected top-20 actors to dominate; got {top20_frac}"
        );

        // Ensure it's not completely degenerate — some light actors got sampled
        let unique_pks = counts.len();
        assert!(
            unique_pks > 20,
            "expected more than 20 unique PKs; got {unique_pks}"
        );
    }

    #[test]
    fn actor_fk_fallback_on_pool_count_mismatch() {
        let pool = Arc::new(make_test_pool()); // 100 actors
        let ks = make_key_store(50); // Only 50 keys — mismatch!
        let gen = ActorForeignKeyGenerator::new(pool, "users".to_string(), ks);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let cols = std::collections::HashMap::new();
        let params = std::collections::HashMap::new();
        let ctx = GenContext {
            batch_columns: &cols,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "posts",
            params: &params,
        };

        let arr = gen.generate(&mut rng, 100, &ctx);
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // Should fall back to uniform — all values in [1, 50]
        for i in 0..100 {
            let v = int_arr.value(i);
            assert!(v >= 1 && v <= 50, "value {v} out of range");
        }
    }

    #[test]
    fn actor_fk_fallback_on_missing_entity() {
        let pool = Arc::new(make_test_pool());
        let ks = make_key_store(10);
        let gen = ActorForeignKeyGenerator::new(pool, "nonexistent".to_string(), ks);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let cols = std::collections::HashMap::new();
        let params = std::collections::HashMap::new();
        let ctx = GenContext {
            batch_columns: &cols,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "posts",
            params: &params,
        };

        let arr = gen.generate(&mut rng, 50, &ctx);
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // Should fall back to uniform — all values in [1, 10]
        for i in 0..50 {
            let v = int_arr.value(i);
            assert!(v >= 1 && v <= 10, "value {v} out of range");
        }
    }
}
