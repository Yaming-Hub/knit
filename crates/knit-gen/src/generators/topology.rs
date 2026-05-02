//! Graph topology generators — synthetic edge/parent-id columns following
//! well-known network models.
//!
//! Two concrete generators are provided:
//!
//! - [`BarabasiAlbertGenerator`] — preferential-attachment model producing
//!   scale-free degree distributions.
//! - [`TreeGenerator`] — random hierarchical tree with Poisson branching factor.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, Int64Builder};
use arrow::datatypes::DataType;
use rand::RngCore;
use rand_distr::{Distribution, Poisson, Uniform};

use crate::context::GenContext;
use crate::traits::FieldGenerator;

// ── BarabasiAlbertGenerator ─────────────────────────────────────────

/// Generates edge targets following the Barabási–Albert preferential-attachment model.
///
/// For *N* rows (nodes 0..N−1), the first *m* nodes form a complete initial
/// clique. Each subsequent node *v* adds *m* edges, selecting targets with
/// probability proportional to their current degree.
///
/// The output column contains the target node id for each row's primary edge
/// (i.e., one edge per node after the seed clique). Seed-clique rows point to
/// node 0 as a sentinel.
///
/// # Output
///
/// `DataType::Int64`
pub struct BarabasiAlbertGenerator {
    /// Edges per new node.
    m: usize,
}

impl BarabasiAlbertGenerator {
    /// Create from plan parameters. Expected key: `m`.
    pub fn new(params: &BTreeMap<String, f64>) -> Self {
        let m = params.get("m").copied().unwrap_or(2.0).max(1.0) as usize;
        Self { m }
    }
}

impl FieldGenerator for BarabasiAlbertGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        if count == 0 {
            return Arc::new(Int64Array::from(Vec::<i64>::new()));
        }
        let m = self.m.min(count);

        // degree[v] = current degree of node v.
        let mut degree = vec![0usize; count];
        // Repeated-entry list for O(1) proportional sampling.
        let mut stubs: Vec<usize> = Vec::with_capacity(count * m * 2);
        // Output: primary edge target per node.
        let mut targets = vec![0i64; count];

        // Seed clique: nodes 0..m are fully connected.
        for i in 0..m {
            for j in (i + 1)..m {
                degree[i] += 1;
                degree[j] += 1;
                stubs.push(i);
                stubs.push(j);
            }
            targets[i] = if i == 0 { 0 } else { (i - 1) as i64 };
        }

        // Preferential attachment for remaining nodes.
        for v in m..count {
            let mut added = 0;
            let mut first_target = 0i64;
            // Try up to m edges, avoiding duplicates within this node's batch.
            let mut attempts = 0;
            let mut connected = vec![false; count];
            connected[v] = true; // no self-loops

            while added < m && attempts < m * 10 {
                attempts += 1;
                if stubs.is_empty() {
                    // Fallback: uniform random.
                    let t = Uniform::new(0, v).sample(rng);
                    if !connected[t] {
                        connected[t] = true;
                        degree[v] += 1;
                        degree[t] += 1;
                        stubs.push(v);
                        stubs.push(t);
                        if added == 0 {
                            first_target = t as i64;
                        }
                        added += 1;
                    }
                } else {
                    let idx = Uniform::new(0, stubs.len()).sample(rng);
                    let t = stubs[idx];
                    if !connected[t] {
                        connected[t] = true;
                        degree[v] += 1;
                        degree[t] += 1;
                        stubs.push(v);
                        stubs.push(t);
                        if added == 0 {
                            first_target = t as i64;
                        }
                        added += 1;
                    }
                }
            }
            targets[v] = first_target;
        }

        Arc::new(Int64Array::from(targets))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

// ── TreeGenerator ───────────────────────────────────────────────────

/// Generates a hierarchical tree structure as a nullable `parent_id` column.
///
/// Nodes are assigned in breadth-first order. The root node(s) have null
/// parents. Each node's child count is drawn from a Poisson distribution
/// with mean `branching_mean`, up to `max_depth` levels.
///
/// # Output
///
/// `DataType::Int64` (nullable — root nodes are null).
pub struct TreeGenerator {
    /// Maximum tree depth (root = depth 0).
    max_depth: usize,
    /// Poisson mean for number of children per node.
    branching_mean: f64,
}

