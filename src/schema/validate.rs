//! Semantic validation of a parsed [`DataModel`](crate::core::DataModel).
//!
//! Checks include: duplicate entity/field/relationship names, missing
//! distribution parameters, invalid count specs, unknown entity references
//! in relationships, noise profiles, and correlations.

use std::collections::HashSet;

use crate::core::*;

use crate::schema::error::SchemaError;

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
    validate_personas(model, &mut errors);
    validate_actor_relationships(model, &mut errors);
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
    let names = entity_names(model);
    let mut seen = HashSet::new();
    for entity in &model.entities {
        if !seen.insert(&entity.name) {
            errors.push(SchemaError::Validation {
                path: format!("entities.{}", entity.name),
                message: format!("duplicate entity name '{}'", entity.name),
            });
        }
        validate_fields(entity, &names, model, errors);
        validate_entity_count(entity, errors);
        validate_activity_count(entity, model, errors);
    }
}

fn validate_fields(
    entity: &Entity,
    entity_names: &HashSet<&str>,
    model: &DataModel,
    errors: &mut Vec<SchemaError>,
) {
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
        if let Some(gen) = &field.generator {
            validate_generator(
                &format!("entities.{}.fields.{}.generator", entity.name, field.name),
                gen,
                &field.name,
                &field.data_type,
                entity,
                entity_names,
                model,
                false,
                errors,
            );
        }
        validate_null_spec(
            &format!("entities.{}.fields.{}.nullable", entity.name, field.name),
            &field.nullable,
            errors,
        );
        // precision is only meaningful for float types
        if field.precision.is_some() && field.data_type != DataType::Float {
            errors.push(SchemaError::Validation {
                path: format!("entities.{}.fields.{}.precision", entity.name, field.name),
                message: "precision is only valid for float64 fields".to_string(),
            });
        }
    }
    if pk_count > 1 {
        errors.push(SchemaError::Validation {
            path: format!("entities.{}", entity.name),
            message: format!("entity has {} primary keys, expected at most 1", pk_count),
        });
    }
}

fn validate_null_spec(path: &str, spec: &NullSpec, errors: &mut Vec<SchemaError>) {
    match spec {
        NullSpec::Probability(p) => {
            if !(*p >= 0.0 && *p <= 1.0) {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: format!("null probability must be in [0, 1], got {}", p),
                });
            }
        }
        NullSpec::Pattern { every_n } => {
            if *every_n == 0 {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "null pattern every_n must be > 0".to_string(),
                });
            }
        }
        _ => {}
    }
}

fn validate_count_spec(path: &str, count: &CountSpec, errors: &mut Vec<SchemaError>) {
    match count {
        CountSpec::Fixed(0) => {
            errors.push(SchemaError::Validation {
                path: path.to_string(),
                message: "count must be > 0".to_string(),
            });
        }
        CountSpec::Range { min, max } => {
            if min > max {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: format!("range requires min <= max, got min={}, max={}", min, max),
                });
            }
        }
        CountSpec::Distribution(spec) => {
            validate_distribution(path, spec, errors);
        }
        _ => {}
    }
}

fn validate_entity_count(entity: &Entity, errors: &mut Vec<SchemaError>) {
    validate_count_spec(
        &format!("entities.{}.count", entity.name),
        &entity.count,
        errors,
    );
}

