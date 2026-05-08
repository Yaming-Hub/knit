//! Actor relationship discovery — detect actor-to-actor interaction patterns.
//!
//! When a table has multiple actor columns (e.g., `sender_id` and `recipient_id`),
//! this module analyzes the co-occurrence patterns to determine the graph structure
//! of actor interactions: degree distribution, reciprocity, clustering, and
//! community structure.
//!
//! The output is an [`ActorRelationshipSpec`] that can be emitted as an
//! `[[actor_relationships]]` section in the learned schema.

use std::collections::{BTreeMap, HashMap};

use arrow::array::{Array, StringArray};
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use tracing::{debug, info};

use crate::core::GraphType;

/// Discovered actor-to-actor relationship specification.
#[derive(Debug, Clone)]
pub struct ActorRelationshipSpec {
    /// Relationship name (auto-generated from column names).
    pub name: String,
    /// Source entity (the table containing the actor columns).
    pub from_entity: String,
    /// Target entity (same as from_entity for self-referencing relationships).
    pub to_entity: String,
    /// Detected graph type based on structural analysis.
    pub graph_type: GraphType,
    /// Graph parameters derived from the data.
    pub params: BTreeMap<String, f64>,
    /// Estimated community count (if communities were detected).
    pub community_count: Option<u32>,
    /// Estimated hierarchy depth (for hierarchical graphs).
    pub hierarchy_depth: Option<u32>,
}

/// Configuration for relationship discovery.
#[derive(Debug, Clone)]
pub struct RelationshipDiscoveryConfig {
    /// Minimum number of edges to consider a relationship valid.
    pub min_edges: usize,
    /// Minimum number of distinct actors to analyze.
    pub min_actors: usize,
}

impl Default for RelationshipDiscoveryConfig {
    fn default() -> Self {
        Self {
            min_edges: 10,
            min_actors: 3,
        }
    }
}

/// Accumulates directed edges between actors from record batches.
#[derive(Debug)]
pub struct RelationshipAccumulator {
    /// Source actor column name.
    from_column: String,
    /// Target actor column name.
    to_column: String,
    /// Entity/table name.
    entity_name: String,
    /// Edge counts: (from_actor, to_actor) → count.
    edges: HashMap<(String, String), u64>,
    /// Total edges observed.
    total_edges: u64,
}

impl RelationshipAccumulator {
    /// Create a new accumulator for a pair of actor columns.
    pub fn new(from_column: String, to_column: String, entity_name: String) -> Self {
        Self {
            from_column,
            to_column,
            entity_name,
            edges: HashMap::new(),
            total_edges: 0,
        }
    }

    /// Process a RecordBatch to extract edges between actors.
    pub fn observe_batch(&mut self, batch: &RecordBatch) {
        let schema = batch.schema();

        let from_idx = match schema.index_of(&self.from_column) {
            Ok(idx) => idx,
            Err(_) => return,
        };
        let to_idx = match schema.index_of(&self.to_column) {
            Ok(idx) => idx,
            Err(_) => return,
        };

        let from_array = batch.column(from_idx);
        let to_array = batch.column(to_idx);

        let from_strings = extract_strings(from_array);
        let to_strings = extract_strings(to_array);

        let (from_arr, to_arr) = match (from_strings, to_strings) {
            (Some(f), Some(t)) => (f, t),
            _ => return,
        };

        for row in 0..batch.num_rows() {
            if from_arr.is_null(row) || to_arr.is_null(row) {
                continue;
            }
            let from_val = from_arr.value(row).to_string();
            let to_val = to_arr.value(row).to_string();

            // Skip self-edges (same actor in both columns)
            if from_val == to_val {
                continue;
            }

            *self.edges.entry((from_val, to_val)).or_insert(0) += 1;
            self.total_edges += 1;
        }
    }

