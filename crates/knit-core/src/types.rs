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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Array(Vec<Value>),
    Map(BTreeMap<String, Value>),
}

// ── DataModel ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DataModel {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_seed")]
    pub seed: u64,
    #[serde(default = "default_locale")]
    pub locale: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub entities: Vec<Entity>,
    #[serde(default)]
    pub relationships: Vec<Relationship>,
    #[serde(default, rename = "noise")]
    pub noise_profiles: Vec<NoiseProfile>,
    #[serde(default)]
    pub correlations: Vec<Correlation>,
    #[serde(default)]
    pub params: BTreeMap<String, Value>,
    #[serde(default = "default_schema_version")]
    pub schema_version: String,
}

// ── Entity & Field ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entity {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default)]
    pub count: CountSpec,
    #[serde(default)]
    pub fields: Vec<Field>,
    #[serde(default)]
    pub constraints: Vec<Constraint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topology: Option<TopologySpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default = "default_data_type")]
    pub data_type: DataType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<GeneratorSpec>,
    #[serde(default)]
    pub nullable: NullSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_key: Option<bool>,
}

fn default_data_type() -> DataType {
    DataType::String
}

// ── DataType ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataType {
    Bool,
    Int,
    Float,
    String,
    Uuid,
    Date,
    Time,
    Datetime,
    Datetimetz,
    Duration,
    Bytes,
    Array,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum GeneratorSpec {
    Distribution {
        #[serde(flatten)]
        spec: DistributionSpec,
    },
    Faker {
        method: String,
        #[serde(default)]
        args: Vec<Value>,
    },
    Sequence {
        #[serde(default)]
        start: i64,
        #[serde(default = "default_step")]
        step: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        prefix: Option<String>,
    },
    OneOf {
        choices: Vec<WeightedChoice>,
    },
    Pattern {
        pattern: String,
    },
    Derived {
        expr: String,
    },
    Conditional {
        field: String,
        branches: Vec<ConditionalBranch>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        default: Option<Box<GeneratorSpec>>,
    },
    Composite {
        template: String,
        #[serde(default)]
        generators: BTreeMap<String, GeneratorSpec>,
    },
    Lookup {
        entity: String,
        field: String,
    },
    Constant {
        value: Value,
    },
    #[serde(rename = "uuid")]
    UuidGen {
        #[serde(default = "default_uuid_version")]
        version: u8,
    },
    Unique {
        inner: Box<GeneratorSpec>,
        #[serde(default = "default_max_retries")]
        max_retries: u32,
    },
    Relative {
        field: String,
        offset: Value,
    },
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DistributionSpec {
    pub kind: DistributionKind,
    #[serde(default)]
    pub params: BTreeMap<String, f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DistributionKind {
    Uniform,
    Normal,
    LogNormal,
    Exponential,
    Poisson,
    Bernoulli,
    Binomial,
    Geometric,
    Pareto,
    Weibull,
    Gamma,
    Beta,
    Cauchy,
    ChiSquared,
    StudentT,
    Triangular,
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

#[derive(Debug, Clone, PartialEq)]
pub enum NullSpec {
    Never,
    Always,
    Probability(f64),
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CountSpec {
    Fixed(u64),
    Range { min: u64, max: u64 },
    Distribution(DistributionSpec),
}

impl Default for CountSpec {
    fn default() -> Self {
        CountSpec::Fixed(1000)
    }
}

// ── Relationship ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Relationship {
    pub name: String,
    pub from: String,
    pub to: String,
    #[serde(default)]
    pub kind: RelationshipKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub foreign_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cardinality: Option<CountSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipKind {
    OneToOne,
    OneToMany,
    ManyToOne,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoiseProfile {
    pub name: String,
    #[serde(default)]
    pub entity: String,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub null_rate: f64,
    #[serde(default)]
    pub duplicate_rate: f64,
    #[serde(default)]
    pub typo_rate: f64,
    #[serde(default)]
    pub outlier_rate: f64,
}

// ── Constraint ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Constraint {
    Unique {
        fields: Vec<String>,
    },
    Check {
        expr: String,
    },
    Range {
        field: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<Value>,
    },
}

// ── WeightedChoice ───────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WeightedChoice {
    pub value: Value,
    #[serde(default = "default_weight")]
    pub weight: f64,
}

fn default_weight() -> f64 {
    1.0
}

// ── Correlation ──────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Correlation {
    pub entity: String,
    pub fields: Vec<String>,
    #[serde(default)]
    pub matrix: Vec<Vec<f64>>,
    #[serde(default)]
    pub conditional: Vec<ConditionalCorrelation>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionalCorrelation {
    pub field: String,
    pub branches: Vec<ConditionalBranch>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConditionalBranch {
    #[serde(rename = "when")]
    pub condition: Value,
    pub generator: GeneratorSpec,
}

// ── TopologySpec ─────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TopologySpec {
    Tree { max_depth: u32, branching_factor: u32 },
    Dag { max_depth: u32, max_parents: u32 },
    Graph { edge_probability: f64 },
}

// ── DateRange ────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DateRange {
    pub start: String,
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
