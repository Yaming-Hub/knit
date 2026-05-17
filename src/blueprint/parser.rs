//! TOML and JSON parsing for Weave schema files.
//!
//! Converts raw schema text into a [`DataModel`](crate::core::DataModel) via an
//! intermediate `RawSchema` representation that handles the TOML/JSON
//! structural differences (e.g. `[model]` section, `[[entities]]` arrays).

use serde::Deserialize;
use std::collections::BTreeMap;

use crate::core::{
    ActorRelationship, Correlation, DataModel, Entity, NoiseProfile, Persona, Relationship, Value,
};

use crate::blueprint::error::BlueprintError;
use crate::blueprint::includes::StringOrVec;

// ── Intermediate schema representation ──────────────────────────────

/// Raw schema as it appears in TOML/JSON — wraps top-level fields in `[model]`.
#[derive(Debug, Deserialize)]
struct RawSchema {
    blueprint_version: Option<String>,
    #[serde(default)]
    extends: Option<String>,
    #[serde(default)]
    include: Option<StringOrVec>,
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
    #[serde(default)]
    personas: Vec<Persona>,
    #[serde(default)]
    actor_relationships: Vec<ActorRelationship>,
    #[serde(default)]
    types: Vec<crate::core::CustomType>,
    #[serde(default)]
    mixins: Vec<crate::core::Mixin>,
    #[serde(default)]
    companion_files: Vec<String>,
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
    fn into_data_model(self) -> Result<DataModel, BlueprintError> {
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
            blueprint_version: self.blueprint_version.unwrap_or_else(|| "1.0".to_string()),
            personas: self.personas,
            actor_relationships: self.actor_relationships,
            custom_types: self.types,
            mixins: self.mixins,
            companion_files: self.companion_files,
        };
        Ok(model)
    }
}

// ── Public API ──────────────────────────────────────────────────────

/// Parse a Weave schema from a TOML string.
pub fn parse_toml(input: &str) -> Result<DataModel, BlueprintError> {
    let mut model = parse_toml_raw(input)?;
    resolve_mixins(&mut model)?;
    resolve_custom_types(&mut model)?;
    Ok(model)
}

/// Parse TOML into DataModel without resolving mixins or custom types.
/// Used by includes/extends to defer resolution until after merge.
pub(crate) fn parse_toml_raw(input: &str) -> Result<DataModel, BlueprintError> {
    let raw: RawSchema = toml::from_str(input)?;
    raw.into_data_model()
}

/// Parse a Weave schema from a JSON string.
pub fn parse_json(input: &str) -> Result<DataModel, BlueprintError> {
    let mut model = parse_json_raw(input)?;
    resolve_mixins(&mut model)?;
    resolve_custom_types(&mut model)?;
    Ok(model)
}

/// Parse JSON into DataModel without resolving custom types.
/// Used by includes/extends to defer resolution until after merge.
pub(crate) fn parse_json_raw(input: &str) -> Result<DataModel, BlueprintError> {
    let raw: RawSchema = serde_json::from_str(input)?;
    raw.into_data_model()
}

/// Parse a Weave schema from a TOML file.
///
/// If the schema specifies `include` directives, the included fragments are
/// resolved and merged first. If the schema specifies an `extends` field,
/// the parent schema is resolved and merged on top.
pub fn parse_toml_file(path: &std::path::Path) -> Result<DataModel, BlueprintError> {
    let mut model = parse_toml_file_raw(path)?;
    resolve_mixins(&mut model)?;
    resolve_custom_types(&mut model)?;
    Ok(model)
}

/// Parse a TOML file into DataModel without resolving custom types.
/// Handles includes and extends merging but defers type resolution.
pub(crate) fn parse_toml_file_raw(path: &std::path::Path) -> Result<DataModel, BlueprintError> {
    let content = std::fs::read_to_string(path)?;
    let raw: RawSchema = toml::from_str(&content)?;
    let extends = raw.extends.clone();
    let includes = raw.include.clone();
    let model = raw.into_data_model()?;

    // Step 1: resolve includes (if any)
    let model = if let Some(inc) = includes {
        let inc_vec = inc.into_vec();
        if inc_vec.is_empty() {
            model
        } else {
            let mut visited = std::collections::HashSet::new();
            let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            let mut stack = vec![canonical.clone()];
            visited.insert(canonical);
            let included = crate::blueprint::includes::resolve_includes(
                path,
                &inc_vec,
                &mut visited,
                &mut stack,
            )?;
            crate::blueprint::includes::merge_main_over_includes(&included, &model)
        }
    } else {
        model
    };

    // Step 2: resolve extends (if any)
    let model = if let Some(ref parent_ref) = extends {
        crate::blueprint::resolve_extends(path, &model, parent_ref)?
    } else {
        model
    };

    Ok(model)
}

