//! Core type definitions for the Weave data model.
//!
//! All types derive `Serialize`/`Deserialize` so schemas can be read from and
//! written to TOML, JSON, or any serde-compatible format.

use serde::{Deserialize, Deserializer, Serialize};
use std::collections::BTreeMap;

// ── Default helpers ──────────────────────────────────────────────────

fn default_seed() -> u64 {
    42
}
fn default_locale() -> String {
    "en_US".into()
}
fn default_timezone() -> String {
    "UTC".into()
}
fn default_schema_version() -> String {
    "1.0".into()
}
fn default_uuid_version() -> u8 {
    4
}

// ── Value ────────────────────────────────────────────────────────────

/// A loosely-typed value used in parameters, constants, conditional branches,
/// and weighted choices throughout the schema.
///
/// Maps directly to JSON/TOML value types via `#[serde(untagged)]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    /// JSON/TOML `null`.
    Null,
    /// Boolean `true` or `false`.
    Bool(bool),
    /// Signed 64-bit integer.
    Int(i64),
    /// 64-bit floating-point number.
    Float(f64),
    /// UTF-8 string.
    String(String),
    /// Ordered array of values.
    Array(Vec<Value>),
    /// String-keyed map of values.
    Map(BTreeMap<String, Value>),
}

// ── DataModel ────────────────────────────────────────────────────────

/// The root data model describing an entire synthetic dataset.
///
/// A `DataModel` is produced by `knit-schema`'s parser and consumed by the
/// `knit-plan` compiler. It contains all entities, relationships, noise
/// profiles, and correlations needed to generate data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataModel {
    /// Human-readable name for this model (e.g. `"ecommerce"`).
    pub name: String,
    /// Optional free-text description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Global RNG seed for deterministic generation.
    #[serde(default = "default_seed")]
    pub seed: u64,
    /// Default locale for faker-style generators (e.g. `"en_US"`).
    #[serde(default = "default_locale")]
    pub locale: String,
    /// Default timezone for temporal generators (e.g. `"UTC"`).
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Entities (tables) to generate.
    #[serde(default)]
    pub entities: Vec<Entity>,
    /// Foreign-key and association relationships between entities.
    #[serde(default)]
    pub relationships: Vec<Relationship>,
    /// Noise injection profiles (null rates, typos, outliers).
    #[serde(default, rename = "noise")]
    pub noise_profiles: Vec<NoiseProfile>,
    /// Inter-field correlation specifications.
    #[serde(default)]
    pub correlations: Vec<Correlation>,
    /// User-defined key-value parameters available to generators.
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
    /// Schema format version (currently `"1.0"`).
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
    /// Persona definitions for human behavioral modeling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub personas: Vec<Persona>,
    /// Actor-to-actor relationship graph specifications.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actor_relationships: Vec<ActorRelationship>,
}

impl Default for DataModel {
    fn default() -> Self {
        Self {
            name: "unnamed".to_string(),
            description: None,
            seed: default_seed(),
            locale: default_locale(),
            timezone: default_timezone(),
            entities: Vec::new(),
            relationships: Vec::new(),
            noise_profiles: Vec::new(),
            correlations: Vec::new(),
            params: BTreeMap::new(),
            schema_version: default_schema_version(),
            personas: Vec::new(),
            actor_relationships: Vec::new(),
        }
    }
}

// ── Entity & Field ───────────────────────────────────────────────────

/// A single entity (analogous to a database table) in the data model.
///
/// Each entity produces one output file/table with `count` rows and the
/// specified `fields`. Constraints and topology are optional refinements.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    /// Unique entity name, used as the table/file name in output.
    pub name: String,
    /// Optional free-text description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// How many rows to generate (fixed, range, or distribution).
    #[serde(default)]
    pub count: CountSpec,
    /// Column definitions for this entity.
    #[serde(default)]
    pub fields: Vec<Field>,
    /// Integrity constraints (unique, check, range) applied after generation.
    #[serde(default)]
    pub constraints: Vec<Constraint>,
    /// Optional graph/tree topology for hierarchical entities.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology: Option<TopologySpec>,
    /// Whether this entity represents an actor (human/person) population.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub actor: bool,
    /// Name of the personas section to use for actor persona assignment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub persona_distribution: Option<String>,
    /// Dynamic row count driven by per-actor activity rates.
    ///
    /// When set, the total row count for this entity is computed as the sum
    /// of each actor's activity trait value (from the actor pool), instead of
    /// using the static `count` field. The `count` field still serves as a
    /// fallback estimate for planning when the actor pool is unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity_count: Option<ActivityCount>,
}

/// A single field (column) within an [`Entity`].
///
/// Each field has a data type, an optional generator that produces values, and
/// a null specification. The `knit-plan` compiler translates the `generator`
/// into a fully resolved `GeneratorPlan` for execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// Column name.
    pub name: String,
    /// Optional free-text description.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Output data type (defaults to `String`).
    #[serde(default = "default_data_type")]
    pub data_type: DataType,
    /// How to generate values for this column. If `None`, the planner infers a
    /// default (e.g. UUID for primary keys, sequence for int PKs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<GeneratorSpec>,
    /// When and how to inject `NULL` values.
    #[serde(default)]
    pub nullable: NullSpec,
    /// Whether this field is the entity's primary key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<bool>,
    /// Number of decimal places for float output (e.g. `2` for currency).
    /// When set, generated float values are rounded to this many decimal places.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precision: Option<u8>,
    /// Whether this field references an actor (human) entity.
    /// Used by behavioral modeling to identify actor columns in non-actor entities.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub actor_column: bool,
}

fn default_data_type() -> DataType {
    DataType::String
}

// ── DataType ─────────────────────────────────────────────────────────

