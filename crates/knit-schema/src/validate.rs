//! Semantic validation of a parsed [`DataModel`](knit_core::DataModel).
//!
//! Checks include: duplicate entity/field/relationship names, missing
//! distribution parameters, invalid count specs, unknown entity references
//! in relationships, noise profiles, and correlations.

use std::collections::HashSet;

use knit_core::*;

use crate::error::SchemaError;

/// Validate a [`DataModel`] and return all semantic errors found.
///
/// This performs a full pass over entities, relationships, noise profiles,
/// and correlations. It does **not** short-circuit — all errors are collected
/// so the user can fix them in one go.
pub fn validate(model: &DataModel) -> Vec<SchemaError> {
    let mut errors = Vec::new();
    validate_entities(model, &mut errors);
    validate_relationships(model, &mut errors);
    validate_noise_profiles(model, &mut errors);
    validate_correlations(model, &mut errors);
    errors
}

fn entity_names(model: &DataModel) -> HashSet<&str> {
    model.entities.iter().map(|e| e.name.as_str()).collect()
}

fn entity_field_names<'a>(model: &'a DataModel, entity_name: &str) -> HashSet<&'a str> {
    model
        .entities
        .iter()
        .find(|e| e.name == entity_name)
        .map(|e| e.fields.iter().map(|f| f.name.as_str()).collect())
        .unwrap_or_default()
}

fn validate_entities(model: &DataModel, errors: &mut Vec<SchemaError>) {
    let mut seen = HashSet::new();
    for entity in &model.entities {
        if !seen.insert(&entity.name) {
            errors.push(SchemaError::Validation {
                path: format!("entities.{}", entity.name),
                message: format!("duplicate entity name '{}'", entity.name),
            });
        }
        validate_fields(entity, errors);
        validate_entity_count(entity, errors);
    }
}

fn validate_fields(entity: &Entity, errors: &mut Vec<SchemaError>) {
    let mut seen = HashSet::new();
    let mut pk_count = 0u32;
    for field in &entity.fields {
        if !seen.insert(&field.name) {
            errors.push(SchemaError::Validation {
                path: format!("entities.{}.fields.{}", entity.name, field.name),
                message: format!("duplicate field name '{}'", field.name),
            });
        }
        if field.primary_key == Some(true) {
            pk_count += 1;
        }
        if let Some(GeneratorSpec::Distribution { spec }) = &field.generator {
            validate_distribution(
                &format!("entities.{}.fields.{}.generator", entity.name, field.name),
                spec,
                errors,
            );
        }
    }
    if pk_count > 1 {
        errors.push(SchemaError::Validation {
            path: format!("entities.{}", entity.name),
            message: format!("entity has {} primary keys, expected at most 1", pk_count),
        });
    }
}

fn validate_entity_count(entity: &Entity, errors: &mut Vec<SchemaError>) {
    if let CountSpec::Fixed(n) = &entity.count {
        if *n == 0 {
            errors.push(SchemaError::Validation {
                path: format!("entities.{}.count", entity.name),
                message: "entity count must be > 0".to_string(),
            });
        }
    }
}

fn validate_distribution(path: &str, spec: &DistributionSpec, errors: &mut Vec<SchemaError>) {
    let params = &spec.params;
    match spec.kind {
        DistributionKind::Normal => {
            if !params.contains_key("mean") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "normal distribution requires 'mean' param".to_string(),
                });
            }
            if !params.contains_key("std_dev") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "normal distribution requires 'std_dev' param".to_string(),
                });
            } else if let Some(&sd) = params.get("std_dev") {
                if sd <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "normal distribution 'std_dev' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Uniform => {
            if !params.contains_key("min") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "uniform distribution requires 'min' param".to_string(),
                });
            }
            if !params.contains_key("max") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "uniform distribution requires 'max' param".to_string(),
                });
            }
            if let (Some(&min), Some(&max)) = (params.get("min"), params.get("max")) {
                if min >= max {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "uniform distribution requires min < max, got min={}, max={}",
                            min, max
                        ),
                    });
                }
            }
        }
        DistributionKind::Exponential => {
            if !params.contains_key("lambda") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "exponential distribution requires 'lambda' param".to_string(),
                });
            } else if let Some(&l) = params.get("lambda") {
                if l <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "exponential distribution 'lambda' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Poisson => {
            if !params.contains_key("lambda") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "poisson distribution requires 'lambda' param".to_string(),
                });
            } else if let Some(&l) = params.get("lambda") {
                if l <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "poisson distribution 'lambda' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Bernoulli => {
            if !params.contains_key("p") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "bernoulli distribution requires 'p' param".to_string(),
                });
            } else if let Some(&p) = params.get("p") {
                if !(0.0..=1.0).contains(&p) {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "bernoulli distribution 'p' must be in [0, 1]".to_string(),
                    });
                }
            }
        }
        // For other distributions, no specific param validation yet
        _ => {}
    }
}