fn validate_activity_count(entity: &Entity, model: &DataModel, errors: &mut Vec<SchemaError>) {
    let ac = match &entity.activity_count {
        Some(ac) => ac,
        None => return,
    };

    let path = format!("entities.{}.activity_count", entity.name);

    // Cannot use activity_count on actor entities themselves (would cause
    // stale actor-pool counts since pools are built before row overrides).
    if entity.actor {
        errors.push(SchemaError::Validation {
            path: path.clone(),
            message: "activity_count cannot be used on actor entities".to_string(),
        });
        return;
    }

    // actor_field must reference an existing field in this entity
    if !entity.fields.iter().any(|f| f.name == ac.actor_field) {
        errors.push(SchemaError::Validation {
            path: path.clone(),
            message: format!(
                "actor_field '{}' not found in entity '{}'",
                ac.actor_field, entity.name
            ),
        });
    }

    // trait_name must not be empty
    if ac.trait_name.is_empty() {
        errors.push(SchemaError::Validation {
            path: path.clone(),
            message: "trait name must not be empty".to_string(),
        });
    }

    // actor_field must point to a known FK relationship
    let target_rel = model.relationships.iter().find(|r| {
        r.from == entity.name
            && r.foreign_key.as_deref().unwrap_or(&format!("{}_id", r.to)) == ac.actor_field
    });

    match target_rel {
        None => {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!(
                    "actor_field '{}' does not match any FK relationship from '{}'",
                    ac.actor_field, entity.name
                ),
            });
        }
        Some(rel) => {
            // Target entity must be an actor entity
            let target_entity = model.entities.iter().find(|e| e.name == rel.to);
            if let Some(target) = target_entity {
                if !target.actor {
                    errors.push(SchemaError::Validation {
                        path: path.clone(),
                        message: format!(
                            "actor_field '{}' references '{}' which is not an actor entity",
                            ac.actor_field, rel.to
                        ),
                    });
                }
            }

            // Validate that trait_name exists in applicable personas.
            // Use the same selection rule as compile_actor_pool(): personas
            // prefixed with "{target_entity}_" OR unprefixed (global) personas.
            if !ac.trait_name.is_empty() {
                let entity_prefix = format!("{}_", rel.to);
                let missing_trait: Vec<&str> = model
                    .personas
                    .iter()
                    .filter(|p| {
                        p.name.starts_with(&entity_prefix)
                            || !model
                                .entities
                                .iter()
                                .any(|e| p.name.starts_with(&format!("{}_", e.name)))
                    })
                    .filter(|p| {
                        // Trait must be present and scalar (Int or Float)
                        match p.traits.get(&ac.trait_name) {
                            Some(Value::Float(_)) | Some(Value::Int(_)) => false,
                            Some(_) => true, // non-scalar
                            None => true,    // missing
                        }
                    })
                    .map(|p| p.name.as_str())
                    .collect();

                if !missing_trait.is_empty() {
                    errors.push(SchemaError::Validation {
                        path: path.clone(),
                        message: format!(
                            "trait '{}' missing or non-numeric in persona(s): {}",
                            ac.trait_name,
                            missing_trait.join(", ")
                        ),
                    });
                }
            }
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
        DistributionKind::LogNormal => {
            if !params.contains_key("mu") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "lognormal distribution requires 'mu' param".to_string(),
                });
            }
            if !params.contains_key("sigma") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "lognormal distribution requires 'sigma' param".to_string(),
                });
            } else if let Some(&s) = params.get("sigma") {
                if s <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "lognormal distribution 'sigma' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Binomial => {
            if !params.contains_key("n") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "binomial distribution requires 'n' param".to_string(),
                });
            } else if let Some(&n) = params.get("n") {
                if n < 0.0 || n.fract() != 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "binomial distribution 'n' must be >= 0 and integer-valued"
                            .to_string(),
                    });
                }
            }
            if !params.contains_key("p") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "binomial distribution requires 'p' param".to_string(),
                });
            } else if let Some(&p) = params.get("p") {
                if !(0.0..=1.0).contains(&p) {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "binomial distribution 'p' must be in [0, 1]".to_string(),
                    });
                }
            }
        }
        DistributionKind::Geometric => {
            if !params.contains_key("p") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "geometric distribution requires 'p' param".to_string(),
                });
            } else if let Some(&p) = params.get("p") {
                if p <= 0.0 || p > 1.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "geometric distribution 'p' must be in (0, 1]".to_string(),
                    });
                }
            }
        }
        DistributionKind::Pareto => {
            if !params.contains_key("scale") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "pareto distribution requires 'scale' param".to_string(),
                });
            } else if let Some(&v) = params.get("scale") {
                if v <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "pareto distribution 'scale' must be > 0".to_string(),
                    });
                }
            }
            if !params.contains_key("shape") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "pareto distribution requires 'shape' param".to_string(),
                });
            } else if let Some(&v) = params.get("shape") {
                if v <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "pareto distribution 'shape' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Weibull => {
            if !params.contains_key("scale") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "weibull distribution requires 'scale' param".to_string(),
                });
            } else if let Some(&v) = params.get("scale") {
                if v <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "weibull distribution 'scale' must be > 0".to_string(),
                    });
                }
            }
            if !params.contains_key("shape") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "weibull distribution requires 'shape' param".to_string(),
                });
            } else if let Some(&v) = params.get("shape") {
                if v <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "weibull distribution 'shape' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Gamma => {
            if !params.contains_key("shape") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "gamma distribution requires 'shape' param".to_string(),
                });
            } else if let Some(&v) = params.get("shape") {
                if v <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "gamma distribution 'shape' must be > 0".to_string(),
                    });
                }
            }
            if !params.contains_key("scale") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "gamma distribution requires 'scale' param".to_string(),
                });
            } else if let Some(&v) = params.get("scale") {
                if v <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "gamma distribution 'scale' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Beta => {
            if !params.contains_key("alpha") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "beta distribution requires 'alpha' param".to_string(),
                });
            } else if let Some(&v) = params.get("alpha") {
                if v <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "beta distribution 'alpha' must be > 0".to_string(),
                    });
                }
            }
            if !params.contains_key("beta") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "beta distribution requires 'beta' param".to_string(),
                });
            } else if let Some(&v) = params.get("beta") {
                if v <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "beta distribution 'beta' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Cauchy => {
            if !params.contains_key("median") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "cauchy distribution requires 'median' param".to_string(),
                });
            }
            if !params.contains_key("scale") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "cauchy distribution requires 'scale' param".to_string(),
                });
            } else if let Some(&v) = params.get("scale") {
                if v <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "cauchy distribution 'scale' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::ChiSquared => {
            if !params.contains_key("k") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "chi-squared distribution requires 'k' param".to_string(),
                });
            } else if let Some(&v) = params.get("k") {
                if v <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "chi-squared distribution 'k' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::StudentT => {
            if !params.contains_key("n") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "student-t distribution requires 'n' param".to_string(),
                });
            } else if let Some(&v) = params.get("n") {
                if v <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "student-t distribution 'n' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Triangular => {
            if !params.contains_key("min") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "triangular distribution requires 'min' param".to_string(),
                });
            }
            if !params.contains_key("max") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "triangular distribution requires 'max' param".to_string(),
                });
            }
            if !params.contains_key("mode") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "triangular distribution requires 'mode' param".to_string(),
                });
            }
            if let (Some(&min), Some(&max)) = (params.get("min"), params.get("max")) {
                if min >= max {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "triangular distribution requires min < max, got min={}, max={}",
                            min, max
                        ),
                    });
                }
                if let Some(&mode) = params.get("mode") {
                    if mode < min || mode > max {
                        errors.push(SchemaError::Validation {
                            path: path.to_string(),
                            message: format!(
                                "triangular distribution requires min <= mode <= max, got min={}, mode={}, max={}",
                                min, mode, max
                            ),
                        });
                    }
                }
            }
        }
        DistributionKind::Zipf => {
            if !params.contains_key("n") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "zipf distribution requires 'n' param".to_string(),
                });
            } else if let Some(&n) = params.get("n") {
                if n < 1.0 || n.fract() != 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "zipf distribution 'n' must be >= 1 and integer-valued"
                            .to_string(),
                    });
                }
            }
            if !params.contains_key("s") {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "zipf distribution requires 's' param".to_string(),
                });
            } else if let Some(&s) = params.get("s") {
                if s <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "zipf distribution 's' must be > 0".to_string(),
                    });
                }
            }
        }
    }
}

const KNOWN_FAKER_METHODS: &[&str] = &[
    "first_name",
    "last_name",
    "full_name",
    "name",
    "username",
    "email",
    "word",
    "sentence",
    "paragraph",
    "title",
    "phone",
    "address",
    "city",
    "state",
    "country",
    "zip_code",
    "zipcode",
    "postal_code",
    "company",
    "product_name",
    "product",
    "url",
    "domain",
    "ipv4",
    "ip_address",
    "ipv6",
    "color",
    "hex_color",
    "hex_string",
    "date",
    "datetime",
    "timestamp",
];