/// Supported column data types.
///
/// Used by the planner and generator to select appropriate output encoding
/// and value ranges.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    /// Boolean `true`/`false`.
    Bool,
    /// Signed 64-bit integer.
    Int,
    /// Signed 32-bit integer.
    #[serde(rename = "int32")]
    Int32,
    /// 64-bit IEEE 754 floating point.
    Float,
    /// Variable-length UTF-8 string.
    String,
    /// Universally unique identifier (v4 by default).
    Uuid,
    /// Calendar date without time (e.g. `2024-01-15`).
    Date,
    /// Time of day without date (e.g. `14:30:00`).
    Time,
    /// Date and time without timezone.
    Datetime,
    /// Date and time without timezone (microsecond precision).
    #[serde(rename = "datetime_us")]
    DatetimeUs,
    /// Date and time with timezone offset.
    Datetimetz,
    /// Time duration / interval.
    Duration,
    /// Raw byte sequence.
    Bytes,
    /// Ordered array of values.
    Array,
    /// String-keyed map of values.
    Map,
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataType::Bool => write!(f, "bool"),
            DataType::Int => write!(f, "int"),
            DataType::Int32 => write!(f, "int32"),
            DataType::Float => write!(f, "float"),
            DataType::String => write!(f, "string"),
            DataType::Uuid => write!(f, "uuid"),
            DataType::Date => write!(f, "date"),
            DataType::Time => write!(f, "time"),
            DataType::Datetime => write!(f, "datetime"),
            DataType::DatetimeUs => write!(f, "datetime_us"),
            DataType::Datetimetz => write!(f, "datetimetz"),
            DataType::Duration => write!(f, "duration"),
            DataType::Bytes => write!(f, "bytes"),
            DataType::Array => write!(f, "array"),
            DataType::Map => write!(f, "map"),
        }
    }
}

// ── GeneratorSpec ────────────────────────────────────────────────────

/// Specifies how to generate values for a field.
///
/// Each variant corresponds to a generation strategy. The `knit-plan` compiler
/// translates supported variants into `GeneratorPlan` entries for `knit-gen`.
///
/// All variants are fully supported in the planner and generator: `Distribution`,
/// `Sequence`, `Uuid`, `OneOf`, `Pattern`, `Ref`, `TimestampRange`, `Expression`,
/// `TimeSeries`, `Unique`, `Relative`, `BusinessHours`, `Faker`, `Conditional`,
/// and `Constant`.
///
/// Tagged with `#[serde(tag = "type")]` so TOML/JSON uses `type = "distribution"`, etc.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GeneratorSpec {
    /// Sample from a statistical distribution (normal, uniform, etc.).
    Distribution {
        /// The distribution specification with parameters.
        #[serde(flatten)]
        spec: DistributionSpec,
    },
    /// Generate structured fake data (names, emails, addresses) via locale-aware faker.
    Faker {
        /// Faker method name (e.g. `"name"`, `"email"`).
        method: String,
        /// Optional arguments passed to the faker method.
        #[serde(default)]
        args: Vec<Value>,
    },
    /// Auto-incrementing or stepped sequence, optionally with a string prefix.
    Sequence {
        /// Initial value of the sequence (default: 0).
        #[serde(default)]
        start: i64,
        /// Increment between consecutive values (default: 1).
        #[serde(default = "default_step")]
        step: i64,
        /// Optional string prefix prepended to each value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
    },
    /// Weighted random choice from a fixed set of values.
    OneOf {
        /// The set of weighted values to choose from.
        choices: Vec<WeightedChoice>,
    },
    /// Regex-like pattern expansion (e.g. `"###-???-AAA"`).
    Pattern {
        /// Regex-like pattern template string.
        pattern: String,
    },
    /// Expression that references other fields in the same entity.
    Derived {
        /// Expression referencing other fields in the same entity.
        expr: String,
    },
    /// Value depends on another field's value via branch conditions.
    Conditional {
        /// Name of the field whose value selects the branch.
        field: String,
        /// Ordered branch conditions and their generators.
        branches: Vec<ConditionalBranch>,
        /// Fallback generator when no branch matches.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Box<GeneratorSpec>>,
    },
    /// Template-based composition of multiple sub-generators.
    Composite {
        /// Template string with placeholders for sub-generators.
        template: String,
        /// Named sub-generators that fill template placeholders.
        #[serde(default)]
        generators: BTreeMap<String, GeneratorSpec>,
    },
    /// Foreign key lookup — copies values from another entity's field.
    Lookup {
        /// Name of the referenced entity.
        entity: String,
        /// Field name on the referenced entity to copy from.
        field: String,
    },
    /// Every row receives the same fixed value.
    Constant {
        /// The fixed value assigned to every row.
        value: Value,
    },
    /// Generate a UUID (v4 by default).
    #[serde(rename = "uuid")]
    UuidGen {
        /// UUID version to generate (default: 4).
        #[serde(default = "default_uuid_version")]
        version: u8,
    },
    /// Wrap an inner generator with uniqueness enforcement via retry.
    Unique {
        /// The wrapped inner generator to enforce uniqueness on.
        inner: Box<GeneratorSpec>,
        /// Maximum retries before accepting a duplicate (default: 1000).
        #[serde(default = "default_max_retries")]
        max_retries: u32,
    },
    /// Value relative to another field (e.g. `end_date = start_date + 7 days`).
    Relative {
        /// Name of the base field to offset from.
        field: String,
        /// Offset value added to the base field.
        offset: Value,
    },
    /// Timestamps constrained to business hours (and optionally weekdays).
    BusinessHours {
        /// Start of the business-hours window (default: 9).
        #[serde(default = "default_start_hour")]
        start_hour: u8,
        /// End of the business-hours window (default: 17).
        #[serde(default = "default_end_hour")]
        end_hour: u8,
        /// Whether to exclude Saturday and Sunday.
        #[serde(default)]
        exclude_weekends: bool,
    },
    /// Sample from an external dictionary file (one value per line).
    ///
    /// When more unique values are needed than the dictionary contains,
    /// the `expansion` strategy determines how to grow the value space.
    Dictionary {
        /// Path to the dictionary file (relative to schema file).
        file: String,
        /// Expansion strategy when dictionary is exhausted.
        ///
        /// - `"sample"` — sample with replacement (duplicates allowed, default)
        /// - `"combinatorial"` — tokenize entries, recombine from positional pools
        /// - `"suffix"` — append numeric suffixes (-001, -002, etc.)
        #[serde(default = "default_expansion")]
        expansion: String,
    },
    /// Reference an actor entity, selecting actors weighted by persona activity rate.
    /// Used in behavioral entities to assign records to actors.
    ActorRef {
        /// Name of the actor entity to reference.
        entity: String,
    },
    /// Generate timestamps following the assigned actor's temporal traits.
    /// Produces timestamps biased toward the actor's peak activity hours/days.
    ActorTemporal {
        /// Name of the persona trait to use for temporal distribution (e.g. `"peak_hours"`).
        #[serde(rename = "trait")]
        trait_name: String,
        /// Optional cross-entity causal constraint: timestamp >= referenced entity's timestamp.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        temporal_after: Option<TemporalAfterSpec>,
        /// Optional burst/session pattern: events cluster into bursts with idle gaps.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        burst: Option<BurstSpec>,
    },
    /// Select target actor based on relationship graph topology.
    /// Used for fields like `receiver_id` where the target depends on the source actor's graph neighbors.
    RelationshipRef {
        /// Name of the actor_relationship to use for edge selection.
        relationship: String,
        /// Name of the source actor field in the same entity (e.g. `"sender_id"`).
        /// If omitted, auto-detected from other actor_column FK fields in the entity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_field: Option<String>,
    },
    /// Generate a field value based on the current actor's persona traits.
    /// The trait value determines the distribution from which to sample.
    PersonaField {
        /// Name of the persona trait that governs this field's generation.
        #[serde(rename = "trait")]
        trait_name: String,
    },
    /// Generate self-referential thread/conversation structure.
    /// Produces nullable int values: NULL = thread starter, non-null = reply to a previous PK.
    ThreadRef {
        /// Probability that a row is a reply (vs. starting a new thread). Range: 0.0–1.0.
        #[serde(default = "default_reply_probability")]
        reply_probability: f64,
        /// Maximum thread depth (prevents infinitely deep chains). Default: 10.
        #[serde(default = "default_max_depth")]
        max_depth: u32,
        /// Size of the "recent messages" window to sample replies from. Default: 100.
        #[serde(default = "default_reply_window")]
        reply_window: usize,
    },
    /// Custom generator supplied by a runtime plugin.
    ///
    /// The plugin is looked up in the global plugin registry
    /// by name at generation time. Parameters are passed as typed key-value pairs.
    Plugin {
        /// Registered plugin name (must match `GeneratorPlugin::name()`).
        name: String,
        /// Arbitrary parameters passed to the plugin's `create()` method.
        #[serde(default)]
        params: BTreeMap<String, Value>,
    },
    /// Sample values from an external data file (CSV, JSON, or Parquet).
    ///
    /// Loads a named column from a structured file and samples values during
    /// generation. Supports uniform, weighted, and sequential sampling modes.
    ExternalLookup {
        /// Path to the data file (relative to the schema file).
        source: String,
        /// Column name to sample values from.
        column: String,
        /// File format: `csv`, `json`, or `parquet`.
        format: LookupFormat,
        /// Sampling strategy (default: uniform random).
        #[serde(default)]
        sampling: SamplingMode,
        /// Column name containing weights (required when `sampling = "weighted"`).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        weight_column: Option<String>,
    },
}

