//! Plan type definitions consumed by [`gen`](crate::gen) to drive data generation.
//!
//! All types in this module derive `Serialize`/`Deserialize` so the plan can be
//! inspected as JSON (via `knit plan --json`) or cached to disk.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;

use crate::core::{DistributionKind, Value, WeightedChoice};

// ── ExecutionPlan ────────────────────────────────────────────────────

/// A complete execution plan produced by [`compile()`](crate::plan::compile) from a
/// validated [`DataModel`](crate::core::DataModel).
///
/// The plan is consumed by [`gen`](crate::gen) to drive parallel data
/// generation. It contains phase ordering, partition assignments, generator
/// plans, and the deterministic RNG seed tree.
///
/// # Determinism
///
/// The same `DataModel` always produces the same `ExecutionPlan`, regardless of
/// platform or thread count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionPlan {
    /// Ordered generation phases. Phase 0 runs first, then phase 1, etc.
    /// Entities within a phase have no inter-dependencies and can run in parallel.
    pub phases: Vec<Phase>,
    /// Hierarchical deterministic seed tree for reproducible RNG initialization.
    pub rng_tree: RngTree,
    /// Per-entity key store sizing decisions (in-memory vs mmap vs sampled).
    pub index_strategy: IndexStrategy,
    /// Actor pool plan: persona assignments and relationship graphs.
    /// Generated before entity phases; the engine uses this to drive
    /// persona-aware generation for behavioral entities.
    #[serde(default)]
    pub actor_pool: ActorPoolPlan,
    /// Informational metadata for inspection and debugging.
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
        if !self.actor_pool.pools.is_empty() {
            writeln!(
                f,
                "  Actor pool: {} entities, {} graph plans",
                self.actor_pool.pools.len(),
                self.actor_pool.graph_plans.len(),
            )?;
            for pool in &self.actor_pool.pools {
                writeln!(
                    f,
                    "    {} ({} actors, {} personas)",
                    pool.entity_name,
                    pool.actor_count,
                    pool.persona_weights.len(),
                )?;
            }
        }
        Ok(())
    }
}

// ── Phase ────────────────────────────────────────────────────────────

/// A generation phase. All entity plans within a phase are independent and can
/// execute in parallel. Deferred refs are resolved after all entity plans in
/// the phase complete.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Phase {
    /// Entity generation plans that can run concurrently.
    pub entity_plans: Vec<EntityPlan>,
    /// Foreign key references that must be backpatched after this phase completes
    /// (used for cyclic/self-referential relationships).
    pub deferred_refs: Vec<DeferredRef>,
}

// ── EntityPlan ───────────────────────────────────────────────────────

/// Plan for generating a single entity's data, including how to split work
/// across partitions and what generator to use for each field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntityPlan {
    /// Name of the entity (matches [`Entity::name`](crate::core::Entity)).
    pub entity_name: String,
    /// Contiguous row ranges assigned to parallel workers.
    pub partitions: Vec<PartitionRange>,
    /// Per-field generation instructions, sorted by dependency order.
    pub field_plans: Vec<FieldPlan>,
    /// Total rows to generate (resolved from [`CountSpec`](crate::core::CountSpec)).
    pub estimated_row_count: u64,
    /// Estimated output size in bytes (used for progress reporting).
    pub estimated_byte_size: u64,
    /// Index of the primary-key field within `field_plans`, if any.
    /// Used by the engine to extract PK values into the key store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_key_field_index: Option<usize>,
    /// Copula joint-generation plans (entity-level, applied after independent fields).
    /// Each plan replaces the independently generated columns with jointly
    /// correlated values via the specified copula.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub copula_plans: Vec<CopulaPlan>,
}

// ── PartitionRange ───────────────────────────────────────────────────

/// A contiguous range of rows assigned to one partition.
/// Each partition is generated by a single thread with its own deterministic RNG.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PartitionRange {
    /// Zero-based partition identifier.
    pub partition_id: u32,
    /// First row index (inclusive).
    pub start_row: u64,
    /// Last row index (exclusive).
    pub end_row: u64,
    /// Deterministic seed for this partition's RNG, derived from the RNG tree.
    pub seed: u64,
}