fn validate_relationships(model: &DataModel, errors: &mut Vec<SchemaError>) {
    let names = entity_names(model);
    let mut seen = HashSet::new();
    for rel in &model.relationships {
        let path = format!("relationships.{}", rel.name);
        if !seen.insert(&rel.name) {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!("duplicate relationship name '{}'", rel.name),
            });
        }
        if !names.contains(rel.from.as_str()) {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!("'from' references unknown entity '{}'", rel.from),
            });
        }
        if !names.contains(rel.to.as_str()) {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!("'to' references unknown entity '{}'", rel.to),
            });
        }
        if let Some(fk) = &rel.foreign_key {
            let fields = entity_field_names(model, &rel.from);
            if !fields.contains(fk.as_str()) {
                errors.push(SchemaError::Validation {
                    path,
                    message: format!(
                        "foreign_key '{}' not found in entity '{}'",
                        fk, rel.from
                    ),
                });
            }
        }
    }
}

fn validate_noise_profiles(model: &DataModel, errors: &mut Vec<SchemaError>) {
    let names = entity_names(model);
    let mut seen = HashSet::new();
    for noise in &model.noise_profiles {
        let path = format!("noise.{}", noise.name);
        if !seen.insert(&noise.name) {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!("duplicate noise profile name '{}'", noise.name),
            });
        }
        if !names.contains(noise.entity.as_str()) {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!("references unknown entity '{}'", noise.entity),
            });
        } else {
            let fields = entity_field_names(model, &noise.entity);
            for f in &noise.fields {
                if !fields.contains(f.as_str()) {
                    errors.push(SchemaError::Validation {
                        path: path.clone(),
                        message: format!(
                            "field '{}' not found in entity '{}'",
                            f, noise.entity
                        ),
                    });
                }
            }
        }
        validate_rate(&path, "null_rate", noise.null_rate, errors);
        validate_rate(&path, "duplicate_rate", noise.duplicate_rate, errors);
        validate_rate(&path, "typo_rate", noise.typo_rate, errors);
        validate_rate(&path, "outlier_rate", noise.outlier_rate, errors);
    }
}

fn validate_rate(path: &str, name: &str, value: f64, errors: &mut Vec<SchemaError>) {
    if !(0.0..=1.0).contains(&value) {
        errors.push(SchemaError::Validation {
            path: path.to_string(),
            message: format!("{} must be in [0.0, 1.0], got {}", name, value),
        });
    }
}