/// File format for external lookup data sources.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LookupFormat {
    /// Comma-separated values with header row.
    Csv,
    /// JSON array of objects.
    Json,
    /// Apache Parquet columnar format.
    Parquet,
}

impl std::fmt::Display for LookupFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Csv => write!(f, "csv"),
            Self::Json => write!(f, "json"),
            Self::Parquet => write!(f, "parquet"),
        }
    }
}

/// Sampling strategy for external lookup generators.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SamplingMode {
    /// Uniform random sampling with replacement.
    #[default]
    Uniform,
    /// Weighted sampling using a weight column.
    Weighted,
    /// Sequential round-robin based on row position.
    Sequential,
}

/// Schema-level specification for cross-entity temporal ordering.
///
/// When attached to an `actor_temporal` generator, ensures the generated
/// timestamp is >= the referenced entity's timestamp (looked up via FK).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TemporalAfterSpec {
    /// Referenced entity name (e.g. `"posts"`).
    pub entity: String,
    /// Timestamp field in the referenced entity (e.g. `"created_at"`).
    pub field: String,
    /// FK field in the current entity that references the parent's PK.
    pub fk: String,
}

/// Burst/session pattern specification for temporal generation.
///
/// When attached to an `actor_temporal` generator, events are clustered
/// into bursts of activity separated by idle periods, creating realistic
/// session-like behavior (e.g., a user posts 5 times in an hour then goes
/// offline for 8 hours).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BurstSpec {
    /// Average number of events per burst session (Poisson-sampled, min 1).
    pub avg_events: f64,
    /// Average gap between events within a burst (in minutes).
    pub avg_gap_minutes: f64,
    /// Average idle time between bursts (in hours).
    pub avg_idle_hours: f64,
}

fn default_step() -> i64 {
    1
}
fn default_expansion() -> String {
    "sample".to_string()
}
fn default_max_retries() -> u32 {
    1000
}
fn default_start_hour() -> u8 {
    9
}
fn default_end_hour() -> u8 {
    17
}
fn default_reply_probability() -> f64 {
    0.6
}
fn default_max_depth() -> u32 {
    10
}
fn default_reply_window() -> usize {
    100
}

// ── DistributionSpec & Kind ──────────────────────────────────────────

/// Configuration for a statistical distribution generator.
///
/// Pairs a [`DistributionKind`] with named parameters (e.g. `mean`, `std_dev`).
/// Parameter requirements vary by distribution; `knit-schema` validates them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistributionSpec {
    /// Which distribution family to sample from.
    pub kind: DistributionKind,
    /// Named numeric parameters (distribution-specific).
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
    /// When true, round sampled values to the nearest integer.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub round: bool,
}