// ── FieldPlan ────────────────────────────────────────────────────────

/// Plan for generating a single field within an entity.
/// Produced by compiling a [`GeneratorSpec`](crate::core::GeneratorSpec) with
/// resolved parameters and dependency information.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldPlan {
    /// Field name (matches [`Field::name`](crate::core::Field)).
    pub field_name: String,
    /// Declared data type from the schema (used for output type selection).
    #[serde(default = "default_data_type")]
    pub data_type: crate::core::DataType,
    /// Compiled generator with all parameters resolved.
    pub generator_plan: GeneratorPlan,
    /// How to apply null values to this field's output.
    pub null_plan: NullPlan,
    /// Execution order within the entity. Fields with lower values are generated
    /// first. Derived fields have higher order than their dependencies.
    pub dependency_order: u32,
    /// Original position in the schema's field list (0-based). Used to restore
    /// the declared column order in output after dependency-ordered generation.
    #[serde(default)]
    pub schema_position: usize,
    /// Number of decimal places for float output. When set, the generated array
    /// is rounded to this precision after generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision: Option<u8>,
    /// Whether this field references an actor entity and should use
    /// persona-weighted sampling instead of uniform FK generation.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub actor_column: bool,
    /// Sub-field plans for nested object fields (`type = "object"`).
    /// Empty for non-object fields.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_field_plans: Vec<FieldPlan>,
}

fn default_data_type() -> crate::core::DataType {
    crate::core::DataType::String
}

// ── GeneratorPlan ────────────────────────────────────────────────────

