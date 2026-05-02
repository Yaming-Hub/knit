//! Dependency graph construction and phase assignment.
//!
//! Builds a directed graph from the model's relationships, detects strongly
//! connected components (cycles) via Tarjan's algorithm, and assigns entities
//! to generation phases via topological sorting of the condensation DAG.
//!
//! Cyclic relationships (self-referential or mutual) produce [`DeferredRef`]
//! entries that `knit-gen` backpatches after the initial generation phase.

use std::collections::{BTreeMap, HashMap};

use petgraph::algo::tarjan_scc;
use petgraph::graph::DiGraph;
use petgraph::visit::EdgeRef;

use knit_core::DataModel;

use crate::error::PlanError;
use crate::types::{DeferralStrategy, DeferredRef};

/// Result of dependency graph analysis: entities grouped into ordered phases,
/// plus any deferred references needed for cycle-breaking.
#[derive(Debug, Clone)]
pub struct PhaseAssignment {
    /// Entities in topological phase order. Each inner `Vec` is one phase;
    /// entities within a phase have no inter-dependencies and can run in parallel.
    pub phases: Vec<Vec<String>>,
    /// Deferred references (cycle-breaking edges) that must be backpatched
    /// after their phase completes.
    pub deferred_refs: Vec<DeferredRef>,
}

