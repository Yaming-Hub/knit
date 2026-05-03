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
            DataType::Float => write!(f, "float"),
            DataType::String => write!(f, "string"),
            DataType::Uuid => write!(f, "uuid"),
            DataType::Date => write!(f, "date"),
            DataType::Time => write!(f, "time"),
            DataType::Datetime => write!(f, "datetime"),
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
/// **Note:** Some variants (`Conditional`, `Relative`,
/// `BusinessHours`) are not yet implemented in the planner/generator and
/// currently produce null placeholder output. Fully supported variants include
/// `Distribution`, `Sequence`, `Uuid`, `OneOf`, `Pattern`, `Ref`, `TimestampRange`,
/// `Expression`, `TimeSeries`, `Unique`, and `Constant`.
///
/// Tagged with `#[serde(tag = "type")]` so TOML/JSON uses `type = "distribution"`, etc.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GeneratorSpec {
    /// Sample from a statistical distribution (normal, uniform, etc.).
    Distribution {
        #[serde(flatten)]
        spec: DistributionSpec,
    },
    /// Generate structured fake data (names, emails, addresses) via locale-aware faker.
    Faker {
        method: String,
        #[serde(default)]
        args: Vec<Value>,
    },
    /// Auto-incrementing or stepped sequence, optionally with a string prefix.
    Sequence {
        #[serde(default)]
        start: i64,
        #[serde(default = "default_step")]
        step: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
    },
    /// Weighted random choice from a fixed set of values.
    OneOf {
        choices: Vec<WeightedChoice>,
    },
    /// Regex-like pattern expansion (e.g. `"###-???-AAA"`).
    Pattern {
        pattern: String,
    },
    /// Expression that references other fields in the same entity.
    Derived {
        expr: String,
    },
    /// Value depends on another field's value via branch conditions.
    Conditional {
        field: String,
        branches: Vec<ConditionalBranch>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Box<GeneratorSpec>>,
    },
    /// Template-based composition of multiple sub-generators.
    Composite {
        template: String,
        #[serde(default)]
        generators: BTreeMap<String, GeneratorSpec>,
    },
    /// Foreign key lookup — copies values from another entity's field.
    Lookup {
        entity: String,
        field: String,
    },
    /// Every row receives the same fixed value.
    Constant {
        value: Value,
    },
    /// Generate a UUID (v4 by default).
    #[serde(rename = "uuid")]
    UuidGen {
        #[serde(default = "default_uuid_version")]
        version: u8,
    },
    /// Wrap an inner generator with uniqueness enforcement via retry.
    Unique {
        inner: Box<GeneratorSpec>,
        #[serde(default = "default_max_retries")]
        max_retries: u32,
    },
    /// Value relative to another field (e.g. `end_date = start_date + 7 days`).
    Relative {
        field: String,
        offset: Value,
    },
    /// Timestamps constrained to business hours (and optionally weekdays).
    BusinessHours {
        #[serde(default = "default_start_hour")]
        start_hour: u8,
        #[serde(default = "default_end_hour")]
        end_hour: u8,
        #[serde(default)]
        exclude_weekends: bool,
    },
}

fn default_step() -> i64 {
    1
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
    /// Gamma with shape `alpha` and rate `beta`.
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
#[derive(Debug, Clone, PartialEq)]
pub enum NullSpec {
    /// The field never produces nulls (default).
    Never,
    /// Every value is null.
    Always,
    /// Each value has an independent probability of being null.
    Probability(f64),
    /// Every Nth row is null (deterministic pattern).
    Pattern { every_n: u64 },
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

impl Default for NullSpec {
    fn default() -> Self {
        NullSpec::Never
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
    Range { min: u64, max: u64 },
    /// Row count sampled from a statistical distribution.
    Distribution(DistributionSpec),
}

impl Default for CountSpec {
    fn default() -> Self {
        CountSpec::Fixed(1000)
    }
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
pub enum RelationshipKind {
    /// Each parent row maps to exactly one child row.
    OneToOne,
    /// Each parent row may have many child rows.
    OneToMany,
    /// Many child rows map to one parent row (inverse of `OneToMany`).
    ManyToOne,
    /// Many-to-many via an implicit junction table.
    ManyToMany,
}

impl Default for RelationshipKind {
    fn default() -> Self {
        RelationshipKind::OneToMany
    }
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
        fields: Vec<String>,
    },
    /// A boolean expression that every row must satisfy.
    Check {
        expr: String,
    },
    /// A field's value must fall within `[min, max]` (inclusive).
    Range {
        field: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<Value>,
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
    Tree { max_depth: u32, branching_factor: u32 },
    /// Directed acyclic graph with bounded depth and maximum parents per node.
    Dag { max_depth: u32, max_parents: u32 },
    /// Random graph where each possible edge exists with the given probability.
    Graph { edge_probability: f64 },
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
            GeneratorSpec::Sequence { start, step, prefix } => {
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
            ("3.14", Value::Float(3.14)),
            ("\"hello\"", Value::String("hello".into())),
            ("[1, 2, 3]", Value::Array(vec![
                Value::Int(1), Value::Int(2), Value::Int(3),
            ])),
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
                    },
                ],
                constraints: vec![Constraint::Unique {
                    fields: vec!["email".into()],
                }],
                topology: None,
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
            }],
            correlations: vec![],
            params: BTreeMap::new(),
            schema_version: "1.0".into(),
        };

        let toml_str = toml::to_string_pretty(&model).unwrap();
        let back: DataModel = toml::from_str(&toml_str).unwrap();
        assert_eq!(model.name, back.name);
        assert_eq!(model.entities.len(), back.entities.len());
        assert_eq!(model.entities[0].fields.len(), back.entities[0].fields.len());
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
}
