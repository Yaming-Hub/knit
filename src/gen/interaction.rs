//! Interaction generation — persona-driven record production for behavioral entities.
//!
//! This module generates interaction records (e.g., events, transactions, messages)
//! between actors, driven by their persona traits and relationship graph. Each
//! interaction is attributed to a source actor (and optionally a target actor),
//! with persona traits influencing the generated field values.
//!
//! ## Lifecycle
//!
//! 1. The engine builds an [`ActorPool`] and generates relationship graphs.
//! 2. For behavioral entities, an [`InteractionGenerator`] is constructed from
//!    the pool, graphs, and entity configuration.
//! 3. Each call to [`InteractionGenerator::generate_batch`] produces a batch of
//!    interaction records with actor attribution and trait-influenced values.

use std::collections::{BTreeMap, HashMap};

use crate::core::Value;
use rand::rngs::ChaCha8Rng;
use rand::{Rng, RngExt, SeedableRng};

use crate::r#gen::actor_pool::ActorPool;
use crate::r#gen::graph::{Edge, GeneratedGraph};

/// Configuration for generating interactions for one behavioral entity.
#[derive(Debug, Clone)]
pub struct InteractionConfig {
    /// Name of the behavioral entity (e.g., "events", "transactions").
    pub entity_name: String,
    /// Name of the actor entity whose pool to sample from.
    pub actor_entity: String,
    /// Optional target actor entity for directed interactions.
    pub target_entity: Option<String>,
    /// Name of the field in the output that holds the source actor reference.
    pub actor_field: String,
    /// Optional field name for target actor reference.
    pub target_field: Option<String>,
    /// Trait-to-field mappings: actor trait name → output field name.
    /// These fields will be populated from the sampled actor's traits.
    pub trait_fields: HashMap<String, String>,
    /// Total number of interactions to generate.
    pub total_count: u64,
}

/// An interaction record produced by the generator.
#[derive(Debug, Clone)]
pub struct InteractionRecord {
    /// Source actor index.
    pub actor_index: usize,
    /// Target actor index (if directed interaction).
    pub target_index: Option<usize>,
    /// Persona of the source actor.
    pub persona: String,
    /// Trait values inherited from the source actor.
    pub trait_values: BTreeMap<String, Value>,
}

/// Generator that produces interaction records driven by actor pool and graph.
#[derive(Debug)]
pub struct InteractionGenerator {
    config: InteractionConfig,
    /// Precomputed edge list for directed interactions (source → target).
    edge_list: Vec<Edge>,
    /// Whether a graph was explicitly provided (even if empty).
    graph_provided: bool,
}

impl InteractionGenerator {
    /// Create a new interaction generator.
    ///
    /// If a matching graph is provided, interactions follow the graph edges
    /// (sampled uniformly). If the graph is empty, no interactions are generated.
    /// Without a graph, source actors are sampled from the pool weighted by
    /// activity rate and targets (if any) are sampled independently.
    pub fn new(config: InteractionConfig, graph: Option<&GeneratedGraph>) -> Self {
        let (edge_list, graph_provided) = match graph {
            Some(g) => {
                // Validate graph matches config entities
                debug_assert!(
                    g.from_entity == config.actor_entity,
                    "Graph from_entity '{}' doesn't match config actor_entity '{}'",
                    g.from_entity,
                    config.actor_entity
                );
                (g.edges.clone(), true)
            }
            None => (Vec::new(), false),
        };

        Self {
            config,
            edge_list,
            graph_provided,
        }
    }

    /// Generate a batch of interaction records.
    ///
    /// Uses the actor pool for weighted sampling and trait lookup.
    /// Returns up to `batch_size` records starting from `offset`.
    /// Returns empty if the pool has no actors or graph is empty.
    pub fn generate_batch(
        &self,
        pool: &ActorPool,
        rng: &mut impl Rng,
        batch_size: usize,
        offset: u64,
    ) -> Vec<InteractionRecord> {
        let count = batch_size.min((self.config.total_count.saturating_sub(offset)) as usize);

        // If graph was provided but is empty, no interactions can be generated
        if self.graph_provided && self.edge_list.is_empty() {
            return Vec::new();
        }

        // If pool has no actors for this entity, cannot generate
        if pool.actor_count(&self.config.actor_entity).unwrap_or(0) == 0 {
            return Vec::new();
        }

        let mut records = Vec::with_capacity(count);

        for i in 0..count {
            let record = if self.graph_provided {
                self.generate_graph_interaction(pool, rng, offset + i as u64)
            } else {
                self.generate_pool_interaction(pool, rng)
            };
            records.push(record);
        }

        records
    }

