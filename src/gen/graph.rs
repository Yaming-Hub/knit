//! Graph generation algorithms for actor relationship networks.
//!
//! Generates edge lists from a [`GraphPlan`] and actor pool, supporting
//! multiple graph models: Scale-Free (Barabási–Albert), Small-World
//! (Watts–Strogatz), Erdős–Rényi, Hierarchical, and Custom degree sequences.
//!
//! ## Usage
//!
//! ```no_run
//! # use knit::r#gen::graph::generate_graph;
//! # fn example(graph_plan: &knit::plan::GraphPlan, actor_pool: &knit::r#gen::ActorPool) {
//! let graph = generate_graph(graph_plan, actor_pool, 42);
//! println!("generated {} edges", graph.edges.len());
//! # }
//! ```

use crate::core::GraphType;
use crate::plan::GraphPlan;
use rand::rngs::ChaCha8Rng;
use rand::{Rng, RngExt, SeedableRng};

use crate::r#gen::ActorPool;

/// A directed edge between two actors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edge {
    /// Source actor index.
    pub from: usize,
    /// Target actor index.
    pub to: usize,
}

/// Result of graph generation: an edge list with metadata.
#[derive(Debug, Clone)]
pub struct GeneratedGraph {
    /// Name of the relationship.
    pub name: String,
    /// Source entity name.
    pub from_entity: String,
    /// Target entity name.
    pub to_entity: String,
    /// Generated edges.
    pub edges: Vec<Edge>,
    /// Number of source actors.
    pub source_count: u64,
    /// Number of target actors.
    pub target_count: u64,
}

/// Generate a relationship graph from a plan and actor pool.
///
/// Returns edges connecting actor indices in the source entity to actor
/// indices in the target entity.
pub fn generate_graph(plan: &GraphPlan, pool: &ActorPool, seed: u64) -> GeneratedGraph {
    let source_count = pool.actor_count(&plan.from_entity).unwrap_or(0);
    let target_count = pool.actor_count(&plan.to_entity).unwrap_or(0);

    if source_count == 0 || target_count == 0 {
        return GeneratedGraph {
            name: plan.name.clone(),
            from_entity: plan.from_entity.clone(),
            to_entity: plan.to_entity.clone(),
            edges: Vec::new(),
            source_count,
            target_count,
        };
    }

    // Derive graph-specific seed
    let graph_hash = fnv1a_hash(plan.name.as_bytes());
    let mut rng = ChaCha8Rng::seed_from_u64(seed.wrapping_add(graph_hash));

    let avg_degree = plan.params.get("avg_degree").copied().unwrap_or(4.0);

    let edges = match plan.graph_type {
        GraphType::ScaleFree => {
            generate_scale_free(source_count, target_count, avg_degree, &mut rng)
        }
        GraphType::SmallWorld => {
            let rewire_prob = plan
                .params
                .get("rewire_probability")
                .copied()
                .unwrap_or(0.1);
            if source_count != target_count || plan.from_entity != plan.to_entity {
                // Small-world is inherently unipartite; fall back to Erdős-Rényi for bipartite
                generate_erdos_renyi(source_count, target_count, avg_degree, &mut rng)
            } else {
                generate_small_world(source_count, avg_degree, rewire_prob, &mut rng)
            }
        }
        GraphType::ErdosRenyi => {
            generate_erdos_renyi(source_count, target_count, avg_degree, &mut rng)
        }
        GraphType::Hierarchical => {
            let depth = plan.hierarchy_depth.unwrap_or(3);
            if source_count != target_count || plan.from_entity != plan.to_entity {
                // Hierarchical is inherently unipartite; fall back to Erdős-Rényi for bipartite
                generate_erdos_renyi(source_count, target_count, avg_degree, &mut rng)
            } else {
                generate_hierarchical(source_count, depth, avg_degree, &mut rng)
            }
        }
        GraphType::Custom => {
            // Custom uses avg_degree as uniform edge count per node
            generate_erdos_renyi(source_count, target_count, avg_degree, &mut rng)
        }
    };

    // Apply reciprocity if specified
    let reciprocity = plan.params.get("reciprocity").copied().unwrap_or(0.0);
    let final_edges = if reciprocity > 0.0 {
        apply_reciprocity(edges, reciprocity, &mut rng)
    } else {
        edges
    };

    GeneratedGraph {
        name: plan.name.clone(),
        from_entity: plan.from_entity.clone(),
        to_entity: plan.to_entity.clone(),
        edges: final_edges,
        source_count,
        target_count,
    }
}