    /// Finalize the accumulator into a relationship spec.
    ///
    /// Returns `None` if insufficient data for meaningful analysis.
    pub fn finalize(self, config: &RelationshipDiscoveryConfig) -> Option<ActorRelationshipSpec> {
        if self.total_edges < config.min_edges as u64 {
            debug!(
                from = %self.from_column,
                to = %self.to_column,
                edges = self.total_edges,
                "insufficient edges for relationship discovery"
            );
            return None;
        }

        let metrics = compute_graph_metrics(&self.edges);

        if metrics.unique_actors < config.min_actors {
            return None;
        }

        let graph_type = classify_graph_type(&metrics);
        let mut params = BTreeMap::new();
        params.insert("avg_degree".into(), metrics.avg_degree);
        params.insert("reciprocity".into(), metrics.reciprocity);
        if metrics.clustering_coefficient > 0.0 {
            params.insert("clustering".into(), metrics.clustering_coefficient);
        }

        let community_count = if metrics.community_estimate > 1 {
            Some(metrics.community_estimate as u32)
        } else {
            None
        };

        let hierarchy_depth = if matches!(graph_type, GraphType::Hierarchical) {
            Some(metrics.estimated_depth)
        } else {
            None
        };

        let name = format!(
            "{}_{}_{}_network",
            self.entity_name, self.from_column, self.to_column
        );

        info!(
            name = %name,
            graph_type = ?graph_type,
            edges = self.total_edges,
            actors = metrics.unique_actors,
            avg_degree = %format!("{:.1}", metrics.avg_degree),
            reciprocity = %format!("{:.2}", metrics.reciprocity),
            "discovered actor relationship"
        );

        Some(ActorRelationshipSpec {
            name,
            from_entity: self.entity_name.clone(),
            to_entity: self.entity_name,
            graph_type,
            params,
            community_count,
            hierarchy_depth,
        })
    }

    /// Number of unique edges observed.
    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Total edge observations (including repeats).
    pub fn total_observations(&self) -> u64 {
        self.total_edges
    }
}

/// Graph metrics computed from edge data.
#[derive(Debug)]
struct GraphMetrics {
    unique_actors: usize,
    avg_degree: f64,
    reciprocity: f64,
    clustering_coefficient: f64,
    degree_variance: f64,
    community_estimate: usize,
    estimated_depth: u32,
}

/// Compute structural metrics from a directed edge set.
fn compute_graph_metrics(edges: &HashMap<(String, String), u64>) -> GraphMetrics {
    if edges.is_empty() {
        return GraphMetrics {
            unique_actors: 0,
            avg_degree: 0.0,
            reciprocity: 0.0,
            clustering_coefficient: 0.0,
            degree_variance: 0.0,
            community_estimate: 1,
            estimated_depth: 1,
        };
    }

    // Collect unique actors and out-degree
    let mut out_degree: HashMap<&str, u64> = HashMap::new();
    let mut in_degree: HashMap<&str, u64> = HashMap::new();
    let mut actors: HashMap<&str, ()> = HashMap::new();

    for ((from, to), &count) in edges {
        *out_degree.entry(from.as_str()).or_insert(0) += count;
        *in_degree.entry(to.as_str()).or_insert(0) += count;
        actors.insert(from.as_str(), ());
        actors.insert(to.as_str(), ());
    }

    let unique_actors = actors.len();

    // Use total degree (in + out) consistently for both mean and variance.
    let degrees: Vec<f64> = actors
        .keys()
        .map(|a| {
            let out = out_degree.get(a).copied().unwrap_or(0);
            let in_d = in_degree.get(a).copied().unwrap_or(0);
            (out + in_d) as f64
        })
        .collect();
    let avg_degree = if unique_actors > 0 {
        degrees.iter().sum::<f64>() / unique_actors as f64
    } else {
        0.0
    };
    let degree_variance = degrees
        .iter()
        .map(|d| (d - avg_degree).powi(2))
        .sum::<f64>()
        / degrees.len().max(1) as f64;

    // Reciprocity: fraction of unique directed edges that have a reverse edge.
    // This is unweighted/topological — edge multiplicity is ignored intentionally
    // since we're classifying graph *structure*, not interaction *volume*.
    let mut reciprocal_count = 0u64;
    let mut total_directed = 0u64;
    for (from, to) in edges.keys() {
        total_directed += 1;
        if edges.contains_key(&(to.clone(), from.clone())) {
            reciprocal_count += 1;
        }
    }
    let reciprocity = if total_directed > 0 {
        reciprocal_count as f64 / total_directed as f64
    } else {
        0.0
    };

    // Clustering coefficient (simplified: fraction of connected triplets that are closed)
    let clustering_coefficient = estimate_clustering(&out_degree, edges, unique_actors);

    // Community estimate (simple: based on connected components heuristic)
    let community_estimate = estimate_communities(unique_actors, avg_degree, &degree_variance);

    // Hierarchy depth estimate
    let estimated_depth = estimate_depth(unique_actors, reciprocity, &degree_variance);

    GraphMetrics {
        unique_actors,
        avg_degree,
        reciprocity,
        clustering_coefficient,
        degree_variance,
        community_estimate,
        estimated_depth,
    }
}

