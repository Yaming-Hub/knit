use serde::Deserialize;
use std::collections::BTreeMap;

use knit_core::{
    Correlation, DataModel, Entity, NoiseProfile, Relationship, Value,
};

use crate::error::SchemaError;

// ── Intermediate schema representation ──────────────────────────────

/// Raw schema as it appears in TOML/JSON — wraps top-level fields in `[model]`.
#[derive(Debug, Deserialize)]
struct RawSchema {
    schema_version: Option<String>,
    #[serde(default)]
    model: RawModel,
    #[serde(default)]
    entities: Vec<Entity>,
    #[serde(default)]
    relationships: Vec<Relationship>,
    #[serde(default)]
    noise: Vec<NoiseProfile>,
    #[serde(default)]
    correlations: Vec<Correlation>,
}

#[derive(Debug, Default, Deserialize)]
struct RawModel {
    name: Option<String>,
    description: Option<String>,
    seed: Option<u64>,
    locale: Option<String>,
    timezone: Option<String>,
    #[serde(default)]
    params: BTreeMap<String, Value>,
}

impl RawSchema {
    fn into_data_model(self) -> Result<DataModel, SchemaError> {
        let model = DataModel {
            name: self.model.name.unwrap_or_else(|| "unnamed".to_string()),
            description: self.model.description,
            seed: self.model.seed.unwrap_or(42),
            locale: self.model.locale.unwrap_or_else(|| "en_US".to_string()),
            timezone: self.model.timezone.unwrap_or_else(|| "UTC".to_string()),
            entities: self.entities,
            relationships: self.relationships,
            noise_profiles: self.noise,
            correlations: self.correlations,
            params: self.model.params,
            schema_version: self.schema_version.unwrap_or_else(|| "1.0".to_string()),
        };
        Ok(model)
    }
}

// ── Public API ──────────────────────────────────────────────────────

/// Parse a Weave schema from a TOML string.
pub fn parse_toml(input: &str) -> Result<DataModel, SchemaError> {
    let raw: RawSchema = toml::from_str(input)?;
    raw.into_data_model()
}

/// Parse a Weave schema from a JSON string.
pub fn parse_json(input: &str) -> Result<DataModel, SchemaError> {
    let raw: RawSchema = serde_json::from_str(input)?;
    raw.into_data_model()
}

/// Parse a Weave schema from a TOML file.
pub fn parse_toml_file(path: &std::path::Path) -> Result<DataModel, SchemaError> {
    let content = std::fs::read_to_string(path)?;
    parse_toml(&content)
}