/// Build dependency graph from the model's relationships and assign phases.
///
/// An edge from A→B means "A depends on B" (A has a FK pointing to B),
/// so B must be generated before A.
pub fn assign_phases(model: &DataModel) -> Result<PhaseAssignment, PlanError> {
    let entity_names: Vec<&str> = model.entities.iter().map(|e| e.name.as_str()).collect();
    let entity_index: HashMap<&str, usize> =
        entity_names.iter().enumerate().map(|(i, n)| (*n, i)).collect();

    let mut graph = DiGraph::<&str, ()>::new();
    let nodes: Vec<_> = entity_names.iter().map(|n| graph.add_node(*n)).collect();

    // Build edges: "from" entity has FK pointing to "to" entity,
    // so "from" depends on "to" → edge from_node → to_node.
    for rel in &model.relationships {
        let from_idx = entity_index.get(rel.from.as_str()).ok_or_else(|| {
            PlanError::UnknownEntity {
                name: rel.from.clone(),
            }
        })?;
        let to_idx = entity_index.get(rel.to.as_str()).ok_or_else(|| {
            PlanError::UnknownEntity {
                name: rel.to.clone(),
            }
        })?;
        graph.add_edge(nodes[*from_idx], nodes[*to_idx], ());
    }

    // Find SCCs using Tarjan's algorithm.
    let sccs = tarjan_scc(&graph);

    // Build condensation: map each node to its SCC index.
    let mut node_to_scc: HashMap<usize, usize> = HashMap::new();
    // Tarjan returns SCCs in reverse topological order, so reverse them.
    let sccs_topo: Vec<Vec<usize>> = sccs
        .into_iter()
        .rev()
        .enumerate()
        .map(|(scc_idx, scc)| {
            let indices: Vec<usize> = scc.into_iter().map(|n| n.index()).collect();
            for &idx in &indices {
                node_to_scc.insert(idx, scc_idx);
            }
            indices
        })
        .collect();

    // Collect deferred refs for edges within SCCs (cycles).
    let mut deferred_refs = Vec::new();
    for rel in &model.relationships {
        let from_idx = entity_index[rel.from.as_str()];
        let to_idx = entity_index[rel.to.as_str()];
        let from_scc = node_to_scc[&from_idx];
        let to_scc = node_to_scc[&to_idx];

        if from_scc == to_scc && sccs_topo[from_scc].len() > 1 {
            // Multi-entity cycle → deferred ref.
            let fk_field = rel
                .foreign_key
                .clone()
                .unwrap_or_else(|| format!("{}_id", rel.to));
            deferred_refs.push(DeferredRef {
                from_entity: rel.from.clone(),
                from_field: fk_field,
                to_entity: rel.to.clone(),
                to_field: "id".to_string(),
                strategy: DeferralStrategy::UniformSample,
            });
        } else if from_scc == to_scc && from_idx == to_idx {
            // Self-referential.
            let fk_field = rel
                .foreign_key
                .clone()
                .unwrap_or_else(|| format!("{}_id", rel.to));
            deferred_refs.push(DeferredRef {
                from_entity: rel.from.clone(),
                from_field: fk_field,
                to_entity: rel.to.clone(),
                to_field: "id".to_string(),
                strategy: DeferralStrategy::SelfReference {
                    nullable_root_probability: 0.1,
                },
            });
        }
    }

    // Build condensation DAG and topologically sort it.
    // Since Tarjan already gives reverse topo order, after our reversal
    // sccs_topo is in topological order of the condensation.
    // But we need to account for cross-SCC edges to assign correct phases.
    let num_sccs = sccs_topo.len();
    let mut scc_graph = DiGraph::<usize, ()>::new();
    let scc_nodes: Vec<_> = (0..num_sccs).map(|i| scc_graph.add_node(i)).collect();

    for edge in graph.edge_references() {
        let from_scc = node_to_scc[&edge.source().index()];
        let to_scc = node_to_scc[&edge.target().index()];
        if from_scc != to_scc {
            scc_graph.add_edge(scc_nodes[from_scc], scc_nodes[to_scc], ());
        }
    }

    // Compute phase assignment: phase = longest path from any source in condensation DAG.
    // Entities that depend on nothing go to phase 0.
    // An entity goes to phase max(phases of dependencies) + 1.
    let mut scc_phase = vec![0usize; num_sccs];
    // Process in topological order (dependencies first).
    // Since sccs_topo is already in topo order of the condensation,
    // we process them and update dependents.
    // Actually we need reverse: targets before sources.
    // "from" depends on "to", edge from→to in graph.
    // In condensation, edge from_scc→to_scc means from_scc depends on to_scc.
    // For topo sort, we want to process to_scc before from_scc.
    // Tarjan returns in reverse topo order; we reversed to get topo order.
    // Topo order of condensation: a node comes before its successors.
    // But our edges point from dependent to dependency (from→to).
    // So topo order should process "to" (dependency) before "from" (dependent).
    // That means we want reverse topological order of our edge direction,
    // or equivalently topological order of the reversed graph.
    // Let's just compute phases using the edges directly:
    // phase[scc] = max(phase[dep_scc] + 1) for all dep_scc that scc depends on.
    // We need to process dependencies before dependents.

    // Build in-edges for each SCC (what does each SCC depend on?).
    let mut depends_on: Vec<Vec<usize>> = vec![Vec::new(); num_sccs];
    for edge in scc_graph.edge_references() {
        let from_scc = edge.source().index();
        let to_scc = edge.target().index();
        // from_scc depends on to_scc
        depends_on[from_scc].push(to_scc);
    }

    // Simple BFS/iterative approach: keep computing until stable.
    // With a DAG this converges in at most `num_sccs` iterations.
    let mut changed = true;
    while changed {
        changed = false;
        for scc_idx in 0..num_sccs {
            for &dep in &depends_on[scc_idx] {
                let new_phase = scc_phase[dep] + 1;
                if new_phase > scc_phase[scc_idx] {
                    scc_phase[scc_idx] = new_phase;
                    changed = true;
                }
            }
        }
    }

    // Group entities by phase.
    let max_phase = scc_phase.iter().copied().max().unwrap_or(0);
    let mut phases: Vec<Vec<String>> = vec![Vec::new(); max_phase + 1];
    for (scc_idx, entity_indices) in sccs_topo.iter().enumerate() {
        let phase = scc_phase[scc_idx];
        for &eidx in entity_indices {
            phases[phase].push(entity_names[eidx].to_string());
        }
    }

    // Sort entity names within each phase for determinism.
    for phase in &mut phases {
        phase.sort();
    }

    Ok(PhaseAssignment {
        phases,
        deferred_refs,
    })
}

/// Resolve each entity's [`CountSpec`](knit_core::CountSpec) to a concrete row count.
///
/// Returns a sorted map from entity name to resolved row count. Used by the
/// compiler to size partitions and estimate output bytes.
pub fn resolve_row_counts(model: &DataModel) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for entity in &model.entities {
        let row_count = crate::partition::resolve_count(&entity.count);
        counts.insert(entity.name.clone(), row_count);
    }
    counts
}