/// Barabási–Albert preferential attachment model.
///
/// Produces power-law degree distributions (rich-get-richer).
/// For self-referential graphs (from_entity == to_entity), nodes attach to
/// existing nodes. For bipartite graphs, source nodes attach to target nodes
/// proportional to target degree.
fn generate_scale_free(
    source_count: u64,
    target_count: u64,
    avg_degree: f64,
    rng: &mut impl Rng,
) -> Vec<Edge> {
    let n = source_count as usize;
    let target_n = target_count as usize;
    let m = (avg_degree / 2.0).max(1.0).round() as usize; // edges per new node
    let is_bipartite = n != target_n;
    let mut edges = Vec::new();

    if is_bipartite {
        // Bipartite preferential attachment: each source connects to m targets
        let mut target_degrees = vec![1u64; target_n]; // start with degree 1 to avoid cold start

        for i in 0..n {
            let total_degree: u64 = target_degrees.iter().sum();
            let mut attached = 0;
            let mut targets_used = Vec::with_capacity(m);

            for _ in 0..(m * 3) {
                if attached >= m {
                    break;
                }
                let r = rng.random_range(0..total_degree);
                let mut cum = 0u64;
                let mut selected = 0;
                for (idx, &d) in target_degrees.iter().enumerate() {
                    cum += d;
                    if cum > r {
                        selected = idx;
                        break;
                    }
                }
                if !targets_used.contains(&selected) {
                    targets_used.push(selected);
                    edges.push(Edge {
                        from: i,
                        to: selected,
                    });
                    target_degrees[selected] += 1;
                    attached += 1;
                }
            }
        }
    } else {
        // Unipartite preferential attachment (standard BA model)
        if n <= m {
            for i in 0..n {
                for j in 0..n {
                    if i != j {
                        edges.push(Edge { from: i, to: j });
                    }
                }
            }
            return edges;
        }

        let seed_size = (m + 1).min(n);
        let mut degrees = vec![0u64; n];

        for i in 0..seed_size {
            for j in (i + 1)..seed_size {
                edges.push(Edge { from: i, to: j });
                edges.push(Edge { from: j, to: i });
                degrees[i] += 1;
                degrees[j] += 1;
            }
        }

        for new_node in seed_size..n {
            let total_degree: u64 = degrees.iter().take(new_node).sum();
            let mut attached = 0;
            let mut targets_used = Vec::with_capacity(m);

            for _ in 0..(m * 3) {
                if attached >= m {
                    break;
                }

                let target = if total_degree == 0 {
                    rng.random_range(0..new_node)
                } else {
                    let r = rng.random_range(0..total_degree);
                    let mut cum = 0u64;
                    let mut selected = 0;
                    for (idx, &d) in degrees.iter().take(new_node).enumerate() {
                        cum += d;
                        if cum > r {
                            selected = idx;
                            break;
                        }
                    }
                    selected
                };

                if !targets_used.contains(&target) {
                    targets_used.push(target);
                    edges.push(Edge {
                        from: new_node,
                        to: target,
                    });
                    degrees[new_node] += 1;
                    degrees[target] += 1;
                    attached += 1;
                }
            }
        }
    }

    edges
}