/// Parse a Weave schema from a JSON file.
///
/// If the schema specifies `include` directives, the included fragments are
/// resolved and merged first. If the schema specifies an `extends` field,
/// the parent schema is resolved and merged on top.
pub fn parse_json_file(path: &std::path::Path) -> Result<DataModel, BlueprintError> {
    let mut model = parse_json_file_raw(path)?;
    resolve_mixins(&mut model)?;
    resolve_custom_types(&mut model)?;
    Ok(model)
}

/// Parse a JSON file into DataModel without resolving custom types.
pub(crate) fn parse_json_file_raw(path: &std::path::Path) -> Result<DataModel, BlueprintError> {
    let content = std::fs::read_to_string(path)?;
    let raw: RawSchema = serde_json::from_str(&content)?;
    let extends = raw.extends.clone();
    let includes = raw.include.clone();
    let model = raw.into_data_model()?;

    // Step 1: resolve includes (if any)
    let model = if let Some(inc) = includes {
        let inc_vec = inc.into_vec();
        if inc_vec.is_empty() {
            model
        } else {
            let mut visited = std::collections::HashSet::new();
            let canonical = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            let mut stack = vec![canonical.clone()];
            visited.insert(canonical);
            let included = crate::blueprint::includes::resolve_includes(
                path,
                &inc_vec,
                &mut visited,
                &mut stack,
            )?;
            crate::blueprint::includes::merge_main_over_includes(&included, &model)
        }
    } else {
        model
    };

    // Step 2: resolve extends (if any)
    let model = if let Some(ref parent_ref) = extends {
        crate::blueprint::resolve_extends(path, &model, parent_ref)?
    } else {
        model
    };

    Ok(model)
}

// ── Mixin Resolution ──────────────────────────────────────────────

use crate::core::Mixin;

/// Resolve mixin references in entities, expanding mixin fields.
///
/// For each entity with `mixin_refs = Some(["name1", "name2"])`:
/// - Look up each mixin name in model.mixins
/// - Prepend mixin fields to entity fields (in declared order)
/// - Entity fields with the same name override mixin fields
/// - Error on mixin-vs-mixin field name collisions
/// - Clear entity.mixin_refs after resolution
pub fn resolve_mixins(model: &mut DataModel) -> Result<(), BlueprintError> {
    if model.mixins.is_empty() {
        // Check for references to undefined mixins
        for entity in &mut model.entities {
            if let Some(ref refs) = entity.mixin_refs
                && let Some(name) = refs.first() {
                    return Err(BlueprintError::Validation {
                        path: format!("entities.{}.mixins", entity.name),
                        message: format!("references undefined mixin '{}'", name),
                    });
                }
            // Normalize empty mixin_refs
            entity.mixin_refs = None;
        }
        return Ok(());
    }

    // Validate mixin definitions
    let mut seen_names = std::collections::HashSet::new();
    for mixin in &model.mixins {
        if mixin.name.is_empty() {
            return Err(BlueprintError::Validation {
                path: "mixins".to_string(),
                message: "mixin name cannot be empty".to_string(),
            });
        }
        if !seen_names.insert(&mixin.name) {
            return Err(BlueprintError::Validation {
                path: format!("mixins.{}", mixin.name),
                message: "duplicate mixin name".to_string(),
            });
        }
    }

    // Build lookup map
    let mixin_map: std::collections::HashMap<&str, &Mixin> =
        model.mixins.iter().map(|m| (m.name.as_str(), m)).collect();

    // Resolve entity mixin references
    for entity in &mut model.entities {
        if let Some(ref refs) = entity.mixin_refs {
            let mut expanded_fields: Vec<Field> = Vec::new();
            let mut mixin_field_names = std::collections::HashSet::new();

            for mixin_name in refs {
                let mixin = mixin_map.get(mixin_name.as_str()).ok_or_else(|| {
                    BlueprintError::Validation {
                        path: format!("entities.{}.mixins", entity.name),
                        message: format!("references undefined mixin '{}'", mixin_name),
                    }
                })?;

                for mixin_field in &mixin.fields {
                    // Check for mixin-vs-mixin field collision
                    if !mixin_field_names.insert(mixin_field.name.clone()) {
                        return Err(BlueprintError::Validation {
                            path: format!("entities.{}", entity.name),
                            message: format!(
                                "mixin field '{}' conflicts with field from another mixin",
                                mixin_field.name
                            ),
                        });
                    }
                    expanded_fields.push(mixin_field.clone());
                }
            }

            // Collect entity field names for override check
            let entity_field_names: std::collections::HashSet<&str> =
                entity.fields.iter().map(|f| f.name.as_str()).collect();

            // Filter out mixin fields that the entity overrides
            expanded_fields.retain(|f| !entity_field_names.contains(f.name.as_str()));

            // Prepend mixin fields before entity fields
            expanded_fields.append(&mut entity.fields);
            entity.fields = expanded_fields;
        }
        // Clear mixin_refs after resolution
        entity.mixin_refs = None;
    }

    Ok(())
}