fn validate_correlations(model: &DataModel, errors: &mut Vec<SchemaError>) {
    let names = entity_names(model);
    for (i, corr) in model.correlations.iter().enumerate() {
        let path = format!("correlations[{}]", i);
        if !names.contains(corr.entity.as_str()) {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!("references unknown entity '{}'", corr.entity),
            });
        } else {
            let fields = entity_field_names(model, &corr.entity);
            for f in &corr.fields {
                if !fields.contains(f.as_str()) {
                    errors.push(SchemaError::Validation {
                        path: path.clone(),
                        message: format!(
                            "field '{}' not found in entity '{}'",
                            f, corr.entity
                        ),
                    });
                }
            }
        }
        if !corr.matrix.is_empty() {
            let n = corr.fields.len();
            if corr.matrix.len() != n {
                errors.push(SchemaError::Validation {
                    path: path.clone(),
                    message: format!(
                        "correlation matrix has {} rows but {} fields",
                        corr.matrix.len(),
                        n
                    ),
                });
            } else {
                for (ri, row) in corr.matrix.iter().enumerate() {
                    if row.len() != n {
                        errors.push(SchemaError::Validation {
                            path: path.clone(),
                            message: format!(
                                "correlation matrix row {} has {} columns, expected {}",
                                ri,
                                row.len(),
                                n
                            ),
                        });
                    }
                }
            }
        }
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn minimal_model() -> DataModel {
        DataModel {
            name: "test".to_string(),
            description: None,
            seed: 42,
            locale: "en_US".to_string(),
            timezone: "UTC".to_string(),
            entities: vec![Entity {
                name: "user".to_string(),
                description: None,
                count: CountSpec::Fixed(100),
                fields: vec![
                    Field {
                        name: "id".to_string(),
                        description: None,
                        data_type: DataType::Uuid,
                        generator: None,
                        nullable: NullSpec::Never,
                        primary_key: Some(true),
                    },
                    Field {
                        name: "email".to_string(),
                        description: None,
                        data_type: DataType::String,
                        generator: None,
                        nullable: NullSpec::Never,
                        primary_key: None,
                    },
                ],
                constraints: vec![],
                topology: None,
            }],
            relationships: vec![],
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            schema_version: "1.0".to_string(),
        }
    }

    #[test]
    fn test_validate_valid_model() {
        let model = minimal_model();
        let errors = validate(&model);
        assert!(errors.is_empty(), "expected no errors, got: {:?}", errors);
    }

    #[test]
    fn test_validate_duplicate_entity_names() {
        let mut model = minimal_model();
        model.entities.push(model.entities[0].clone());
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("duplicate entity"))
        }));
    }

    #[test]
    fn test_validate_duplicate_field_names() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("duplicate field"))
        }));
    }

    #[test]
    fn test_validate_multiple_primary_keys() {
        let mut model = minimal_model();
        model.entities[0].fields[1].primary_key = Some(true);
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("primary keys"))
        }));
    }

    #[test]
    fn test_validate_relationship_unknown_entity() {
        let mut model = minimal_model();
        model.relationships.push(Relationship {
            name: "user_order".to_string(),
            from: "user".to_string(),
            to: "order".to_string(),
            kind: RelationshipKind::OneToMany,
            foreign_key: None,
            cardinality: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("unknown entity 'order'"))
        }));
    }

    #[test]
    fn test_validate_relationship_unknown_field() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "order".to_string(),
            description: None,
            count: CountSpec::Fixed(200),
            fields: vec![Field {
                name: "id".to_string(),
                description: None,
                data_type: DataType::Uuid,
                generator: None,
                nullable: NullSpec::Never,
                primary_key: Some(true),
            }],
            constraints: vec![],
            topology: None,
        });
        model.relationships.push(Relationship {
            name: "user_order".to_string(),
            from: "order".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: Some("user_id".to_string()),
            cardinality: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("foreign_key 'user_id'"))
        }));
    }

    #[test]
    fn test_validate_noise_unknown_entity() {
        let mut model = minimal_model();
        model.noise_profiles.push(NoiseProfile {
            name: "bad_noise".to_string(),
            entity: "nonexistent".to_string(),
            fields: vec![],
            null_rate: 0.0,
            duplicate_rate: 0.0,
            typo_rate: 0.0,
            outlier_rate: 0.0,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("unknown entity 'nonexistent'"))
        }));
    }

    #[test]
    fn test_validate_noise_invalid_rates() {
        let mut model = minimal_model();
        model.noise_profiles.push(NoiseProfile {
            name: "bad_rates".to_string(),
            entity: "user".to_string(),
            fields: vec![],
            null_rate: 1.5,
            duplicate_rate: -0.1,
            typo_rate: 0.0,
            outlier_rate: 0.0,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("null_rate"))
        }));
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("duplicate_rate"))
        }));
    }

    #[test]
    fn test_validate_correlation_unknown_entity() {
        let mut model = minimal_model();
        model.correlations.push(Correlation {
            entity: "nonexistent".to_string(),
            fields: vec!["a".to_string()],
            matrix: vec![],
            conditional: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("unknown entity 'nonexistent'"))
        }));
    }

    #[test]
    fn test_validate_correlation_matrix_dimensions() {
        let mut model = minimal_model();
        model.correlations.push(Correlation {
            entity: "user".to_string(),
            fields: vec!["id".to_string(), "email".to_string()],
            matrix: vec![vec![1.0, 0.5]], // 1 row but 2 fields
            conditional: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("matrix"))
        }));
    }

    #[test]
    fn test_validate_duplicate_relationship_names() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "order".to_string(),
            description: None,
            count: CountSpec::Fixed(50),
            fields: vec![],
            constraints: vec![],
            topology: None,
        });
        let rel = Relationship {
            name: "user_order".to_string(),
            from: "user".to_string(),
            to: "order".to_string(),
            kind: RelationshipKind::OneToMany,
            foreign_key: None,
            cardinality: None,
        };
        model.relationships.push(rel.clone());
        model.relationships.push(rel);
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("duplicate relationship"))
        }));
    }

    #[test]
    fn test_validate_duplicate_noise_profile_names() {
        let mut model = minimal_model();
        let noise = NoiseProfile {
            name: "n1".to_string(),
            entity: "user".to_string(),
            fields: vec![],
            null_rate: 0.0,
            duplicate_rate: 0.0,
            typo_rate: 0.0,
            outlier_rate: 0.0,
        };
        model.noise_profiles.push(noise.clone());
        model.noise_profiles.push(noise);
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("duplicate noise profile"))
        }));
    }

    #[test]
    fn test_validate_uniform_min_ge_max() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "score".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Uniform,
                    params: [("min".to_string(), 100.0), ("max".to_string(), 10.0)]
                        .into_iter()
                        .collect(),
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("min < max"))
        }));
    }
}
