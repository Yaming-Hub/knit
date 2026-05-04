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
    let names = entity_names(model);
    let mut seen = HashSet::new();
    for entity in &model.entities {
        if !seen.insert(&entity.name) {
            errors.push(SchemaError::Validation {
                path: format!("entities.{}", entity.name),
                message: format!("duplicate entity name '{}'", entity.name),
            });
        }
        validate_fields(entity, &names, errors);
        validate_entity_count(entity, errors);
    }
}

fn validate_fields(
    entity: &Entity,
    entity_names: &HashSet<&str>,
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
                entity,
                entity_names,
                false,
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
                    message: format!(
                        "range requires min <= max, got min={}, max={}",
                        min, max
                    ),
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
    "phone",
    "address",
    "city",
    "company",
];

fn validate_generator(
    path: &str,
    gen: &GeneratorSpec,
    field_name: &str,
    entity: &Entity,
    entity_names: &HashSet<&str>,
    nested: bool,
    errors: &mut Vec<SchemaError>,
) {
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
                    message: format!(
                        "only UUID version 4 is supported, got {}",
                        version
                    ),
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
                    message: format!(
                        "lookup references unknown entity '{}'",
                        lookup_entity
                    ),
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
                    entity,
                    entity_names,
                    true,
                    errors,
                );
            }
            if let Some(def) = default {
                validate_generator(
                    &format!("{}.default", path),
                    def,
                    field_name,
                    entity,
                    entity_names,
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
            validate_generator(path, inner, field_name, entity, entity_names, true, errors);
        }
        GeneratorSpec::Composite { generators, .. } => {
            for (key, sub_gen) in generators {
                validate_generator(
                    &format!("{}.generators.{}", path, key),
                    sub_gen,
                    field_name,
                    entity,
                    entity_names,
                    true,
                    errors,
                );
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
        if let Some(fk) = &rel.foreign_key {
            let fields = entity_field_names(model, &rel.from);
            if !fields.contains(fk.as_str()) {
                errors.push(SchemaError::Validation {
                    path: path.clone(),
                    message: format!(
                        "foreign_key '{}' not found in entity '{}'",
                        fk, rel.from
                    ),
                });
            }
        }
        if let Some(ref count) = rel.cardinality {
            validate_count_spec(
                &format!("{}.cardinality", path),
                count,
                errors,
            );
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
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
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
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
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
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
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
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
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
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
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
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
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
            data_type: DataType::String,
            generator: Some(GeneratorSpec::BusinessHours {
                start_hour: 20,
                end_hour: 24,
                exclude_weekends: false,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
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
                    },
                }),
                max_retries: 100,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
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
            }],
            constraints: vec![],
            topology: None,
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
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, SchemaError::Validation { message, .. } if message.contains("lookup cannot be nested"))
        }));
    }
}