/// Estimate clustering coefficient by sampling triads.
fn estimate_clustering(
    out_degree: &HashMap<&str, u64>,
    edges: &HashMap<(String, String), u64>,
    _unique_actors: usize,
) -> f64 {
    // For each actor with degree >= 2, check if their neighbors connect
    let mut closed_triplets = 0u64;
    let mut total_triplets = 0u64;

    // Build adjacency for sampling (undirected view)
    let mut neighbors: HashMap<&str, Vec<&str>> = HashMap::new();
    for (from, to) in edges.keys() {
        neighbors
            .entry(from.as_str())
            .or_default()
            .push(to.as_str());
    }

    // Sample up to 100 actors deterministically (sorted by name)
    let mut sorted_actors: Vec<&&str> = out_degree.keys().collect();
    sorted_actors.sort();
    let sample_actors: Vec<&&str> = sorted_actors.into_iter().take(100).collect();
    for &actor in &sample_actors {
        let nbrs = match neighbors.get(actor) {
            Some(n) if n.len() >= 2 => n,
            _ => continue,
        };

        // Check pairs of neighbors (cap at 20 pairs per actor)
        let check_count = nbrs.len().min(20);
        for i in 0..check_count {
            for j in (i + 1)..check_count {
                total_triplets += 1;
                let key = (nbrs[i].to_string(), nbrs[j].to_string());
                let key_rev = (nbrs[j].to_string(), nbrs[i].to_string());
                if edges.contains_key(&key) || edges.contains_key(&key_rev) {
                    closed_triplets += 1;
                }
            }
        }
    }

    if total_triplets > 0 {
        closed_triplets as f64 / total_triplets as f64
    } else {
        0.0
    }
}

/// Estimate number of communities from graph properties.
fn estimate_communities(unique_actors: usize, avg_degree: f64, degree_variance: &f64) -> usize {
    if unique_actors < 6 {
        return 1;
    }
    // Heuristic: high variance + moderate degree suggests multiple communities
    let cv = if avg_degree > 0.0 {
        degree_variance.sqrt() / avg_degree
    } else {
        0.0
    };

    if cv > 1.5 {
        // High variance — likely scale-free with communities
        (unique_actors as f64 / avg_degree).ceil().min(10.0) as usize
    } else if cv > 0.5 {
        // Moderate variance — some community structure
        ((unique_actors as f64).sqrt() / 2.0).ceil().min(5.0) as usize
    } else {
        1
    }
}

/// Estimate hierarchy depth from graph properties.
fn estimate_depth(unique_actors: usize, reciprocity: f64, degree_variance: &f64) -> u32 {
    if reciprocity > 0.5 || unique_actors < 4 {
        return 1;
    }
    // Low reciprocity + high variance suggests hierarchy
    let log_n = (unique_actors as f64).ln();
    let base_depth = (log_n / 2.0).ceil() as u32;
    let variance_factor = if *degree_variance > 10.0 { 1 } else { 0 };
    (base_depth + variance_factor).min(8)
}

/// Classify graph type based on structural metrics.
fn classify_graph_type(metrics: &GraphMetrics) -> GraphType {
    let cv = if metrics.avg_degree > 0.0 {
        metrics.degree_variance.sqrt() / metrics.avg_degree
    } else {
        0.0
    };

    if metrics.reciprocity < 0.2 && cv > 1.0 {
        // Low reciprocity + high degree variance → hierarchical
        GraphType::Hierarchical
    } else if cv > 1.5 {
        // Very high degree variance → scale-free (power law)
        GraphType::ScaleFree
    } else if metrics.clustering_coefficient > 0.3 && metrics.reciprocity > 0.3 {
        // High clustering + reciprocity → small-world
        GraphType::SmallWorld
    } else if cv < 0.5 && metrics.reciprocity > 0.4 {
        // Low variance + high reciprocity → random/uniform
        GraphType::ErdosRenyi
    } else {
        // Ambiguous — insufficient evidence for specific classification
        GraphType::Custom
    }
}

