//! Unit tests for knit-core types: serde round-trips, Display impls, custom
//! deserialization, and error formatting.

use std::collections::BTreeMap;

use crate::types::*;
use crate::error::ModelError;

// ── Value serde ─────────────────────────────────────────────────────

#[test]
fn value_null_json_roundtrip() {
    let v = Value::Null;
    let json = serde_json::to_string(&v).unwrap();
    assert_eq!(json, "null");
    let back: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(back, v);
}

#[test]
fn value_bool_json_roundtrip() {
    let v = Value::Bool(true);
    let json = serde_json::to_string(&v).unwrap();
    assert_eq!(json, "true");
    let back: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(back, v);
}

#[test]
fn value_int_json_roundtrip() {
    let v = Value::Int(42);
    let json = serde_json::to_string(&v).unwrap();
    assert_eq!(json, "42");
    let back: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(back, v);
}

#[test]
fn value_float_json_roundtrip() {
    let v = Value::Float(1.234);
    let json = serde_json::to_string(&v).unwrap();
    let back: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(back, v);
}

#[test]
fn value_string_json_roundtrip() {
    let v = Value::String("hello world".into());
    let json = serde_json::to_string(&v).unwrap();
    assert_eq!(json, r#""hello world""#);
    let back: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(back, v);
}

#[test]
fn value_array_json_roundtrip() {
    let v = Value::Array(vec![Value::Int(1), Value::String("two".into())]);
    let json = serde_json::to_string(&v).unwrap();
    let back: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(back, v);
}

#[test]
fn value_map_json_roundtrip() {
    let mut map = BTreeMap::new();
    map.insert("key".into(), Value::Int(99));
    let v = Value::Map(map);
    let json = serde_json::to_string(&v).unwrap();
    let back: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(back, v);
}

// ── NullSpec custom serde ───────────────────────────────────────────

#[test]
fn null_spec_never_from_false() {
    let ns: NullSpec = serde_json::from_str("false").unwrap();
    assert_eq!(ns, NullSpec::Never);
}

#[test]
fn null_spec_always_from_true() {
    let ns: NullSpec = serde_json::from_str("true").unwrap();
    assert_eq!(ns, NullSpec::Always);
}

#[test]
fn null_spec_probability_from_object() {
    let ns: NullSpec = serde_json::from_str(r#"{"probability": 0.25}"#).unwrap();
    assert_eq!(ns, NullSpec::Probability(0.25));
}

#[test]
fn null_spec_pattern_from_object() {
    let ns: NullSpec = serde_json::from_str(r#"{"every_n": 10}"#).unwrap();
    assert_eq!(ns, NullSpec::Pattern { every_n: 10 });
}

#[test]
fn null_spec_serialize_never() {
    let json = serde_json::to_string(&NullSpec::Never).unwrap();
    assert_eq!(json, "false");
}

#[test]
fn null_spec_serialize_always() {
    let json = serde_json::to_string(&NullSpec::Always).unwrap();
    assert_eq!(json, "true");
}

#[test]
fn null_spec_serialize_probability() {
    let json = serde_json::to_string(&NullSpec::Probability(0.5)).unwrap();
    assert_eq!(json, r#"{"probability":0.5}"#);
}

#[test]
fn null_spec_serialize_pattern() {
    let json = serde_json::to_string(&NullSpec::Pattern { every_n: 7 }).unwrap();
    assert_eq!(json, r#"{"every_n":7}"#);
}

#[test]
fn null_spec_unknown_field_error() {
    let result: Result<NullSpec, _> = serde_json::from_str(r#"{"bad_key": 1}"#);
    assert!(result.is_err());
}

// ── NullSpec Display ────────────────────────────────────────────────

#[test]
fn null_spec_display() {
    assert_eq!(NullSpec::Never.to_string(), "never");
    assert_eq!(NullSpec::Always.to_string(), "always");
    assert_eq!(NullSpec::Probability(0.1).to_string(), "probability(0.1)");
    assert_eq!(
        NullSpec::Pattern { every_n: 5 }.to_string(),
        "every_5"
    );
}

// ── DataType Display ────────────────────────────────────────────────

#[test]
fn data_type_display() {
    assert_eq!(DataType::Bool.to_string(), "bool");
    assert_eq!(DataType::Int.to_string(), "int");
    assert_eq!(DataType::Float.to_string(), "float");
    assert_eq!(DataType::String.to_string(), "string");
    assert_eq!(DataType::Uuid.to_string(), "uuid");
    assert_eq!(DataType::Date.to_string(), "date");
    assert_eq!(DataType::Time.to_string(), "time");
    assert_eq!(DataType::Datetime.to_string(), "datetime");
    assert_eq!(DataType::Datetimetz.to_string(), "datetimetz");
    assert_eq!(DataType::Duration.to_string(), "duration");
    assert_eq!(DataType::Bytes.to_string(), "bytes");
    assert_eq!(DataType::Array.to_string(), "array");
    assert_eq!(DataType::Map.to_string(), "map");
}

// ── DataType serde ──────────────────────────────────────────────────

#[test]
fn data_type_serde_roundtrip() {
    for dt in [
        DataType::Bool,
        DataType::Int,
        DataType::Float,
        DataType::String,
        DataType::Uuid,
        DataType::Date,
        DataType::Datetime,
    ] {
        let json = serde_json::to_string(&dt).unwrap();
        let back: DataType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dt, "failed for {:?}", dt);
    }
}

// ── DistributionKind Display + serde ────────────────────────────────

#[test]
fn distribution_kind_display() {
    assert_eq!(DistributionKind::Normal.to_string(), "normal");
    assert_eq!(DistributionKind::Uniform.to_string(), "uniform");
    assert_eq!(DistributionKind::Poisson.to_string(), "poisson");
    assert_eq!(DistributionKind::Bernoulli.to_string(), "bernoulli");
    assert_eq!(DistributionKind::Zipf.to_string(), "zipf");
    assert_eq!(DistributionKind::LogNormal.to_string(), "log_normal");
}

#[test]
fn distribution_kind_serde_roundtrip() {
    for kind in [
        DistributionKind::Uniform,
        DistributionKind::Normal,
        DistributionKind::LogNormal,
        DistributionKind::Exponential,
        DistributionKind::Poisson,
        DistributionKind::Bernoulli,
        DistributionKind::Binomial,
        DistributionKind::Geometric,
        DistributionKind::Pareto,
        DistributionKind::Weibull,
        DistributionKind::Gamma,
        DistributionKind::Beta,
        DistributionKind::Cauchy,
        DistributionKind::ChiSquared,
        DistributionKind::StudentT,
        DistributionKind::Triangular,
        DistributionKind::Zipf,
    ] {
        let json = serde_json::to_string(&kind).unwrap();
        let back: DistributionKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind, "failed for {:?}", kind);
    }
}

// ── RelationshipKind Display + serde ────────────────────────────────

#[test]
fn relationship_kind_display() {
    assert_eq!(RelationshipKind::OneToOne.to_string(), "one_to_one");
    assert_eq!(RelationshipKind::OneToMany.to_string(), "one_to_many");
    assert_eq!(RelationshipKind::ManyToOne.to_string(), "many_to_one");
    assert_eq!(RelationshipKind::ManyToMany.to_string(), "many_to_many");
}

#[test]
fn relationship_kind_serde_roundtrip() {
    for kind in [
        RelationshipKind::OneToOne,
        RelationshipKind::OneToMany,
        RelationshipKind::ManyToOne,
        RelationshipKind::ManyToMany,
    ] {
        let json = serde_json::to_string(&kind).unwrap();
        let back: RelationshipKind = serde_json::from_str(&json).unwrap();
        assert_eq!(back, kind);
    }
}

// ── CountSpec serde ─────────────────────────────────────────────────

#[test]
fn count_spec_fixed_from_integer() {
    let cs: CountSpec = serde_json::from_str("5000").unwrap();
    assert_eq!(cs, CountSpec::Fixed(5000));
}

#[test]
fn count_spec_range_from_object() {
    let cs: CountSpec = serde_json::from_str(r#"{"min": 100, "max": 500}"#).unwrap();
    assert_eq!(cs, CountSpec::Range { min: 100, max: 500 });
}

#[test]
fn count_spec_default_is_1000() {
    assert_eq!(CountSpec::default(), CountSpec::Fixed(1000));
}

// ── GeneratorSpec serde (tagged enum) ───────────────────────────────

#[test]
fn generator_spec_distribution_serde() {
    let json = r#"{"type":"distribution","kind":"normal","params":{"mean":0.0,"std_dev":1.0}}"#;
    let gs: GeneratorSpec = serde_json::from_str(json).unwrap();
    match gs {
        GeneratorSpec::Distribution { spec } => {
            assert_eq!(spec.kind, DistributionKind::Normal);
            assert_eq!(spec.params.get("mean"), Some(&0.0));
        }
        _ => panic!("expected Distribution"),
    }
}

#[test]
fn generator_spec_sequence_serde() {
    let json = r#"{"type":"sequence","start":10,"step":2}"#;
    let gs: GeneratorSpec = serde_json::from_str(json).unwrap();
    match gs {
        GeneratorSpec::Sequence { start, step, .. } => {
            assert_eq!(start, 10);
            assert_eq!(step, 2);
        }
        _ => panic!("expected Sequence"),
    }
}

#[test]
fn generator_spec_one_of_serde() {
    let json = r#"{"type":"one_of","choices":[{"value":"a","weight":2.0},{"value":"b"}]}"#;
    let gs: GeneratorSpec = serde_json::from_str(json).unwrap();
    match gs {
        GeneratorSpec::OneOf { choices } => {
            assert_eq!(choices.len(), 2);
            assert_eq!(choices[0].weight, 2.0);
            assert_eq!(choices[1].weight, 1.0); // default
        }
        _ => panic!("expected OneOf"),
    }
}

#[test]
fn generator_spec_pattern_serde() {
    let json = "{\"type\":\"pattern\",\"pattern\":\"###-???\"}";
    let gs: GeneratorSpec = serde_json::from_str(json).unwrap();
    match gs {
        GeneratorSpec::Pattern { pattern } => assert_eq!(pattern, "###-???"),
        _ => panic!("expected Pattern"),
    }
}

#[test]
fn generator_spec_constant_serde() {
    let json = r#"{"type":"constant","value":42}"#;
    let gs: GeneratorSpec = serde_json::from_str(json).unwrap();
    match gs {
        GeneratorSpec::Constant { value } => assert_eq!(value, Value::Int(42)),
        _ => panic!("expected Constant"),
    }
}

#[test]
fn generator_spec_faker_serde() {
    let json = r#"{"type":"faker","method":"first_name","args":[]}"#;
    let gs: GeneratorSpec = serde_json::from_str(json).unwrap();
    match gs {
        GeneratorSpec::Faker { method, args } => {
            assert_eq!(method, "first_name");
            assert!(args.is_empty());
        }
        _ => panic!("expected Faker"),
    }
}

// ── Constraint serde ────────────────────────────────────────────────

#[test]
fn constraint_unique_serde() {
    let json = r#"{"type":"unique","fields":["email","name"]}"#;
    let c: Constraint = serde_json::from_str(json).unwrap();
    match c {
        Constraint::Unique { fields } => assert_eq!(fields, vec!["email", "name"]),
        _ => panic!("expected Unique"),
    }
}

#[test]
fn constraint_check_serde() {
    let json = r#"{"type":"check","expr":"age > 0"}"#;
    let c: Constraint = serde_json::from_str(json).unwrap();
    match c {
        Constraint::Check { expr } => assert_eq!(expr, "age > 0"),
        _ => panic!("expected Check"),
    }
}

#[test]
fn constraint_range_serde() {
    let json = r#"{"type":"range","field":"score","min":0,"max":100}"#;
    let c: Constraint = serde_json::from_str(json).unwrap();
    match c {
        Constraint::Range { field, min, max } => {
            assert_eq!(field, "score");
            assert_eq!(min, Some(Value::Int(0)));
            assert_eq!(max, Some(Value::Int(100)));
        }
        _ => panic!("expected Range"),
    }
}

// ── ModelError formatting ───────────────────────────────────────────

#[test]
fn model_error_missing_field_message() {
    let err = ModelError::MissingField {
        path: "entities[0]".into(),
        field: "name".into(),
    };
    assert_eq!(err.to_string(), "entities[0]: missing required field 'name'");
}

#[test]
fn model_error_invalid_reference_message() {
    let err = ModelError::InvalidReference {
        path: "relationships[0]".into(),
        target: "nonexistent".into(),
        message: "entity not found".into(),
    };
    assert!(err.to_string().contains("nonexistent"));
    assert!(err.to_string().contains("entity not found"));
}

#[test]
fn model_error_invalid_probability_message() {
    let err = ModelError::InvalidProbability {
        path: "fields[0].nullable".into(),
        value: 1.5,
    };
    assert!(err.to_string().contains("1.5"));
    assert!(err.to_string().contains("[0.0, 1.0]"));
}

#[test]
fn model_error_duplicate_name_message() {
    let err = ModelError::DuplicateName {
        scope: "entities".into(),
        name: "users".into(),
    };
    assert_eq!(err.to_string(), "entities: duplicate name 'users'");
}

// ── DataModel serde round-trip ──────────────────────────────────────

#[test]
fn minimal_data_model_roundtrip() {
    let model = DataModel {
        name: "test".into(),
        description: None,
        seed: 42,
        locale: "en_US".into(),
        timezone: "UTC".into(),
        entities: vec![Entity {
            name: "users".into(),
            description: None,
            count: CountSpec::Fixed(100),
            fields: vec![Field {
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
            }],
            constraints: vec![],
            topology: None,
        }],
        relationships: vec![],
        noise_profiles: vec![],
        correlations: vec![],
        params: BTreeMap::new(),
        schema_version: "1.0".into(),
    };

    let json = serde_json::to_string_pretty(&model).unwrap();
    let back: DataModel = serde_json::from_str(&json).unwrap();
    assert_eq!(back.name, "test");
    assert_eq!(back.entities.len(), 1);
    assert_eq!(back.entities[0].name, "users");
    assert_eq!(back.entities[0].count, CountSpec::Fixed(100));
}

// ── WeightedChoice ──────────────────────────────────────────────────

#[test]
fn weighted_choice_default_weight() {
    let json = r#"{"value": "hello"}"#;
    let wc: WeightedChoice = serde_json::from_str(json).unwrap();
    assert_eq!(wc.weight, 1.0);
    assert_eq!(wc.value, Value::String("hello".into()));
}

#[test]
fn weighted_choice_explicit_weight() {
    let json = r#"{"value": 10, "weight": 3.5}"#;
    let wc: WeightedChoice = serde_json::from_str(json).unwrap();
    assert_eq!(wc.weight, 3.5);
    assert_eq!(wc.value, Value::Int(10));
}
