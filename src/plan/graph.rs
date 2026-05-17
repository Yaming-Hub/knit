//! Dependency graph construction and phase assignment.
//!
//! Builds a directed graph from the model's relationships, detects strongly
//! connected components (cycles) via Tarjan's algorithm, and assigns entities
//! to generation phases via topological sorting of the condensation DAG.
//!
//! Cyclic relationships (self-referential or mutual) produce [`DeferredRef`]
//! entries that [`gen`](crate::gen) backpatches after the initial generation
//! phase.

use std::collections::{BTreeMap, HashMap};

use petgraph::algo::tarjan_scc;
use petgraph::graph::DiGraph;
use petgraph::visit::EdgeRef;

use crate::core::DataModel;

use crate::plan::error::PlanError;
use crate::plan::types::{DeferralStrategy, DeferredRef};

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
    let entity_index: HashMap<&str, usize> = entity_names
        .iter()
        .enumerate()
        .map(|(i, n)| (*n, i))
        .collect();

    let mut graph = DiGraph::<&str, ()>::new();
    let nodes: Vec<_> = entity_names.iter().map(|n| graph.add_node(*n)).collect();

    // Build edges: "from" entity has FK pointing to "to" entity,
    // so "from" depends on "to" → edge from_node → to_node.
    for rel in &model.relationships {
        let from_idx =
            entity_index
                .get(rel.from.as_str())
                .ok_or_else(|| PlanError::UnknownEntity {
                    name: rel.from.clone(),
                })?;
        let to_idx = entity_index
            .get(rel.to.as_str())
            .ok_or_else(|| PlanError::UnknownEntity {
                name: rel.to.clone(),
            })?;
        graph.add_edge(nodes[*from_idx], nodes[*to_idx], ());
    }

    // Build edges from ActorRef generators: if entity A has a field with
    // generator = { type = "actor_ref", entity = "B" }, then A depends on B.
    for entity in &model.entities {
        let from_idx = entity_index[entity.name.as_str()];
        for field in &entity.fields {
            if let Some(crate::core::GeneratorSpec::ActorRef { entity: ref target }) =
                field.generator
                && let Some(&to_idx) = entity_index.get(target.as_str())
                && from_idx != to_idx
            {
                graph.add_edge(nodes[from_idx], nodes[to_idx], ());
            }
        }
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
                    nullable_root_probability: rel.root_probability.unwrap_or(0.1),
                    acyclic: rel.acyclic.unwrap_or(false),
                    max_depth: rel.max_depth,
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

/// Resolve each entity's [`CountSpec`](crate::core::CountSpec) to a concrete row count.
///
/// Returns a sorted map from entity name to resolved row count. Used by the
/// compiler to size partitions and estimate output bytes.
///
/// Returns an error if a count expression fails to evaluate (e.g. missing
/// parameter or invalid arithmetic).
pub fn resolve_row_counts(model: &DataModel) -> Result<BTreeMap<String, u64>, String> {
    let mut counts = BTreeMap::new();
    for entity in &model.entities {
        let row_count = crate::plan::partition::resolve_count(&entity.count, &model.params)
            .map_err(|e| format!("entity '{}': {}", entity.name, e))?;
        counts.insert(entity.name.clone(), row_count);
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{
        CountSpec, DistributionKind, DistributionSpec, Entity, IntOrString, Relationship,
        RelationshipKind,
    };
    use std::collections::BTreeMap;

    /// Helper to build a minimal entity with a given name and count.
    fn entity(name: &str, count: u64) -> Entity {
        Entity {
            name: name.to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(count),
            fields: vec![],
            stats: None,
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
            output: None,
            scaling: None,
            sort_by: None,
        }
    }

    /// Helper to build a relationship.
    fn rel(name: &str, from: &str, to: &str) -> Relationship {
        Relationship {
            name: name.to_string(),
            from: from.to_string(),
            to: to.to_string(),
            kind: RelationshipKind::OneToMany,
            foreign_key: None,
            cardinality: None,
            degree: None,

            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![],
        }
    }

    fn model_with(entities: Vec<Entity>, relationships: Vec<Relationship>) -> DataModel {
        DataModel {
            name: "test".to_string(),
            description: None,
            seed: 42,
            locale: "en_US".to_string(),
            timezone: "UTC".to_string(),
            entities,
            relationships,
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            blueprint_version: "1.0".to_string(),
            personas: Vec::new(),
            actor_relationships: Vec::new(),
            custom_types: Vec::new(),
            mixins: Vec::new(),
            companion_files: Vec::new(),
        }
    }

    // ── assign_phases tests ──────────────────────────────────────────

    #[test]
    fn single_entity_no_deps() {
        let model = model_with(vec![entity("users", 100)], vec![]);
        let result = assign_phases(&model).unwrap();
        assert_eq!(result.phases.len(), 1);
        assert_eq!(result.phases[0], vec!["users"]);
        assert!(result.deferred_refs.is_empty());
    }

    #[test]
    fn two_independent_entities_same_phase() {
        let model = model_with(vec![entity("users", 100), entity("products", 50)], vec![]);
        let result = assign_phases(&model).unwrap();
        assert_eq!(result.phases.len(), 1);
        // Both in phase 0, sorted alphabetically
        assert!(result.phases[0].contains(&"users".to_string()));
        assert!(result.phases[0].contains(&"products".to_string()));
    }

    #[test]
    fn linear_dependency_chain() {
        // orders → users (orders depend on users)
        let model = model_with(
            vec![entity("users", 100), entity("orders", 500)],
            vec![rel("orders_users", "orders", "users")],
        );
        let result = assign_phases(&model).unwrap();
        assert_eq!(result.phases.len(), 2);
        assert_eq!(result.phases[0], vec!["users"]);
        assert_eq!(result.phases[1], vec!["orders"]);
        assert!(result.deferred_refs.is_empty());
    }

    #[test]
    fn three_level_chain() {
        // line_items → orders → users
        let model = model_with(
            vec![
                entity("users", 100),
                entity("orders", 500),
                entity("line_items", 2000),
            ],
            vec![
                rel("orders_users", "orders", "users"),
                rel("items_orders", "line_items", "orders"),
            ],
        );
        let result = assign_phases(&model).unwrap();
        assert_eq!(result.phases.len(), 3);
        assert_eq!(result.phases[0], vec!["users"]);
        assert_eq!(result.phases[1], vec!["orders"]);
        assert_eq!(result.phases[2], vec!["line_items"]);
    }

    #[test]
    fn diamond_dependency() {
        // D depends on B and C; B and C both depend on A.
        // A(phase 0) → B,C(phase 1) → D(phase 2)
        let model = model_with(
            vec![
                entity("A", 10),
                entity("B", 20),
                entity("C", 30),
                entity("D", 40),
            ],
            vec![
                rel("B_A", "B", "A"),
                rel("C_A", "C", "A"),
                rel("D_B", "D", "B"),
                rel("D_C", "D", "C"),
            ],
        );
        let result = assign_phases(&model).unwrap();
        assert_eq!(result.phases.len(), 3);
        assert_eq!(result.phases[0], vec!["A"]);
        let mut phase1 = result.phases[1].clone();
        phase1.sort();
        assert_eq!(phase1, vec!["B", "C"]);
        assert_eq!(result.phases[2], vec!["D"]);
    }

    #[test]
    fn self_referential_entity() {
        // employees references itself (manager_id → employees.id)
        let model = model_with(
            vec![entity("employees", 100)],
            vec![rel("self_ref", "employees", "employees")],
        );
        let result = assign_phases(&model).unwrap();
        // Still one phase (self-ref is deferred)
        assert_eq!(result.phases.len(), 1);
        assert_eq!(result.phases[0], vec!["employees"]);
        // Should have a deferred ref with SelfReference strategy
        assert_eq!(result.deferred_refs.len(), 1);
        let dr = &result.deferred_refs[0];
        assert_eq!(dr.from_entity, "employees");
        assert_eq!(dr.to_entity, "employees");
        assert_eq!(dr.from_field, "employees_id"); // default FK naming
        assert!(matches!(
            dr.strategy,
            DeferralStrategy::SelfReference { .. }
        ));
    }

    #[test]
    fn mutual_cycle_produces_deferred_refs() {
        // A → B and B → A (mutual dependency)
        let model = model_with(
            vec![entity("A", 10), entity("B", 20)],
            vec![rel("A_B", "A", "B"), rel("B_A", "B", "A")],
        );
        let result = assign_phases(&model).unwrap();
        // Both in same phase (SCC)
        assert_eq!(result.phases.len(), 1);
        assert_eq!(result.deferred_refs.len(), 2);
        // Both should be UniformSample strategy
        for dr in &result.deferred_refs {
            assert!(matches!(dr.strategy, DeferralStrategy::UniformSample));
        }
    }

    #[test]
    fn cycle_with_external_dep() {
        // A and B are in a cycle; C depends on A.
        // The SCC {A,B} goes to phase 0, C to phase 1.
        let model = model_with(
            vec![entity("A", 10), entity("B", 20), entity("C", 30)],
            vec![
                rel("A_B", "A", "B"),
                rel("B_A", "B", "A"),
                rel("C_A", "C", "A"),
            ],
        );
        let result = assign_phases(&model).unwrap();
        assert_eq!(result.phases.len(), 2);
        // Phase 0 has A and B (the cycle)
        let mut phase0 = result.phases[0].clone();
        phase0.sort();
        assert_eq!(phase0, vec!["A", "B"]);
        // Phase 1 has C
        assert_eq!(result.phases[1], vec!["C"]);
        // 2 deferred refs for the cycle
        assert_eq!(result.deferred_refs.len(), 2);
    }

    #[test]
    fn explicit_foreign_key_name_in_deferred_ref() {
        // Self-referential with explicit FK name — should appear in deferred ref
        let model = model_with(
            vec![entity("employees", 100)],
            vec![Relationship {
                name: "manager".to_string(),
                from: "employees".to_string(),
                to: "employees".to_string(),
                kind: RelationshipKind::OneToMany,
                foreign_key: Some("manager_id".to_string()),
                cardinality: None,
                degree: None,

                selection: None,
                nullable: None,
                acyclic: None,
                root_probability: None,
                max_depth: None,
                properties: vec![],
            }],
        );
        let result = assign_phases(&model).unwrap();
        assert_eq!(result.deferred_refs.len(), 1);
        assert_eq!(result.deferred_refs[0].from_field, "manager_id");
    }

    #[test]
    fn unknown_entity_in_relationship_errors() {
        let model = model_with(
            vec![entity("users", 100)],
            vec![rel("bad_rel", "orders", "users")],
        );
        let result = assign_phases(&model);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(matches!(err, PlanError::UnknownEntity { ref name } if name == "orders"));
    }

    #[test]
    fn unknown_target_entity_errors() {
        let model = model_with(
            vec![entity("orders", 500)],
            vec![rel("bad_rel", "orders", "users")],
        );
        let result = assign_phases(&model);
        assert!(result.is_err());
    }

    // ── resolve_row_counts tests ─────────────────────────────────────

    #[test]
    fn resolve_counts_fixed() {
        let model = model_with(vec![entity("a", 100), entity("b", 200)], vec![]);
        let counts = resolve_row_counts(&model).unwrap();
        assert_eq!(counts["a"], 100);
        assert_eq!(counts["b"], 200);
    }

    #[test]
    fn resolve_counts_range() {
        let model = model_with(
            vec![Entity {
                name: "x".to_string(),
                description: None,
                tags: Vec::new(),
                count: CountSpec::Range { min: 50, max: 150 },
                fields: vec![],
                stats: None,
                constraints: vec![],
                topology: None,
                actor: false,
                persona_distribution: None,
                activity_count: None,
                mixin_refs: None,
                output: None,
                scaling: None,
                sort_by: None,
            }],
            vec![],
        );
        let counts = resolve_row_counts(&model).unwrap();
        // Range resolves to max
        assert_eq!(counts["x"], 150);
    }

    #[test]
    fn resolve_counts_distribution() {
        let mut params = BTreeMap::new();
        params.insert("mean".to_string(), 500.0);
        params.insert("std_dev".to_string(), 50.0);
        let model = model_with(
            vec![Entity {
                name: "d".to_string(),
                description: None,
                tags: Vec::new(),
                count: CountSpec::Distribution(DistributionSpec {
                    kind: DistributionKind::Normal,
                    params,
                    array_params: BTreeMap::new(),
                    round: false,
                }),
                fields: vec![],
                stats: None,
                constraints: vec![],
                topology: None,
                actor: false,
                persona_distribution: None,
                activity_count: None,
                mixin_refs: None,
                output: None,
                scaling: None,
                sort_by: None,
            }],
            vec![],
        );
        let counts = resolve_row_counts(&model).unwrap();
        // Normal distribution resolves to mean
        assert_eq!(counts["d"], 500);
    }

    #[test]
    fn actor_ref_creates_dependency_edge() {
        use crate::core::{DataType, Field, GeneratorSpec, NullSpec};
        // "events" has actor_ref pointing to "users" → events depends on users
        let users = Entity {
            name: "users".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(100),
            fields: vec![Field {
                name: "id".to_string(),
                description: None,
                data_type: DataType::Int,
                generator: Some(GeneratorSpec::Sequence {
                    start: IntOrString::Int(1),
                    step: IntOrString::Int(1),
                    prefix: None,
                    values: None,
                    cycle: None,
                    jitter: None,
                }),
                nullable: NullSpec::Never,
                primary_key: Some(true),
                precision: None,
                actor_column: false,
                fields: vec![],
                stats: None,
                traits: None,
            }],
            constraints: vec![],
            topology: None,
            actor: true,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
            output: None,
            stats: None,
            scaling: None,
            sort_by: None,
        };
        let events = Entity {
            name: "events".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(1000),
            fields: vec![Field {
                name: "user_id".to_string(),
                description: None,
                data_type: DataType::Int,
                generator: Some(GeneratorSpec::ActorRef {
                    entity: "users".to_string(),
                }),
                nullable: NullSpec::Never,
                primary_key: None,
                precision: None,
                actor_column: true,
                fields: vec![],
                stats: None,
                traits: None,
            }],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
            output: None,
            stats: None,
            scaling: None,
            sort_by: None,
        };
        // No explicit relationship — only ActorRef generator
        let model = model_with(vec![users, events], vec![]);
        let result = assign_phases(&model).unwrap();
        // Users must be in an earlier phase than events
        assert!(result.phases.len() >= 2);
        let users_phase = result
            .phases
            .iter()
            .position(|p| p.contains(&"users".to_string()))
            .unwrap();
        let events_phase = result
            .phases
            .iter()
            .position(|p| p.contains(&"events".to_string()))
            .unwrap();
        assert!(
            users_phase < events_phase,
            "users (phase {}) must come before events (phase {})",
            users_phase,
            events_phase
        );
    }

    #[test]
    fn actor_ref_self_referential_no_edge() {
        use crate::core::{DataType, Field, GeneratorSpec, NullSpec};
        // Self-referential actor_ref (same entity) should not create an edge
        let users = Entity {
            name: "users".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(100),
            fields: vec![Field {
                name: "manager_id".to_string(),
                description: None,
                data_type: DataType::Int,
                generator: Some(GeneratorSpec::ActorRef {
                    entity: "users".to_string(),
                }),
                nullable: NullSpec::Never,
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
                stats: None,
                traits: None,
            }],
            constraints: vec![],
            topology: None,
            actor: true,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
            output: None,
            stats: None,
            scaling: None,
            sort_by: None,
        };
        let model = model_with(vec![users], vec![]);
        let result = assign_phases(&model).unwrap();
        // Should still be single phase (no dependency)
        assert_eq!(result.phases.len(), 1);
        assert_eq!(result.phases[0], vec!["users"]);
    }
}