/// Check whether a generator type is compatible with the declared field data type.
/// Returns `Some(error_message)` if incompatible, `None` if OK.
///
/// Rules:
/// - `distribution` → numeric types (int, int32, float) or temporal (date, time, datetime, datetime_us, datetimetz, duration)
/// - `faker` → string
/// - `uuid` → uuid or string
/// - `sequence` → int, int32, or string
/// - `business_hours` → datetime, datetime_us, datetimetz, or time
/// - `relative` → numeric or temporal (same family as the field it references)
/// - `dictionary` → string
/// - `pattern` → string
///
/// Generators that produce arbitrary types (constant, derived, oneOf, lookup,
/// composite, conditional, unique) are not checked here.
fn check_generator_type_compat(gen: &GeneratorSpec, data_type: &DataType) -> Option<String> {
    match gen {
        GeneratorSpec::Distribution { .. } => {
            let compatible = matches!(
                data_type,
                DataType::Bool
                    | DataType::Int
                    | DataType::Int32
                    | DataType::Float
                    | DataType::Date
                    | DataType::Time
                    | DataType::Datetime
                    | DataType::DatetimeUs
                    | DataType::Datetimetz
                    | DataType::Duration
            );
            if !compatible {
                Some(format!(
                    "distribution generator is not compatible with data_type '{}'; \
                     expected a numeric type (int, int32, float), bool, or temporal type",
                    data_type
                ))
            } else {
                None
            }
        }
        GeneratorSpec::Faker { .. } => {
            if *data_type != DataType::String {
                Some(format!(
                    "faker generator produces strings but field has data_type '{}'",
                    data_type
                ))
            } else {
                None
            }
        }
        GeneratorSpec::UuidGen { .. } => {
            let compatible = matches!(data_type, DataType::Uuid | DataType::String);
            if !compatible {
                Some(format!(
                    "uuid generator is not compatible with data_type '{}'; expected 'uuid' or 'string'",
                    data_type
                ))
            } else {
                None
            }
        }
        GeneratorSpec::Sequence { .. } => {
            let compatible = matches!(
                data_type,
                DataType::Int | DataType::Int32 | DataType::String
            );
            if !compatible {
                Some(format!(
                    "sequence generator is not compatible with data_type '{}'; expected 'int', 'int32', or 'string'",
                    data_type
                ))
            } else {
                None
            }
        }
        GeneratorSpec::BusinessHours { .. } => {
            let compatible = matches!(
                data_type,
                DataType::Datetime | DataType::DatetimeUs | DataType::Datetimetz | DataType::Time
            );
            if !compatible {
                Some(format!(
                    "business_hours generator is not compatible with data_type '{}'; \
                     expected 'datetime', 'datetime_us', 'datetimetz', or 'time'",
                    data_type
                ))
            } else {
                None
            }
        }
        GeneratorSpec::Dictionary { .. } => {
            if *data_type != DataType::String {
                Some(format!(
                    "dictionary generator produces strings but field has data_type '{}'",
                    data_type
                ))
            } else {
                None
            }
        }
        GeneratorSpec::Pattern { .. } => {
            if *data_type != DataType::String {
                Some(format!(
                    "pattern generator produces strings but field has data_type '{}'",
                    data_type
                ))
            } else {
                None
            }
        }
        GeneratorSpec::Composite { .. } => {
            if *data_type != DataType::String {
                Some(format!(
                    "composite generator produces strings but field has data_type '{}'",
                    data_type
                ))
            } else {
                None
            }
        }
        GeneratorSpec::ActorRef { .. } => {
            // ActorRef produces a foreign key; compatible with int, int32, string, uuid
            let compatible = matches!(
                data_type,
                DataType::Int | DataType::Int32 | DataType::String | DataType::Uuid
            );
            if !compatible {
                Some(format!(
                    "actor_ref generator produces key values but field has data_type '{}'; \
                     expected 'int', 'int32', 'string', or 'uuid'",
                    data_type
                ))
            } else {
                None
            }
        }
        GeneratorSpec::ActorTemporal { .. } => {
            // ActorTemporal produces temporal values
            let compatible = matches!(
                data_type,
                DataType::Datetime
                    | DataType::DatetimeUs
                    | DataType::Datetimetz
                    | DataType::Date
                    | DataType::Time
                    | DataType::Duration
            );
            if !compatible {
                Some(format!(
                    "actor_temporal generator produces temporal values but field has data_type '{}'; \
                     expected a temporal type (datetime, date, time, duration)",
                    data_type
                ))
            } else {
                None
            }
        }
        GeneratorSpec::RelationshipRef { .. } => {
            // RelationshipRef currently only supports Int64 PKs at runtime.
            let compatible = matches!(data_type, DataType::Int);
            if !compatible {
                Some(format!(
                    "relationship_ref generator requires data_type 'int' (Int64 actor PKs); \
                     field has data_type '{}'",
                    format!("{:?}", data_type).to_lowercase()
                ))
            } else {
                None
            }
        }
        GeneratorSpec::PersonaField { .. } => {
            // PersonaField can produce any type depending on the trait — skip static check
            None
        }
        GeneratorSpec::ThreadRef { .. } => {
            // ThreadRef produces nullable Int64 (self-referencing PK)
            let compatible = matches!(data_type, DataType::Int);
            if !compatible {
                Some(format!(
                    "thread_ref generator produces Int64 (self-referential PK) but field has data_type '{}'; \
                     expected 'int'",
                    format!("{:?}", data_type).to_lowercase()
                ))
            } else {
                None
            }
        }
        // Generators with flexible output types — no static type check
        _ => None,
    }
}

