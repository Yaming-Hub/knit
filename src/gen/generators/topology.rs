//! Graph topology generators — synthetic edge/parent-id columns following
//! well-known network models.
//!
//! Seven concrete generators are provided:
//!
//! - [`BarabasiAlbertGenerator`] — preferential-attachment model producing
//!   scale-free degree distributions.
//! - [`TreeGenerator`] — random hierarchical tree with Poisson branching factor.
//! - [`WattsStrogatzGenerator`] — small-world model with ring lattice and
//!   random rewiring.
//! - [`ErdosRenyiGenerator`] — random graph where each edge exists independently
//!   with probability *p*.
//! - [`StochasticBlockGenerator`] — simplified community structure model with
//!   equal-sized communities and scalar intra/inter-community probabilities.
//! - [`ConfigurationGenerator`] — custom degree distribution (Poisson or
//!   power-law) with stub-pairing.
//! - [`CompleteGenerator`] — fully connected graph (uniform random target).

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, Int64Builder};
use arrow::datatypes::DataType;
use rand::Rng;
use rand::distr::{Distribution, Uniform};
use rand_distr::Poisson;

use crate::r#gen::context::GenContext;
use crate::r#gen::traits::FieldGenerator;

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
    fn generate(&self, rng: &mut dyn Rng, count: usize, _ctx: &GenContext) -> ArrayRef {
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
                    let t = Uniform::new(0, v)
                        .expect("preferential-attachment fallback requires a non-empty range")
                        .sample(rng);
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
                    let idx = Uniform::new(0, stubs.len())
                        .expect("stub sampling requires at least one stub")
                        .sample(rng);
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
        let branching_mean = params
            .get("branching_mean")
            .copied()
            .unwrap_or(3.0)
            .max(0.1);
        Self {
            max_depth,
            branching_mean,
        }
    }
}

impl FieldGenerator for TreeGenerator {
    fn generate(&self, rng: &mut dyn Rng, count: usize, _ctx: &GenContext) -> ArrayRef {
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
            .unwrap_or_else(|_| Poisson::new(2.0).expect("lambda=2.0 is always valid"));

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
        let k = if k_raw.is_multiple_of(2) {
            k_raw
        } else {
            k_raw + 1
        };
        let beta = params.get("beta").copied().unwrap_or(0.3).clamp(0.0, 1.0);
        Self { k, beta }
    }
}