/// Watts–Strogatz small-world model.
///
/// Starts with a ring lattice where each node connects to k nearest neighbors,
/// then rewires each edge with probability p.
fn generate_small_world(
    node_count: u64,
    avg_degree: f64,
    rewire_prob: f64,
    rng: &mut impl Rng,
) -> Vec<Edge> {
    let n = node_count as usize;
    let k = (avg_degree as usize).max(2); // each side neighbors
    let half_k = k / 2;
    let mut edges = Vec::new();

    if n < 3 {
        // Trivial: fully connect
        for i in 0..n {
            for j in (i + 1)..n {
                edges.push(Edge { from: i, to: j });
            }
        }
        return edges;
    }

    // Build ring lattice
    for i in 0..n {
        for offset in 1..=half_k {
            let j = (i + offset) % n;
            edges.push(Edge { from: i, to: j });
        }
    }

    // Rewire edges
    let p = rewire_prob.clamp(0.0, 1.0);
    for edge in edges.iter_mut() {
        if rng.random::<f64>() < p {
            // Rewire to random target (not self, not duplicate)
            let new_target = rng.random_range(0..n);
            if new_target != edge.from {
                edge.to = new_target;
            }
        }
    }

    edges
}

/// Erdős–Rényi random graph model.
///
/// Each possible edge exists independently with probability p = avg_degree / target_count.
/// For bipartite graphs (from != to entity), self-loop prevention is skipped.
fn generate_erdos_renyi(
    source_count: u64,
    target_count: u64,
    avg_degree: f64,
    rng: &mut impl Rng,
) -> Vec<Edge> {
    let n = source_count as usize;
    let t = target_count as usize;
    let mut edges = Vec::new();

    if n == 0 || t == 0 {
        return edges;
    }

    let is_self_referential = n == t; // proxy for same-entity graph

    // For large graphs, use geometric skip sampling instead of iterating all pairs
    let p = if t > 1 {
        (avg_degree / (t - 1).max(1) as f64).clamp(0.0, 1.0)
    } else {
        1.0
    };

    if n as u64 * t as u64 > 100_000 {
        // Geometric skip: expected edges = n * t * p
        let expected_edges = (n as f64 * t as f64 * p) as usize;
        edges.reserve(expected_edges);
        for i in 0..n {
            // Sample number of edges for this source node from binomial approx
            let target_edges = (t as f64 * p).round() as usize;
            for _ in 0..target_edges {
                let j = rng.random_range(0..t);
                if !is_self_referential || i != j {
                    edges.push(Edge { from: i, to: j });
                }
            }
        }
    } else {
        for i in 0..n {
            for j in 0..t {
                if is_self_referential && i == j {
                    continue; // no self-loops for same-entity graphs
                }
                if rng.random::<f64>() < p {
                    edges.push(Edge { from: i, to: j });
                }
            }
        }
    }

    edges
}

/// Hierarchical tree with lateral connections.
///
/// Builds a tree structure with branching factor derived from node count and
/// depth, then adds random lateral edges within each level.
fn generate_hierarchical(
    node_count: u64,
    depth: u32,
    avg_degree: f64,
    rng: &mut impl Rng,
) -> Vec<Edge> {
    let n = node_count as usize;
    let mut edges = Vec::new();

    if n <= 1 || depth == 0 {
        return edges;
    }

    // Compute branching factor from node count and depth
    // n ≈ b^depth → b = n^(1/depth)
    let branching = (n as f64).powf(1.0 / depth as f64).round().max(2.0) as usize;

    // Assign levels: level[i] = depth of node i in tree
    let mut parent = vec![0usize; n];
    let mut level = vec![0u32; n];
    let mut current_level_start = 0;
    let mut current_level_end = 1; // root is node 0
    let mut next_node = 1;

    for d in 1..=depth {
        let level_start = next_node;
        for p_idx in current_level_start..current_level_end {
            for _ in 0..branching {
                if next_node >= n {
                    break;
                }
                parent[next_node] = p_idx;
                level[next_node] = d;
                edges.push(Edge {
                    from: p_idx,
                    to: next_node,
                });
                next_node += 1;
            }
            if next_node >= n {
                break;
            }
        }
        current_level_start = level_start;
        current_level_end = next_node;
        if next_node >= n {
            break;
        }
    }

    // Add lateral connections within the same level
    let lateral_count = ((avg_degree - 1.0).max(0.0) * n as f64 / 2.0) as usize;
    for _ in 0..lateral_count {
        let a = rng.random_range(0..n);
        let b = rng.random_range(0..n);
        if a != b && level[a] == level[b] {
            edges.push(Edge { from: a, to: b });
        }
    }

    edges
}