/// A compiled generator — all parameters fully resolved, ready for execution.
/// This is the plan-time counterpart of the schema-level
/// [`GeneratorSpec`](crate::core::GeneratorSpec).
///
/// [`gen`](crate::gen) maps each variant to a concrete `FieldGenerator`
/// implementation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GeneratorPlan {
    /// Statistical distribution with resolved, validated parameters.
    Distribution {
        /// The distribution family (e.g. Normal, Uniform).
        kind: DistributionKind,
        /// Resolved numeric parameters for the distribution.
        params: BTreeMap<String, f64>,
        /// Array parameters for vector-valued distributions (Dirichlet alpha, Multinomial p).
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        array_params: BTreeMap<String, Vec<f64>>,
        /// Optional lower bound (values below are clamped).
        clamp_min: Option<f64>,
        /// Optional upper bound (values above are clamped).
        clamp_max: Option<f64>,
        /// When true, round sampled values to the nearest integer.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        round: bool,
    },
    /// Faker-style structured data (names, emails, addresses) with resolved locale.
    Faker {
        /// Faker category (e.g. `"name"`, `"email"`).
        category: String,
        /// Locale for locale-aware fake data.
        locale: String,
        /// Optional arguments (e.g. date range).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        args: Vec<crate::core::Value>,
    },
    /// Auto-increment or cyclic sequence. Start/step are resolved per-partition
    /// to avoid collisions across parallel workers.
    Sequence {
        /// Initial value for the sequence.
        start: i64,
        /// Increment between consecutive values.
        step: i64,
        /// Optional jitter in milliseconds. When present, each value receives a
        /// random offset drawn uniformly from `[-jitter_ms, +jitter_ms]`.
        jitter_ms: Option<i64>,
    },
    /// Cycle through a fixed list of string values round-robin.
    /// Row assignment is deterministic: `values[(row_offset + i) % values.len()]`.
    CyclicValues {
        /// The values to cycle through.
        values: Vec<String>,
    },
    /// Weighted random choice with pre-computed cumulative weights for O(log n) sampling.
    OneOf {
        /// The set of weighted values to choose from.
        choices: Vec<WeightedChoice>,
        /// Normalized cumulative distribution for binary-search sampling.
        cumulative_weights: Vec<f64>,
    },
    /// Derived field: expression referencing other fields in the same entity.
    Derived {
        /// Expression referencing other fields in the same entity.
        expr: String,
        /// Field names this expression depends on (must be generated first).
        depends_on: Vec<String>,
    },
    /// Fixed value for every row.
    Constant(Value),
    /// Composite/array generator: produces arrays with element strategy and length distribution.
    Composite {
        /// Generator plan for each array element.
        element: Box<GeneratorPlan>,
        /// Generator plan for the array length.
        length: Box<GeneratorPlan>,
    },
    /// Foreign key lookup — samples from a parent entity's key store.
    ForeignKey {
        /// Name of the parent entity to reference.
        target_entity: String,
        /// Primary key field on the parent entity.
        target_field: String,
        /// Storage strategy for the parent's key store.
        key_store_kind: KeyStoreKind,
        /// Optional degree distribution for non-uniform parent selection.
        /// When set, some parents receive disproportionately more children.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        degree: Option<DegreePlan>,
        /// Optional selection strategy for FK parent selection.
        /// Mutually exclusive with `degree`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        selection: Option<SelectionPlan>,
    },
    /// Random UUID v4.
    Uuid,
    /// Pattern-based string generation (e.g. `"###-???-AAA"`).
    Pattern {
        /// The pattern template string.
        pattern: String,
    },
    /// Temporal generator — relative timestamps, time series, or business hours.
    Temporal {
        /// Which temporal strategy to use.
        kind: TemporalKind,
        /// Numeric parameters (strategy-specific, e.g. `start_hour`, `trend_slope`).
        params: BTreeMap<String, f64>,
        /// Optional base field for relative timestamps.
        base_field: Option<String>,
        /// String parameters for timezone, exclude_dates, etc.
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        string_params: BTreeMap<String, String>,
    },
    /// Correlated field — generates values correlated with an existing column.
    Correlated {
        /// Name of the already-generated field to correlate with.
        target_field: String,
        /// Target Pearson correlation coefficient (−1.0 to 1.0).
        correlation: f64,
    },
    /// Graph topology generator — preferential attachment, trees, etc.
    Topology {
        /// Which graph model to use.
        model: TopologyModel,
        /// Numeric parameters (model-specific, e.g. `m`, `max_depth`).
        params: BTreeMap<String, f64>,
    },
    /// Wraps an inner generator plan with uniqueness enforcement.
    ///
    /// Generated values are deduplicated via retry. If `max_retries` is
    /// exceeded for a single row, the duplicate value is included and a
    /// warning is logged.
    Unique {
        /// The inner generator plan to wrap.
        inner: Box<GeneratorPlan>,
        /// Maximum number of retries per row before accepting a duplicate.
        max_retries: u32,
    },
    /// Conditional generator — switches generator based on another field's value.
    ///
    /// At runtime, reads the reference field from `batch_columns` and for each
    /// row picks the branch whose condition matches, falling back to `default`.
    Conditional {
        /// Name of the field to branch on (must be generated before this field).
        field: String,
        /// Ordered list of (condition_value, generator) pairs.
        branches: Vec<(Value, Box<GeneratorPlan>)>,
        /// Fallback generator when no branch matches.
        default: Box<GeneratorPlan>,
    },
    /// Dictionary-based generator — samples from an external word list.
    ///
    /// Entries are loaded from a text file (one value per line). The expansion
    /// strategy controls behavior when more values are needed than the
    /// dictionary contains.
    Dictionary {
        /// The loaded dictionary entries (populated by CLI after compilation).
        entries: Vec<String>,
        /// Expansion strategy: `"sample"`, `"combinatorial"`, or `"suffix"`.
        expansion: String,
        /// Original file path from the schema (used for resolution).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_file: Option<String>,
    },
    /// Graph-aware FK — samples target actor from source actor's graph neighbors.
    ///
    /// At runtime, reads the source field column from `batch_columns`, maps each
    /// PK value back to an actor index, looks up the actor's outgoing edges in the
    /// named graph, and samples a neighbor's PK as the target value.
    GraphTarget {
        /// Name of the actor relationship graph to follow.
        graph_name: String,
        /// Field in the same entity to read source actor PKs from.
        source_field: String,
        /// Source entity for the graph (graph's from_entity, used for PK reverse map).
        from_entity: String,
        /// Entity that the target FK references (same as graph's to_entity).
        target_entity: String,
        /// Key store strategy for the target entity.
        key_store_kind: KeyStoreKind,
    },
    /// Persona-driven field — outputs the current actor's persona trait value.
    ///
    /// At runtime, reads the actor FK column from `batch_columns`, maps each PK
    /// value to an actor index via the reverse map, and returns the trait value
    /// from the actor pool. Output type matches the field's declared data type.
    PersonaField {
        /// Name of the persona trait to look up (e.g. `"activity_rate"`).
        trait_name: String,
        /// Actor entity name to look up traits from.
        actor_entity: String,
        /// FK field in this entity that references the actor entity.
        actor_field: String,
    },
    /// Actor-temporal generator — timestamps biased toward actor's preferred hours.
    ///
    /// Reads the actor FK column, looks up the actor's temporal trait (expected
    /// to be a float representing preferred hour 0–23), and generates timestamps
    /// with a wrapped-normal distribution centered on that hour.
    ///
    /// When `temporal_start_field` is set, generated timestamps are constrained
    /// to be **after** the actor's creation time (captured from the actor entity's
    /// datetime column during generation).
    ///
    /// When `temporal_after` is set, generated timestamps are also constrained
    /// to be **after** the referenced entity's timestamp (cross-entity causality).
    ActorTemporal {
        /// Name of the persona trait for temporal bias (e.g. `"peak_hours"`).
        trait_name: String,
        /// Actor entity name to look up traits from.
        actor_entity: String,
        /// FK field in this entity that references the actor entity.
        actor_field: String,
        /// Optional: datetime field in the actor entity whose value serves as
        /// the lower bound for generated timestamps (e.g. `"signup_date"`).
        temporal_start_field: Option<String>,
        /// Minimum milliseconds between consecutive events from the same actor.
        /// Defaults to 60_000 (1 minute) when `None`.
        min_event_gap_ms: Option<i64>,
        /// Optional cross-entity causal constraint: timestamp must be >= the
        /// referenced entity's timestamp field (looked up via FK).
        temporal_after: Option<TemporalAfter>,
        /// Optional burst/session pattern for clustered event generation.
        burst: Option<BurstPlan>,
    },
    /// Self-referential thread/conversation generator.
    ///
    /// Produces nullable Int64 values: NULL for thread starters, a previous
    /// row's PK for replies. Uses a recency-weighted ring buffer to select
    /// parent messages, creating realistic conversation tree structures.
    ThreadRef {
        /// Probability that a row is a reply (0.0 = all starters, 1.0 = all replies).
        reply_probability: f64,
        /// Maximum thread depth before forcing a new thread.
        max_depth: u32,
        /// Number of recent PKs to consider for reply targets.
        reply_window: usize,
        /// Name of the PK field in the same entity (to read generated PKs).
        pk_field: String,
    },
    /// Custom generator supplied by a runtime plugin.
    ///
    /// At execution time, the plugin is looked up by name in the global registry.
    /// If not found, generation fails with an error.
    Plugin {
        /// Registered plugin name.
        name: String,
        /// Typed parameters passed to the plugin factory.
        params: BTreeMap<String, crate::core::Value>,
    },
    /// External lookup — samples from a column in a CSV/JSON/Parquet file.
    ///
    /// Entries are loaded by the CLI layer after compilation (like Dictionary),
    /// which resolves the file path relative to the schema directory.
    ExternalLookup {
        /// Loaded string values from the source column (populated after compilation).
        entries: Vec<String>,
        /// Optional weights for weighted sampling (populated after compilation).
        weights: Option<Vec<f64>>,
        /// Sampling strategy.
        sampling: crate::core::SamplingMode,
        /// Original source file path (used for resolution, cleared after loading).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_file: Option<String>,
        /// Column name to extract (used during resolution).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_column: Option<String>,
        /// Weight column name (used during resolution for weighted sampling).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight_column: Option<String>,
        /// File format (used during resolution).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_format: Option<crate::core::LookupFormat>,
    },
    /// Nested object generator — assembles child field generators into an Arrow StructArray.
    Struct,
    /// Numeric time series with composable additive components.
    ///
    /// Produces Float64 values: `baseline + Σ components`.
    /// Stateful components (AR, level_shift, spike) force sequential execution.
    NumericTimeSeries {
        /// Base value around which the series fluctuates.
        baseline: f64,
        /// Resolved time series components.
        components: Vec<crate::core::TimeSeriesComponent>,
        /// Optional minimum clamp value.
        min: Option<f64>,
        /// Optional maximum clamp value.
        max: Option<f64>,
        /// Optional timestamp field name for calendar-aware components.
        timestamp_field: Option<String>,
        /// Whether this generator has stateful components (AR, level_shift, spike)
        /// that require sequential partition execution.
        needs_sequential: bool,
    },
    /// Event stream — strictly-increasing timestamps with random inter-arrival times.
    ///
    /// Uses an exponential distribution for gaps, optionally modulated by
    /// seasonality, weekend, and business-hour components via thinning.
    /// Always forces sequential execution to maintain cumulative state.
    EventStream {
        /// Epoch-millisecond start time.
        start_ms: i64,
        /// Base rate parameter (events per millisecond).
        lambda_per_ms: f64,
        /// Rate-modulation components.
        components: Vec<crate::core::EventStreamComponent>,
    },
}