impl FieldGenerator for WattsStrogatzGenerator {
    fn generate(&self, rng: &mut dyn Rng, count: usize, _ctx: &GenContext) -> ArrayRef {
        if count == 0 {
            return Arc::new(Int64Array::from(Vec::<i64>::new()));
        }
        if count == 1 {
            return Arc::new(Int64Array::from(vec![0i64]));
        }

        let n = count;
        let half_k = (self.k / 2).min(n / 2).max(1);
        let uniform_node = Uniform::new(0, n).expect("node range must be non-empty");
        let uniform_01 = Uniform::new(0.0f64, 1.0).expect("unit interval must be valid");

        // Build neighbour lists: for each node, k/2 clockwise neighbours.
        // neighbours[i] = [offset_1_target, offset_2_target, ...]
        let mut neighbours: Vec<Vec<usize>> = (0..n)
            .map(|i| (1..=half_k).map(|off| (i + off) % n).collect())
            .collect();

        // Rewire: for each node's each neighbour slot, rewire with probability beta
        for (i, node_neighbours) in neighbours.iter_mut().enumerate() {
            for slot in 0..half_k {
                if uniform_01.sample(rng) < self.beta {
                    let mut new_target = uniform_node.sample(rng);
                    let mut attempts = 0;
                    while (new_target == i || node_neighbours.contains(&new_target))
                        && attempts < 20
                    {
                        new_target = uniform_node.sample(rng);
                        attempts += 1;
                    }
                    if new_target != i {
                        node_neighbours[slot] = new_target;
                    }
                }
            }
        }

        // Output the first neighbour for each node
        let targets: Vec<i64> = neighbours
            .iter()
            .map(|nb| nb.first().copied().unwrap_or(0) as i64)
            .collect();

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
    fn generate(&self, rng: &mut dyn Rng, count: usize, _ctx: &GenContext) -> ArrayRef {
        if count == 0 {
            return Arc::new(Int64Array::from(Vec::<i64>::new()));
        }

        // p=0: all nodes are isolated
        if self.p == 0.0 {
            let targets: Vec<i64> = (0..count as i64).collect();
            return Arc::new(Int64Array::from(targets));
        }

        let n = count;
        let uniform_01 = Uniform::new(0.0f64, 1.0).expect("unit interval must be valid");

        // For each node, find its first neighbour where the edge exists
        let mut targets: Vec<i64> = Vec::with_capacity(n);

        for i in 0..n {
            let mut first_neighbour: Option<usize> = None;
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
                // Geometric skip for large sparse graphs (p > 0 guaranteed here)
                let log_1mp = (1.0 - self.p).ln();
                let mut j = 0usize;
                while j < n {
                    if j == i {
                        j += 1;
                        continue;
                    }
                    let u = uniform_01.sample(rng);
                    let skip = if u <= 0.0 {
                        0
                    } else {
                        (u.ln() / log_1mp).floor() as usize
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

// ── StochasticBlockGenerator ──────────────────────────────────────

/// Generates edge targets following a simplified Stochastic Block Model.
///
/// Nodes are partitioned into equal-sized communities. Edges within a
/// community occur with probability `p_intra`; edges between communities
/// occur with probability `p_inter`. This produces graphs with detectable
/// community structure.
///
/// **Note:** This is a simplified SBM with equal community sizes and
/// scalar intra/inter probabilities. Full SBM (arbitrary sizes + p_matrix)
/// may be added in a future release.
///
/// # Parameters
///
/// - `communities` — Number of communities (default: 3, clamped to \[1, n\]).
/// - `p_intra` — Within-community edge probability (default: 0.5, clamped to \[0, 1\]).
/// - `p_inter` — Between-community edge probability (default: 0.05, clamped to \[0, 1\]).
///
/// # Output
///
/// `DataType::Int64`
pub struct StochasticBlockGenerator {
    communities: usize,
    p_intra: f64,
    p_inter: f64,
}

impl StochasticBlockGenerator {
    /// Creates a new Stochastic Block Model generator.
    pub fn new(params: &BTreeMap<String, f64>) -> Self {
        let communities = params.get("communities").copied().unwrap_or(3.0).max(1.0) as usize;
        let p_intra = params
            .get("p_intra")
            .copied()
            .unwrap_or(0.5)
            .clamp(0.0, 1.0);
        let p_inter = params
            .get("p_inter")
            .copied()
            .unwrap_or(0.05)
            .clamp(0.0, 1.0);
        Self {
            communities,
            p_intra,
            p_inter,
        }
    }
}

impl FieldGenerator for StochasticBlockGenerator {
    fn generate(&self, rng: &mut dyn Rng, count: usize, _ctx: &GenContext) -> ArrayRef {
        if count == 0 {
            return Arc::new(Int64Array::from(Vec::<i64>::new()));
        }
        if count == 1 {
            return Arc::new(Int64Array::from(vec![0i64]));
        }

        let n = count;
        let k = self.communities.min(n).max(1);
        let uniform_01 = Uniform::new(0.0f64, 1.0).expect("unit interval must be valid");

        // Assign nodes to communities: community_of[i] = community index
        // Communities are approximately equal-sized. Remainder nodes go to early communities.
        let base_size = n / k;
        let remainder = n % k;
        let mut community_of = vec![0usize; n];
        let mut node_idx = 0;
        for c in 0..k {
            let size = base_size + if c < remainder { 1 } else { 0 };
            for _ in 0..size {
                if node_idx < n {
                    community_of[node_idx] = c;
                    node_idx += 1;
                }
            }
        }

        // For each node, find first neighbor using block probabilities.
        // Shuffle candidate order to eliminate positional bias.
        let mut targets: Vec<i64> = Vec::with_capacity(n);
        let mut candidates: Vec<usize> = (0..n).collect();
        for i in 0..n {
            let my_comm = community_of[i];
            let mut first_neighbour: Option<usize> = None;

            // Fisher-Yates shuffle of candidates for unbiased ordering
            for idx in (1..candidates.len()).rev() {
                let swap = Uniform::new(0, idx + 1)
                    .expect("shuffle range must be non-empty")
                    .sample(rng);
                candidates.swap(idx, swap);
            }

            for &j in &candidates {
                if j == i {
                    continue;
                }
                let p = if community_of[j] == my_comm {
                    self.p_intra
                } else {
                    self.p_inter
                };
                if uniform_01.sample(rng) < p {
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

// ── ConfigurationGenerator ────────────────────────────────────────

/// Generates edge targets following the configuration model.
///
/// Each node is assigned a degree drawn from a specified distribution.
/// "Stubs" (half-edges) are created and randomly paired to form edges.
/// The output is the first neighbor for each node from the paired edges.
///
/// # Parameters
///
/// - `mean_degree` — Average node degree for Poisson distribution (default: 4.0).
/// - `exponent` — If present, use discrete power-law distribution instead of
///   Poisson. Typical values: 2.0–3.0 for scale-free networks.
/// - `min_degree` — Minimum degree when using power-law (default: 1).
/// - `max_degree` — Maximum degree when using power-law (default: sqrt(n)).
///
/// # Output
///
/// `DataType::Int64`
pub struct ConfigurationGenerator {
    mean_degree: f64,
    exponent: Option<f64>,
    min_degree: usize,
    max_degree_param: Option<usize>,
}

impl ConfigurationGenerator {
    /// Creates a new Configuration Model generator.
    pub fn new(params: &BTreeMap<String, f64>) -> Self {
        let mean_degree = params.get("mean_degree").copied().unwrap_or(4.0).max(0.1);
        let exponent = params.get("exponent").copied();
        let min_degree = params.get("min_degree").copied().unwrap_or(1.0).max(0.0) as usize;
        let max_degree_param = params.get("max_degree").map(|&v| v.max(1.0) as usize);
        Self {
            mean_degree,
            exponent,
            min_degree,
            max_degree_param,
        }
    }

    /// Sample a degree from discrete power-law: P(k) ∝ k^{-exponent}, k ∈ [min, max].
    fn sample_power_law_degree(
        &self,
        rng: &mut dyn Rng,
        exponent: f64,
        min_k: usize,
        max_k: usize,
    ) -> usize {
        // Build CDF by rejection: sample uniform, transform
        let uniform_01 = Uniform::new(0.0f64, 1.0).expect("unit interval must be valid");
        let u = uniform_01.sample(rng);

        // Continuous power-law inverse CDF, then discretize
        let min_f = min_k.max(1) as f64;
        let max_f = max_k as f64;
        let exp1 = 1.0 - exponent;

        if exp1.abs() < 1e-10 {
            // exponent ≈ 1 → log-uniform
            let k = (min_f * (max_f / min_f).powf(u)).round() as usize;
            return k.clamp(min_k, max_k);
        }

        let k = ((min_f.powf(exp1) + u * (max_f.powf(exp1) - min_f.powf(exp1))).powf(1.0 / exp1))
            .round() as usize;
        k.clamp(min_k, max_k)
    }
}

impl FieldGenerator for ConfigurationGenerator {
    fn generate(&self, rng: &mut dyn Rng, count: usize, _ctx: &GenContext) -> ArrayRef {
        if count == 0 {
            return Arc::new(Int64Array::from(Vec::<i64>::new()));
        }
        if count == 1 {
            return Arc::new(Int64Array::from(vec![0i64]));
        }

        let n = count;
        let max_degree = self
            .max_degree_param
            .unwrap_or_else(|| (n as f64).sqrt().ceil() as usize)
            .min(n - 1)
            .max(1);

        // Clamp min_degree so it never exceeds max_degree (avoids panic in .clamp())
        let min_degree = self.min_degree.min(max_degree);

        // Assign degrees
        let degrees: Vec<usize> = if let Some(exponent) = self.exponent {
            (0..n)
                .map(|_| self.sample_power_law_degree(rng, exponent, min_degree, max_degree))
                .collect()
        } else {
            let poisson = Poisson::new(self.mean_degree)
                .unwrap_or_else(|_| Poisson::new(2.0).expect("lambda=2.0 is always valid"));
            (0..n)
                .map(|_| {
                    let d = poisson.sample(rng) as usize;
                    d.clamp(min_degree, max_degree)
                })
                .collect()
        };

        // Build stubs: each node i contributes degrees[i] stubs
        let mut stubs: Vec<usize> = Vec::with_capacity(degrees.iter().sum());
        for (i, &d) in degrees.iter().enumerate() {
            for _ in 0..d {
                stubs.push(i);
            }
        }

        // Ensure even number of stubs (can't pair an odd number)
        if stubs.len() % 2 == 1 {
            // Add one more stub to a random node
            let uniform_node = Uniform::new(0, n).expect("node range must be non-empty");
            stubs.push(uniform_node.sample(rng));
        }

        // Shuffle stubs and pair them
        // Fisher-Yates shuffle
        for i in (1..stubs.len()).rev() {
            let j = Uniform::new(0, i + 1)
                .expect("shuffle range must be non-empty")
                .sample(rng);
            stubs.swap(i, j);
        }

        // Build adjacency: first neighbor per node
        let mut first_neighbor: Vec<Option<usize>> = vec![None; n];
        for chunk in stubs.chunks(2) {
            if chunk.len() == 2 {
                let (a, b) = (chunk[0], chunk[1]);
                if a != b {
                    // Skip self-loops
                    if first_neighbor[a].is_none() {
                        first_neighbor[a] = Some(b);
                    }
                    if first_neighbor[b].is_none() {
                        first_neighbor[b] = Some(a);
                    }
                }
            }
        }

        // Nodes with no neighbors get self-reference
        let targets: Vec<i64> = first_neighbor
            .iter()
            .enumerate()
            .map(|(i, nb)| nb.unwrap_or(i) as i64)
            .collect();

        Arc::new(Int64Array::from(targets))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

// ── CompleteGenerator ─────────────────────────────────────────────

/// Generates edge targets for a fully connected (complete) graph.
///
/// In a complete graph, every node is connected to every other node.
/// Under the single-target-per-row contract, each node's output is a
/// uniformly random neighbor (since all are valid targets).
///
/// # Parameters
///
/// None required.
///
/// # Output
///
/// `DataType::Int64`
pub struct CompleteGenerator;

impl CompleteGenerator {
    /// Creates a new Complete Graph generator.
    pub fn new(_params: &BTreeMap<String, f64>) -> Self {
        Self
    }
}

impl FieldGenerator for CompleteGenerator {
    fn generate(&self, rng: &mut dyn Rng, count: usize, _ctx: &GenContext) -> ArrayRef {
        if count == 0 {
            return Arc::new(Int64Array::from(Vec::<i64>::new()));
        }
        if count == 1 {
            return Arc::new(Int64Array::from(vec![0i64]));
        }

        let n = count;
        // Each node picks a uniform random neighbor (any node except itself)
        let targets: Vec<i64> = (0..n)
            .map(|i| {
                let offset = Uniform::new(1, n)
                    .expect("complete graph requires at least two nodes")
                    .sample(rng);
                ((i + offset) % n) as i64
            })
            .collect();

        Arc::new(Int64Array::from(targets))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand::rngs::ChaCha8Rng;
    use std::collections::HashMap;

    fn test_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, 0, 0, 1, "test")
    }

    #[test]
    fn ba_produces_valid_targets() {
        let mut params = BTreeMap::new();
        params.insert("m".into(), 2.0);
        let r#gen = BarabasiAlbertGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 200, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // All targets should be valid node ids [0, 200).
        for (i, &t) in targets.values().iter().enumerate().take(200) {
            assert!((0..200).contains(&t), "row {i}: target {t} out of range");
        }

        // Check power-law-ish: some nodes should have much higher in-degree than others.
        let mut in_degree = vec![0usize; 200];
        for &t in targets.values().iter().take(200) {
            in_degree[t as usize] += 1;
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
        let r#gen = TreeGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 500, &ctx);
        let parents = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        assert_eq!(parents.len(), 500);

        // Check: at least one root (null parent).
        let null_count = (0..500).filter(|&i| parents.is_null(i)).count();
        assert!(
            null_count >= 1,
            "expected at least 1 root, got {null_count}"
        );

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
        let r#gen = TreeGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 50, &ctx);
        assert_eq!(arr.len(), 50);
    }

    #[test]
    fn watts_strogatz_produces_valid_targets() {
        let mut params = BTreeMap::new();
        params.insert("k".into(), 4.0);
        params.insert("beta".into(), 0.3);
        let r#gen = WattsStrogatzGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 100, &ctx);
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
        assert!(
            ring_targets > 30,
            "expected some ring edges, got {ring_targets}"
        );
        assert!(
            ring_targets < 95,
            "expected some rewired edges, got {ring_targets} ring"
        );
    }

    #[test]
    fn watts_strogatz_no_rewiring() {
        let mut params = BTreeMap::new();
        params.insert("k".into(), 4.0);
        params.insert("beta".into(), 0.0); // no rewiring
        let r#gen = WattsStrogatzGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 50, &ctx);
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
        let r#gen = ErdosRenyiGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 100, &ctx);
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
        let r#gen = ErdosRenyiGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 50, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // With p=0.99, almost all nodes should have a neighbour (very few isolated)
        let non_self = (0..50).filter(|&i| targets.value(i) != i as i64).count();
        assert!(
            non_self > 45,
            "expected most nodes to have edges, got {non_self}"
        );
    }

    #[test]
    fn erdos_renyi_sparse_graph() {
        let mut params = BTreeMap::new();
        params.insert("p".into(), 0.01);
        let r#gen = ErdosRenyiGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 100, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // With p=0.01, most nodes should be isolated (self-referencing)
        let isolated = (0..100).filter(|&i| targets.value(i) == i as i64).count();
        assert!(
            isolated > 30,
            "expected many isolated nodes with p=0.01, got {isolated}"
        );
    }

    #[test]
    fn erdos_renyi_zero_probability() {
        let mut params = BTreeMap::new();
        params.insert("p".into(), 0.0);
        let r#gen = ErdosRenyiGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 50, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // All nodes should be isolated (self-referencing)
        for i in 0..50 {
            assert_eq!(
                targets.value(i),
                i as i64,
                "row {i}: expected self-reference with p=0"
            );
        }
    }

    // ── StochasticBlock tests ──────────────────────────────────────

    #[test]
    fn sbm_produces_valid_targets() {
        let mut params = BTreeMap::new();
        params.insert("communities".into(), 3.0);
        params.insert("p_intra".into(), 0.5);
        params.insert("p_inter".into(), 0.05);
        let r#gen = StochasticBlockGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 150, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        assert_eq!(targets.len(), 150);
        for i in 0..150 {
            let t = targets.value(i) as usize;
            assert!(t < 150, "row {i}: target {t} out of range");
        }
    }

    #[test]
    fn sbm_community_structure() {
        let mut params = BTreeMap::new();
        params.insert("communities".into(), 2.0);
        params.insert("p_intra".into(), 0.9);
        params.insert("p_inter".into(), 0.01);
        let r#gen = StochasticBlockGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 100, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        // With 2 communities of 50, high p_intra=0.9 and low p_inter=0.01,
        // most targets should be within the same half.
        let same_community = (0..100)
            .filter(|&i| {
                let t = targets.value(i) as usize;
                if t == i {
                    return true; // isolated, count as same
                }
                // Community 0: nodes 0-49, Community 1: nodes 50-99
                (i < 50) == (t < 50)
            })
            .count();
        assert!(
            same_community > 70,
            "expected most edges within community, got {same_community}/100"
        );
    }

    #[test]
    fn sbm_single_node() {
        let params = BTreeMap::new();
        let r#gen = StochasticBlockGenerator::new(&params);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 1, &ctx);
        assert_eq!(arr.len(), 1);
    }

    // ── Configuration model tests ─────────────────────────────────

    #[test]
    fn config_poisson_produces_valid_targets() {
        let mut params = BTreeMap::new();
        params.insert("mean_degree".into(), 4.0);
        let r#gen = ConfigurationGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 200, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        assert_eq!(targets.len(), 200);
        for i in 0..200 {
            let t = targets.value(i) as usize;
            assert!(t < 200, "row {i}: target {t} out of range");
        }

        // With mean_degree=4, most nodes should have at least one neighbor
        let non_self = (0..200).filter(|&i| targets.value(i) != i as i64).count();
        assert!(
            non_self > 100,
            "expected most nodes to have neighbors, got {non_self}/200"
        );
    }

    #[test]
    fn config_power_law_produces_valid_targets() {
        let mut params = BTreeMap::new();
        params.insert("exponent".into(), 2.5);
        params.insert("min_degree".into(), 1.0);
        let r#gen = ConfigurationGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 100, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        assert_eq!(targets.len(), 100);
        for i in 0..100 {
            let t = targets.value(i) as usize;
            assert!(t < 100, "row {i}: target {t} out of range");
        }
    }

    #[test]
    fn config_single_node() {
        let params = BTreeMap::new();
        let r#gen = ConfigurationGenerator::new(&params);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 1, &ctx);
        assert_eq!(arr.len(), 1);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(targets.value(0), 0);
    }

    // ── Complete graph tests ──────────────────────────────────────

    #[test]
    fn complete_produces_valid_non_self_targets() {
        let r#gen = CompleteGenerator::new(&BTreeMap::new());
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 100, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();

        assert_eq!(targets.len(), 100);
        for i in 0..100 {
            let t = targets.value(i) as usize;
            assert!(t < 100, "row {i}: target {t} out of range");
            assert_ne!(t, i, "row {i}: complete graph should not self-reference");
        }
    }

    #[test]
    fn complete_single_node() {
        let r#gen = CompleteGenerator::new(&BTreeMap::new());
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 1, &ctx);
        assert_eq!(arr.len(), 1);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(targets.value(0), 0);
    }

    #[test]
    fn complete_empty() {
        let r#gen = CompleteGenerator::new(&BTreeMap::new());
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 0, &ctx);
        assert_eq!(arr.len(), 0);
    }

    #[test]
    fn config_min_degree_exceeds_max_degree_no_panic() {
        // min_degree=10 with count=5 → max_degree=4, should not panic
        let mut params = BTreeMap::new();
        params.insert("min_degree".into(), 10.0);
        params.insert("mean_degree".into(), 2.0);
        let r#gen = ConfigurationGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 5, &ctx);
        let targets = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(targets.len(), 5);
        for i in 0..5 {
            assert!((targets.value(i) as usize) < 5);
        }
    }
}