impl TreeGenerator {
    /// Create from plan parameters. Expected keys: `max_depth`, `branching_mean`.
    pub fn new(params: &BTreeMap<String, f64>) -> Self {
        let max_depth = params.get("max_depth").copied().unwrap_or(4.0).max(1.0) as usize;
        let branching_mean = params.get("branching_mean").copied().unwrap_or(3.0).max(0.1);
        Self {
            max_depth,
            branching_mean,
        }
    }
}

impl FieldGenerator for TreeGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        if count == 0 {
            let mut builder = Int64Builder::new();
            return Arc::new(builder.finish());
        }

        let mut parent_ids: Vec<Option<i64>> = Vec::with_capacity(count);

        // BFS queue: (node_index, depth).
        let mut queue: std::collections::VecDeque<(usize, usize)> =
            std::collections::VecDeque::new();

        // Create root(s). At least one root.
        parent_ids.push(None);
        queue.push_back((0, 0));

        let poisson = Poisson::new(self.branching_mean)
            .unwrap_or_else(|_| Poisson::new(2.0).unwrap());

        while parent_ids.len() < count {
            let (parent_idx, depth) = match queue.pop_front() {
                Some(v) => v,
                None => {
                    // Queue exhausted before filling count — add another root.
                    let idx = parent_ids.len();
                    parent_ids.push(None);
                    queue.push_back((idx, 0));
                    continue;
                }
            };

            if depth >= self.max_depth {
                continue;
            }

            let n_children = poisson.sample(rng) as usize;
            for _ in 0..n_children {
                if parent_ids.len() >= count {
                    break;
                }
                let child_idx = parent_ids.len();
                parent_ids.push(Some(parent_idx as i64));
                queue.push_back((child_idx, depth + 1));
            }
        }

        // Truncate to exactly `count`.
        parent_ids.truncate(count);

        let mut builder = Int64Builder::with_capacity(count);
        for pid in &parent_ids {
            match pid {
                Some(v) => builder.append_value(*v),
                None => builder.append_null(),
            }
        }
        Arc::new(builder.finish())
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Array, Int64Array};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn test_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext {
            batch_columns: map,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test",
        }
    }

    #[test]
    fn ba_produces_valid_targets() {
        let mut params = BTreeMap::new();
        params.insert("m".into(), 2.0);
        let gen = BarabasiAlbertGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = gen.generate(&mut rng, 200, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // All targets should be valid node ids [0, 200).
        for i in 0..200 {
            let t = targets.value(i);
            assert!(t >= 0 && t < 200, "row {i}: target {t} out of range");
        }

        // Check power-law-ish: some nodes should have much higher in-degree than others.
        let mut in_degree = vec![0usize; 200];
        for i in 0..200 {
            in_degree[targets.value(i) as usize] += 1;
        }
        let max_deg = *in_degree.iter().max().unwrap();
        let min_deg = *in_degree.iter().min().unwrap();
        // BA model should produce skewed distribution.
        assert!(
            max_deg > min_deg + 1,
            "expected skew in degree distribution: max={max_deg} min={min_deg}"
        );
    }

    #[test]
    fn tree_has_valid_structure() {
        let mut params = BTreeMap::new();
        params.insert("max_depth".into(), 4.0);
        params.insert("branching_mean".into(), 3.0);
        let gen = TreeGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = gen.generate(&mut rng, 500, &ctx);
        let parents = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        assert_eq!(parents.len(), 500);

        // Check: at least one root (null parent).
        let null_count = (0..500).filter(|&i| parents.is_null(i)).count();
        assert!(null_count >= 1, "expected at least 1 root, got {null_count}");

        // Check: all non-null parent ids point to a valid earlier node.
        for i in 0..500 {
            if !parents.is_null(i) {
                let p = parents.value(i) as usize;
                assert!(p < i, "row {i}: parent {p} not before child");
            }
        }

        // Check depth: walk from any leaf to root should be <= max_depth.
        for i in (0..500).rev().take(20) {
            let mut depth = 0;
            let mut cur = i;
            while !parents.is_null(cur) {
                cur = parents.value(cur) as usize;
                depth += 1;
                assert!(depth <= 4, "depth exceeded max_depth at node {i}");
            }
        }
    }

    #[test]
    fn tree_fills_exact_count() {
        let mut params = BTreeMap::new();
        params.insert("max_depth".into(), 2.0);
        params.insert("branching_mean".into(), 1.5);
        let gen = TreeGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let ctx = test_ctx();
        let arr = gen.generate(&mut rng, 50, &ctx);
        assert_eq!(arr.len(), 50);
    }
}