/// Parse a Weave schema from a JSON file.
pub fn parse_json_file(path: &std::path::Path) -> Result<DataModel, SchemaError> {
    let content = std::fs::read_to_string(path)?;
    parse_json(&content)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use indoc::indoc;
    use knit_core::{CountSpec, GeneratorSpec, NullSpec};

    #[test]
    fn test_parse_minimal_schema() {
        let input = indoc! {r#"
            schema_version = "1.0"

            [model]
            name = "test"
        "#};
        let model = parse_toml(input).unwrap();
        assert_eq!(model.name, "test");
        assert_eq!(model.schema_version, "1.0");
        assert_eq!(model.seed, 42);
        assert_eq!(model.locale, "en_US");
        assert_eq!(model.timezone, "UTC");
        assert!(model.entities.is_empty());
        assert!(model.relationships.is_empty());
        assert!(model.noise_profiles.is_empty());
    }

    #[test]
    fn test_parse_with_entities() {
        let input = indoc! {r#"
            [model]
            name = "basic"

            [[entities]]
            name = "user"
            count = 500

            [[entities.fields]]
            name = "id"
            data_type = "uuid"
            primary_key = true

            [[entities.fields]]
            name = "email"
            data_type = "string"
        "#};
        let model = parse_toml(input).unwrap();
        assert_eq!(model.entities.len(), 1);
        let user = &model.entities[0];
        assert_eq!(user.name, "user");
        assert_eq!(user.count, CountSpec::Fixed(500));
        assert_eq!(user.fields.len(), 2);
        assert_eq!(user.fields[0].name, "id");
        assert_eq!(user.fields[0].primary_key, Some(true));
        assert_eq!(user.fields[1].name, "email");
    }

    #[test]
    fn test_parse_distribution_generator() {
        let input = indoc! {r#"
            [model]
            name = "dist_test"

            [[entities]]
            name = "user"
            count = 100

            [[entities.fields]]
            name = "age"
            data_type = "int"
            generator = { type = "distribution", kind = "normal", params = { mean = 35.0, std_dev = 12.0 } }
        "#};
        let model = parse_toml(input).unwrap();
        let field = &model.entities[0].fields[0];
        match field.generator.as_ref().unwrap() {
            GeneratorSpec::Distribution { spec } => {
                assert_eq!(
                    spec.kind,
                    knit_core::DistributionKind::Normal
                );
                assert_eq!(spec.params.get("mean"), Some(&35.0));
                assert_eq!(spec.params.get("std_dev"), Some(&12.0));
            }
            other => panic!("expected Distribution, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_one_of_generator() {
        let input = indoc! {r#"
            [model]
            name = "choice_test"

            [[entities]]
            name = "user"
            count = 100

            [[entities.fields]]
            name = "tier"
            data_type = "string"
            generator = { type = "one_of", choices = [
                { value = "free", weight = 0.6 },
                { value = "premium", weight = 0.4 },
            ] }
        "#};
        let model = parse_toml(input).unwrap();
        let field = &model.entities[0].fields[0];
        match field.generator.as_ref().unwrap() {
            GeneratorSpec::OneOf { choices } => {
                assert_eq!(choices.len(), 2);
                assert_eq!(choices[0].value, Value::String("free".into()));
                assert_eq!(choices[0].weight, 0.6);
                assert_eq!(choices[1].value, Value::String("premium".into()));
                assert_eq!(choices[1].weight, 0.4);
            }
            other => panic!("expected OneOf, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_faker_generator() {
        let input = indoc! {r#"
            [model]
            name = "faker_test"

            [[entities]]
            name = "user"
            count = 100

            [[entities.fields]]
            name = "name"
            data_type = "string"
            generator = { type = "faker", method = "name", args = [] }
        "#};
        let model = parse_toml(input).unwrap();
        let field = &model.entities[0].fields[0];
        match field.generator.as_ref().unwrap() {
            GeneratorSpec::Faker { method, args } => {
                assert_eq!(method, "name");
                assert!(args.is_empty());
            }
            other => panic!("expected Faker, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_sequence_generator() {
        let input = indoc! {r#"
            [model]
            name = "seq_test"

            [[entities]]
            name = "item"
            count = 50

            [[entities.fields]]
            name = "seq_id"
            data_type = "int"
            generator = { type = "sequence", start = 1000, step = 10 }
        "#};
        let model = parse_toml(input).unwrap();
        let field = &model.entities[0].fields[0];
        match field.generator.as_ref().unwrap() {
            GeneratorSpec::Sequence {
                start,
                step,
                prefix,
            } => {
                assert_eq!(*start, 1000);
                assert_eq!(*step, 10);
                assert!(prefix.is_none());
            }
            other => panic!("expected Sequence, got {other:?}"),
        }
    }

    #[test]
    fn test_parse_relationships() {
        let input = indoc! {r#"
            [model]
            name = "rel_test"

            [[entities]]
            name = "user"
            count = 100

            [[entities]]
            name = "order"
            count = 500

            [[relationships]]
            name = "order_user"
            from = "order"
            to = "user"
            kind = "many_to_one"
            foreign_key = "user_id"
        "#};
        let model = parse_toml(input).unwrap();
        assert_eq!(model.relationships.len(), 1);
        let rel = &model.relationships[0];
        assert_eq!(rel.name, "order_user");
        assert_eq!(rel.from, "order");
        assert_eq!(rel.to, "user");
        assert_eq!(rel.kind, knit_core::RelationshipKind::ManyToOne);
        assert_eq!(rel.foreign_key, Some("user_id".into()));
    }

    #[test]
    fn test_parse_noise_profiles() {
        let input = indoc! {r#"
            [model]
            name = "noise_test"

            [[noise]]
            name = "user_typos"
            entity = "user"
            fields = ["name", "email"]
            typo_rate = 0.01
            null_rate = 0.05
        "#};
        let model = parse_toml(input).unwrap();
        assert_eq!(model.noise_profiles.len(), 1);
        let np = &model.noise_profiles[0];
        assert_eq!(np.name, "user_typos");
        assert_eq!(np.entity, "user");
        assert_eq!(np.fields, vec!["name", "email"]);
        assert!((np.typo_rate - 0.01).abs() < f64::EPSILON);
        assert!((np.null_rate - 0.05).abs() < f64::EPSILON);
    }

    #[test]
    fn test_parse_json() {
        let input = r#"{
            "schema_version": "1.0",
            "model": {
                "name": "json_test",
                "seed": 99
            },
            "entities": [
                {
                    "name": "product",
                    "count": 200,
                    "fields": [
                        {
                            "name": "id",
                            "data_type": "uuid",
                            "primary_key": true,
                            "generator": { "type": "uuid", "version": 4 }
                        },
                        {
                            "name": "price",
                            "data_type": "float",
                            "generator": {
                                "type": "distribution",
                                "kind": "uniform",
                                "params": { "min": 1.0, "max": 999.0 }
                            }
                        }
                    ]
                }
            ]
        }"#;
        let model = parse_json(input).unwrap();
        assert_eq!(model.name, "json_test");
        assert_eq!(model.seed, 99);
        assert_eq!(model.entities.len(), 1);
        assert_eq!(model.entities[0].name, "product");
        assert_eq!(model.entities[0].fields.len(), 2);
    }

    #[test]
    fn test_parse_full_ecommerce() {
        let input = indoc! {r#"
            schema_version = "1.0"

            [model]
            name = "ecommerce"
            seed = 12345

            [[entities]]
            name = "user"
            count = 100000

            [[entities.fields]]
            name = "id"
            data_type = "uuid"
            primary_key = true
            generator = { type = "uuid", version = 4 }

            [[entities.fields]]
            name = "name"
            data_type = "string"
            generator = { type = "faker", method = "name", args = [] }

            [[entities.fields]]
            name = "age"
            data_type = "int"
            generator = { type = "distribution", kind = "normal", params = { mean = 35.0, std_dev = 12.0 } }

            [[entities.fields]]
            name = "tier"
            data_type = "string"
            generator = { type = "one_of", choices = [
                { value = "free", weight = 0.6 },
                { value = "premium", weight = 0.4 },
            ] }

            [[entities]]
            name = "order"
            count = 500000

            [[entities.fields]]
            name = "id"
            data_type = "uuid"
            primary_key = true
            generator = { type = "uuid", version = 4 }

            [[entities.fields]]
            name = "user_id"
            data_type = "uuid"

            [[entities.fields]]
            name = "amount"
            data_type = "float"
            generator = { type = "distribution", kind = "pareto", params = { scale = 10.0, shape = 1.5 } }

            [[relationships]]
            name = "order_user"
            from = "order"
            to = "user"
            kind = "many_to_one"
            foreign_key = "user_id"

            [[noise]]
            name = "user_typos"
            entity = "user"
            fields = ["name"]
            typo_rate = 0.01
        "#};
        let model = parse_toml(input).unwrap();
        assert_eq!(model.name, "ecommerce");
        assert_eq!(model.seed, 12345);
        assert_eq!(model.entities.len(), 2);
        assert_eq!(model.entities[0].name, "user");
        assert_eq!(model.entities[0].fields.len(), 4);
        assert_eq!(model.entities[1].name, "order");
        assert_eq!(model.entities[1].fields.len(), 3);
        assert_eq!(model.relationships.len(), 1);
        assert_eq!(model.relationships[0].name, "order_user");
        assert_eq!(model.noise_profiles.len(), 1);
        assert_eq!(model.noise_profiles[0].name, "user_typos");
    }

    #[test]
    fn test_parse_error_invalid_toml() {
        let input = "this is not [valid toml {{{";
        let result = parse_toml(input);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            matches!(err, SchemaError::TomlParse(_)),
            "expected TomlParse error, got {err:?}"
        );
    }

    #[test]
    fn test_parse_null_spec_variants() {
        // NullSpec::Never (bool false)
        let input = indoc! {r#"
            [model]
            name = "null_test"

            [[entities]]
            name = "item"
            count = 10

            [[entities.fields]]
            name = "a"
            data_type = "string"
            nullable = false

            [[entities.fields]]
            name = "b"
            data_type = "string"
            nullable = true

            [[entities.fields]]
            name = "c"
            data_type = "string"
            nullable = { probability = 0.3 }

            [[entities.fields]]
            name = "d"
            data_type = "string"
            nullable = { every_n = 5 }
        "#};
        let model = parse_toml(input).unwrap();
        let fields = &model.entities[0].fields;
        assert_eq!(fields[0].nullable, NullSpec::Never);
        assert_eq!(fields[1].nullable, NullSpec::Always);
        assert_eq!(fields[2].nullable, NullSpec::Probability(0.3));
        assert_eq!(fields[3].nullable, NullSpec::Pattern { every_n: 5 });
    }

    #[test]
    fn test_parse_count_spec_variants() {
        let input = indoc! {r#"
            [model]
            name = "count_test"

            [[entities]]
            name = "fixed_entity"
            count = 1000

            [[entities]]
            name = "range_entity"

            [entities.count]
            min = 100
            max = 500

            [[entities]]
            name = "dist_entity"

            [entities.count]
            kind = "normal"
            params = { mean = 1000.0, std_dev = 50.0 }
        "#};
        let model = parse_toml(input).unwrap();
        assert_eq!(model.entities[0].count, CountSpec::Fixed(1000));
        match &model.entities[1].count {
            CountSpec::Range { min, max } => {
                assert_eq!(*min, 100);
                assert_eq!(*max, 500);
            }
            other => panic!("expected Range, got {other:?}"),
        }
        match &model.entities[2].count {
            CountSpec::Distribution(spec) => {
                assert_eq!(spec.kind, knit_core::DistributionKind::Normal);
            }
            other => panic!("expected Distribution, got {other:?}"),
        }
    }
}