#[allow(clippy::too_many_arguments)]
fn validate_generator(
    path: &str,
    gen: &GeneratorSpec,
    field_name: &str,
    data_type: &DataType,
    entity: &Entity,
    entity_names: &HashSet<&str>,
    model: &DataModel,
    nested: bool,
    errors: &mut Vec<SchemaError>,
) {
    // Check generator ↔ field type compatibility
    if let Some(msg) = check_generator_type_compat(gen, data_type) {
        errors.push(SchemaError::Validation {
            path: path.to_string(),
            message: msg,
        });
    }

    match gen {
        GeneratorSpec::Distribution { spec } => {
            validate_distribution(path, spec, errors);
        }
        GeneratorSpec::Sequence { step, .. } => {
            if *step == 0 {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "sequence step must not be 0".to_string(),
                });
            }
        }
        GeneratorSpec::OneOf { choices } => {
            if choices.is_empty() {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "oneOf requires at least one choice".to_string(),
                });
            }
        }
        GeneratorSpec::Faker { method, .. } => {
            if !KNOWN_FAKER_METHODS.contains(&method.as_str()) {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "unknown faker method '{}', expected one of: {}",
                        method,
                        KNOWN_FAKER_METHODS.join(", ")
                    ),
                });
            }
        }
        GeneratorSpec::UuidGen { version } => {
            if *version != 4 {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: format!("only UUID version 4 is supported, got {}", version),
                });
            }
        }
        GeneratorSpec::BusinessHours {
            start_hour,
            end_hour,
            ..
        } => {
            if *start_hour > 23 {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "business_hours start_hour must be in [0, 23]".to_string(),
                });
            }
            if *end_hour < 1 || *end_hour > 24 {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "business_hours end_hour must be in [1, 24]".to_string(),
                });
            }
            if *start_hour >= *end_hour {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "business_hours requires start_hour < end_hour".to_string(),
                });
            }
        }
        GeneratorSpec::Lookup {
            entity: ref lookup_entity,
            ..
        } => {
            if nested {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "lookup cannot be nested inside Unique, Conditional, or Composite"
                        .to_string(),
                });
            }
            if !entity_names.contains(lookup_entity.as_str()) {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: format!("lookup references unknown entity '{}'", lookup_entity),
                });
            }
        }
        GeneratorSpec::ExternalLookup {
            source,
            sampling,
            weight_column,
            ..
        } => {
            if nested {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "external_lookup cannot be nested inside Unique, Conditional, or Composite"
                        .to_string(),
                });
            }
            if source.is_empty() {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "external_lookup source path must not be empty".to_string(),
                });
            }
            if *sampling == crate::core::SamplingMode::Weighted && weight_column.is_none() {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "external_lookup with weighted sampling requires weight_column"
                        .to_string(),
                });
            }
            if *sampling != crate::core::SamplingMode::Weighted && weight_column.is_some() {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "external_lookup weight_column is only valid with weighted sampling"
                        .to_string(),
                });
            }
        }
        GeneratorSpec::Conditional {
            field,
            branches,
            default,
        } => {
            let field_names: HashSet<&str> =
                entity.fields.iter().map(|f| f.name.as_str()).collect();
            if !field_names.contains(field.as_str()) {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "conditional references unknown field '{}' in entity '{}'",
                        field, entity.name
                    ),
                });
            }
            if field == field_name {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "conditional cannot reference its own field".to_string(),
                });
            }
            for (i, branch) in branches.iter().enumerate() {
                validate_generator(
                    &format!("{}.branches[{}]", path, i),
                    &branch.generator,
                    field_name,
                    data_type,
                    entity,
                    entity_names,
                    model,
                    true,
                    errors,
                );
            }
            if let Some(def) = default {
                validate_generator(
                    &format!("{}.default", path),
                    def,
                    field_name,
                    data_type,
                    entity,
                    entity_names,
                    model,
                    true,
                    errors,
                );
            }
        }
        GeneratorSpec::Relative { field, .. } => {
            let field_names: HashSet<&str> =
                entity.fields.iter().map(|f| f.name.as_str()).collect();
            if !field_names.contains(field.as_str()) {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "relative references unknown field '{}' in entity '{}'",
                        field, entity.name
                    ),
                });
            }
            if field == field_name {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "relative cannot reference itself".to_string(),
                });
            }
        }
        GeneratorSpec::Unique { inner, .. } => {
            validate_generator(
                path,
                inner,
                field_name,
                data_type,
                entity,
                entity_names,
                model,
                true,
                errors,
            );
        }
        GeneratorSpec::Composite { generators, .. } => {
            // Composite always produces a string by concatenating sub-generator outputs,
            // so sub-generators are validated against String (not the parent field type).
            for (key, sub_gen) in generators {
                validate_generator(
                    &format!("{}.generators.{}", path, key),
                    sub_gen,
                    field_name,
                    &DataType::String,
                    entity,
                    entity_names,
                    model,
                    true,
                    errors,
                );
            }
        }
        GeneratorSpec::Dictionary {
            file, expansion, ..
        } => {
            if file.is_empty() {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "dictionary generator requires a non-empty 'file' path".to_string(),
                });
            }
            let valid_expansions = ["sample", "combinatorial", "suffix"];
            if !valid_expansions.contains(&expansion.as_str()) {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "unknown dictionary expansion '{}', expected one of: {}",
                        expansion,
                        valid_expansions.join(", ")
                    ),
                });
            }
        }
        GeneratorSpec::ActorRef {
            entity: ref actor_entity,
        } => {
            if !entity_names.contains(actor_entity.as_str()) {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: format!("actor_ref references unknown entity '{}'", actor_entity),
                });
            } else if let Some(target) = model.entities.iter().find(|e| e.name == *actor_entity) {
                if !target.actor {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "actor_ref references entity '{}' which is not marked as actor = true",
                            actor_entity
                        ),
                    });
                }
            }
        }
        GeneratorSpec::RelationshipRef {
            relationship,
            source_field,
        } => {
            if relationship.is_empty() {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "relationship_ref requires a non-empty 'relationship' name"
                        .to_string(),
                });
            } else if !model
                .actor_relationships
                .iter()
                .any(|ar| ar.name == *relationship)
            {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "relationship_ref references unknown actor_relationship '{}'",
                        relationship
                    ),
                });
            }
            if let Some(src) = source_field {
                if src.is_empty() {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "relationship_ref source_field must not be empty when specified"
                            .to_string(),
                    });
                }
            }
        }
        GeneratorSpec::ActorTemporal {
            trait_name,
            temporal_after,
            burst,
            ..
        } => {
            if trait_name.is_empty() {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "actor_temporal requires a non-empty 'trait' name".to_string(),
                });
            }
            if let Some(ta) = temporal_after {
                // Validate referenced entity exists
                if !entity_names.contains(ta.entity.as_str()) {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "temporal_after references unknown entity '{}'",
                            ta.entity
                        ),
                    });
                } else {
                    // Validate referenced field exists and is a temporal type
                    let ref_entity = model.entities.iter().find(|e| e.name == ta.entity);
                    if let Some(ref_ent) = ref_entity {
                        match ref_ent.fields.iter().find(|f| f.name == ta.field) {
                            None => {
                                errors.push(SchemaError::Validation {
                                    path: path.to_string(),
                                    message: format!(
                                        "temporal_after.field '{}' not found in entity '{}'",
                                        ta.field, ta.entity
                                    ),
                                });
                            }
                            Some(f) => {
                                if !matches!(
                                    f.data_type,
                                    DataType::Datetime
                                        | DataType::DatetimeUs
                                        | DataType::Datetimetz
                                        | DataType::Date
                                ) {
                                    errors.push(SchemaError::Validation {
                                        path: path.to_string(),
                                        message: format!(
                                            "temporal_after.field '{}' must be a temporal type, found '{}'",
                                            ta.field, f.data_type
                                        ),
                                    });
                                }
                            }
                        }
                    }
                }
                // Validate FK field exists in the current entity
                if !entity.fields.iter().any(|f| f.name == ta.fk) {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "temporal_after.fk '{}' not found in entity '{}'",
                            ta.fk, entity.name
                        ),
                    });
                }
            }
            if let Some(b) = burst {
                if !b.avg_events.is_finite() || b.avg_events <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "burst.avg_events must be a finite number > 0".to_string(),
                    });
                }
                if !b.avg_gap_minutes.is_finite() || b.avg_gap_minutes <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "burst.avg_gap_minutes must be a finite number > 0".to_string(),
                    });
                }
                if !b.avg_idle_hours.is_finite() || b.avg_idle_hours <= 0.0 {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: "burst.avg_idle_hours must be a finite number > 0".to_string(),
                    });
                }
            }
        }
        GeneratorSpec::PersonaField { trait_name } => {
            if trait_name.is_empty() {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "persona_field requires a non-empty 'trait' name".to_string(),
                });
            }
        }
        GeneratorSpec::ThreadRef {
            reply_probability,
            max_depth,
            reply_window,
            ..
        } => {
            if nested {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "thread_ref cannot be nested inside Unique, Conditional, or Composite"
                        .to_string(),
                });
            }
            if *reply_window == 0 {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "thread_ref reply_window must be at least 1".to_string(),
                });
            }
            if !reply_probability.is_finite()
                || *reply_probability < 0.0
                || *reply_probability > 1.0
            {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "thread_ref reply_probability must be in [0.0, 1.0], got {reply_probability}"
                    ),
                });
            }
            if *max_depth == 0 {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "thread_ref max_depth must be at least 1".to_string(),
                });
            }
            // Verify entity has a PK field with Int type
            let pk_field = entity
                .fields
                .iter()
                .find(|f| f.primary_key.unwrap_or(false));
            if let Some(pk) = pk_field {
                if pk.data_type != DataType::Int {
                    errors.push(SchemaError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "thread_ref requires entity PK to be 'int' (Int64), but '{}' has data_type '{:?}'",
                            pk.name,
                            pk.data_type
                        ),
                    });
                }
            } else {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "thread_ref requires entity to have a primary_key field".to_string(),
                });
            }
        }
        GeneratorSpec::Plugin { name, .. } => {
            if name.is_empty() {
                errors.push(SchemaError::Validation {
                    path: path.to_string(),
                    message: "plugin generator requires a non-empty 'name'".to_string(),
                });
            }
        }
        // Pattern, Derived, Constant — no additional validation needed
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
        // Compute effective FK field name (explicit or implicit default: <to>_id)
        let effective_fk: String = rel
            .foreign_key
            .clone()
            .unwrap_or_else(|| format!("{}_id", rel.to));

        // Validate FK field exists in the "from" entity
        let fields = entity_field_names(model, &rel.from);
        if !fields.contains(effective_fk.as_str()) {
            // Only report if the FK was explicitly specified — implicit FKs
            // might simply not exist yet (the planner handles that separately)
            if rel.foreign_key.is_some() {
                errors.push(SchemaError::Validation {
                    path: path.clone(),
                    message: format!(
                        "foreign_key '{}' not found in entity '{}'",
                        effective_fk, rel.from
                    ),
                });
            }
        }

        // Validate the "to" entity has a primary key for FK resolution
        if let Some(to_entity) = model.entities.iter().find(|e| e.name == rel.to) {
            let pk_field = to_entity
                .fields
                .iter()
                .find(|f| f.primary_key == Some(true));
            if pk_field.is_none() {
                errors.push(SchemaError::Validation {
                    path: path.clone(),
                    message: format!(
                        "entity '{}' referenced as relationship target has no primary key",
                        rel.to
                    ),
                });
            }

            // Validate FK field type matches target PK type
            if let Some(pk) = pk_field {
                if let Some(from_entity) = model.entities.iter().find(|e| e.name == rel.from) {
                    if let Some(fk_field) =
                        from_entity.fields.iter().find(|f| f.name == effective_fk)
                    {
                        if fk_field.data_type != pk.data_type {
                            errors.push(SchemaError::Validation {
                                path: path.clone(),
                                message: format!(
                                    "foreign_key '{}' type ({:?}) does not match target entity '{}' primary key type ({:?})",
                                    effective_fk, fk_field.data_type, rel.to, pk.data_type
                                ),
                            });
                        }
                    }
                }
            }
        }

        if let Some(ref count) = rel.cardinality {
            validate_count_spec(&format!("{}.cardinality", path), count, errors);
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
                        message: format!("field '{}' not found in entity '{}'", f, noise.entity),
                    });
                }
            }
        }
        validate_rate(&path, "null_rate", noise.null_rate, errors);
        validate_rate(&path, "duplicate_rate", noise.duplicate_rate, errors);
        validate_rate(&path, "typo_rate", noise.typo_rate, errors);
        validate_rate(&path, "outlier_rate", noise.outlier_rate, errors);
        validate_rate(&path, "swap_rate", noise.swap_rate, errors);
        validate_rate(&path, "truncate_rate", noise.truncate_rate, errors);
        validate_rate(&path, "fk_violate_rate", noise.fk_violate_rate, errors);
        validate_rate(&path, "temporal_spike_rate", noise.temporal_spike_rate, errors);
        validate_rate(&path, "missing_field_rate", noise.missing_field_rate, errors);
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
                        message: format!("field '{}' not found in entity '{}'", f, corr.entity),
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