/// Statistical distribution families supported by knit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistributionKind {
    /// Continuous uniform over `[min, max]`.
    Uniform,
    /// Gaussian / bell curve with `mean` and `std_dev`.
    Normal,
    /// Log-normal: `exp(Normal(mean, std_dev))`.
    LogNormal,
    /// Exponential with rate `lambda`.
    Exponential,
    /// Poisson with rate `lambda`.
    Poisson,
    /// Bernoulli trial with probability `p`.
    Bernoulli,
    /// Binomial: `n` trials, each with probability `p`.
    Binomial,
    /// Geometric: trials until first success with probability `p`.
    Geometric,
    /// Pareto with shape `alpha` and scale `x_m`.
    Pareto,
    /// Weibull with shape `k` and scale `lambda`.
    Weibull,
    /// Gamma with `shape` and `scale` parameters.
    Gamma,
    /// Beta on `[0, 1]` with shape parameters `alpha` and `beta`.
    Beta,
    /// Cauchy with location `x0` and scale `gamma`.
    Cauchy,
    /// Chi-squared with `k` degrees of freedom.
    ChiSquared,
    /// Student's t with `nu` degrees of freedom.
    StudentT,
    /// Triangular with `min`, `mode`, and `max`.
    Triangular,
    /// Zipf (power-law) with exponent `s` and `n` elements.
    Zipf,
}

impl std::fmt::Display for DistributionKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DistributionKind::Uniform => write!(f, "uniform"),
            DistributionKind::Normal => write!(f, "normal"),
            DistributionKind::LogNormal => write!(f, "log_normal"),
            DistributionKind::Exponential => write!(f, "exponential"),
            DistributionKind::Poisson => write!(f, "poisson"),
            DistributionKind::Bernoulli => write!(f, "bernoulli"),
            DistributionKind::Binomial => write!(f, "binomial"),
            DistributionKind::Geometric => write!(f, "geometric"),
            DistributionKind::Pareto => write!(f, "pareto"),
            DistributionKind::Weibull => write!(f, "weibull"),
            DistributionKind::Gamma => write!(f, "gamma"),
            DistributionKind::Beta => write!(f, "beta"),
            DistributionKind::Cauchy => write!(f, "cauchy"),
            DistributionKind::ChiSquared => write!(f, "chi_squared"),
            DistributionKind::StudentT => write!(f, "student_t"),
            DistributionKind::Triangular => write!(f, "triangular"),
            DistributionKind::Zipf => write!(f, "zipf"),
        }
    }
}

// ── NullSpec ─────────────────────────────────────────────────────────

/// Controls whether and how `NULL` values are injected into a field.
///
/// Serialized as a bare boolean (`false`/`true`) or an object
/// (`{ "probability": 0.05 }`, `{ "every_n": 10 }`).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum NullSpec {
    /// The field never produces nulls (default).
    #[default]
    Never,
    /// Every value is null.
    Always,
    /// Each value has an independent probability of being null.
    Probability(f64),
    /// Every Nth row is null (deterministic pattern).
    Pattern {
        /// Generate null for every Nth row.
        every_n: u64,
    },
}

impl Serialize for NullSpec {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        match self {
            NullSpec::Never => serializer.serialize_bool(false),
            NullSpec::Always => serializer.serialize_bool(true),
            NullSpec::Probability(p) => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("probability", p)?;
                map.end()
            }
            NullSpec::Pattern { every_n } => {
                let mut map = serializer.serialize_map(Some(1))?;
                map.serialize_entry("every_n", every_n)?;
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for NullSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de;

        struct NullSpecVisitor;

        impl<'de> de::Visitor<'de> for NullSpecVisitor {
            type Value = NullSpec;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a boolean or object for NullSpec")
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<NullSpec, E> {
                Ok(if v { NullSpec::Always } else { NullSpec::Never })
            }

            fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<NullSpec, A::Error> {
                let key: String = map
                    .next_key()?
                    .ok_or_else(|| de::Error::custom("empty object for NullSpec"))?;
                match key.as_str() {
                    "probability" => {
                        let v: f64 = map.next_value()?;
                        Ok(NullSpec::Probability(v))
                    }
                    "every_n" => {
                        let v: u64 = map.next_value()?;
                        Ok(NullSpec::Pattern { every_n: v })
                    }
                    other => Err(de::Error::unknown_field(other, &["probability", "every_n"])),
                }
            }
        }

        deserializer.deserialize_any(NullSpecVisitor)
    }
}

impl std::fmt::Display for NullSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NullSpec::Never => write!(f, "never"),
            NullSpec::Always => write!(f, "always"),
            NullSpec::Probability(p) => write!(f, "probability({p})"),
            NullSpec::Pattern { every_n } => write!(f, "every_{every_n}"),
        }
    }
}

// ── CountSpec ────────────────────────────────────────────────────────

/// Specifies how many rows an entity should produce.
///
/// Deserialized from a bare integer (`1000`), an object (`{ min, max }`),
/// or a distribution spec. The planner resolves this to a concrete count.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CountSpec {
    /// Exact row count.
    Fixed(u64),
    /// Random count drawn uniformly from `[min, max]`.
    Range {
        /// Minimum count (inclusive).
        min: u64,
        /// Maximum count (inclusive).
        max: u64,
    },
    /// Row count sampled from a statistical distribution.
    Distribution(DistributionSpec),
}

impl Default for CountSpec {
    fn default() -> Self {
        CountSpec::Fixed(1000)
    }
}

/// Specification for dynamic, activity-driven row counts.
///
/// When an entity has this set, total rows = Σ(`actor[i].trait_value`) across
/// all actors in the referenced entity's actor pool. The `actor_field`
/// identifies the FK column pointing to the actor entity, and `trait_name`
/// names the persona trait to sum.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActivityCount {
    /// FK field in this entity that references the actor entity (e.g. `"sender_id"`).
    pub actor_field: String,
    /// Persona trait name whose values are summed to determine total rows
    /// (e.g. `"activity_rate"`).
    #[serde(rename = "trait")]
    pub trait_name: String,
}

// ── Relationship ─────────────────────────────────────────────────────

/// A foreign-key or association relationship between two entities.
///
/// Relationships drive the dependency graph in `knit-plan`: the `to` entity
/// must be generated before the `from` entity so foreign keys can reference
/// valid primary keys.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relationship {
    /// Unique relationship name (for merging and diagnostics).
    pub name: String,
    /// Entity that holds the foreign key (the "child" side).
    pub from: String,
    /// Entity whose primary key is referenced (the "parent" side).
    pub to: String,
    /// Cardinality kind (defaults to `OneToMany`).
    #[serde(default)]
    pub kind: RelationshipKind,
    /// Explicit FK column name on the `from` entity. If omitted, defaults to
    /// `"{to}_id"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreign_key: Option<String>,
    /// Optional cardinality count/distribution per parent row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<CountSpec>,
}

