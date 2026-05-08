//! Actor pool runtime — persona assignment and per-actor trait storage.
//!
//! This module materializes an [`ActorPoolPlan`] into an in-memory pool of
//! actors, each assigned a persona and pre-sampled trait values. Downstream
//! generators (e.g., `ActorRef`, `PersonaField`) query this pool to produce
//! persona-aware data.
//!
//! ## Lifecycle
//!
//! 1. The `GenerationEngine` calls [`ActorPool::from_plan`] before phase execution.
//! 2. Actor entities are generated normally (their row counts come from the pool).
//! 3. Behavioral entity generators call [`ActorPool::sample_actor`] to select actors
//!    weighted by activity rate, then read traits via [`ActorPool::get_trait`].

use std::collections::{BTreeMap, HashMap};

use knit_core::Value;
use knit_plan::{ActorEntityPool, ActorPoolPlan, PersonaWeight};
use rand::SeedableRng;
use rand::{Rng, RngCore};
use rand_chacha::ChaCha8Rng;

/// Runtime actor pool — holds per-actor persona assignments and trait values.
#[derive(Debug, Clone)]
pub struct ActorPool {
    /// Per-entity actor pools.
    pools: HashMap<String, EntityActorPool>,
}

/// Pool for a single actor entity.
#[derive(Debug, Clone)]
struct EntityActorPool {
    /// Number of actors.
    actor_count: u64,
    /// Per-actor persona name assignment.
    persona_assignments: Vec<String>,
    /// Per-actor trait values: actor_index → (trait_name → value).
    actor_traits: Vec<BTreeMap<String, Value>>,
    /// Cumulative weights for weighted actor sampling (activity-based).
    /// If no activity_rate trait exists, uniform sampling is used.
    cumulative_weights: Vec<f64>,
}

impl ActorPool {
    /// Create an actor pool from the compiled plan.
    ///
    /// Uses the provided seed to deterministically assign personas and sample
    /// trait values for each actor.
    pub fn from_plan(plan: &ActorPoolPlan, seed: u64) -> Self {
        let mut pools = HashMap::new();

        for pool_plan in &plan.pools {
            let entity_pool = build_entity_pool(pool_plan, seed);
            pools.insert(pool_plan.entity_name.clone(), entity_pool);
        }

        Self { pools }
    }

    /// Get the number of actors for a given entity.
    pub fn actor_count(&self, entity: &str) -> Option<u64> {
        self.pools.get(entity).map(|p| p.actor_count)
    }

    /// Get the persona assigned to a specific actor.
    pub fn get_persona(&self, entity: &str, actor_index: usize) -> Option<&str> {
        self.pools
            .get(entity)
            .and_then(|p| p.persona_assignments.get(actor_index))
            .map(|s| s.as_str())
    }

    /// Get a trait value for a specific actor.
    pub fn get_trait(&self, entity: &str, actor_index: usize, trait_name: &str) -> Option<&Value> {
        self.pools
            .get(entity)
            .and_then(|p| p.actor_traits.get(actor_index))
            .and_then(|traits| traits.get(trait_name))
    }

    /// Sample an actor index from the pool, weighted by activity rate.
    ///
    /// Returns a deterministic actor index based on the provided RNG.
    pub fn sample_actor(&self, entity: &str, rng: &mut dyn RngCore) -> Option<usize> {
        let pool = self.pools.get(entity)?;
        if pool.actor_count == 0 {
            return None;
        }

        if pool.cumulative_weights.is_empty() {
            // Uniform sampling
            Some(gen_range_usize(rng, pool.actor_count as usize))
        } else {
            // Weighted sampling via cumulative distribution
            let total = *pool.cumulative_weights.last().unwrap_or(&1.0);
            let sample: f64 = gen_f64(rng) * total;
            let idx = pool
                .cumulative_weights
                .partition_point(|&w| w <= sample)
                .min(pool.actor_count as usize - 1);
            Some(idx)
        }
    }