/// Cross-entity causal ordering: ensures a timestamp is >= the referenced
/// entity's timestamp, creating a parent→child temporal dependency.
///
/// Example: a comment's `created_at` must be >= the parent post's `created_at`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalAfter {
    /// Name of the referenced entity (e.g. `"posts"`).
    pub entity: String,
    /// Timestamp field in the referenced entity (e.g. `"created_at"`).
    pub field: String,
    /// FK field in *this* entity that references the parent entity's PK.
    pub fk: String,
}

/// Burst/session plan: events cluster into sessions with idle gaps.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BurstPlan {
    /// Average number of events per burst (Poisson-sampled, min 1).
    pub avg_events: f64,
    /// Average gap between events within a burst (milliseconds).
    pub avg_gap_ms: i64,
    /// Average idle time between bursts (milliseconds).
    pub avg_idle_ms: i64,
}

// ── TemporalKind ─────────────────────────────────────────────────────

/// Compiled degree distribution for non-uniform FK sampling.
///
/// Pre-resolved from the schema-level `DistributionSpec` so the generator
/// can sample directly without re-parsing parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DegreePlan {
    /// Distribution family (e.g. Zipf, Uniform, Normal).
    pub kind: crate::core::DistributionKind,
    /// Resolved numeric parameters.
    pub params: std::collections::BTreeMap<String, f64>,
    /// Planned parent row count (used as Zipf `n` when not explicitly set).
    pub parent_count: u64,
}