/// Cardinality of a [`Relationship`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum RelationshipKind {
    /// Each parent row maps to exactly one child row.
    OneToOne,
    /// Each parent row may have many child rows.
    #[default]
    OneToMany,
    /// Many child rows map to one parent row (inverse of `OneToMany`).
    ManyToOne,
    /// Many-to-many via an implicit junction table.
    ManyToMany,
}

impl std::fmt::Display for RelationshipKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RelationshipKind::OneToOne => write!(f, "one_to_one"),
            RelationshipKind::OneToMany => write!(f, "one_to_many"),
            RelationshipKind::ManyToOne => write!(f, "many_to_one"),
            RelationshipKind::ManyToMany => write!(f, "many_to_many"),
        }
    }
}

// ── NoiseProfile ─────────────────────────────────────────────────────

/// A noise injection profile that degrades generated data to simulate
/// real-world imperfections (nulls, duplicates, typos, outliers).
///
/// Stored as schema metadata and consumed by `knit-noise` perturbators
/// (not `knit-gen` directly). The CLI orchestrates noise application as
/// a post-generation step.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoiseProfile {
    /// Unique profile name (for merging).
    pub name: String,
    /// Target entity name.
    #[serde(default)]
    pub entity: String,
    /// Target field names within the entity (empty = all fields).
    #[serde(default)]
    pub fields: Vec<String>,
    /// Fraction of values to replace with `NULL` (0.0–1.0).
    #[serde(default)]
    pub null_rate: f64,
    /// Fraction of rows to duplicate.
    #[serde(default)]
    pub duplicate_rate: f64,
    /// Fraction of string values to inject typos into.
    #[serde(default)]
    pub typo_rate: f64,
    /// Fraction of numeric values to replace with statistical outliers.
    #[serde(default)]
    pub outlier_rate: f64,
    /// Fraction of rows to swap within each column.
    #[serde(default)]
    pub swap_rate: f64,
    /// Fraction of string values to truncate at random positions.
    #[serde(default)]
    pub truncate_rate: f64,
    /// Fraction of FK values to replace with non-existent references.
    #[serde(default)]
    pub fk_violate_rate: f64,
    /// Fraction of timestamps to cluster around spike points.
    #[serde(default)]
    pub temporal_spike_rate: f64,
    /// Fraction of rows where targeted fields are omitted entirely.
    ///
    /// Only affects document-oriented sinks (JSON/JSONL). For columnar
    /// formats (CSV, Parquet, Avro) this degrades to null injection with a
    /// warning.
    #[serde(default)]
    pub missing_field_rate: f64,
}

// ── Constraint ───────────────────────────────────────────────────────

/// An integrity constraint applied to an entity.
///
/// Constraints are currently used as schema metadata for validation and
/// documentation. Runtime enforcement (e.g. retry-based generation) is
/// planned for a future release.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Constraint {
    /// A combination of fields must be unique across all rows.
    Unique {
        /// Column names that must be unique together.
        fields: Vec<String>,
    },
    /// A boolean expression that every row must satisfy.
    Check {
        /// Boolean expression that every row must satisfy.
        expr: String,
    },
    /// A field's value must fall within `[min, max]` (inclusive).
    Range {
        /// Name of the field to constrain.
        field: String,
        /// Optional inclusive lower bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<Value>,
        /// Optional inclusive upper bound.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<Value>,
    },
}

// ── WeightedChoice ───────────────────────────────────────────────────

/// A value with an associated selection weight, used by [`GeneratorSpec::OneOf`].
///
/// During generation, values are sampled proportionally to their weights.
/// The default weight is `1.0` (uniform).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightedChoice {
    /// The value to emit when this choice is selected.
    pub value: Value,
    /// Relative selection weight (default `1.0`).
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_weight() -> f64 {
    1.0
}

// ── Correlation ──────────────────────────────────────────────────────

/// Specifies inter-field correlations within a single entity.
///
/// Used to generate columns whose values are statistically dependent
/// (e.g. height and weight). The planner converts this into a Cholesky
/// decomposition for correlated sampling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Correlation {
    /// Target entity name.
    pub entity: String,
    /// Ordered list of correlated field names.
    pub fields: Vec<String>,
    /// Correlation matrix (rows/columns correspond to `fields`). Must be
    /// symmetric and positive semi-definite.
    #[serde(default)]
    pub matrix: Vec<Vec<f64>>,
    /// Optional conditional correlations that override the matrix based on
    /// another field's value.
    #[serde(default)]
    pub conditional: Vec<ConditionalCorrelation>,
}

/// Conditional correlation: overrides correlation based on another field's value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionalCorrelation {
    /// Field whose value determines which branch to use.
    pub field: String,
    /// Branches mapping field values to generator overrides.
    pub branches: Vec<ConditionalBranch>,
}

/// A single branch in a conditional generator or conditional correlation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionalBranch {
    /// The value (or pattern) to match against.
    #[serde(rename = "when")]
    pub condition: Value,
    /// Generator to use when the condition matches.
    pub generator: GeneratorSpec,
}

// ── TopologySpec ─────────────────────────────────────────────────────

/// Graph/tree topology specification for hierarchical entity generation.
///
/// When present on an [`Entity`], the generator creates parent-child
/// relationships or graph edges following the specified model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TopologySpec {
    /// Hierarchical tree with bounded depth and branching factor.
    Tree {
        /// Maximum depth of the tree.
        max_depth: u32,
        /// Maximum children per node.
        branching_factor: u32,
    },
    /// Directed acyclic graph with bounded depth and maximum parents per node.
    Dag {
        /// Maximum depth of the DAG.
        max_depth: u32,
        /// Maximum parent nodes per child.
        max_parents: u32,
    },
    /// Random graph where each possible edge exists with the given probability.
    Graph {
        /// Probability that any given edge exists.
        edge_probability: f64,
    },
}

// ── DateRange ────────────────────────────────────────────────────────

/// A date range with ISO 8601 string boundaries (e.g. `"2020-01-01"`).
///
/// Used as a parameter for temporal generators.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DateRange {
    /// Start date (inclusive).
    pub start: String,
    /// End date (exclusive).
    pub end: String,
}

// ── Persona ──────────────────────────────────────────────────────────