// ── Custom Type Resolution ─────────────────────────────────────────

use crate::core::{CustomType, DataType, Field, NullSpec};

/// Built-in type names that custom types must not shadow.
const BUILTIN_TYPES: &[&str] = &[
    "bool",
    "int",
    "int32",
    "float",
    "string",
    "uuid",
    "date",
    "time",
    "datetime",
    "datetime_us",
    "datetimetz",
    "duration",
    "bytes",
    "array",
    "map",
    "object",
];

/// Resolve all `DataType::Custom(name)` references in the model.
///
/// For each field whose `data_type` is `Custom(name)`:
/// - Replace `data_type` with the custom type's `base` type
/// - If the field has no generator, inherit the custom type's generator
/// - If the field has no precision, inherit the custom type's precision
/// - If the field has default nullable, inherit the custom type's nullable
///
/// Errors if a custom type name conflicts with a built-in type, if names
/// are duplicated, or if a field references an undefined custom type.
pub fn resolve_custom_types(model: &mut DataModel) -> Result<(), BlueprintError> {
    if model.custom_types.is_empty() {
        // Check for Custom references (including nested fields) without types defined
        for entity in &model.entities {
            check_undefined_custom_refs(&entity.fields, &entity.name)?;
        }
        return Ok(());
    }

    // Validate custom type definitions
    let mut seen_names = std::collections::HashSet::new();
    for ct in &model.custom_types {
        // No built-in name conflicts
        if BUILTIN_TYPES.contains(&ct.name.as_str()) {
            return Err(BlueprintError::Validation {
                path: format!("types.{}", ct.name),
                message: "conflicts with built-in type name".to_string(),
            });
        }
        // No duplicate names
        if !seen_names.insert(&ct.name) {
            return Err(BlueprintError::Validation {
                path: format!("types.{}", ct.name),
                message: "duplicate custom type name".to_string(),
            });
        }
        // Base must not be Custom (no chaining in v1)
        if matches!(ct.base, DataType::Custom(_)) {
            return Err(BlueprintError::Validation {
                path: format!("types.{}", ct.name),
                message: "cannot reference another custom type as base".to_string(),
            });
        }
        // Base must not be complex types
        if matches!(ct.base, DataType::Object | DataType::Array | DataType::Map) {
            return Err(BlueprintError::Validation {
                path: format!("types.{}", ct.name),
                message: format!("cannot use complex base type '{}'", ct.base),
            });
        }
    }

    // Build lookup map
    let type_map: std::collections::HashMap<&str, &CustomType> = model
        .custom_types
        .iter()
        .map(|ct| (ct.name.as_str(), ct))
        .collect();

    // Resolve fields in all entities
    for entity in &mut model.entities {
        resolve_fields(&mut entity.fields, &type_map, &entity.name)?;
    }

    Ok(())
}