/// Compiled selection strategy for FK parent selection.
///
/// Pre-resolved from the schema-level `SelectionStrategy` so the generator
/// can select parents without re-parsing the schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SelectionPlan {
    /// Uniform random selection (the default).
    Uniform,
    /// Round-robin assignment: child row i → parent `(row_offset + i) % parent_count`.
    Sequential,
    /// Locality-based clustering: child rows reference nearby parents.
    Clustered {
        /// Window size controlling locality spread.
        cluster_size: u64,
    },
}

/// Temporal generation strategy.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TemporalKind {
    /// Timestamp relative to another field (offset by a distribution).
    Relative,
    /// Synthetic time series with trend, seasonality, and noise.
    TimeSeries,
    /// Timestamps constrained to business hours (and optionally weekdays).
    BusinessHours,
}

// ── TopologyModel ───────────────────────────────────────────────────

/// Graph topology model for generating edge or parent-id columns.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TopologyModel {
    /// Barabási–Albert preferential attachment.
    BarabasiAlbert,
    /// Random hierarchical tree with Poisson branching.
    Tree,
    /// Watts–Strogatz small-world network with ring lattice and random rewiring.
    WattsStrogatz,
    /// Erdős–Rényi G(n, p) random graph.
    ErdosRenyi,
    /// Simplified Stochastic Block Model with equal community sizes.
    StochasticBlock,
    /// Configuration model with custom degree distribution.
    Configuration,
    /// Fully connected (complete) graph.
    Complete,
}