    /// Get all trait values for a specific actor.
    pub fn get_all_traits(
        &self,
        entity: &str,
        actor_index: usize,
    ) -> Option<&BTreeMap<String, Value>> {
        self.pools
            .get(entity)
            .and_then(|p| p.actor_traits.get(actor_index))
    }

    /// Check if a pool exists for the given entity.
    pub fn has_entity(&self, entity: &str) -> bool {
        self.pools.contains_key(entity)
    }

    /// Get all entity names that have actor pools (sorted for determinism).
    pub fn entity_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.pools.keys().map(|s| s.as_str()).collect();
        names.sort_unstable();
        names
    }
}

/// Generate a uniform f64 in [0, 1) from a dyn RngCore.
fn gen_f64(rng: &mut dyn RngCore) -> f64 {
    // Same algorithm as rand's Standard distribution for f64
    let bits = rng.next_u64();
    (bits >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
}

/// Generate a uniform usize in [0, n) using rejection sampling.
fn gen_range_usize(rng: &mut dyn RngCore, n: usize) -> usize {
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

/// Build an entity actor pool from the plan specification.
fn build_entity_pool(plan: &ActorEntityPool, seed: u64) -> EntityActorPool {
    // Derive per-entity seed using a stable hash to avoid collisions
    // (e.g., "ab" vs "ba" would collide with simple byte-sum)
    let entity_hash = {
        let mut h: u64 = 14695981039346656037; // FNV-1a offset basis
        for b in plan.entity_name.bytes() {
            h ^= b as u64;
            h = h.wrapping_mul(1099511628211); // FNV-1a prime
        }
        h
    };
    let mut rng = ChaCha8Rng::seed_from_u64(seed.wrapping_add(entity_hash));

    let actor_count = plan.actor_count;

    // Guard: if no personas defined, return an empty pool
    if plan.persona_weights.is_empty() || actor_count == 0 {
        return EntityActorPool {
            actor_count: 0,
            persona_assignments: Vec::new(),
            actor_traits: Vec::new(),
            cumulative_weights: Vec::new(),
        };
    }

    let mut persona_assignments = Vec::with_capacity(actor_count as usize);
    let mut actor_traits = Vec::with_capacity(actor_count as usize);

    // Filter out non-positive weights
    let valid_personas: Vec<&PersonaWeight> = plan
        .persona_weights
        .iter()
        .filter(|p| p.weight > 0.0 && p.weight.is_finite())
        .collect();

    if valid_personas.is_empty() {
        return EntityActorPool {
            actor_count: 0,
            persona_assignments: Vec::new(),
            actor_traits: Vec::new(),
            cumulative_weights: Vec::new(),
        };
    }

    let persona_cum_weights: Vec<f64> = {
        let mut cum = Vec::with_capacity(valid_personas.len());
        let mut total = 0.0;
        for p in &valid_personas {
            total += p.weight;
            cum.push(total);
        }
        cum
    };

    for _ in 0..actor_count {
        // Assign persona via weighted sampling
        let persona_idx = sample_from_cumulative(&persona_cum_weights, &mut rng);
        let persona = valid_personas[persona_idx];
        persona_assignments.push(persona.name.clone());

        // Sample trait values for this actor from the persona's trait definitions
        let traits = sample_traits(&persona.traits, &mut rng);
        actor_traits.push(traits);
    }

    // Build activity-weighted cumulative distribution for actor sampling
    let cumulative_weights = build_activity_weights(&actor_traits);

    EntityActorPool {
        actor_count,
        persona_assignments,
        actor_traits,
        cumulative_weights,
    }
}

/// Sample an index from a cumulative weight distribution.
fn sample_from_cumulative(cum_weights: &[f64], rng: &mut impl Rng) -> usize {
    if cum_weights.is_empty() {
        return 0;
    }
    let total = *cum_weights.last().unwrap();
    if total <= 0.0 {
        return 0;
    }
    let sample: f64 = rng.gen::<f64>() * total;
    cum_weights
        .partition_point(|&w| w <= sample)
        .min(cum_weights.len() - 1)
}

/// Sample trait values for one actor from a persona's trait definitions.
///
/// Traits can be:
/// - Scalar values (copied directly)
/// - Distribution specs (sampled)
/// - Arrays (copied directly)
fn sample_traits(traits: &BTreeMap<String, Value>, rng: &mut impl Rng) -> BTreeMap<String, Value> {
    let mut result = BTreeMap::new();

    for (name, value) in traits {
        let sampled = sample_trait_value(value, rng);
        result.insert(name.clone(), sampled);
    }

    result
}

/// Sample a single trait value.
///
/// If the value is a Map with "kind" and "params" keys, treat it as a
/// distribution specification and sample from it. Otherwise copy as-is.
fn sample_trait_value(value: &Value, rng: &mut impl Rng) -> Value {
    match value {
        Value::Map(map) => {
            // Check if this is a distribution spec: { kind = "...", params = { ... } }
            if let (Some(Value::String(kind)), Some(Value::Map(params))) =
                (map.get("kind"), map.get("params"))
            {
                sample_from_distribution(kind, params, rng)
            } else {
                value.clone()
            }
        }
        // Scalar and array values are used directly
        _ => value.clone(),
    }
}

/// Sample a value from a distribution specification.
fn sample_from_distribution(
    kind: &str,
    params: &BTreeMap<String, Value>,
    rng: &mut impl Rng,
) -> Value {
    let get_f64 = |key: &str| -> f64 {
        params
            .get(key)
            .and_then(|v| match v {
                Value::Float(f) => Some(*f),
                Value::Int(i) => Some(*i as f64),
                _ => None,
            })
            .unwrap_or(0.0)
    };

    match kind {
        "normal" | "gaussian" => {
            let mean = get_f64("mean");
            let std_dev = get_f64("std_dev").max(0.001);
            // Box-Muller transform for normal sampling
            let u1: f64 = rng.gen::<f64>().max(1e-10);
            let u2: f64 = rng.gen::<f64>();
            let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
            Value::Float(mean + std_dev * z)
        }
        "uniform" => {
            let min = get_f64("min");
            let max = get_f64("max").max(min + 0.001);
            Value::Float(rng.gen::<f64>() * (max - min) + min)
        }
        "poisson" => {
            let lambda = get_f64("lambda").max(0.0);
            if lambda == 0.0 {
                return Value::Int(0);
            }
            if lambda > 30.0 {
                // Normal approximation for large lambda: N(lambda, sqrt(lambda))
                let u1: f64 = rng.gen::<f64>().max(1e-10);
                let u2: f64 = rng.gen::<f64>();
                let z = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
                let sample = lambda + lambda.sqrt() * z;
                Value::Int(sample.round().max(0.0) as i64)
            } else {
                // Knuth's algorithm (fine for lambda <= 30)
                let l = (-lambda).exp();
                let mut k = 0i64;
                let mut p = 1.0f64;
                loop {
                    k += 1;
                    p *= rng.gen::<f64>();
                    if p <= l {
                        break;
                    }
                }
                Value::Int(k - 1)
            }
        }
        "exponential" => {
            let lambda = get_f64("lambda").max(0.001);
            let u: f64 = rng.gen::<f64>().max(1e-10);
            Value::Float(-u.ln() / lambda)
        }
        _ => {
            // Unknown distribution — return the mean if available
            let mean = get_f64("mean");
            Value::Float(mean)
        }
    }
}

/// Build cumulative activity weights for actor sampling.
///
/// If actors have an "activity_rate" trait that is numeric, use it as the
/// sampling weight. If no actor has the trait at all, returns empty (uniform).
/// If actors have the trait but total weight is zero, still returns empty
/// (uniform fallback — explicitly inactive actors are treated equally).
fn build_activity_weights(actor_traits: &[BTreeMap<String, Value>]) -> Vec<f64> {
    // First check: does any actor have an activity_rate trait?
    let has_activity_trait = actor_traits
        .iter()
        .any(|traits| traits.contains_key("activity_rate"));

    if !has_activity_trait {
        return Vec::new(); // No trait → uniform sampling
    }

    let weights: Vec<f64> = actor_traits
        .iter()
        .map(|traits| {
            traits
                .get("activity_rate")
                .and_then(|v| match v {
                    Value::Float(f) => Some(f.max(0.0)),
                    Value::Int(i) => Some((*i as f64).max(0.0)),
                    _ => None,
                })
                .unwrap_or(0.0)
        })
        .collect();

    // If no actor has a meaningful activity_rate, return empty (uniform)
    let total: f64 = weights.iter().sum();
    if total <= 0.0 {
        return Vec::new();
    }

    // Build cumulative
    let mut cum = Vec::with_capacity(weights.len());
    let mut running = 0.0;
    for w in &weights {
        running += w;
        cum.push(running);
    }
    cum
}

#[cfg(test)]
mod tests {
    use super::*;
    use knit_plan::{ActorEntityPool, ActorPoolPlan, PersonaWeight};

    fn make_plan(actor_count: u64, personas: Vec<PersonaWeight>) -> ActorPoolPlan {
        ActorPoolPlan {
            pools: vec![ActorEntityPool {
                entity_name: "users".into(),
                actor_count,
                persona_weights: personas,
            }],
            graph_plans: Vec::new(),
        }
    }

    #[test]
    fn basic_pool_creation() {
        let plan = make_plan(
            100,
            vec![
                PersonaWeight {
                    name: "power_user".into(),
                    weight: 0.3,
                    traits: BTreeMap::from([("activity_rate".into(), Value::Float(50.0))]),
                },
                PersonaWeight {
                    name: "casual_user".into(),
                    weight: 0.7,
                    traits: BTreeMap::from([("activity_rate".into(), Value::Float(5.0))]),
                },
            ],
        );

        let pool = ActorPool::from_plan(&plan, 42);

        assert_eq!(pool.actor_count("users"), Some(100));
        assert!(pool.has_entity("users"));
        assert!(!pool.has_entity("orders"));

        // Every actor should have a persona
        for i in 0..100 {
            let persona = pool.get_persona("users", i).unwrap();
            assert!(persona == "power_user" || persona == "casual_user");
        }
    }

    #[test]
    fn persona_distribution_matches_weights() {
        let plan = make_plan(
            1000,
            vec![
                PersonaWeight {
                    name: "a".into(),
                    weight: 0.2,
                    traits: BTreeMap::new(),
                },
                PersonaWeight {
                    name: "b".into(),
                    weight: 0.8,
                    traits: BTreeMap::new(),
                },
            ],
        );

        let pool = ActorPool::from_plan(&plan, 123);

        let a_count = (0..1000)
            .filter(|&i| pool.get_persona("users", i) == Some("a"))
            .count();

        // With 1000 actors and weight 0.2, expect ~200 ± 30
        assert!(
            (150..250).contains(&a_count),
            "Expected ~200 'a' personas, got {a_count}"
        );
    }

    #[test]
    fn trait_sampling_from_distribution() {
        let plan = make_plan(
            50,
            vec![PersonaWeight {
                name: "test".into(),
                weight: 1.0,
                traits: BTreeMap::from([(
                    "rate".into(),
                    Value::Map(BTreeMap::from([
                        ("kind".into(), Value::String("normal".into())),
                        (
                            "params".into(),
                            Value::Map(BTreeMap::from([
                                ("mean".into(), Value::Float(100.0)),
                                ("std_dev".into(), Value::Float(10.0)),
                            ])),
                        ),
                    ])),
                )]),
            }],
        );

        let pool = ActorPool::from_plan(&plan, 42);

        // All actors should have a "rate" trait that's a Float
        let mut sum = 0.0;
        for i in 0..50 {
            match pool.get_trait("users", i, "rate") {
                Some(Value::Float(f)) => sum += f,
                other => panic!("Expected Float trait, got {other:?}"),
            }
        }
        let mean = sum / 50.0;
        // Mean should be approximately 100 (within ~20 of the target)
        assert!(
            (80.0..120.0).contains(&mean),
            "Expected mean ~100, got {mean}"
        );
    }

    #[test]
    fn scalar_traits_copied_directly() {
        let plan = make_plan(
            10,
            vec![PersonaWeight {
                name: "test".into(),
                weight: 1.0,
                traits: BTreeMap::from([
                    (
                        "peak_hours".into(),
                        Value::Array(vec![Value::Int(9), Value::Int(10), Value::Int(14)]),
                    ),
                    ("label".into(), Value::String("vip".into())),
                ]),
            }],
        );

        let pool = ActorPool::from_plan(&plan, 42);

        for i in 0..10 {
            assert_eq!(
                pool.get_trait("users", i, "label"),
                Some(&Value::String("vip".into()))
            );
            assert_eq!(
                pool.get_trait("users", i, "peak_hours"),
                Some(&Value::Array(vec![
                    Value::Int(9),
                    Value::Int(10),
                    Value::Int(14)
                ]))
            );
        }
    }

    #[test]
    fn weighted_actor_sampling() {
        let plan = make_plan(
            100,
            vec![
                PersonaWeight {
                    name: "heavy".into(),
                    weight: 0.1,
                    traits: BTreeMap::from([("activity_rate".into(), Value::Float(100.0))]),
                },
                PersonaWeight {
                    name: "light".into(),
                    weight: 0.9,
                    traits: BTreeMap::from([("activity_rate".into(), Value::Float(1.0))]),
                },
            ],
        );

        let pool = ActorPool::from_plan(&plan, 42);
        let mut rng = ChaCha8Rng::seed_from_u64(99);

        // Sample 1000 actors — heavy users should be overrepresented
        let mut heavy_count = 0;
        for _ in 0..1000 {
            let idx = pool.sample_actor("users", &mut rng).unwrap();
            if pool.get_persona("users", idx) == Some("heavy") {
                heavy_count += 1;
            }
        }

        // ~10% of actors are "heavy" but they have 100x activity_rate,
        // so they should appear much more than 10% of samples.
        // Expected: heavy has ~10 actors × 100 weight = 1000 total weight
        // light has ~90 actors × 1 weight = 90 total weight
        // heavy fraction ≈ 1000/1090 ≈ 92%
        assert!(
            heavy_count > 700,
            "Expected heavy users to dominate sampling, got {heavy_count}/1000"
        );
    }

    #[test]
    fn deterministic_with_same_seed() {
        let plan = make_plan(
            50,
            vec![
                PersonaWeight {
                    name: "a".into(),
                    weight: 0.5,
                    traits: BTreeMap::from([("x".into(), Value::Float(10.0))]),
                },
                PersonaWeight {
                    name: "b".into(),
                    weight: 0.5,
                    traits: BTreeMap::new(),
                },
            ],
        );

        let pool1 = ActorPool::from_plan(&plan, 42);
        let pool2 = ActorPool::from_plan(&plan, 42);

        for i in 0..50 {
            assert_eq!(pool1.get_persona("users", i), pool2.get_persona("users", i));
            assert_eq!(
                pool1.get_all_traits("users", i),
                pool2.get_all_traits("users", i)
            );
        }
    }

    #[test]
    fn empty_plan_produces_empty_pool() {
        let plan = ActorPoolPlan::default();
        let pool = ActorPool::from_plan(&plan, 42);
        assert!(!pool.has_entity("users"));
        assert_eq!(pool.entity_names().len(), 0);
    }

    #[test]
    fn uniform_sampling_without_activity_rate() {
        let plan = make_plan(
            100,
            vec![PersonaWeight {
                name: "default".into(),
                weight: 1.0,
                traits: BTreeMap::new(), // no activity_rate
            }],
        );

        let pool = ActorPool::from_plan(&plan, 42);
        let mut rng = ChaCha8Rng::seed_from_u64(7);

        // Should not panic and should sample uniformly
        let mut counts = vec![0u32; 100];
        for _ in 0..10000 {
            let idx = pool.sample_actor("users", &mut rng).unwrap();
            counts[idx] += 1;
        }
        // Each actor should get ~100 samples; check no extreme outliers
        let min = *counts.iter().min().unwrap();
        let max = *counts.iter().max().unwrap();
        assert!(min > 50, "min count {min} too low for uniform");
        assert!(max < 200, "max count {max} too high for uniform");
    }
}