/// Resolve custom type references in a list of fields (recursively for nested objects).
fn resolve_fields(
    fields: &mut [Field],
    type_map: &std::collections::HashMap<&str, &CustomType>,
    entity_name: &str,
) -> Result<(), BlueprintError> {
    for field in fields.iter_mut() {
        if let DataType::Custom(ref name) = field.data_type {
            let ct = type_map
                .get(name.as_str())
                .ok_or_else(|| BlueprintError::Validation {
                    path: format!("{}.{}", entity_name, field.name),
                    message: format!("references undefined type '{}'", name),
                })?;

            // Replace data_type with the custom type's base
            field.data_type = ct.base.clone();

            // Inherit generator if field doesn't specify one
            if field.generator.is_none() {
                field.generator = ct.generator.clone();
            }

            // Inherit precision if field doesn't specify one
            if field.precision.is_none() {
                field.precision = ct.precision;
            }

            // Inherit nullable if field has default (Never) and custom type specifies one
            if field.nullable == NullSpec::default()
                && let Some(ref ns) = ct.nullable {
                    field.nullable = ns.clone();
                }
        }

        // Recurse into nested object sub-fields
        if !field.fields.is_empty() {
            resolve_fields(&mut field.fields, type_map, entity_name)?;
        }
    }
    Ok(())
}