/// A behavioral persona (archetype) for human behavioral modeling.
///
/// Personas represent clusters of actors with similar behavioral traits.
/// During generation, each synthetic actor is assigned a persona, and their
/// behavior is sampled from the persona's trait distributions.
///
/// ```toml
/// [[personas]]
/// name = "power_user"
/// weight = 0.15
/// traits.peak_hours = [9, 10, 11, 14, 15, 16]
/// traits.active_days = "weekday_heavy"
/// traits.activity_rate = { kind = "normal", params = { mean = 25.0, std_dev = 8.0 } }
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Persona {
    /// Unique persona name (e.g. `"power_user"`, `"casual_browser"`).
    pub name: String,
    /// Fraction of the actor population assigned this persona (0.0–1.0).
    /// Weights across all personas in a group should sum to 1.0.
    pub weight: f64,
    /// Behavioral trait specifications. Keys are trait names (e.g.
    /// `"peak_hours"`, `"activity_rate"`), values are trait definitions
    /// (arrays, distributions, or scalar values).
    #[serde(default)]
    pub traits: BTreeMap<String, Value>,
}

// ── ActorRelationship ────────────────────────────────────────────────

/// Specifies the topology of actor-to-actor relationships.
///
/// Used for behavioral modeling: defines how actors are connected (e.g.
/// manager→report, sender→receiver) and the statistical properties of
/// the resulting social graph.
///
/// ```toml
/// [[actor_relationships]]
/// name = "email_network"
/// from_entity = "users"
/// to_entity = "users"
/// graph_type = "scale_free"
/// params.avg_degree = 8.0
/// params.reciprocity = 0.4
/// params.clustering = 0.3
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActorRelationship {
    /// Unique relationship name (e.g. `"email_network"`, `"reports_to"`).
    pub name: String,
    /// Source actor entity name.
    pub from_entity: String,
    /// Target actor entity name (may be same as `from_entity` for self-referential graphs).
    pub to_entity: String,
    /// Graph generation model.
    #[serde(default)]
    pub graph_type: GraphType,
    /// Model-specific parameters (e.g. `avg_degree`, `reciprocity`, `clustering`).
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
    /// Number of communities/sub-groups to generate within the graph.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub community_count: Option<CountSpec>,
    /// Maximum hierarchy depth for hierarchical graph types.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hierarchy_depth: Option<u32>,
}

/// Graph generation model for actor relationship networks.
///
/// Each model produces graphs with different structural properties
/// (degree distributions, clustering, hierarchy).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GraphType {
    /// Barabási–Albert preferential attachment (power-law degree distribution).
    /// Produces hub-and-spoke networks typical of social media.
    #[default]
    ScaleFree,
    /// Watts–Strogatz small-world model (high clustering, short path lengths).
    /// Produces networks typical of real-world social connections.
    SmallWorld,
    /// Tree-like structure with lateral connections (org charts, management hierarchies).
    Hierarchical,
    /// Erdős–Rényi random graph (baseline; each edge exists with equal probability).
    ErdosRenyi,
    /// User-defined degree sequence for custom graph structures.
    Custom,
}