// ── NullPlan ─────────────────────────────────────────────────────────

/// How to apply nulls to a field's generated output.
/// Compiled from [`NullSpec`](crate::core::NullSpec).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NullPlan {
    /// Never produce null values.
    Never,
    /// Every value is null (the field exists but is always empty).
    Always,
    /// Each value has an independent probability of being null.
    Probability(f64),
    /// Every Nth row is null (deterministic pattern).
    Pattern {
        /// Generate null for every Nth row.
        every_n: usize,
    },
}

// ── DeferredRef ──────────────────────────────────────────────────────

/// A foreign key reference that cannot be resolved during normal generation
/// because it creates a dependency cycle. These are backpatched after the
/// phase completes and the target entity's key store is populated.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeferredRef {
    /// Entity that holds the foreign key field.
    pub from_entity: String,
    /// Foreign key field name on the source entity.
    pub from_field: String,
    /// Entity whose primary key is referenced.
    pub to_entity: String,
    /// Primary key field name on the target entity.
    pub to_field: String,
    /// How to sample values during backpatch.
    pub strategy: DeferralStrategy,
}

// ── DeferralStrategy ─────────────────────────────────────────────────

/// Strategy for resolving deferred foreign key references during backpatch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DeferralStrategy {
    /// Sample uniformly from the target entity's key store.
    UniformSample,
    /// Sample according to a cardinality distribution (e.g., Zipf for skewed FKs).
    DistributionSample(DistributionKind, BTreeMap<String, f64>),
    /// Self-referential: sample from own key store (e.g., `employee.manager_id → employee.id`).
    /// A fraction of rows are left null to serve as hierarchy roots.
    SelfReference {
        /// Fraction of rows left null to serve as hierarchy roots.
        nullable_root_probability: f64,
        /// When true, build a proper tree/forest — no cycles.
        acyclic: bool,
        /// Maximum hierarchy depth (root = depth 0). `None` = unlimited.
        max_depth: Option<u32>,
    },
}

// ── RngTree ──────────────────────────────────────────────────────────

/// Hierarchical deterministic seed tree ensuring reproducible generation.
///
/// Seeds are derived via SipHash: `global_seed → entity_seed → field_seed → partition_seed`.
/// This guarantees that adding an entity or field does not affect seeds of
/// existing ones, and that output is identical regardless of thread count.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RngTree {
    /// Top-level seed from the schema's `model.seed` value.
    pub global_seed: u64,
    /// Per-entity seed nodes keyed by entity name.
    pub entity_nodes: BTreeMap<String, EntitySeedNode>,
}

/// Seed node for a single entity in the RNG tree.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntitySeedNode {
    /// Derived from `hash(global_seed, entity_name)`.
    pub entity_seed: u64,
    /// Per-field seed nodes keyed by field name.
    pub field_seeds: BTreeMap<String, FieldSeedNode>,
}

/// Seed node for a single field, containing per-partition seeds.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldSeedNode {
    /// Derived from `hash(entity_seed, field_name)`.
    pub field_seed: u64,
    /// One seed per partition, derived from `hash(field_seed, partition_id)`.
    pub partition_seeds: Vec<u64>,
}

// ── IndexStrategy ────────────────────────────────────────────────────

/// Per-entity key store sizing decisions.
///
/// Determines how primary keys are stored for foreign key sampling. The choice
/// depends on entity row count and available memory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStrategy {
    /// Maps entity name to its key store implementation choice.
    pub per_entity: BTreeMap<String, KeyStoreKind>,
}

/// How to store primary keys for foreign key sampling in [`gen`](crate::gen).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum KeyStoreKind {
    /// In-memory `Vec<PK>` — fast random access, used for entities < 10M rows.
    InMemoryVec,
    /// Memory-mapped file — for 10M–100M rows where full in-memory is too expensive.
    MemoryMapped,
    /// Sampled subset — for > 100M rows. Stores a representative sample for
    /// approximate FK distribution.
    SampledSubset {
        /// Number of representative samples to store.
        sample_size: usize,
    },
}

// ── PlanMetadata ─────────────────────────────────────────────────────

