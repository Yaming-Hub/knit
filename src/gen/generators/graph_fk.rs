//! Graph-aware FK generator — samples target actors from source actor's graph neighbors.
//!
//! When an entity has two actor FK columns connected by an actor_relationship graph
//! (e.g., `sender_id` and `receiver_id` linked by a "messages" graph), this generator
//! reads the already-generated source column and selects targets from the source actor's
//! outbound edges in the graph.

use std::sync::Arc;

use arrow::array::{Array, ArrayRef, Int64Array};
use arrow::datatypes::DataType;
use rand::Rng;

use crate::r#gen::context::GenContext;
use crate::r#gen::traits::{FieldGenerator, KeyStore};

/// Generate a uniform usize in [0, n) using rejection sampling (dyn-compatible).
fn gen_range_usize(rng: &mut dyn Rng, n: usize) -> usize {
    debug_assert!(n > 0);
    let n = n as u64;
    let threshold = u64::MAX - (u64::MAX % n);
    loop {
        let r = rng.next_u64();
        if r < threshold {
            return (r % n) as usize;
        }
    }
}

/// Pre-computed adjacency list: for each source actor index, the list of target actor indices
/// that are reachable via the graph's outbound edges.
pub type AdjacencyList = Vec<Vec<usize>>;

/// Graph-aware FK generator that samples targets from the source actor's graph neighbors.
///
/// # Invariants
///
/// - Source field must be generated before this field (enforced by `dependency_order`).
/// - Actor entity must use Int64 PKs with InMemoryVec key store.
/// - Actor entity must be single-partition (sequential PK insertion).
pub struct GraphTargetFkGenerator {
    /// Adjacency list: source_actor_index → Vec<target_actor_index>.
    adjacency: Arc<AdjacencyList>,
    /// Reverse map: PK value → actor index (for the source entity).
    pk_to_index: Arc<std::collections::HashMap<i64, usize>>,
    /// Key store for the target entity (to convert actor index → PK value).
    target_key_store: Arc<dyn KeyStore>,
    /// Name of the source field to read from batch_columns.
    source_field: String,
}

impl GraphTargetFkGenerator {
    /// Create a new graph-aware FK generator.
    ///
    /// - `adjacency`: pre-built outbound adjacency list indexed by source actor.
    /// - `pk_to_index`: reverse map from PK values to actor indices (for source entity).
    /// - `target_key_store`: key store for converting target actor indices to PK values.
    /// - `source_field`: name of the field in the same entity to read source PKs from.
    pub fn new(
        adjacency: Arc<AdjacencyList>,
        pk_to_index: Arc<std::collections::HashMap<i64, usize>>,
        target_key_store: Arc<dyn KeyStore>,
        source_field: String,
    ) -> Self {
        Self {
            adjacency,
            pk_to_index,
            target_key_store,
            source_field,
        }
    }
}