impl std::fmt::Display for GraphType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GraphType::ScaleFree => write!(f, "scale_free"),
            GraphType::SmallWorld => write!(f, "small_world"),
            GraphType::Hierarchical => write!(f, "hierarchical"),
            GraphType::ErdosRenyi => write!(f, "erdos_renyi"),
            GraphType::Custom => write!(f, "custom"),
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_data_type_serde() {
        let cases = vec![
            (DataType::Bool, "\"bool\""),
            (DataType::Int, "\"int\""),
            (DataType::Float, "\"float\""),
            (DataType::String, "\"string\""),
            (DataType::Uuid, "\"uuid\""),
            (DataType::Date, "\"date\""),
            (DataType::Time, "\"time\""),
            (DataType::Datetime, "\"datetime\""),
            (DataType::Datetimetz, "\"datetimetz\""),
            (DataType::Duration, "\"duration\""),
            (DataType::Bytes, "\"bytes\""),
            (DataType::Array, "\"array\""),
            (DataType::Map, "\"map\""),
        ];
        for (dt, expected) in cases {
            let json = serde_json::to_string(&dt).unwrap();
            assert_eq!(json, expected, "serialize {dt}");
            let back: DataType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, dt, "roundtrip {dt}");
        }
    }

    #[test]
    fn test_distribution_kind_serde() {
        let kinds = vec![
            (DistributionKind::Uniform, "\"uniform\""),
            (DistributionKind::Normal, "\"normal\""),
            (DistributionKind::LogNormal, "\"log_normal\""),
            (DistributionKind::Exponential, "\"exponential\""),
            (DistributionKind::Zipf, "\"zipf\""),
        ];
        for (kind, expected) in kinds {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, expected);
            let back: DistributionKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn test_null_spec_default() {
        assert_eq!(NullSpec::default(), NullSpec::Never);
    }

    #[test]
    fn test_null_spec_serde_bool() {
        let always: NullSpec = serde_json::from_str("true").unwrap();
        assert_eq!(always, NullSpec::Always);
        let never: NullSpec = serde_json::from_str("false").unwrap();
        assert_eq!(never, NullSpec::Never);
    }

    #[test]
    fn test_null_spec_serde_object() {
        let prob: NullSpec = serde_json::from_str(r#"{"probability": 0.05}"#).unwrap();
        assert_eq!(prob, NullSpec::Probability(0.05));
        let pattern: NullSpec = serde_json::from_str(r#"{"every_n": 5}"#).unwrap();
        assert_eq!(pattern, NullSpec::Pattern { every_n: 5 });
    }

    #[test]
    fn test_count_spec_serde() {
        let fixed: CountSpec = serde_json::from_str("100000").unwrap();
        assert_eq!(fixed, CountSpec::Fixed(100000));

        let range: CountSpec = serde_json::from_str(r#"{"min": 10, "max": 50}"#).unwrap();
        assert_eq!(range, CountSpec::Range { min: 10, max: 50 });

        let dist: CountSpec = serde_json::from_str(
            r#"{"kind": "normal", "params": {"mean": 100.0, "std_dev": 10.0}}"#,
        )
        .unwrap();
        assert!(matches!(dist, CountSpec::Distribution(_)));
    }

    #[test]
    fn test_generator_spec_distribution() {
        let json = r#"{
            "type": "distribution",
            "kind": "normal",
            "params": {"mean": 50.0, "std_dev": 10.0}
        }"#;
        let gen: GeneratorSpec = serde_json::from_str(json).unwrap();
        match gen {
            GeneratorSpec::Distribution { spec } => {
                assert_eq!(spec.kind, DistributionKind::Normal);
                assert_eq!(spec.params["mean"], 50.0);
            }
            _ => panic!("expected Distribution"),
        }
    }

    #[test]
    fn test_generator_spec_faker() {
        let json = r#"{"type": "faker", "method": "name.first_name", "args": []}"#;
        let gen: GeneratorSpec = serde_json::from_str(json).unwrap();
        assert!(matches!(gen, GeneratorSpec::Faker { .. }));
    }

    #[test]
    fn test_generator_spec_sequence() {
        let json = r#"{"type": "sequence", "start": 1, "step": 1, "prefix": "ORD-"}"#;
        let gen: GeneratorSpec = serde_json::from_str(json).unwrap();
        match gen {
            GeneratorSpec::Sequence {
                start,
                step,
                prefix,
            } => {
                assert_eq!(start, 1);
                assert_eq!(step, 1);
                assert_eq!(prefix, Some("ORD-".into()));
            }
            _ => panic!("expected Sequence"),
        }
    }

    #[test]
    fn test_generator_spec_one_of() {
        let json = r#"{
            "type": "one_of",
            "choices": [
                {"value": "active", "weight": 0.7},
                {"value": "inactive", "weight": 0.3}
            ]
        }"#;
        let gen: GeneratorSpec = serde_json::from_str(json).unwrap();
        match gen {
            GeneratorSpec::OneOf { choices } => {
                assert_eq!(choices.len(), 2);
                assert_eq!(choices[0].value, Value::String("active".into()));
            }
            _ => panic!("expected OneOf"),
        }
    }

    #[test]
    fn test_generator_spec_derived() {
        let json = r#"{"type": "derived", "expr": "first_name + ' ' + last_name"}"#;
        let gen: GeneratorSpec = serde_json::from_str(json).unwrap();
        assert!(matches!(gen, GeneratorSpec::Derived { .. }));
    }

    #[test]
    fn test_value_serde() {
        let cases: Vec<(&str, Value)> = vec![
            ("null", Value::Null),
            ("true", Value::Bool(true)),
            ("42", Value::Int(42)),
            ("2.72", Value::Float(2.72)),
            ("\"hello\"", Value::String("hello".into())),
            (
                "[1, 2, 3]",
                Value::Array(vec![Value::Int(1), Value::Int(2), Value::Int(3)]),
            ),
        ];
        for (json, expected) in cases {
            let parsed: Value = serde_json::from_str(json).unwrap();
            assert_eq!(parsed, expected, "parsing {json}");
            let roundtrip = serde_json::to_string(&parsed).unwrap();
            let back: Value = serde_json::from_str(&roundtrip).unwrap();
            assert_eq!(back, expected, "roundtrip {json}");
        }
    }

    #[test]
    fn test_display_impls() {
        assert_eq!(DataType::Bool.to_string(), "bool");
        assert_eq!(DataType::Datetimetz.to_string(), "datetimetz");
        assert_eq!(DistributionKind::Normal.to_string(), "normal");
        assert_eq!(DistributionKind::LogNormal.to_string(), "log_normal");
        assert_eq!(RelationshipKind::OneToMany.to_string(), "one_to_many");
        assert_eq!(RelationshipKind::ManyToMany.to_string(), "many_to_many");
        assert_eq!(NullSpec::Never.to_string(), "never");
        assert_eq!(NullSpec::Probability(0.1).to_string(), "probability(0.1)");
    }

    #[test]
    fn test_data_model_toml_roundtrip() {
        let model = DataModel {
            name: "test_model".into(),
            description: Some("A test model".into()),
            seed: 42,
            locale: "en_US".into(),
            timezone: "UTC".into(),
            entities: vec![Entity {
                name: "users".into(),
                description: None,
                count: CountSpec::Fixed(1000),
                fields: vec![
                    Field {
                        name: "id".into(),
                        description: None,
                        data_type: DataType::Int,
                        generator: Some(GeneratorSpec::Sequence {
                            start: 1,
                            step: 1,
                            prefix: None,
                        }),
                        nullable: NullSpec::Never,
                        primary_key: Some(true),
                        precision: None,
                        actor_column: false,
                    },
                    Field {
                        name: "email".into(),
                        description: Some("User email".into()),
                        data_type: DataType::String,
                        generator: Some(GeneratorSpec::Faker {
                            method: "internet.email".into(),
                            args: vec![],
                        }),
                        nullable: NullSpec::Probability(0.01),
                        primary_key: None,
                        precision: None,
                        actor_column: false,
                    },
                ],
                constraints: vec![Constraint::Unique {
                    fields: vec!["email".into()],
                }],
                topology: None,
                actor: false,
                persona_distribution: None,
                activity_count: None,
            }],
            relationships: vec![],
            noise_profiles: vec![NoiseProfile {
                name: "light".into(),
                entity: "users".into(),
                fields: vec!["email".into()],
                null_rate: 0.01,
                duplicate_rate: 0.0,
                typo_rate: 0.005,
                outlier_rate: 0.0,
                swap_rate: 0.0,
                truncate_rate: 0.0,
                fk_violate_rate: 0.0,
                temporal_spike_rate: 0.0,
                missing_field_rate: 0.0,
            }],
            correlations: vec![],
            params: BTreeMap::new(),
            schema_version: "1.0".into(),
            personas: Vec::new(),
            actor_relationships: Vec::new(),
        };

        let toml_str = toml::to_string_pretty(&model).unwrap();
        let back: DataModel = toml::from_str(&toml_str).unwrap();
        assert_eq!(model.name, back.name);
        assert_eq!(model.entities.len(), back.entities.len());
        assert_eq!(
            model.entities[0].fields.len(),
            back.entities[0].fields.len()
        );
        assert_eq!(model.noise_profiles.len(), back.noise_profiles.len());
    }

    #[test]
    fn test_relationship_kind_serde() {
        let kinds = vec![
            (RelationshipKind::OneToOne, "\"one_to_one\""),
            (RelationshipKind::OneToMany, "\"one_to_many\""),
            (RelationshipKind::ManyToOne, "\"many_to_one\""),
            (RelationshipKind::ManyToMany, "\"many_to_many\""),
        ];
        for (kind, expected) in kinds {
            let json = serde_json::to_string(&kind).unwrap();
            assert_eq!(json, expected);
            let back: RelationshipKind = serde_json::from_str(&json).unwrap();
            assert_eq!(back, kind);
        }
    }

    #[test]
    fn test_persona_serde() {
        let persona = Persona {
            name: "power_user".into(),
            weight: 0.15,
            traits: BTreeMap::from([
                (
                    "peak_hours".into(),
                    Value::Array(vec![Value::Int(9), Value::Int(10)]),
                ),
                ("active_days".into(), Value::String("weekday_heavy".into())),
            ]),
        };
        let json = serde_json::to_string(&persona).unwrap();
        let back: Persona = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "power_user");
        assert_eq!(back.weight, 0.15);
        assert_eq!(back.traits.len(), 2);
    }

    #[test]
    fn test_graph_type_serde() {
        let cases = vec![
            (GraphType::ScaleFree, "\"scale_free\""),
            (GraphType::SmallWorld, "\"small_world\""),
            (GraphType::Hierarchical, "\"hierarchical\""),
            (GraphType::ErdosRenyi, "\"erdos_renyi\""),
            (GraphType::Custom, "\"custom\""),
        ];
        for (gt, expected) in cases {
            let json = serde_json::to_string(&gt).unwrap();
            assert_eq!(json, expected, "serialize {:?}", gt);
            let back: GraphType = serde_json::from_str(&json).unwrap();
            assert_eq!(back, gt, "roundtrip {:?}", gt);
        }
    }

    #[test]
    fn test_graph_type_display() {
        assert_eq!(GraphType::ScaleFree.to_string(), "scale_free");
        assert_eq!(GraphType::SmallWorld.to_string(), "small_world");
        assert_eq!(GraphType::Hierarchical.to_string(), "hierarchical");
        assert_eq!(GraphType::ErdosRenyi.to_string(), "erdos_renyi");
        assert_eq!(GraphType::Custom.to_string(), "custom");
    }

    #[test]
    fn test_actor_relationship_serde() {
        let ar = ActorRelationship {
            name: "email_network".into(),
            from_entity: "users".into(),
            to_entity: "users".into(),
            graph_type: GraphType::ScaleFree,
            params: BTreeMap::from([("avg_degree".into(), 8.0), ("reciprocity".into(), 0.4)]),
            community_count: Some(CountSpec::Fixed(5)),
            hierarchy_depth: Some(3),
        };
        let json = serde_json::to_string(&ar).unwrap();
        let back: ActorRelationship = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "email_network");
        assert_eq!(back.graph_type, GraphType::ScaleFree);
        assert_eq!(back.params["avg_degree"], 8.0);
        assert_eq!(back.community_count, Some(CountSpec::Fixed(5)));
        assert_eq!(back.hierarchy_depth, Some(3));
    }

    #[test]
    fn test_actor_generators_serde() {
        let specs = vec![
            (
                GeneratorSpec::ActorRef {
                    entity: "users".into(),
                },
                r#"{"type":"actor_ref","entity":"users"}"#,
            ),
            (
                GeneratorSpec::ActorTemporal {
                    trait_name: "peak_hours".into(),
                    temporal_after: None,
                    burst: None,
                },
                r#"{"type":"actor_temporal","trait":"peak_hours"}"#,
            ),
            (
                GeneratorSpec::RelationshipRef {
                    relationship: "email_net".into(),
                    source_field: None,
                },
                r#"{"type":"relationship_ref","relationship":"email_net"}"#,
            ),
            (
                GeneratorSpec::PersonaField {
                    trait_name: "activity_rate".into(),
                },
                r#"{"type":"persona_field","trait":"activity_rate"}"#,
            ),
        ];
        for (spec, expected) in specs {
            let json = serde_json::to_string(&spec).unwrap();
            assert_eq!(json, expected);
            let back: GeneratorSpec = serde_json::from_str(&json).unwrap();
            assert_eq!(back, spec);
        }
    }

    #[test]
    fn test_entity_actor_fields() {
        let entity = Entity {
            name: "users".into(),
            description: None,
            count: CountSpec::Fixed(1000),
            fields: vec![],
            constraints: vec![],
            topology: None,
            actor: true,
            persona_distribution: Some("personas".into()),
            activity_count: None,
        };
        let json = serde_json::to_string(&entity).unwrap();
        assert!(json.contains("\"actor\":true"));
        assert!(json.contains("\"persona_distribution\":\"personas\""));
        let back: Entity = serde_json::from_str(&json).unwrap();
        assert!(back.actor);
        assert_eq!(back.persona_distribution, Some("personas".into()));
    }

    #[test]
    fn test_field_actor_column() {
        let field = Field {
            name: "sender_id".into(),
            description: None,
            data_type: DataType::Uuid,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: true,
        };
        let json = serde_json::to_string(&field).unwrap();
        assert!(json.contains("\"actor_column\":true"));
        let back: Field = serde_json::from_str(&json).unwrap();
        assert!(back.actor_column);
    }

    #[test]
    fn test_persona_toml_roundtrip() {
        let toml_str = r#"
[[personas]]
name = "early_bird"
weight = 0.3

[personas.traits]
peak_hours = [6, 7, 8, 9]
active_days = "weekday_heavy"

[[personas]]
name = "night_owl"
weight = 0.7

[personas.traits]
peak_hours = [20, 21, 22, 23]
active_days = "uniform"
"#;
        #[derive(Deserialize)]
        struct Wrapper {
            personas: Vec<Persona>,
        }
        let w: Wrapper = toml::from_str(toml_str).unwrap();
        assert_eq!(w.personas.len(), 2);
        assert_eq!(w.personas[0].name, "early_bird");
        assert_eq!(w.personas[0].weight, 0.3);
        assert_eq!(w.personas[1].name, "night_owl");
        assert_eq!(w.personas[1].weight, 0.7);
    }
}