/// Informational metadata about the execution plan, used for inspection
/// (the `knit plan` command) and progress reporting during generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanMetadata {
    /// Schema name from the `DataModel`.
    pub schema_name: String,
    /// Number of entities in the model.
    pub total_entities: usize,
    /// Number of generation phases.
    pub total_phases: usize,
    /// Total partitions across all entities.
    pub total_partitions: usize,
    /// Sum of all entity row counts.
    pub estimated_total_rows: u64,
    /// Sum of estimated byte sizes across all entities.
    pub estimated_total_bytes: u64,
    /// Whether the dependency graph contains cycles (requiring deferred refs).
    pub has_cycles: bool,
    /// Number of foreign key references that require backpatching.
    pub deferred_ref_count: usize,
    /// Number of entities marked as actors (`actor = true`).
    pub actor_entity_count: usize,
    /// Number of persona definitions in the model.
    pub persona_count: usize,
    /// Number of actor relationship definitions in the model.
    pub actor_relationship_count: usize,
}

// ── Actor Pool Plan ─────────────────────────────────────────────────

/// Plan for generating the actor pool before behavioral entity generation.
///
/// The actor pool assigns each actor a persona and pre-samples individual
/// trait parameters from the persona's distribution. This allows downstream
/// behavioral generators to look up per-actor traits at generation time.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ActorPoolPlan {
    /// Per-entity actor pool specifications (one per actor entity).
    pub pools: Vec<ActorEntityPool>,
    /// Graph plans for actor-to-actor relationships.
    pub graph_plans: Vec<GraphPlan>,
}

/// Actor pool specification for a single actor entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorEntityPool {
    /// The actor entity name (e.g. "users").
    pub entity_name: String,
    /// Number of actors to generate in the pool.
    pub actor_count: u64,
    /// Persona assignments: persona name → weight (fraction of actors).
    pub persona_weights: Vec<PersonaWeight>,
}

/// A single persona assignment entry with traits.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PersonaWeight {
    /// Persona name.
    pub name: String,
    /// Fraction of actors assigned this persona (0.0–1.0).
    pub weight: f64,
    /// Trait definitions for this persona. Keys are trait names,
    /// values are the specification (scalar, distribution, or array).
    pub traits: BTreeMap<String, Value>,
}

/// Plan for generating an actor-to-actor relationship graph.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphPlan {
    /// Relationship name.
    pub name: String,
    /// Source actor entity.
    pub from_entity: String,
    /// Target actor entity.
    pub to_entity: String,
    /// Graph generation algorithm.
    pub graph_type: crate::core::GraphType,
    /// Algorithm parameters (avg_degree, reciprocity, clustering, etc.).
    pub params: BTreeMap<String, f64>,
    /// Number of communities to generate (if applicable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community_count: Option<u64>,
    /// Maximum hierarchy depth (for hierarchical graphs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hierarchy_depth: Option<u32>,
}

// ── CopulaPlan ──────────────────────────────────────────────────────

/// Entity-level plan for copula-based joint distribution generation.
///
/// After fields are independently generated, the copula plan replaces
/// the specified columns with jointly correlated values. The marginal
/// distributions of each field are preserved while the copula controls
/// the dependence structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopulaPlan {
    /// Field names involved in the joint distribution.
    pub fields: Vec<String>,
    /// Which copula family to apply.
    pub family: crate::core::CopulaFamily,
    /// Cholesky lower triangle of the correlation matrix (row-major).
    /// Present only for Gaussian copula.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cholesky_l: Option<Vec<Vec<f64>>>,
    /// Copula parameter (theta) for Archimedean families.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theta: Option<f64>,
    /// Marginal distribution info for each field (for inverse CDF transform).
    /// Order matches `fields`.
    pub marginals: Vec<MarginalInfo>,
}

/// Information about a field's marginal distribution, used for
/// CDF/inverse-CDF transformations in copula generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarginalInfo {
    /// Distribution family.
    pub kind: DistributionKind,
    /// Distribution parameters.
    pub params: BTreeMap<String, f64>,
    /// Whether to round the final output to integer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub round: bool,
}