/// Extract string values from an Arrow array.
fn extract_strings(array: &dyn Array) -> Option<StringArray> {
    match array.data_type() {
        DataType::Utf8 => {
            let arr = array.as_any().downcast_ref::<StringArray>()?;
            Some(arr.clone())
        }
        DataType::LargeUtf8 => {
            let arr = array
                .as_any()
                .downcast_ref::<arrow::array::LargeStringArray>()?;
            let values: Vec<Option<&str>> = (0..arr.len())
                .map(|i| {
                    if arr.is_null(i) {
                        None
                    } else {
                        Some(arr.value(i))
                    }
                })
                .collect();
            Some(values.into_iter().collect())
        }
        DataType::Int32 => {
            let arr = array.as_any().downcast_ref::<arrow::array::Int32Array>()?;
            let strings: Vec<Option<String>> = (0..arr.len())
                .map(|i| {
                    if arr.is_null(i) {
                        None
                    } else {
                        Some(arr.value(i).to_string())
                    }
                })
                .collect();
            Some(strings.iter().map(|s| s.as_deref()).collect())
        }
        DataType::Int64 => {
            let arr = array.as_any().downcast_ref::<arrow::array::Int64Array>()?;
            let strings: Vec<Option<String>> = (0..arr.len())
                .map(|i| {
                    if arr.is_null(i) {
                        None
                    } else {
                        Some(arr.value(i).to_string())
                    }
                })
                .collect();
            Some(strings.iter().map(|s| s.as_deref()).collect())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int64Array, StringArray};
    use arrow::datatypes::{Field, Schema};
    use arrow::record_batch::RecordBatch;
    use std::sync::Arc;

    fn make_edge_batch(from: &[&str], to: &[&str]) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("sender_id", DataType::Utf8, false),
            Field::new("receiver_id", DataType::Utf8, false),
        ]));
        let from_arr = Arc::new(StringArray::from(from.to_vec()));
        let to_arr = Arc::new(StringArray::from(to.to_vec()));
        RecordBatch::try_new(schema, vec![from_arr, to_arr]).unwrap()
    }

    #[test]
    fn accumulator_counts_edges() {
        let batch = make_edge_batch(
            &["alice", "alice", "bob", "charlie"],
            &["bob", "charlie", "alice", "bob"],
        );
        let mut acc =
            RelationshipAccumulator::new("sender_id".into(), "receiver_id".into(), "emails".into());
        acc.observe_batch(&batch);
        assert_eq!(acc.total_observations(), 4);
        assert_eq!(acc.edge_count(), 4); // 4 unique directed edges
    }

    #[test]
    fn accumulator_skips_self_edges() {
        let batch = make_edge_batch(&["alice", "bob", "alice"], &["alice", "charlie", "bob"]);
        let mut acc =
            RelationshipAccumulator::new("sender_id".into(), "receiver_id".into(), "emails".into());
        acc.observe_batch(&batch);
        // alice→alice is a self-edge and should be skipped
        assert_eq!(acc.total_observations(), 2);
    }

    #[test]
    fn accumulator_missing_column_skips_batch() {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "other",
            DataType::Utf8,
            false,
        )]));
        let arr = Arc::new(StringArray::from(vec!["x"]));
        let batch = RecordBatch::try_new(schema, vec![arr]).unwrap();

        let mut acc =
            RelationshipAccumulator::new("sender_id".into(), "receiver_id".into(), "emails".into());
        acc.observe_batch(&batch);
        assert_eq!(acc.total_observations(), 0);
    }

    #[test]
    fn finalize_insufficient_edges_returns_none() {
        let batch = make_edge_batch(&["alice"], &["bob"]);
        let mut acc =
            RelationshipAccumulator::new("sender_id".into(), "receiver_id".into(), "emails".into());
        acc.observe_batch(&batch);

        let config = RelationshipDiscoveryConfig {
            min_edges: 10,
            min_actors: 3,
        };
        assert!(acc.finalize(&config).is_none());
    }

    #[test]
    fn finalize_produces_relationship_spec() {
        // Create enough edges for analysis
        let mut acc =
            RelationshipAccumulator::new("sender_id".into(), "receiver_id".into(), "emails".into());

        // 5 actors, many interactions
        let senders = vec![
            "alice", "alice", "bob", "bob", "charlie", "charlie", "dave", "dave", "eve", "eve",
            "alice", "bob",
        ];
        let receivers = vec![
            "bob", "charlie", "alice", "charlie", "alice", "dave", "eve", "alice", "dave",
            "charlie", "dave", "eve",
        ];
        let batch = make_edge_batch(&senders, &receivers);
        acc.observe_batch(&batch);

        let config = RelationshipDiscoveryConfig {
            min_edges: 5,
            min_actors: 3,
        };
        let spec = acc.finalize(&config).unwrap();

        assert_eq!(spec.name, "emails_sender_id_receiver_id_network");
        assert_eq!(spec.from_entity, "emails");
        assert!(spec.params.contains_key("avg_degree"));
        assert!(spec.params.contains_key("reciprocity"));
        assert!(*spec.params.get("avg_degree").unwrap() > 0.0);
    }

    #[test]
    fn classify_hierarchical_graph() {
        // Low reciprocity + high variance → hierarchical
        let metrics = GraphMetrics {
            unique_actors: 20,
            avg_degree: 3.0,
            reciprocity: 0.1,
            clustering_coefficient: 0.1,
            degree_variance: 25.0, // CV = 5/3 ≈ 1.67
            community_estimate: 3,
            estimated_depth: 4,
        };
        assert_eq!(classify_graph_type(&metrics), GraphType::Hierarchical);
    }

    #[test]
    fn classify_scale_free_graph() {
        // Very high variance → scale-free
        let metrics = GraphMetrics {
            unique_actors: 100,
            avg_degree: 5.0,
            reciprocity: 0.4,
            clustering_coefficient: 0.2,
            degree_variance: 100.0, // CV = 10/5 = 2.0
            community_estimate: 5,
            estimated_depth: 3,
        };
        assert_eq!(classify_graph_type(&metrics), GraphType::ScaleFree);
    }

    #[test]
    fn classify_small_world_graph() {
        // High clustering + reciprocity → small-world
        let metrics = GraphMetrics {
            unique_actors: 50,
            avg_degree: 4.0,
            reciprocity: 0.6,
            clustering_coefficient: 0.5,
            degree_variance: 4.0, // CV = 2/4 = 0.5
            community_estimate: 2,
            estimated_depth: 2,
        };
        assert_eq!(classify_graph_type(&metrics), GraphType::SmallWorld);
    }

    #[test]
    fn classify_erdos_renyi_graph() {
        // Low variance + high reciprocity → random
        let metrics = GraphMetrics {
            unique_actors: 30,
            avg_degree: 5.0,
            reciprocity: 0.5,
            clustering_coefficient: 0.1,
            degree_variance: 4.0, // CV = 2/5 = 0.4
            community_estimate: 1,
            estimated_depth: 1,
        };
        assert_eq!(classify_graph_type(&metrics), GraphType::ErdosRenyi);
    }

    #[test]
    fn integer_actor_ids() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("sender_id", DataType::Int64, false),
            Field::new("receiver_id", DataType::Int64, false),
        ]));
        let from_arr = Arc::new(Int64Array::from(vec![1, 1, 2, 3]));
        let to_arr = Arc::new(Int64Array::from(vec![2, 3, 3, 1]));
        let batch = RecordBatch::try_new(schema, vec![from_arr, to_arr]).unwrap();

        let mut acc = RelationshipAccumulator::new(
            "sender_id".into(),
            "receiver_id".into(),
            "messages".into(),
        );
        acc.observe_batch(&batch);
        assert_eq!(acc.total_observations(), 4);
        assert_eq!(acc.edge_count(), 4);
    }

    #[test]
    fn reciprocity_calculation() {
        // All edges are reciprocated
        let batch = make_edge_batch(
            &["alice", "bob", "alice", "bob"],
            &["bob", "alice", "bob", "alice"],
        );
        let mut acc =
            RelationshipAccumulator::new("sender_id".into(), "receiver_id".into(), "chat".into());
        acc.observe_batch(&batch);

        let metrics = compute_graph_metrics(&acc.edges);
        assert!(
            (metrics.reciprocity - 1.0).abs() < 1e-10,
            "fully reciprocal graph should have reciprocity 1.0, got {}",
            metrics.reciprocity
        );
    }

    #[test]
    fn no_reciprocity_calculation() {
        // No edges are reciprocated (one-directional broadcast)
        let batch = make_edge_batch(
            &["boss", "boss", "boss", "boss"],
            &["emp1", "emp2", "emp3", "emp4"],
        );
        let mut acc = RelationshipAccumulator::new(
            "sender_id".into(),
            "receiver_id".into(),
            "directives".into(),
        );
        acc.observe_batch(&batch);

        let metrics = compute_graph_metrics(&acc.edges);
        assert_eq!(
            metrics.reciprocity, 0.0,
            "one-directional graph should have reciprocity 0.0"
        );
    }
}