/// Recursively check for `DataType::Custom` references in fields, erroring on the first one found.
fn check_undefined_custom_refs(fields: &[Field], entity_name: &str) -> Result<(), BlueprintError> {
    for field in fields {
        if let DataType::Custom(ref name) = field.data_type {
            return Err(BlueprintError::Validation {
                path: format!("{}.{}", entity_name, field.name),
                message: format!("references undefined type '{}'", name),
            });
        }
        if !field.fields.is_empty() {
            check_undefined_custom_refs(&field.fields, entity_name)?;
        }
    }
    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{CountSpec, GeneratorSpec, IntOrString, NullSpec};
    use indoc::indoc;

    #[test]
    fn test_parse_minimal_schema() {
        let input = indoc! {r#"
            blueprint_version = "1.0"

            [model]
            name = "test"
        "#};
        let model = parse_toml(input).unwrap();
        assert_eq!(model.name, "test");
        assert_eq!(model.blueprint_version, "1.0");
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
                assert_eq!(spec.kind, crate::core::DistributionKind::Normal);
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
                ..
            } => {
                assert_eq!(*start, IntOrString::Int(1000));
                assert_eq!(*step, IntOrString::Int(10));
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
        assert_eq!(rel.kind, crate::core::RelationshipKind::ManyToOne);
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
            "blueprint_version": "1.0",
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
            blueprint_version = "1.0"

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
            matches!(err, BlueprintError::TomlParse(_)),
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
                assert_eq!(spec.kind, crate::core::DistributionKind::Normal);
            }
            other => panic!("expected Distribution, got {other:?}"),
        }
    }

    // ── Custom Type Tests ──────────────────────────────────────────────

    #[test]
    fn custom_type_resolves_base_and_generator() {
        let input = indoc! {r#"
            [model]
            name = "ct_test"

            [[types]]
            name = "money"
            base = "float"
            precision = 2
            [types.generator]
            type = "distribution"
            kind = "normal"
            params = { mean = 100.0, std_dev = 25.0 }

            [[entities]]
            name = "orders"
            count = 10

            [[entities.fields]]
            name = "id"
            data_type = "int"

            [[entities.fields]]
            name = "amount"
            data_type = "money"
        "#};
        let model = parse_toml(input).unwrap();
        let field = &model.entities[0].fields[1];
        assert_eq!(field.name, "amount");
        assert_eq!(field.data_type, DataType::Float);
        assert_eq!(field.precision, Some(2));
        assert!(field.generator.is_some());
        match field.generator.as_ref().unwrap() {
            GeneratorSpec::Distribution { spec } => {
                assert_eq!(spec.kind, crate::core::DistributionKind::Normal);
            }
            other => panic!("expected Distribution, got {other:?}"),
        }
    }

    #[test]
    fn custom_type_field_overrides_generator() {
        let input = indoc! {r#"
            [model]
            name = "override_test"

            [[types]]
            name = "money"
            base = "float"
            precision = 2
            [types.generator]
            type = "distribution"
            kind = "normal"
            params = { mean = 100.0, std_dev = 25.0 }

            [[entities]]
            name = "orders"
            count = 10

            [[entities.fields]]
            name = "total"
            data_type = "money"
            precision = 4
            [entities.fields.generator]
            type = "distribution"
            kind = "uniform"
            params = { min = 0.0, max = 1000.0 }
        "#};
        let model = parse_toml(input).unwrap();
        let field = &model.entities[0].fields[0];
        assert_eq!(field.data_type, DataType::Float);
        // Field overrides precision
        assert_eq!(field.precision, Some(4));
        // Field overrides generator
        match field.generator.as_ref().unwrap() {
            GeneratorSpec::Distribution { spec } => {
                assert_eq!(spec.kind, crate::core::DistributionKind::Uniform);
            }
            other => panic!("expected Uniform distribution, got {other:?}"),
        }
    }

    #[test]
    fn custom_type_inherits_nullable() {
        let input = indoc! {r#"
            [model]
            name = "nullable_test"

            [[types]]
            name = "optional_email"
            base = "string"
            nullable = { probability = 0.1 }
            [types.generator]
            type = "faker"
            method = "email"

            [[entities]]
            name = "users"
            count = 10

            [[entities.fields]]
            name = "email"
            data_type = "optional_email"
        "#};
        let model = parse_toml(input).unwrap();
        let field = &model.entities[0].fields[0];
        assert_eq!(field.data_type, DataType::String);
        assert_eq!(field.nullable, NullSpec::Probability(0.1));
    }

    #[test]
    fn custom_type_undefined_reference_error() {
        let input = indoc! {r#"
            [model]
            name = "undef_test"

            [[entities]]
            name = "orders"
            count = 10

            [[entities.fields]]
            name = "price"
            data_type = "money"
        "#};
        let result = parse_toml(input);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("undefined type"),
            "expected undefined type error, got: {err}"
        );
    }

    #[test]
    fn custom_type_builtin_name_conflict_error() {
        let input = indoc! {r#"
            [model]
            name = "conflict_test"

            [[types]]
            name = "string"
            base = "int"

            [[entities]]
            name = "t"
            count = 1
        "#};
        let result = parse_toml(input);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("built-in"),
            "expected built-in name conflict error, got: {err}"
        );
    }

    #[test]
    fn custom_type_duplicate_name_error() {
        let input = indoc! {r#"
            [model]
            name = "dup_test"

            [[types]]
            name = "money"
            base = "float"

            [[types]]
            name = "money"
            base = "int"

            [[entities]]
            name = "t"
            count = 1
        "#};
        let result = parse_toml(input);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("duplicate"),
            "expected duplicate name error, got: {err}"
        );
    }

    #[test]
    fn custom_type_complex_base_rejected() {
        let input = indoc! {r#"
            [model]
            name = "complex_base_test"

            [[types]]
            name = "nested"
            base = "object"

            [[entities]]
            name = "t"
            count = 1
        "#};
        let result = parse_toml(input);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("complex base type"),
            "expected complex base type error, got: {err}"
        );
    }

    #[test]
    fn custom_type_no_types_no_error() {
        let input = indoc! {r#"
            [model]
            name = "no_types"

            [[entities]]
            name = "users"
            count = 10

            [[entities.fields]]
            name = "id"
            data_type = "int"
        "#};
        let model = parse_toml(input).unwrap();
        assert!(model.custom_types.is_empty());
        assert_eq!(model.entities[0].fields[0].data_type, DataType::Int);
    }

    #[test]
    fn custom_type_multiple_types() {
        let input = indoc! {r#"
            [model]
            name = "multi_types"

            [[types]]
            name = "money"
            base = "float"
            precision = 2

            [[types]]
            name = "email_address"
            base = "string"
            [types.generator]
            type = "faker"
            method = "email"

            [[entities]]
            name = "users"
            count = 10

            [[entities.fields]]
            name = "balance"
            data_type = "money"

            [[entities.fields]]
            name = "email"
            data_type = "email_address"
        "#};
        let model = parse_toml(input).unwrap();
        assert_eq!(model.entities[0].fields[0].data_type, DataType::Float);
        assert_eq!(model.entities[0].fields[0].precision, Some(2));
        assert_eq!(model.entities[0].fields[1].data_type, DataType::String);
        assert!(model.entities[0].fields[1].generator.is_some());
    }

    // ── Mixin Tests ────────────────────────────────────────────────────

    #[test]
    fn mixin_fields_prepended_to_entity() {
        let input = indoc! {r#"
            [model]
            name = "mixin_test"

            [[mixins]]
            name = "timestamped"

            [[mixins.fields]]
            name = "created_at"
            data_type = "datetime"

            [[mixins.fields]]
            name = "updated_at"
            data_type = "datetime"

            [[entities]]
            name = "orders"
            count = 10
            mixins = ["timestamped"]

            [[entities.fields]]
            name = "id"
            data_type = "int"
        "#};
        let model = parse_toml(input).unwrap();
        let fields = &model.entities[0].fields;
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "created_at");
        assert_eq!(fields[1].name, "updated_at");
        assert_eq!(fields[2].name, "id");
        assert!(model.entities[0].mixin_refs.is_none());
    }

    #[test]
    fn mixin_field_overridden_by_entity() {
        let input = indoc! {r#"
            [model]
            name = "override_test"

            [[mixins]]
            name = "timestamped"

            [[mixins.fields]]
            name = "created_at"
            data_type = "datetime"

            [[entities]]
            name = "orders"
            count = 10
            mixins = ["timestamped"]

            [[entities.fields]]
            name = "created_at"
            data_type = "string"
        "#};
        let model = parse_toml(input).unwrap();
        let fields = &model.entities[0].fields;
        assert_eq!(fields.len(), 1);
        assert_eq!(fields[0].name, "created_at");
        assert_eq!(fields[0].data_type, DataType::String);
    }

    #[test]
    fn multiple_mixins() {
        let input = indoc! {r#"
            [model]
            name = "multi_mixin"

            [[mixins]]
            name = "auditable"
            [[mixins.fields]]
            name = "created_at"
            data_type = "datetime"

            [[mixins]]
            name = "versioned"
            [[mixins.fields]]
            name = "version"
            data_type = "int"

            [[entities]]
            name = "orders"
            count = 10
            mixins = ["auditable", "versioned"]

            [[entities.fields]]
            name = "id"
            data_type = "int"
        "#};
        let model = parse_toml(input).unwrap();
        let fields = &model.entities[0].fields;
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0].name, "created_at");
        assert_eq!(fields[1].name, "version");
        assert_eq!(fields[2].name, "id");
    }

    #[test]
    fn mixin_vs_mixin_field_collision_error() {
        let input = indoc! {r#"
            [model]
            name = "collision_test"

            [[mixins]]
            name = "mixin_a"
            [[mixins.fields]]
            name = "created_at"
            data_type = "datetime"

            [[mixins]]
            name = "mixin_b"
            [[mixins.fields]]
            name = "created_at"
            data_type = "string"

            [[entities]]
            name = "orders"
            count = 10
            mixins = ["mixin_a", "mixin_b"]
        "#};
        let result = parse_toml(input);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("conflicts with field from another mixin"),
            "got: {err}"
        );
    }

    #[test]
    fn mixin_undefined_reference_error() {
        let input = indoc! {r#"
            [model]
            name = "undef_test"

            [[entities]]
            name = "orders"
            count = 10
            mixins = ["nonexistent"]
        "#};
        let result = parse_toml(input);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("undefined mixin"), "got: {err}");
    }

    #[test]
    fn mixin_duplicate_name_error() {
        let input = indoc! {r#"
            [model]
            name = "dup_test"

            [[mixins]]
            name = "auditable"
            [[mixins.fields]]
            name = "created_at"
            data_type = "datetime"

            [[mixins]]
            name = "auditable"
            [[mixins.fields]]
            name = "version"
            data_type = "int"

            [[entities]]
            name = "t"
            count = 1
        "#};
        let result = parse_toml(input);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("duplicate mixin name"), "got: {err}");
    }

    #[test]
    fn mixin_with_custom_types() {
        let input = indoc! {r#"
            [model]
            name = "combined_test"

            [[types]]
            name = "money"
            base = "float"
            precision = 2

            [[mixins]]
            name = "priced"
            [[mixins.fields]]
            name = "price"
            data_type = "money"

            [[entities]]
            name = "products"
            count = 10
            mixins = ["priced"]

            [[entities.fields]]
            name = "id"
            data_type = "int"
        "#};
        let model = parse_toml(input).unwrap();
        let fields = &model.entities[0].fields;
        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "price");
        assert_eq!(fields[0].data_type, DataType::Float);
        assert_eq!(fields[0].precision, Some(2));
    }

    #[test]
    fn no_mixins_no_error() {
        let input = indoc! {r#"
            [model]
            name = "no_mixins"

            [[entities]]
            name = "users"
            count = 10

            [[entities.fields]]
            name = "id"
            data_type = "int"
        "#};
        let model = parse_toml(input).unwrap();
        assert!(model.mixins.is_empty());
        assert_eq!(model.entities[0].fields.len(), 1);
    }
}