/// Apply reciprocity: for each existing edge A→B, add B→A with given probability.
fn apply_reciprocity(mut edges: Vec<Edge>, reciprocity: f64, rng: &mut impl Rng) -> Vec<Edge> {
    let p = reciprocity.clamp(0.0, 1.0);
    let original_len = edges.len();

    for i in 0..original_len {
        if rng.random::<f64>() < p {
            edges.push(Edge {
                from: edges[i].to,
                to: edges[i].from,
            });
        }
    }

    edges
}

/// FNV-1a hash for deterministic seed derivation.
fn fnv1a_hash(bytes: &[u8]) -> u64 {
    let mut h: u64 = 14695981039346656037;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(1099511628211);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{ActorEntityPool, ActorPoolPlan, PersonaWeight};
    use std::collections::BTreeMap;

    fn make_pool(entity: &str, count: u64) -> ActorPool {
        let plan = ActorPoolPlan {
            pools: vec![ActorEntityPool {
                entity_name: entity.into(),
                actor_count: count,
                persona_weights: vec![PersonaWeight {
                    name: "default".into(),
                    weight: 1.0,
                    traits: BTreeMap::new(),
                }],
            }],
            graph_plans: Vec::new(),
        };
        ActorPool::from_plan(&plan, 42)
    }

    fn make_graph_plan(graph_type: GraphType, avg_degree: f64) -> GraphPlan {
        GraphPlan {
            name: "follows".into(),
            from_entity: "users".into(),
            to_entity: "users".into(),
            graph_type,
            params: BTreeMap::from([("avg_degree".into(), avg_degree)]),
            community_count: None,
            hierarchy_depth: None,
        }
    }

    #[test]
    fn scale_free_produces_edges() {
        let pool = make_pool("users", 100);
        let plan = make_graph_plan(GraphType::ScaleFree, 4.0);
        let graph = generate_graph(&plan, &pool, 42);

        assert_eq!(graph.name, "follows");
        assert!(!graph.edges.is_empty());
        // Avg degree ~4 means ~200 edges for 100 nodes (directed)
        assert!(
            graph.edges.len() > 50,
            "Too few edges: {}",
            graph.edges.len()
        );
        // All indices valid
        for e in &graph.edges {
            assert!(e.from < 100);
            assert!(e.to < 100);
        }
    }

    #[test]
    fn small_world_produces_ring_like_structure() {
        let pool = make_pool("users", 50);
        let mut plan = make_graph_plan(GraphType::SmallWorld, 6.0);
        plan.params.insert("rewire_probability".into(), 0.1);
        let graph = generate_graph(&plan, &pool, 42);

        // SmallWorld with avg_degree=6 → half_k=3, so 50*3=150 edges
        assert!(
            graph.edges.len() >= 100,
            "Expected ~150 edges, got {}",
            graph.edges.len()
        );
        for e in &graph.edges {
            assert!(e.from < 50);
            assert!(e.to < 50);
        }
    }

    #[test]
    fn erdos_renyi_approximate_degree() {
        let pool = make_pool("users", 100);
        let plan = make_graph_plan(GraphType::ErdosRenyi, 5.0);
        let graph = generate_graph(&plan, &pool, 42);

        // Expected ~500 edges (100 * 5)
        let edge_count = graph.edges.len();
        assert!(
            (300..700).contains(&edge_count),
            "Expected ~500 edges, got {edge_count}"
        );
    }

    #[test]
    fn hierarchical_produces_tree_plus_lateral() {
        let pool = make_pool("users", 50);
        let mut plan = make_graph_plan(GraphType::Hierarchical, 3.0);
        plan.hierarchy_depth = Some(3);
        let graph = generate_graph(&plan, &pool, 42);

        assert!(!graph.edges.is_empty());
        // Tree edges: n-1 = 49, plus lateral
        assert!(
            graph.edges.len() >= 40,
            "Expected at least tree edges, got {}",
            graph.edges.len()
        );
    }

    #[test]
    fn empty_pool_produces_no_edges() {
        let plan = ActorPoolPlan::default();
        let pool = ActorPool::from_plan(&plan, 42);
        let graph_plan = make_graph_plan(GraphType::ScaleFree, 4.0);
        let graph = generate_graph(&graph_plan, &pool, 42);

        assert!(graph.edges.is_empty());
        assert_eq!(graph.source_count, 0);
    }

    #[test]
    fn reciprocity_adds_reverse_edges() {
        let pool = make_pool("users", 50);
        let mut plan = make_graph_plan(GraphType::ErdosRenyi, 4.0);
        plan.params.insert("reciprocity".into(), 1.0); // 100% reciprocity
        let graph = generate_graph(&plan, &pool, 42);

        // With full reciprocity, for every A→B there should be B→A
        let mut edge_set: std::collections::HashSet<(usize, usize)> =
            std::collections::HashSet::new();
        for e in &graph.edges {
            edge_set.insert((e.from, e.to));
        }
        // Count how many edges have reciprocal
        let reciprocal_count = graph
            .edges
            .iter()
            .filter(|e| edge_set.contains(&(e.to, e.from)))
            .count();
        // Should be high with reciprocity=1.0
        assert!(
            reciprocal_count > graph.edges.len() / 2,
            "Expected high reciprocity, got {reciprocal_count}/{}",
            graph.edges.len()
        );
    }

    #[test]
    fn deterministic_generation() {
        let pool = make_pool("users", 50);
        let plan = make_graph_plan(GraphType::ScaleFree, 4.0);

        let g1 = generate_graph(&plan, &pool, 42);
        let g2 = generate_graph(&plan, &pool, 42);

        assert_eq!(g1.edges, g2.edges);
    }

    #[test]
    fn bipartite_graph() {
        // from_entity ≠ to_entity
        let plan_data = ActorPoolPlan {
            pools: vec![
                ActorEntityPool {
                    entity_name: "users".into(),
                    actor_count: 30,
                    persona_weights: vec![PersonaWeight {
                        name: "default".into(),
                        weight: 1.0,
                        traits: BTreeMap::new(),
                    }],
                },
                ActorEntityPool {
                    entity_name: "products".into(),
                    actor_count: 50,
                    persona_weights: vec![PersonaWeight {
                        name: "default".into(),
                        weight: 1.0,
                        traits: BTreeMap::new(),
                    }],
                },
            ],
            graph_plans: Vec::new(),
        };
        let pool = ActorPool::from_plan(&plan_data, 42);

        let plan = GraphPlan {
            name: "purchases".into(),
            from_entity: "users".into(),
            to_entity: "products".into(),
            graph_type: GraphType::ErdosRenyi,
            params: BTreeMap::from([("avg_degree".into(), 3.0)]),
            community_count: None,
            hierarchy_depth: None,
        };

        let graph = generate_graph(&plan, &pool, 42);
        assert!(!graph.edges.is_empty());
        for e in &graph.edges {
            assert!(e.from < 30, "source index out of bounds: {}", e.from);
            assert!(e.to < 50, "target index out of bounds: {}", e.to);
        }
    }
}