    /// Generate a single interaction following graph edges (uniform edge sampling).
    fn generate_graph_interaction(
        &self,
        pool: &ActorPool,
        rng: &mut impl Rng,
        _offset: u64,
    ) -> InteractionRecord {
        // Sample an edge uniformly from the graph
        let edge_idx = rng.random_range(0..self.edge_list.len());
        let edge = &self.edge_list[edge_idx];

        let actor_index = edge.from;
        let target_index = Some(edge.to);

        let persona = pool
            .get_persona(&self.config.actor_entity, actor_index)
            .unwrap_or("unknown")
            .to_string();

        let trait_values = self.collect_trait_values(pool, actor_index);

        InteractionRecord {
            actor_index,
            target_index,
            persona,
            trait_values,
        }
    }

    /// Generate a single interaction by sampling from the actor pool.
    fn generate_pool_interaction(&self, pool: &ActorPool, rng: &mut impl Rng) -> InteractionRecord {
        // Sample source actor weighted by activity (caller ensures pool is non-empty)
        let actor_index = pool
            .sample_actor(&self.config.actor_entity, rng)
            .expect("pool should be non-empty (checked in generate_batch)");

        // Sample target if configured
        let target_index = self
            .config
            .target_entity
            .as_ref()
            .and_then(|target_entity| pool.sample_actor(target_entity, rng));

        let persona = pool
            .get_persona(&self.config.actor_entity, actor_index)
            .unwrap_or("unknown")
            .to_string();

        let trait_values = self.collect_trait_values(pool, actor_index);

        InteractionRecord {
            actor_index,
            target_index,
            persona,
            trait_values,
        }
    }

    /// Collect trait values for the given actor, mapped to output field names.
    fn collect_trait_values(
        &self,
        pool: &ActorPool,
        actor_index: usize,
    ) -> BTreeMap<String, Value> {
        let mut values = BTreeMap::new();

        for (trait_name, field_name) in &self.config.trait_fields {
            if let Some(value) = pool.get_trait(&self.config.actor_entity, actor_index, trait_name)
            {
                values.insert(field_name.clone(), value.clone());
            }
        }

        values
    }

    /// Get the entity name this generator produces records for.
    pub fn entity_name(&self) -> &str {
        &self.config.entity_name
    }

    /// Get the total count of interactions to generate.
    pub fn total_count(&self) -> u64 {
        self.config.total_count
    }
}

