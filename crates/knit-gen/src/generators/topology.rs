//! Graph topology generators — synthetic edge/parent-id columns following
//! well-known network models.
//!
//! Four concrete generators are provided:
//!
//! - [`BarabasiAlbertGenerator`] — preferential-attachment model producing
//!   scale-free degree distributions.
//! - [`TreeGenerator`] — random hierarchical tree with Poisson branching factor.
//! - [`WattsStrogatzGenerator`] — small-world model with ring lattice and
//!   random rewiring.
//! - [`ErdosRenyiGenerator`] — random graph where each edge exists independently
//!   with probability *p*.

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

        // If m=1, seed clique produces no edges. Seed stubs with node 0.
        if stubs.is_empty() && m > 0 {
            stubs.push(0);
            degree[0] += 1;
        }

        // Preferential attachment for remaining nodes.
        for v in m..count {
            let mut added = 0;
            let mut first_target = 0i64;
            let mut attempts = 0;
            let mut connected = Vec::new(); // track connected targets for this node

            while added < m && attempts < m * 10 {
                attempts += 1;
                if stubs.is_empty() {
                    // Fallback: uniform random.
                    let t = Uniform::new(0, v).sample(rng);
                    if !connected.contains(&t) {
                        connected.push(t);
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
                    if !connected.contains(&t) && t != v {
                        connected.push(t);
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

// ── WattsStrogatzGenerator ─────────────────────────────────────────

/// Generates edge targets following the Watts–Strogatz small-world model.
///
/// Nodes are arranged in a ring lattice where each node is connected to its
/// *k* nearest neighbours. Each edge is then rewired with probability *beta*
/// to a uniformly random target. This produces graphs with high clustering
/// and short average path lengths.
///
/// The output column contains the first neighbour (edge target) for each node.
///
/// # Parameters
///
/// - `k` — Number of nearest neighbours in the initial ring (default: 4, minimum: 2).
///   Must be even; odd values are rounded up.
/// - `beta` — Rewiring probability (default: 0.3, clamped to \[0, 1\]).
///
/// # Output
///
/// `DataType::Int64`
pub struct WattsStrogatzGenerator {
    /// Number of nearest neighbours (half on each side).
    k: usize,
    /// Rewiring probability.
    beta: f64,
}

impl WattsStrogatzGenerator {
    /// Create from plan parameters. Expected keys: `k`, `beta`.
    pub fn new(params: &BTreeMap<String, f64>) -> Self {
        let k_raw = params.get("k").copied().unwrap_or(4.0).max(2.0) as usize;
        // Ensure k is even
        let k = if k_raw % 2 == 0 { k_raw } else { k_raw + 1 };
        let beta = params.get("beta").copied().unwrap_or(0.3).clamp(0.0, 1.0);
        Self { k, beta }
    }
}

impl FieldGenerator for WattsStrogatzGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        if count == 0 {
            return Arc::new(Int64Array::from(Vec::<i64>::new()));
        }
        if count == 1 {
            return Arc::new(Int64Array::from(vec![0i64]));
        }

        let n = count;
        let half_k = (self.k / 2).min(n / 2);
        let uniform_node = Uniform::new(0, n);
        let uniform_01 = Uniform::new(0.0f64, 1.0);

        // Build adjacency: for each node, store its first clockwise neighbour.
        // We use the "primary edge" approach: each row outputs one edge target.
        // Start with ring lattice: node i → node (i+1) % n
        let mut targets: Vec<i64> = (0..n).map(|i| ((i + 1) % n) as i64).collect();

        // Rewire each edge with probability beta
        for i in 0..n {
            for offset in 1..=half_k {
                let j = (i + offset) % n;
                if uniform_01.sample(rng) < self.beta {
                    // Rewire: pick a random node != i
                    let mut new_target = uniform_node.sample(rng);
                    let mut attempts = 0;
                    while new_target == i && attempts < 20 {
                        new_target = uniform_node.sample(rng);
                        attempts += 1;
                    }
                    // Update the primary target for node i (last rewire wins)
                    if offset == 1 {
                        targets[i] = new_target as i64;
                    }
                }
            }
        }

        Arc::new(Int64Array::from(targets))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

// ── ErdosRenyiGenerator ────────────────────────────────────────────

/// Generates edge targets following the Erdős–Rényi G(n, p) random graph model.
///
/// Each possible edge between nodes exists independently with probability *p*.
/// For each node, the output column contains the id of its first neighbour,
/// or the node's own id if it has no edges (isolated node).
///
/// # Parameters
///
/// - `p` — Edge probability (default: 0.1, clamped to \[0, 1\]).
///
/// # Output
///
/// `DataType::Int64`
pub struct ErdosRenyiGenerator {
    /// Edge probability.
    p: f64,
}

impl ErdosRenyiGenerator {
    /// Create from plan parameters. Expected key: `p`.
    pub fn new(params: &BTreeMap<String, f64>) -> Self {
        let p = params.get("p").copied().unwrap_or(0.1).clamp(0.0, 1.0);
        Self { p }
    }
}

impl FieldGenerator for ErdosRenyiGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        if count == 0 {
            return Arc::new(Int64Array::from(Vec::<i64>::new()));
        }

        let n = count;
        let uniform_01 = Uniform::new(0.0f64, 1.0);

        // For each node, find its first neighbour where the edge exists
        let mut targets: Vec<i64> = Vec::with_capacity(n);

        for i in 0..n {
            let mut first_neighbour: Option<usize> = None;
            // Check potential edges to other nodes
            // For efficiency with large n and small p, use geometric distribution
            // to skip non-edges. For simplicity, iterate when n is small.
            if n <= 10_000 || self.p > 0.5 {
                // Direct sampling for small graphs or dense graphs
                for j in 0..n {
                    if j == i {
                        continue;
                    }
                    if uniform_01.sample(rng) < self.p {
                        first_neighbour = Some(j);
                        break;
                    }
                }
            } else {
                // Geometric skip for large sparse graphs
                let mut j = 0usize;
                while j < n {
                    if j == i {
                        j += 1;
                        continue;
                    }
                    // Geometric: skip ahead by -ln(U)/ln(1-p) edges
                    let skip = if self.p >= 1.0 {
                        0
                    } else {
                        let u = uniform_01.sample(rng);
                        if u <= 0.0 {
                            0
                        } else {
                            (u.ln() / (1.0 - self.p).ln()).floor() as usize
                        }
                    };
                    j += skip;
                    if j >= n || j == i {
                        j += 1;
                        continue;
                    }
                    first_neighbour = Some(j);
                    break;
                }
            }
            targets.push(first_neighbour.unwrap_or(i) as i64);
        }

        Arc::new(Int64Array::from(targets))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}
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

    #[test]
    fn watts_strogatz_produces_valid_targets() {
        let mut params = BTreeMap::new();
        params.insert("k".into(), 4.0);
        params.insert("beta".into(), 0.3);
        let gen = WattsStrogatzGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = gen.generate(&mut rng, 100, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        assert_eq!(targets.len(), 100);
        // All targets should be valid node ids [0, 100)
        for i in 0..100 {
            let t = targets.value(i) as usize;
            assert!(t < 100, "row {i}: target {t} out of range");
        }

        // With beta=0.3, most edges stay on the ring so most targets should be
        // neighbours (i+1)%n, but some should be rewired to distant nodes.
        let ring_targets: usize = (0..100)
            .filter(|&i| targets.value(i) == ((i + 1) % 100) as i64)
            .count();
        // At least some should be ring-like, and at least some should be rewired
        assert!(ring_targets > 30, "expected some ring edges, got {ring_targets}");
        assert!(ring_targets < 95, "expected some rewired edges, got {ring_targets} ring");
    }

    #[test]
    fn watts_strogatz_no_rewiring() {
        let mut params = BTreeMap::new();
        params.insert("k".into(), 4.0);
        params.insert("beta".into(), 0.0); // no rewiring
        let gen = WattsStrogatzGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = gen.generate(&mut rng, 50, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // With beta=0, all primary edges should be ring: i → (i+1) % n
        for i in 0..50 {
            assert_eq!(
                targets.value(i),
                ((i + 1) % 50) as i64,
                "row {i}: expected ring edge"
            );
        }
    }

    #[test]
    fn erdos_renyi_produces_valid_targets() {
        let mut params = BTreeMap::new();
        params.insert("p".into(), 0.3);
        let gen = ErdosRenyiGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = gen.generate(&mut rng, 100, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        assert_eq!(targets.len(), 100);
        for i in 0..100 {
            let t = targets.value(i) as usize;
            assert!(t < 100, "row {i}: target {t} out of range");
        }
    }

    #[test]
    fn erdos_renyi_dense_graph() {
        let mut params = BTreeMap::new();
        params.insert("p".into(), 0.99);
        let gen = ErdosRenyiGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = gen.generate(&mut rng, 50, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // With p=0.99, almost all nodes should have a neighbour (very few isolated)
        let non_self = (0..50)
            .filter(|&i| targets.value(i) != i as i64)
            .count();
        assert!(non_self > 45, "expected most nodes to have edges, got {non_self}");
    }

    #[test]
    fn erdos_renyi_sparse_graph() {
        let mut params = BTreeMap::new();
        params.insert("p".into(), 0.01);
        let gen = ErdosRenyiGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = gen.generate(&mut rng, 100, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // With p=0.01, most nodes should be isolated (self-referencing)
        let isolated = (0..100)
            .filter(|&i| targets.value(i) == i as i64)
            .count();
        assert!(isolated > 30, "expected many isolated nodes with p=0.01, got {isolated}");
    }
}
