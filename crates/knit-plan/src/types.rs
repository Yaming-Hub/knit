//! All plan types for the execution planner.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

use knit_core::{DistributionKind, Value, WeightedChoice};

// ── ExecutionPlan ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    pub phases: Vec<Phase>,
    pub rng_tree: RngTree,
    pub index_strategy: IndexStrategy,
    pub metadata: PlanMetadata,
}

impl fmt::Display for ExecutionPlan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "ExecutionPlan: {}", self.metadata.schema_name)?;
        writeln!(
            f,
            "  entities: {}  phases: {}  partitions: {}",
            self.metadata.total_entities,
            self.metadata.total_phases,
            self.metadata.total_partitions,
        )?;
        writeln!(
            f,
            "  estimated rows: {}  estimated bytes: {}",
            self.metadata.estimated_total_rows, self.metadata.estimated_total_bytes,
        )?;
        if self.metadata.has_cycles {
            writeln!(
                f,
                "  has cycles: yes  deferred refs: {}",
                self.metadata.deferred_ref_count,
            )?;
        }
        writeln!(f, "  global seed: {}", self.rng_tree.global_seed)?;
        for (i, phase) in self.phases.iter().enumerate() {
            writeln!(f, "  Phase {i}:")?;
            for ep in &phase.entity_plans {
                writeln!(
                    f,
                    "    {} ({} rows, {} partitions, {} fields)",
                    ep.entity_name,
                    ep.estimated_row_count,
                    ep.partitions.len(),
                    ep.field_plans.len(),
                )?;
            }
            for dr in &phase.deferred_refs {
                writeln!(
                    f,
                    "    [deferred] {}.{} -> {}.{}",
                    dr.from_entity, dr.from_field, dr.to_entity, dr.to_field,
                )?;
            }
        }
        Ok(())
    }
}

// ── Phase ────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    pub entity_plans: Vec<EntityPlan>,
    pub deferred_refs: Vec<DeferredRef>,
}

// ── EntityPlan ───────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityPlan {
    pub entity_name: String,
    pub partitions: Vec<PartitionRange>,
    pub field_plans: Vec<FieldPlan>,
    pub estimated_row_count: u64,
    pub estimated_byte_size: u64,
}

// ── PartitionRange ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionRange {
    pub partition_id: u32,
    pub start_row: u64,
    pub end_row: u64,
    pub seed: u64,
}

// ── FieldPlan ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldPlan {
    pub field_name: String,
    pub generator_plan: GeneratorPlan,
    pub null_plan: NullPlan,
    pub dependency_order: u32,
}

// ── GeneratorPlan ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GeneratorPlan {
    Distribution {
        kind: DistributionKind,
        params: BTreeMap<String, f64>,
        clamp_min: Option<f64>,
        clamp_max: Option<f64>,
    },
    Faker {
        category: String,
        locale: String,
    },
    Sequence {
        start: i64,
        step: i64,
    },
    OneOf {
        choices: Vec<WeightedChoice>,
        cumulative_weights: Vec<f64>,
    },
    Derived {
        expr: String,
        depends_on: Vec<String>,
    },
    Constant(Value),
    Composite {
        element: Box<GeneratorPlan>,
        length: Box<GeneratorPlan>,
    },
    ForeignKey {
        target_entity: String,
        target_field: String,
        key_store_kind: KeyStoreKind,
    },
    Uuid,
}

// ── NullPlan ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NullPlan {
    Never,
    Always,
    Probability(f64),
    Pattern { every_n: usize },
}

// ── DeferredRef ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredRef {
    pub from_entity: String,
    pub from_field: String,
    pub to_entity: String,
    pub to_field: String,
    pub strategy: DeferralStrategy,
}

// ── DeferralStrategy ─────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeferralStrategy {
    UniformSample,
    DistributionSample(DistributionKind, BTreeMap<String, f64>),
    SelfReference { nullable_root_probability: f64 },
}

// ── RngTree ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RngTree {
    pub global_seed: u64,
    pub entity_nodes: BTreeMap<String, EntitySeedNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySeedNode {
    pub entity_seed: u64,
    pub field_seeds: BTreeMap<String, FieldSeedNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSeedNode {
    pub field_seed: u64,
    pub partition_seeds: Vec<u64>,
}

// ── IndexStrategy ────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStrategy {
    pub per_entity: BTreeMap<String, KeyStoreKind>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeyStoreKind {
    InMemoryVec,
    MemoryMapped,
    SampledSubset { sample_size: usize },
}

// ── PlanMetadata ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanMetadata {
    pub schema_name: String,
    pub total_entities: usize,
    pub total_phases: usize,
    pub total_partitions: usize,
    pub estimated_total_rows: u64,
    pub estimated_total_bytes: u64,
    pub has_cycles: bool,
    pub deferred_ref_count: usize,
}