fn validate_personas(model: &DataModel, errors: &mut Vec<SchemaError>) {
    let mut seen = HashSet::new();
    for (i, persona) in model.personas.iter().enumerate() {
        let path = format!("personas[{}]", i);

        // Duplicate name check
        if !seen.insert(&persona.name) {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!("duplicate persona name '{}'", persona.name),
            });
        }

        // Name must be non-empty
        if persona.name.is_empty() {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: "persona name must not be empty".to_string(),
            });
        }

        // Weight must be positive
        if persona.weight <= 0.0 {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!(
                    "persona '{}' has weight {} which must be > 0",
                    persona.name, persona.weight
                ),
            });
        }

        // Weight must be finite
        if !persona.weight.is_finite() {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!("persona '{}' has non-finite weight", persona.name),
            });
        }

        // Traits should not be empty
        if persona.traits.is_empty() {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!(
                    "persona '{}' has empty traits; at least one trait is required",
                    persona.name
                ),
            });
        }
    }
}

fn validate_actor_relationships(model: &DataModel, errors: &mut Vec<SchemaError>) {
    let names = entity_names(model);
    let mut seen = HashSet::new();
    for (i, ar) in model.actor_relationships.iter().enumerate() {
        let path = format!("actor_relationships[{}]", i);

        // Duplicate name check
        if !seen.insert(&ar.name) {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!("duplicate actor_relationship name '{}'", ar.name),
            });
        }

        // Name must be non-empty
        if ar.name.is_empty() {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: "actor_relationship name must not be empty".to_string(),
            });
        }

        // from_entity must exist and be an actor
        if !names.contains(ar.from_entity.as_str()) {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!("from_entity '{}' references unknown entity", ar.from_entity),
            });
        } else if let Some(entity) = model.entities.iter().find(|e| e.name == ar.from_entity) {
            if !entity.actor {
                errors.push(SchemaError::Validation {
                    path: path.clone(),
                    message: format!(
                        "from_entity '{}' is not marked as actor = true",
                        ar.from_entity
                    ),
                });
            }
        }

        // to_entity must exist and be an actor
        if !names.contains(ar.to_entity.as_str()) {
            errors.push(SchemaError::Validation {
                path: path.clone(),
                message: format!("to_entity '{}' references unknown entity", ar.to_entity),
            });
        } else if let Some(entity) = model.entities.iter().find(|e| e.name == ar.to_entity) {
            if !entity.actor {
                errors.push(SchemaError::Validation {
                    path: path.clone(),
                    message: format!("to_entity '{}' is not marked as actor = true", ar.to_entity),
                });
            }
        }

        // avg_degree param (if present) must be positive and finite
        if let Some(&avg_degree) = ar.params.get("avg_degree") {
            if !avg_degree.is_finite() || avg_degree <= 0.0 {
                errors.push(SchemaError::Validation {
                    path: path.clone(),
                    message: format!(
                        "actor_relationship '{}' has avg_degree {} which must be a finite value > 0",
                        ar.name, avg_degree
                    ),
                });
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
                        precision: None,
                        actor_column: false,
                    },
                    Field {
                        name: "email".to_string(),
                        description: None,
                        data_type: DataType::String,
                        generator: None,
                        nullable: NullSpec::Never,
                        primary_key: None,
                        precision: None,
                        actor_column: false,
                    },
                ],
                constraints: vec![],
                topology: None,
                actor: false,
                persona_distribution: None,
                activity_count: None,
            }],
            relationships: vec![],
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            schema_version: "1.0".to_string(),
            personas: Vec::new(),
            actor_relationships: Vec::new(),
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
            precision: None,
            actor_column: false,
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
                precision: None,
                actor_column: false,
            }],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
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
    fn test_validate_relationship_target_no_pk() {
        let mut model = minimal_model();
        // Add entity with no primary key
        model.entities.push(Entity {
            name: "order".to_string(),
            description: None,
            count: CountSpec::Fixed(200),
            fields: vec![Field {
                name: "amount".to_string(),
                description: None,
                data_type: DataType::Float,
                generator: None,
                nullable: NullSpec::Never,
                primary_key: None,
                precision: None,
                actor_column: false,
            }],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
        });
        model.relationships.push(Relationship {
            name: "user_order".to_string(),
            from: "user".to_string(),
            to: "order".to_string(),
            kind: RelationshipKind::OneToMany,
            foreign_key: Some("id".to_string()),
            cardinality: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. }
                if message.contains("has no primary key"))
        }));
    }

    #[test]
    fn test_validate_relationship_fk_type_mismatch() {
        let mut model = minimal_model();
        // "user" entity has PK "id" with DataType::Uuid
        // Add "order" entity with FK field as Int (mismatches Uuid)
        model.entities.push(Entity {
            name: "order".to_string(),
            description: None,
            count: CountSpec::Fixed(200),
            fields: vec![
                Field {
                    name: "id".to_string(),
                    description: None,
                    data_type: DataType::Int,
                    generator: None,
                    nullable: NullSpec::Never,
                    primary_key: Some(true),
                    precision: None,
                    actor_column: false,
                },
                Field {
                    name: "user_id".to_string(),
                    description: None,
                    data_type: DataType::Int, // Mismatch: user.id is Uuid
                    generator: None,
                    nullable: NullSpec::Never,
                    primary_key: None,
                    precision: None,
                    actor_column: false,
                },
            ],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
        });
        model.relationships.push(Relationship {
            name: "order_user".to_string(),
            from: "order".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: Some("user_id".to_string()),
            cardinality: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. }
                if message.contains("does not match target entity"))
        }));
    }

    #[test]
    fn test_validate_relationship_fk_type_match_ok() {
        let mut model = minimal_model();
        // "user" entity has PK "id" with DataType::Uuid
        // Add "order" entity with FK field also Uuid — should pass
        model.entities.push(Entity {
            name: "order".to_string(),
            description: None,
            count: CountSpec::Fixed(200),
            fields: vec![
                Field {
                    name: "id".to_string(),
                    description: None,
                    data_type: DataType::Uuid,
                    generator: None,
                    nullable: NullSpec::Never,
                    primary_key: Some(true),
                    precision: None,
                    actor_column: false,
                },
                Field {
                    name: "user_id".to_string(),
                    description: None,
                    data_type: DataType::Uuid, // Matches user.id Uuid
                    generator: None,
                    nullable: NullSpec::Never,
                    primary_key: None,
                    precision: None,
                    actor_column: false,
                },
            ],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
        });
        model.relationships.push(Relationship {
            name: "order_user".to_string(),
            from: "order".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: Some("user_id".to_string()),
            cardinality: None,
        });
        let errors = validate(&model);
        // Should produce no relationship-related errors
        assert!(!errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. }
                if message.contains("does not match target entity") || message.contains("has no primary key"))
        }));
    }

    #[test]
    fn test_validate_relationship_implicit_fk_target_no_pk() {
        let mut model = minimal_model();
        // Add entity with no primary key
        model.entities.push(Entity {
            name: "order".to_string(),
            description: None,
            count: CountSpec::Fixed(200),
            fields: vec![Field {
                name: "amount".to_string(),
                description: None,
                data_type: DataType::Float,
                generator: None,
                nullable: NullSpec::Never,
                primary_key: None,
                precision: None,
                actor_column: false,
            }],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
        });
        // Relationship without explicit foreign_key — implicit FK is "order_id"
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
            matches!(e, SchemaError::Validation { message, .. }
                if message.contains("has no primary key"))
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
                swap_rate: 0.0,
                truncate_rate: 0.0,
                fk_violate_rate: 0.0,
                temporal_spike_rate: 0.0,
                missing_field_rate: 0.0,
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
                swap_rate: 0.0,
                truncate_rate: 0.0,
                fk_violate_rate: 0.0,
                temporal_spike_rate: 0.0,
                missing_field_rate: 0.0,
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
            actor: false,
            persona_distribution: None,
            activity_count: None,
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
                swap_rate: 0.0,
                truncate_rate: 0.0,
                fk_violate_rate: 0.0,
                temporal_spike_rate: 0.0,
                missing_field_rate: 0.0,
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
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("min < max"))
        }));
    }

    #[test]
    fn test_validate_binomial_valid() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "trials".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Binomial,
                    params: [("n".to_string(), 10.0), ("p".to_string(), 0.5)]
                        .into_iter()
                        .collect(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, SchemaError::Validation { message, .. } if message.contains("binomial"))
            }),
            "expected no binomial errors, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_binomial_p_gt_1() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "trials".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Binomial,
                    params: [("n".to_string(), 10.0), ("p".to_string(), 1.5)]
                        .into_iter()
                        .collect(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("binomial") && message.contains("'p'"))
        }));
    }

    #[test]
    fn test_validate_triangular_min_ge_max() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "val".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Triangular,
                    params: [
                        ("min".to_string(), 10.0),
                        ("max".to_string(), 5.0),
                        ("mode".to_string(), 7.0),
                    ]
                    .into_iter()
                    .collect(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("triangular") && message.contains("min < max"))
        }));
    }

    #[test]
    fn test_validate_zipf_n_lt_1() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "rank".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Zipf,
                    params: [("n".to_string(), 0.0), ("s".to_string(), 1.0)]
                        .into_iter()
                        .collect(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("zipf") && message.contains("'n'"))
        }));
    }

    #[test]
    fn test_validate_beta_valid() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ratio".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Beta,
                    params: [("alpha".to_string(), 2.0), ("beta".to_string(), 5.0)]
                        .into_iter()
                        .collect(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, SchemaError::Validation { message, .. } if message.contains("beta"))
            }),
            "expected no beta errors, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_zipf_fractional_n() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "rank".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Zipf,
                    params: [("n".to_string(), 10.5), ("s".to_string(), 1.0)]
                        .into_iter()
                        .collect(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("zipf") && message.contains("integer"))
        }));
    }

    // ── Count spec validation ───────────────────────────────────────

    #[test]
    fn test_validate_count_range_min_gt_max() {
        let mut model = minimal_model();
        model.entities[0].count = CountSpec::Range { min: 100, max: 10 };
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("min <= max"))
        }));
    }

    #[test]
    fn test_validate_count_range_min_zero_valid() {
        let mut model = minimal_model();
        model.entities[0].count = CountSpec::Range { min: 0, max: 10 };
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, SchemaError::Validation { message, .. } if message.contains("range"))
            }),
            "Range min=0 should be valid, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_count_distribution() {
        let mut model = minimal_model();
        model.entities[0].count = CountSpec::Distribution(DistributionSpec {
            kind: DistributionKind::Normal,
            params: BTreeMap::new(), // missing mean and std_dev
            round: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("normal") && message.contains("mean"))
        }));
    }

    // ── Generator validation ────────────────────────────────────────

    #[test]
    fn test_validate_sequence_step_zero() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "seq".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Sequence {
                start: 1,
                step: 0,
                prefix: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("sequence step must not be 0"))
        }));
    }

    #[test]
    fn test_validate_oneof_empty() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "choice".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::OneOf { choices: vec![] }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("oneOf requires at least one choice"))
        }));
    }

    #[test]
    fn test_validate_faker_unknown_method() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "fake".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Faker {
                method: "bogus".to_string(),
                args: vec![],
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("unknown faker method 'bogus'"))
        }));
    }

    #[test]
    fn test_validate_faker_valid() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "fake".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Faker {
                method: "email".to_string(),
                args: vec![],
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, SchemaError::Validation { message, .. } if message.contains("faker"))
            }),
            "expected no faker errors, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_uuid_version_3() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "uid".to_string(),
            description: None,
            data_type: DataType::Uuid,
            generator: Some(GeneratorSpec::UuidGen { version: 3 }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("only UUID version 4 is supported"))
        }));
    }

    #[test]
    fn test_validate_business_hours_invalid() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::BusinessHours {
                start_hour: 20,
                end_hour: 10,
                exclude_weekends: false,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("start_hour < end_hour"))
        }));
    }

    #[test]
    fn test_validate_business_hours_end_24_valid() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::BusinessHours {
                start_hour: 20,
                end_hour: 24,
                exclude_weekends: false,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, SchemaError::Validation { message, .. } if message.contains("business_hours"))
            }),
            "expected no business_hours errors, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_lookup_unknown_entity() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ref_field".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Lookup {
                entity: "nonexistent".to_string(),
                field: "id".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("lookup references unknown entity 'nonexistent'"))
        }));
    }

    #[test]
    fn test_validate_unique_nested_invalid() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "uniq".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Unique {
                inner: Box::new(GeneratorSpec::Distribution {
                    spec: DistributionSpec {
                        kind: DistributionKind::Normal,
                        params: BTreeMap::new(), // missing mean & std_dev
                        round: false,
                    },
                }),
                max_retries: 100,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("normal") && message.contains("mean"))
        }));
    }

    #[test]
    fn test_validate_relative_self_ref() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "self_field".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Relative {
                field: "self_field".to_string(),
                offset: Value::Int(1),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("relative cannot reference itself"))
        }));
    }

    #[test]
    fn test_validate_conditional_self_ref() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "status".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Conditional {
                field: "status".to_string(),
                branches: vec![],
                default: Some(Box::new(GeneratorSpec::Constant {
                    value: Value::String("x".into()),
                })),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("conditional cannot reference its own field"))
        }));
    }

    #[test]
    fn test_validate_nested_lookup_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "other".to_string(),
            description: None,
            count: CountSpec::Fixed(10),
            fields: vec![Field {
                name: "id".to_string(),
                description: None,
                data_type: DataType::Int,
                generator: Some(GeneratorSpec::Sequence {
                    start: 0,
                    step: 1,
                    prefix: None,
                }),
                nullable: NullSpec::Never,
                primary_key: Some(true),
                precision: None,
                actor_column: false,
            }],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
        });
        model.entities[0].fields.push(Field {
            name: "ref_col".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Unique {
                inner: Box::new(GeneratorSpec::Lookup {
                    entity: "other".to_string(),
                    field: "id".to_string(),
                }),
                max_retries: 1000,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("lookup cannot be nested"))
        }));
    }

    #[test]
    fn test_validate_null_probability_out_of_range() {
        let mut model = minimal_model();
        model.entities[0].fields[0].nullable = NullSpec::Probability(1.5);
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("null probability"))
        }));
    }

    #[test]
    fn test_validate_null_probability_valid() {
        let mut model = minimal_model();
        model.entities[0].fields[0].nullable = NullSpec::Probability(0.3);
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, SchemaError::Validation { message, .. } if message.contains("null"))
            }),
            "expected no null errors, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_null_pattern_zero() {
        let mut model = minimal_model();
        model.entities[0].fields[0].nullable = NullSpec::Pattern { every_n: 0 };
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("every_n must be > 0"))
        }));
    }

    #[test]
    fn test_generator_type_compat_distribution_on_string_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "val".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Distribution {
                spec: crate::core::DistributionSpec {
                    kind: crate::core::DistributionKind::Uniform,
                    params: std::collections::BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("distribution generator is not compatible"))
        }));
    }

    #[test]
    fn test_generator_type_compat_faker_on_int_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "val".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Faker {
                method: "name".to_string(),
                args: vec![],
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("faker generator produces strings"))
        }));
    }

    #[test]
    fn test_generator_type_compat_uuid_on_float_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "val".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::UuidGen { version: 4 }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("uuid generator is not compatible"))
        }));
    }

    #[test]
    fn test_generator_type_compat_sequence_on_string_accepted() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "val".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Sequence {
                start: 1,
                step: 1,
                prefix: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(!errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("sequence generator is not compatible"))
        }));
    }

    #[test]
    fn test_generator_type_compat_sequence_on_bool_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "val".to_string(),
            description: None,
            data_type: DataType::Bool,
            generator: Some(GeneratorSpec::Sequence {
                start: 1,
                step: 1,
                prefix: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("sequence generator is not compatible"))
        }));
    }

    #[test]
    fn test_generator_type_compat_pattern_on_int_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "val".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Pattern {
                pattern: "###-###".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("pattern generator produces strings"))
        }));
    }

    #[test]
    fn test_actor_ref_on_non_actor_entity_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "users".to_string(),
            description: None,
            count: CountSpec::Fixed(10),
            fields: vec![Field {
                name: "id".to_string(),
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
            }],
            actor: false,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
        });
        model.entities[0].fields.push(Field {
            name: "user_id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::ActorRef {
                entity: "users".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("not marked as actor"))
        }));
    }

    #[test]
    fn test_actor_ref_type_compat_on_bool_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "users".to_string(),
            description: None,
            count: CountSpec::Fixed(10),
            fields: vec![],
            actor: true,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
        });
        model.entities[0].fields.push(Field {
            name: "user_id".to_string(),
            description: None,
            data_type: DataType::Bool,
            generator: Some(GeneratorSpec::ActorRef {
                entity: "users".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("actor_ref generator produces key values"))
        }));
    }

    #[test]
    fn test_relationship_ref_unknown_relationship_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "manager_id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::RelationshipRef {
                relationship: "reports_to".to_string(),
                source_field: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("unknown actor_relationship"))
        }));
    }

    #[test]
    fn test_actor_temporal_on_string_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "login_time".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::ActorTemporal {
                trait_name: "activity_hours".to_string(),
                temporal_after: None,
                burst: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("actor_temporal generator produces temporal values"))
        }));
    }

    // ── Persona validation tests ──

    #[test]
    fn test_persona_duplicate_name_rejected() {
        let mut model = minimal_model();
        model.personas.push(Persona {
            name: "early_bird".to_string(),
            weight: 0.5,
            traits: BTreeMap::from([("hour".to_string(), Value::Int(7))]),
        });
        model.personas.push(Persona {
            name: "early_bird".to_string(),
            weight: 0.3,
            traits: BTreeMap::from([("hour".to_string(), Value::Int(6))]),
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("duplicate persona name"))
        }));
    }

    #[test]
    fn test_persona_zero_weight_rejected() {
        let mut model = minimal_model();
        model.personas.push(Persona {
            name: "test".to_string(),
            weight: 0.0,
            traits: BTreeMap::from([("x".to_string(), Value::Int(1))]),
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("weight") && message.contains("must be > 0"))
        }));
    }

    #[test]
    fn test_persona_empty_traits_rejected() {
        let mut model = minimal_model();
        model.personas.push(Persona {
            name: "empty".to_string(),
            weight: 0.5,
            traits: BTreeMap::new(),
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("empty traits"))
        }));
    }

    #[test]
    fn test_actor_relationship_unknown_entity_rejected() {
        let mut model = minimal_model();
        model.actor_relationships.push(ActorRelationship {
            name: "reports_to".to_string(),
            from_entity: "employees".to_string(),
            to_entity: "managers".to_string(),
            graph_type: GraphType::Hierarchical,
            params: BTreeMap::from([("avg_degree".into(), 1.0)]),
            community_count: None,
            hierarchy_depth: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("from_entity") && message.contains("unknown entity"))
        }));
    }

    #[test]
    fn test_actor_relationship_non_actor_entity_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "users".to_string(),
            description: None,
            count: CountSpec::Fixed(10),
            fields: vec![],
            actor: false,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
        });
        model.actor_relationships.push(ActorRelationship {
            name: "friends".to_string(),
            from_entity: "users".to_string(),
            to_entity: "users".to_string(),
            graph_type: GraphType::SmallWorld,
            params: BTreeMap::from([("avg_degree".into(), 5.0)]),
            community_count: None,
            hierarchy_depth: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("not marked as actor"))
        }));
    }

    #[test]
    fn test_actor_relationship_zero_connections_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "people".to_string(),
            description: None,
            count: CountSpec::Fixed(10),
            fields: vec![],
            actor: true,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
        });
        model.actor_relationships.push(ActorRelationship {
            name: "knows".to_string(),
            from_entity: "people".to_string(),
            to_entity: "people".to_string(),
            graph_type: GraphType::ErdosRenyi,
            params: BTreeMap::from([("avg_degree".into(), 0.0)]),
            community_count: None,
            hierarchy_depth: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("avg_degree") && message.contains("must be a finite value > 0"))
        }));
    }

    #[test]
    fn test_valid_personas_and_actor_relationships_accepted() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "people".to_string(),
            description: None,
            count: CountSpec::Fixed(50),
            fields: vec![],
            actor: true,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
        });
        model.personas.push(Persona {
            name: "early_bird".to_string(),
            weight: 0.6,
            traits: BTreeMap::from([("start_hour".to_string(), Value::Int(7))]),
        });
        model.personas.push(Persona {
            name: "night_owl".to_string(),
            weight: 0.4,
            traits: BTreeMap::from([("start_hour".to_string(), Value::Int(22))]),
        });
        model.actor_relationships.push(ActorRelationship {
            name: "friends".to_string(),
            from_entity: "people".to_string(),
            to_entity: "people".to_string(),
            graph_type: GraphType::SmallWorld,
            params: BTreeMap::from([("avg_degree".into(), 5.0)]),
            community_count: Some(CountSpec::Fixed(3)),
            hierarchy_depth: None,
        });
        let errors = validate(&model);
        assert!(!errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. }
                if message.contains("persona") || message.contains("actor_relationship"))
        }));
    }

    #[test]
    fn test_persona_empty_name_rejected() {
        let mut model = minimal_model();
        model.personas.push(Persona {
            name: "".to_string(),
            weight: 0.5,
            traits: BTreeMap::from([("x".to_string(), Value::Int(1))]),
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("name must not be empty"))
        }));
    }

    #[test]
    fn test_persona_infinite_weight_rejected() {
        let mut model = minimal_model();
        model.personas.push(Persona {
            name: "inf_persona".to_string(),
            weight: f64::INFINITY,
            traits: BTreeMap::from([("x".to_string(), Value::Int(1))]),
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("non-finite weight"))
        }));
    }

    #[test]
    fn test_actor_relationship_duplicate_name_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "people".to_string(),
            description: None,
            count: CountSpec::Fixed(10),
            fields: vec![],
            actor: true,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
        });
        model.actor_relationships.push(ActorRelationship {
            name: "friends".to_string(),
            from_entity: "people".to_string(),
            to_entity: "people".to_string(),
            graph_type: GraphType::SmallWorld,
            params: BTreeMap::from([("avg_degree".into(), 5.0)]),
            community_count: None,
            hierarchy_depth: None,
        });
        model.actor_relationships.push(ActorRelationship {
            name: "friends".to_string(),
            from_entity: "people".to_string(),
            to_entity: "people".to_string(),
            graph_type: GraphType::ErdosRenyi,
            params: BTreeMap::new(),
            community_count: None,
            hierarchy_depth: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("duplicate actor_relationship name"))
        }));
    }

    #[test]
    fn test_actor_relationship_nan_avg_degree_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "people".to_string(),
            description: None,
            count: CountSpec::Fixed(10),
            fields: vec![],
            actor: true,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
        });
        model.actor_relationships.push(ActorRelationship {
            name: "knows".to_string(),
            from_entity: "people".to_string(),
            to_entity: "people".to_string(),
            graph_type: GraphType::ErdosRenyi,
            params: BTreeMap::from([("avg_degree".into(), f64::NAN)]),
            community_count: None,
            hierarchy_depth: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("avg_degree") && message.contains("finite"))
        }));
    }

    // ── temporal_after validation tests ──

    #[test]
    fn test_temporal_after_unknown_entity_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "created_at".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::ActorTemporal {
                trait_name: "activity_hours".to_string(),
                temporal_after: Some(TemporalAfterSpec {
                    entity: "nonexistent".to_string(),
                    field: "created_at".to_string(),
                    fk: "parent_id".to_string(),
                }),
                burst: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("temporal_after references unknown entity"))
        }));
    }

    #[test]
    fn test_temporal_after_unknown_field_rejected() {
        let mut model = minimal_model();
        // Add a "posts" entity with an "id" PK but no "created_at" field
        model.entities.push(Entity {
            name: "posts".to_string(),
            description: None,
            count: CountSpec::Fixed(50),
            fields: vec![Field {
                name: "id".to_string(),
                description: None,
                data_type: DataType::Int,
                generator: None,
                nullable: NullSpec::Never,
                primary_key: Some(true),
                precision: None,
                actor_column: false,
            }],
            actor: false,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
        });
        model.entities[0].fields.push(Field {
            name: "post_id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        model.entities[0].fields.push(Field {
            name: "created_at".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::ActorTemporal {
                trait_name: "activity_hours".to_string(),
                temporal_after: Some(TemporalAfterSpec {
                    entity: "posts".to_string(),
                    field: "created_at".to_string(),
                    fk: "post_id".to_string(),
                }),
                burst: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("temporal_after.field") && message.contains("not found"))
        }));
    }

    #[test]
    fn test_temporal_after_non_temporal_field_rejected() {
        let mut model = minimal_model();
        // Add "posts" entity with a String "title" field
        model.entities.push(Entity {
            name: "posts".to_string(),
            description: None,
            count: CountSpec::Fixed(50),
            fields: vec![
                Field {
                    name: "id".to_string(),
                    description: None,
                    data_type: DataType::Int,
                    generator: None,
                    nullable: NullSpec::Never,
                    primary_key: Some(true),
                    precision: None,
                    actor_column: false,
                },
                Field {
                    name: "title".to_string(),
                    description: None,
                    data_type: DataType::String,
                    generator: None,
                    nullable: NullSpec::Never,
                    primary_key: None,
                    precision: None,
                    actor_column: false,
                },
            ],
            actor: false,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
        });
        model.entities[0].fields.push(Field {
            name: "post_id".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        model.entities[0].fields.push(Field {
            name: "created_at".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::ActorTemporal {
                trait_name: "activity_hours".to_string(),
                temporal_after: Some(TemporalAfterSpec {
                    entity: "posts".to_string(),
                    field: "title".to_string(),
                    fk: "post_id".to_string(),
                }),
                burst: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("temporal_after.field") && message.contains("must be a temporal type"))
        }));
    }

    #[test]
    fn test_temporal_after_unknown_fk_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "posts".to_string(),
            description: None,
            count: CountSpec::Fixed(50),
            fields: vec![
                Field {
                    name: "id".to_string(),
                    description: None,
                    data_type: DataType::Int,
                    generator: None,
                    nullable: NullSpec::Never,
                    primary_key: Some(true),
                    precision: None,
                    actor_column: false,
                },
                Field {
                    name: "created_at".to_string(),
                    description: None,
                    data_type: DataType::Datetime,
                    generator: None,
                    nullable: NullSpec::Never,
                    primary_key: None,
                    precision: None,
                    actor_column: false,
                },
            ],
            actor: false,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
        });
        // Note: no "post_id" field in user entity
        model.entities[0].fields.push(Field {
            name: "created_at".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::ActorTemporal {
                trait_name: "activity_hours".to_string(),
                temporal_after: Some(TemporalAfterSpec {
                    entity: "posts".to_string(),
                    field: "created_at".to_string(),
                    fk: "post_id".to_string(),
                }),
                burst: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("temporal_after.fk") && message.contains("not found"))
        }));
    }

    #[test]
    fn test_burst_zero_avg_events_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "created_at".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::ActorTemporal {
                trait_name: "activity_hours".to_string(),
                temporal_after: None,
                burst: Some(BurstSpec {
                    avg_events: 0.0,
                    avg_gap_minutes: 5.0,
                    avg_idle_hours: 8.0,
                }),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("burst.avg_events must be a finite number > 0"))
        }));
    }

    #[test]
    fn test_burst_zero_gap_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "created_at".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::ActorTemporal {
                trait_name: "activity_hours".to_string(),
                temporal_after: None,
                burst: Some(BurstSpec {
                    avg_events: 5.0,
                    avg_gap_minutes: 0.0,
                    avg_idle_hours: 8.0,
                }),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("burst.avg_gap_minutes must be a finite number > 0"))
        }));
    }

    #[test]
    fn test_burst_infinity_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "created_at".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::ActorTemporal {
                trait_name: "activity_hours".to_string(),
                temporal_after: None,
                burst: Some(BurstSpec {
                    avg_events: f64::INFINITY,
                    avg_gap_minutes: 5.0,
                    avg_idle_hours: 8.0,
                }),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("burst.avg_events must be a finite number > 0"))
        }));
    }
}