impl FieldGenerator for GraphTargetFkGenerator {
    fn generate(&self, rng: &mut dyn Rng, count: usize, ctx: &GenContext) -> ArrayRef {
        let source_col = ctx.batch_columns.get(&self.source_field);

        let values: Vec<Option<i64>> =
            match source_col.and_then(|col| col.as_any().downcast_ref::<Int64Array>()) {
                Some(source_arr) => {
                    (0..count)
                        .map(|i| {
                            if source_arr.is_null(i) {
                                // Null source → null target
                                return None;
                            }
                            let source_pk = source_arr.value(i);
                            self.sample_target(source_pk, rng)
                        })
                        .collect()
                }
                None => {
                    // Source column not available or wrong type — fall back to uniform FK
                    tracing::warn!(
                        source_field = %self.source_field,
                        entity = %ctx.entity_name,
                        "graph target: source column not found or not Int64, using uniform FK"
                    );
                    (0..count)
                        .map(|_| self.target_key_store.sample(rng))
                        .collect()
                }
            };

        Arc::new(Int64Array::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

impl GraphTargetFkGenerator {
    /// Sample a target PK given a source PK.
    ///
    /// 1. Map source PK → actor index via reverse map.
    /// 2. Look up outbound edges for that actor.
    /// 3. If edges exist, sample one uniformly and convert to PK.
    /// 4. If no edges or unmapped source, fall back to uniform FK.
    fn sample_target(&self, source_pk: i64, rng: &mut dyn Rng) -> Option<i64> {
        // Step 1: reverse lookup
        let actor_idx = match self.pk_to_index.get(&source_pk) {
            Some(&idx) => idx,
            None => return self.target_key_store.sample(rng),
        };

        // Step 2: get neighbors
        let neighbors = self
            .adjacency
            .get(actor_idx)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);

        if neighbors.is_empty() {
            // No outbound edges — fall back to uniform FK
            return self.target_key_store.sample(rng);
        }

        // Step 3: sample a neighbor uniformly
        let idx = gen_range_usize(rng, neighbors.len());
        let target_actor_idx = neighbors[idx];

        // Step 4: convert actor index → PK via key store
        match self.target_key_store.get_by_index(target_actor_idx) {
            Some(pk) => Some(pk),
            None => self.target_key_store.sample(rng),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::r#gen::keystore::InMemoryKeyStore;
    use rand::SeedableRng;
    use rand::rngs::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_adjacency() -> AdjacencyList {
        // 5 actors: actor 0 → [1,2], actor 1 → [0,3], actor 2 → [4], actor 3 → [], actor 4 → [0,1,2,3]
        vec![
            vec![1, 2],
            vec![0, 3],
            vec![4],
            vec![], // isolated — will fall back
            vec![0, 1, 2, 3],
        ]
    }

    fn make_key_store() -> Arc<dyn KeyStore> {
        let ks = InMemoryKeyStore::with_capacity(5);
        for pk in [100, 200, 300, 400, 500] {
            ks.insert(pk);
        }
        Arc::new(ks)
    }

    fn make_pk_to_index() -> HashMap<i64, usize> {
        [(100, 0), (200, 1), (300, 2), (400, 3), (500, 4)]
            .into_iter()
            .collect()
    }

    #[test]
    fn test_graph_target_follows_edges() {
        let adjacency = Arc::new(make_adjacency());
        let pk_to_index = Arc::new(make_pk_to_index());
        let ks = make_key_store();

        let r#gen = GraphTargetFkGenerator::new(
            adjacency.clone(),
            pk_to_index.clone(),
            ks.clone(),
            "sender_id".to_string(),
        );

        // Source PKs: actor 0 (PK=100) can only reach actors 1,2 (PKs 200,300)
        let source_arr = Arc::new(Int64Array::from(vec![100; 100]));
        let mut batch_columns = HashMap::new();
        batch_columns.insert("sender_id".to_string(), source_arr as ArrayRef);

        let ctx = GenContext::new(&batch_columns, 0, 0, 1, "test_entity");
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        let result = r#gen.generate(&mut rng, 100, &ctx);
        let result_arr = result.as_any().downcast_ref::<Int64Array>().unwrap();

        // All values should be 200 or 300 (neighbors of actor 0)
        for i in 0..100 {
            let v = result_arr.value(i);
            assert!(v == 200 || v == 300, "unexpected target PK: {}", v);
        }
    }

    #[test]
    fn test_graph_target_isolated_actor_falls_back() {
        let adjacency = Arc::new(make_adjacency());
        let pk_to_index = Arc::new(make_pk_to_index());
        let ks = make_key_store();

        let r#gen =
            GraphTargetFkGenerator::new(adjacency, pk_to_index, ks, "sender_id".to_string());

        // Actor 3 (PK=400) has no outbound edges — should fall back to uniform
        let source_arr = Arc::new(Int64Array::from(vec![400; 50]));
        let mut batch_columns = HashMap::new();
        batch_columns.insert("sender_id".to_string(), source_arr as ArrayRef);

        let ctx = GenContext::new(&batch_columns, 0, 0, 1, "test_entity");
        let mut rng = ChaCha8Rng::seed_from_u64(99);

        let result = r#gen.generate(&mut rng, 50, &ctx);
        let result_arr = result.as_any().downcast_ref::<Int64Array>().unwrap();

        // All values should be valid PKs from key store
        let valid_pks: std::collections::HashSet<i64> =
            [100, 200, 300, 400, 500].into_iter().collect();
        for i in 0..50 {
            assert!(!result_arr.is_null(i));
            assert!(valid_pks.contains(&result_arr.value(i)));
        }
    }

    #[test]
    fn test_graph_target_null_source_produces_null() {
        let adjacency = Arc::new(make_adjacency());
        let pk_to_index = Arc::new(make_pk_to_index());
        let ks = make_key_store();

        let r#gen =
            GraphTargetFkGenerator::new(adjacency, pk_to_index, ks, "sender_id".to_string());

        // Mix of null and non-null source values
        let source_arr = Arc::new(Int64Array::from(vec![
            Some(100),
            None,
            Some(200),
            None,
            Some(500),
        ]));
        let mut batch_columns = HashMap::new();
        batch_columns.insert("sender_id".to_string(), source_arr as ArrayRef);

        let ctx = GenContext::new(&batch_columns, 0, 0, 1, "test_entity");
        let mut rng = ChaCha8Rng::seed_from_u64(7);

        let result = r#gen.generate(&mut rng, 5, &ctx);
        let result_arr = result.as_any().downcast_ref::<Int64Array>().unwrap();

        // Null sources produce null targets
        assert!(!result_arr.is_null(0)); // source 100 → non-null
        assert!(result_arr.is_null(1)); // source null → null
        assert!(!result_arr.is_null(2)); // source 200 → non-null
        assert!(result_arr.is_null(3)); // source null → null
        assert!(!result_arr.is_null(4)); // source 500 → non-null
    }

    #[test]
    fn output_type_is_int64() {
        let r#gen = GraphTargetFkGenerator::new(
            Arc::new(make_adjacency()),
            Arc::new(make_pk_to_index()),
            make_key_store(),
            "sender_id".to_string(),
        );

        assert_eq!(r#gen.output_type(), DataType::Int64);
    }
}