/// Generate all interactions for a behavioral entity in batches.
///
/// This is the high-level entry point: given a config, pool, optional graph,
/// and seed, it produces all interaction records.
pub fn generate_interactions(
    config: &InteractionConfig,
    pool: &ActorPool,
    graph: Option<&GeneratedGraph>,
    seed: u64,
    batch_size: usize,
) -> Vec<InteractionRecord> {
    let generator = InteractionGenerator::new(config.clone(), graph);
    let mut rng = ChaCha8Rng::seed_from_u64(seed);
    let mut all_records = Vec::new();
    let mut offset = 0u64;

    while offset < config.total_count {
        let batch = generator.generate_batch(pool, &mut rng, batch_size, offset);
        if batch.is_empty() {
            break;
        }
        offset += batch.len() as u64;
        all_records.extend(batch);
    }

    all_records
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{ActorEntityPool, ActorPoolPlan, PersonaWeight};

    fn make_pool() -> ActorPool {
        let plan = ActorPoolPlan {
            pools: vec![ActorEntityPool {
                entity_name: "users".into(),
                actor_count: 50,
                persona_weights: vec![
                    PersonaWeight {
                        name: "power_user".into(),
                        weight: 0.3,
                        traits: BTreeMap::from([
                            ("activity_rate".into(), Value::Float(50.0)),
                            ("session_length".into(), Value::Float(120.0)),
                        ]),
                    },
                    PersonaWeight {
                        name: "casual_user".into(),
                        weight: 0.7,
                        traits: BTreeMap::from([
                            ("activity_rate".into(), Value::Float(5.0)),
                            ("session_length".into(), Value::Float(15.0)),
                        ]),
                    },
                ],
            }],
            graph_plans: Vec::new(),
        };
        ActorPool::from_plan(&plan, 42)
    }

    fn make_config() -> InteractionConfig {
        InteractionConfig {
            entity_name: "events".into(),
            actor_entity: "users".into(),
            target_entity: None,
            actor_field: "user_id".into(),
            target_field: None,
            trait_fields: HashMap::from([("session_length".into(), "avg_session".into())]),
            total_count: 100,
        }
    }

    #[test]
    fn basic_interaction_generation() {
        let pool = make_pool();
        let config = make_config();

        let records = generate_interactions(&config, &pool, None, 42, 32);

        assert_eq!(records.len(), 100);
        for r in &records {
            assert!(r.actor_index < 50);
            assert!(r.persona == "power_user" || r.persona == "casual_user");
            assert!(r.target_index.is_none());
        }
    }

    #[test]
    fn power_users_overrepresented() {
        let pool = make_pool();
        let config = InteractionConfig {
            total_count: 1000,
            ..make_config()
        };

        let records = generate_interactions(&config, &pool, None, 42, 256);

        let power_count = records.iter().filter(|r| r.persona == "power_user").count();
        // Power users have 10x activity_rate, ~30% of actors but should produce
        // much more than 30% of interactions
        assert!(
            power_count > 500,
            "Expected power users to dominate, got {power_count}/1000"
        );
    }

    #[test]
    fn trait_values_populated() {
        let pool = make_pool();
        let config = make_config();

        let records = generate_interactions(&config, &pool, None, 42, 100);

        for r in &records {
            // trait_fields maps "session_length" → "avg_session"
            // so output should use the field name "avg_session"
            assert!(
                r.trait_values.contains_key("avg_session"),
                "Missing avg_session field for actor {}",
                r.actor_index
            );
        }
    }

    #[test]
    fn graph_based_interactions() {
        let pool = make_pool();
        let graph = GeneratedGraph {
            name: "follows".into(),
            from_entity: "users".into(),
            to_entity: "users".into(),
            edges: vec![
                Edge { from: 0, to: 1 },
                Edge { from: 0, to: 2 },
                Edge { from: 1, to: 3 },
                Edge { from: 2, to: 0 },
                Edge { from: 3, to: 1 },
            ],
            source_count: 50,
            target_count: 50,
        };

        let config = InteractionConfig {
            entity_name: "messages".into(),
            actor_entity: "users".into(),
            target_entity: Some("users".into()),
            actor_field: "sender_id".into(),
            target_field: Some("receiver_id".into()),
            trait_fields: HashMap::new(),
            total_count: 50,
        };

        let records = generate_interactions(&config, &pool, Some(&graph), 42, 50);

        assert_eq!(records.len(), 50);
        for r in &records {
            assert!(r.target_index.is_some());
            // All edges should come from our edge list
            let edge = Edge {
                from: r.actor_index,
                to: r.target_index.unwrap(),
            };
            assert!(
                graph.edges.contains(&edge),
                "Generated edge {:?} not in graph",
                edge
            );
        }
    }

    #[test]
    fn deterministic_generation() {
        let pool = make_pool();
        let config = make_config();

        let r1 = generate_interactions(&config, &pool, None, 42, 32);
        let r2 = generate_interactions(&config, &pool, None, 42, 32);

        assert_eq!(r1.len(), r2.len());
        for (a, b) in r1.iter().zip(r2.iter()) {
            assert_eq!(a.actor_index, b.actor_index);
            assert_eq!(a.persona, b.persona);
        }
    }

    #[test]
    fn empty_pool_produces_no_records() {
        let plan = ActorPoolPlan {
            pools: vec![ActorEntityPool {
                entity_name: "users".into(),
                actor_count: 0,
                persona_weights: Vec::new(),
            }],
            graph_plans: Vec::new(),
        };
        let pool = ActorPool::from_plan(&plan, 42);
        let config = InteractionConfig {
            total_count: 10,
            ..make_config()
        };

        // With empty pool, generate_batch returns empty
        let records = generate_interactions(&config, &pool, None, 42, 10);
        assert_eq!(records.len(), 0);
    }

    #[test]
    fn batch_boundary_correctness() {
        let pool = make_pool();
        let config = InteractionConfig {
            total_count: 100,
            ..make_config()
        };

        // Generate with small batch size
        let records = generate_interactions(&config, &pool, None, 42, 7);
        assert_eq!(
            records.len(),
            100,
            "Should produce exactly total_count records"
        );
    }

    #[test]
    fn large_generation_performance() {
        let pool = make_pool();
        let config = InteractionConfig {
            total_count: 10_000,
            ..make_config()
        };

        let records = generate_interactions(&config, &pool, None, 42, 1024);
        assert_eq!(records.len(), 10_000);
    }
}
