//! Semantic validation of a parsed [`DataModel`](crate::core::DataModel).
//!
//! Checks include: duplicate entity/field/relationship names, missing
//! distribution parameters, invalid count specs, unknown entity references
//! in relationships, noise profiles, and correlations, derived expression
//! syntax and field references, and dependency cycle detection.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::core::*;
use crate::gen::expr;

use crate::blueprint::error::BlueprintError;

/// Validate a [`DataModel`] and return all semantic errors found.
///
/// This performs a full pass over entities, relationships, noise profiles,
/// and correlations. It does **not** short-circuit — all errors are collected
/// so the user can fix them in one go.
pub fn validate(model: &DataModel) -> Vec<BlueprintError> {
    let mut errors = Vec::new();
    validate_entities(model, &mut errors);
    validate_relationships(model, &mut errors);
    validate_noise_profiles(model, &mut errors);
    validate_correlations(model, &mut errors);
    validate_personas(model, &mut errors);
    validate_actor_relationships(model, &mut errors);
    validate_dependency_cycles(model, &mut errors);
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

fn validate_entities(model: &DataModel, errors: &mut Vec<BlueprintError>) {
    let names = entity_names(model);
    let mut seen = HashSet::new();
    for entity in &model.entities {
        if !seen.insert(&entity.name) {
            errors.push(BlueprintError::Validation {
                path: format!("entities.{}", entity.name),
                message: format!("duplicate entity name '{}'", entity.name),
            });
        }
        validate_fields(entity, &names, model, errors);
        validate_entity_count(entity, errors);
        validate_activity_count(entity, model, errors);
        validate_constraints(entity, errors);
    }
}

fn validate_fields(
    entity: &Entity,
    entity_names: &HashSet<&str>,
    model: &DataModel,
    errors: &mut Vec<BlueprintError>,
) {
    let mut seen = HashSet::new();
    let mut pk_count = 0u32;
    for field in &entity.fields {
        if !seen.insert(&field.name) {
            errors.push(BlueprintError::Validation {
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
            errors.push(BlueprintError::Validation {
                path: format!("entities.{}.fields.{}.precision", entity.name, field.name),
                message: "precision is only valid for float64 fields".to_string(),
            });
        }
        // Validate nested object fields
        validate_object_field(
            &format!("entities.{}.fields.{}", entity.name, field.name),
            field,
            errors,
        );
    }
    if pk_count > 1 {
        errors.push(BlueprintError::Validation {
            path: format!("entities.{}", entity.name),
            message: format!("entity has {} primary keys, expected at most 1", pk_count),
        });
    }
}

/// Validate entity constraints — check that referenced fields exist.
fn validate_constraints(entity: &Entity, errors: &mut Vec<BlueprintError>) {
    let field_names: HashSet<&str> = entity.fields.iter().map(|f| f.name.as_str()).collect();
    for (i, constraint) in entity.constraints.iter().enumerate() {
        let path = format!("entities.{}.constraints[{}]", entity.name, i);
        match constraint {
            Constraint::NotNull { fields } | Constraint::Unique { fields } => {
                if fields.is_empty() {
                    errors.push(BlueprintError::Validation {
                        path: path.clone(),
                        message: "constraint 'fields' must not be empty".to_string(),
                    });
                }
                for f in fields {
                    if !field_names.contains(f.as_str()) {
                        errors.push(BlueprintError::Validation {
                            path: path.clone(),
                            message: format!(
                                "constraint references unknown field '{f}'"
                            ),
                        });
                    }
                }
            }
            Constraint::Range { field, .. } => {
                if !field_names.contains(field.as_str()) {
                    errors.push(BlueprintError::Validation {
                        path: path.clone(),
                        message: format!(
                            "constraint references unknown field '{field}'"
                        ),
                    });
                }
            }
            Constraint::Check { .. } => {
                // Check expression validation is handled elsewhere
            }
        }
    }
}

/// Validate nested object field constraints.
/// Object fields must have sub-fields, must not have their own generator,
/// and nested fields cannot be primary keys or actor columns.
fn validate_object_field(path: &str, field: &Field, errors: &mut Vec<BlueprintError>) {
    if field.data_type == DataType::Object {
        if field.fields.is_empty() {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: "object field must have at least one sub-field".to_string(),
            });
        }
        if field.generator.is_some() {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: "object field must not have its own generator; \
                          sub-fields define their own generators"
                    .to_string(),
            });
        }
        // Validate sub-fields recursively
        let mut seen = HashSet::new();
        for sub in &field.fields {
            if !seen.insert(&sub.name) {
                errors.push(BlueprintError::Validation {
                    path: format!("{}.fields.{}", path, sub.name),
                    message: format!("duplicate sub-field name '{}'", sub.name),
                });
            }
            // Disallow primary_key and actor_column in nested fields
            if sub.primary_key == Some(true) {
                errors.push(BlueprintError::Validation {
                    path: format!("{}.fields.{}", path, sub.name),
                    message: "primary_key is not allowed on nested object fields".to_string(),
                });
            }
            if sub.actor_column {
                errors.push(BlueprintError::Validation {
                    path: format!("{}.fields.{}", path, sub.name),
                    message: "actor_column is not allowed on nested object fields".to_string(),
                });
            }
            // Restrict nested generator kinds (no FK, graph_target, etc.)
            if let Some(gen) = &sub.generator {
                validate_nested_generator(
                    &format!("{}.fields.{}.generator", path, sub.name),
                    gen,
                    errors,
                );
                // Also validate generator parameters (distribution params, etc.)
                validate_generator_params(
                    &format!("{}.fields.{}.generator", path, sub.name),
                    gen,
                    &sub.data_type,
                    errors,
                );
            }
            // Validate null spec semantics
            validate_null_spec(
                &format!("{}.fields.{}.nullable", path, sub.name),
                &sub.nullable,
                errors,
            );
            // precision is only meaningful for float types
            if sub.precision.is_some() && sub.data_type != DataType::Float {
                errors.push(BlueprintError::Validation {
                    path: format!("{}.fields.{}.precision", path, sub.name),
                    message: "precision is only valid for float64 fields".to_string(),
                });
            }
            // Recurse for deeper nesting
            validate_object_field(
                &format!("{}.fields.{}", path, sub.name),
                sub,
                errors,
            );
        }
    } else if !field.fields.is_empty() {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: format!(
                "non-object field '{}' (type={}) must not have sub-fields",
                field.name, field.data_type
            ),
        });
    }
}

/// Validate that a nested generator is one of the allowed simple types.
fn validate_nested_generator(path: &str, gen: &GeneratorSpec, errors: &mut Vec<BlueprintError>) {
    let allowed = matches!(
        gen,
        GeneratorSpec::Distribution { .. }
            | GeneratorSpec::Faker { .. }
            | GeneratorSpec::Constant { .. }
            | GeneratorSpec::Sequence { .. }
            | GeneratorSpec::OneOf { .. }
            | GeneratorSpec::UuidGen { .. }
    );
    if !allowed {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: "nested object fields only support simple generators: \
                      distribution, faker, constant, sequence, one_of, uuid"
                .to_string(),
        });
    }
}

/// Validate generator parameter semantics (distribution params, type compatibility, etc.)
/// for nested object sub-fields. This checks the same parameter rules as `validate_generator`
/// but without entity-level context (FK, lookup, derived, etc.).
fn validate_generator_params(
    path: &str,
    gen: &GeneratorSpec,
    data_type: &DataType,
    errors: &mut Vec<BlueprintError>,
) {
    // Check generator ↔ field type compatibility
    if let Some(msg) = check_generator_type_compat(gen, data_type) {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: msg,
        });
    }
    match gen {
        GeneratorSpec::Distribution { spec } => {
            validate_distribution(path, spec, errors);
        }
        GeneratorSpec::Sequence {
            step,
            values,
            cycle,
            prefix,
            start,
            jitter: _,
        } => {
            validate_sequence_params(path, start, step, values, cycle, prefix, errors);
        }
        GeneratorSpec::OneOf { choices } => {
            if choices.is_empty() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "oneOf requires at least one choice".to_string(),
                });
            }
        }
        GeneratorSpec::Faker { method, .. } => {
            let bare = method.split_once('.').map(|(_, m)| m).unwrap_or(method.as_str());
            if !KNOWN_FAKER_METHODS.contains(&bare) {
                errors.push(BlueprintError::Validation {
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
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!("only UUID version 4 is supported, got {}", version),
                });
            }
        }
        _ => {} // Other generator types are rejected by validate_nested_generator
    }
}

fn validate_null_spec(path: &str, spec: &NullSpec, errors: &mut Vec<BlueprintError>) {
    match spec {
        NullSpec::Probability(p) => {
            if !(*p >= 0.0 && *p <= 1.0) {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!("null probability must be in [0, 1], got {}", p),
                });
            }
        }
        NullSpec::Pattern { every_n } => {
            if *every_n == 0 {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "null pattern every_n must be > 0".to_string(),
                });
            }
        }
        _ => {}
    }
}

fn validate_count_spec(path: &str, count: &CountSpec, errors: &mut Vec<BlueprintError>) {
    match count {
        CountSpec::Fixed(0) => {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: "count must be > 0".to_string(),
            });
        }
        CountSpec::Range { min, max } => {
            if min > max {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!("range requires min <= max, got min={}, max={}", min, max),
                });
            }
        }
        CountSpec::Expression { expr } => {
            if expr.trim().is_empty() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "count expression must not be empty".to_string(),
                });
                return;
            }
            // Validate that the expression parses and contains only allowed constructs.
            match crate::gen::expr::parser::parse(expr) {
                Ok(ast) => {
                    if let Err(msg) = crate::plan::partition::validate_count_ast(&ast) {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: msg,
                        });
                    }
                }
                Err(e) => {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!("count expression parse error: {}", e.message),
                    });
                }
            }
        }
        CountSpec::Distribution(spec) => {
            validate_distribution(path, spec, errors);
        }
        _ => {}
    }
}

fn validate_entity_count(entity: &Entity, errors: &mut Vec<BlueprintError>) {
    validate_count_spec(
        &format!("entities.{}.count", entity.name),
        &entity.count,
        errors,
    );
}

fn validate_activity_count(entity: &Entity, model: &DataModel, errors: &mut Vec<BlueprintError>) {
    let ac = match &entity.activity_count {
        Some(ac) => ac,
        None => return,
    };

    let path = format!("entities.{}.activity_count", entity.name);

    // Cannot use activity_count on actor entities themselves (would cause
    // stale actor-pool counts since pools are built before row overrides).
    if entity.actor {
        errors.push(BlueprintError::Validation {
            path: path.clone(),
            message: "activity_count cannot be used on actor entities".to_string(),
        });
        return;
    }

    // actor_field must reference an existing field in this entity
    if !entity.fields.iter().any(|f| f.name == ac.actor_field) {
        errors.push(BlueprintError::Validation {
            path: path.clone(),
            message: format!(
                "actor_field '{}' not found in entity '{}'",
                ac.actor_field, entity.name
            ),
        });
    }

    // trait_name must not be empty
    if ac.trait_name.is_empty() {
        errors.push(BlueprintError::Validation {
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
            errors.push(BlueprintError::Validation {
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
                    errors.push(BlueprintError::Validation {
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
                    errors.push(BlueprintError::Validation {
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

fn validate_sequence_params(
    path: &str,
    start: &IntOrString,
    step: &IntOrString,
    values: &Option<Vec<String>>,
    cycle: &Option<bool>,
    prefix: &Option<String>,
    errors: &mut Vec<BlueprintError>,
) {
    // Extract i64 values from IntOrString if possible
    let start_val = match start {
        IntOrString::Int(n) => Some(*n),
        IntOrString::Str(_) => None,
    };
    let step_val = match step {
        IntOrString::Int(n) => Some(*n),
        IntOrString::Str(_) => None,
    };

    if let Some(vals) = values {
        if vals.is_empty() {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: "sequence values must not be empty".to_string(),
            });
        }
        if start_val != Some(0) || step_val != Some(1) {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: "sequence 'values' is mutually exclusive with 'start'/'step'".to_string(),
            });
        }
        if prefix.is_some() {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: "sequence 'prefix' is not valid with 'values'".to_string(),
            });
        }
        if *cycle == Some(false) {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: "sequence values always cycle; 'cycle = false' is not supported"
                    .to_string(),
            });
        }
    } else {
        if step_val == Some(0) {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: "sequence step must not be 0".to_string(),
            });
        }
        // Validate temporal string formats
        if let IntOrString::Str(s) = start {
            if !is_valid_temporal_start(s) {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "sequence start '{}' is not a valid date or datetime; expected format like '2024-01-01' or '2024-01-01T08:00:00'",
                        s
                    ),
                });
            }
        }
        if let IntOrString::Str(s) = step {
            let ms = crate::gen::generators::event_stream::parse_duration_ms(s);
            if ms == 0 {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "sequence step '{}' is not a valid duration; expected format like '1d', '1h', '30m', '500ms'",
                        s
                    ),
                });
            }
        }
        if cycle.is_some() {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: "sequence 'cycle' requires 'values'".to_string(),
            });
        }
    }
}

/// Check if a string is a valid temporal start value (date, datetime, or RFC3339).
fn is_valid_temporal_start(s: &str) -> bool {
    use chrono::{NaiveDate, NaiveDateTime, DateTime};
    let s = s.trim();
    DateTime::parse_from_rfc3339(s).is_ok()
        || NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").is_ok()
        || NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").is_ok()
        || NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}

/// Validate the offset of a relative generator.
///
/// Checks that:
/// - Distribution offsets use an allowed distribution kind (normal, log_normal,
///   uniform, exponential)
/// - Duration strings in min/max/value are valid
/// - min ≤ max when both are specified
/// - Constant offsets have a valid duration string
fn validate_relative_offset(offset: &RelativeOffset, path: &str, errors: &mut Vec<BlueprintError>) {
    use crate::gen::generators::event_stream::parse_duration_ms;

    /// Check that a duration string uses a known unit suffix.
    fn is_valid_duration_string(s: &str) -> bool {
        let s = s.trim();
        if s.is_empty() {
            return false;
        }
        // Pure numeric is valid (interpreted as ms)
        if s.chars().all(|c| c.is_ascii_digit() || c == '.') {
            return true;
        }
        // Must have a numeric prefix + known unit suffix
        let idx = match s.find(|c: char| c.is_alphabetic()) {
            Some(i) => i,
            None => return false,
        };
        if idx == 0 {
            return false; // no numeric prefix
        }
        let unit = &s[idx..];
        matches!(
            unit.to_lowercase().as_str(),
            "ms" | "millisecond"
                | "milliseconds"
                | "s"
                | "sec"
                | "second"
                | "seconds"
                | "m"
                | "min"
                | "minute"
                | "minutes"
                | "h"
                | "hr"
                | "hour"
                | "hours"
                | "d"
                | "day"
                | "days"
                | "w"
                | "week"
                | "weeks"
        )
    }

    match offset {
        RelativeOffset::Simple(val) => {
            // Only numeric values are valid for simple offsets
            match val {
                Value::Int(_) | Value::Float(_) => {}
                _ => {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "relative offset must be numeric (int or float), got {:?}; \
                             for structured offsets use {{ distribution = \"...\", ... }} \
                             or {{ type = \"constant\", value = \"...\" }}",
                            val
                        ),
                    });
                }
            }
        }
        RelativeOffset::Distribution {
            distribution,
            min,
            max,
            ..
        } => {
            // Only allow scalar continuous distributions for offsets
            match distribution {
                DistributionKind::Normal
                | DistributionKind::LogNormal
                | DistributionKind::Uniform
                | DistributionKind::Exponential => {}
                other => {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "relative offset distribution '{:?}' is not supported; \
                             use normal, log_normal, uniform, or exponential",
                            other
                        ),
                    });
                }
            }
            // Validate duration strings
            if let Some(min_str) = min {
                if !is_valid_duration_string(min_str) {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "relative offset min '{}' is not a valid duration \
                             (use e.g. \"1d\", \"30m\", \"2h\")",
                            min_str
                        ),
                    });
                }
            }
            if let Some(max_str) = max {
                if !is_valid_duration_string(max_str) {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "relative offset max '{}' is not a valid duration \
                             (use e.g. \"14d\", \"4h\", \"1000ms\")",
                            max_str
                        ),
                    });
                }
            }
            // Check min <= max
            if let (Some(min_str), Some(max_str)) = (min, max) {
                let min_ms = parse_duration_ms(min_str);
                let max_ms = parse_duration_ms(max_str);
                if min_ms > max_ms && min_ms > 0 && max_ms > 0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "relative offset min '{}' ({}ms) exceeds max '{}' ({}ms)",
                            min_str, min_ms, max_str, max_ms
                        ),
                    });
                }
            }
        }
        RelativeOffset::Constant { offset_type, value } => {
            if offset_type != "constant" {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "relative offset type '{}' is not recognized; use 'constant'",
                        offset_type
                    ),
                });
            }
            if !is_valid_duration_string(value) {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "relative constant offset '{}' is not a valid duration \
                         (use e.g. \"365d\", \"1h\", \"30m\")",
                        value
                    ),
                });
            }
        }
    }
}

fn validate_distribution(path: &str, spec: &DistributionSpec, errors: &mut Vec<BlueprintError>) {
    let params = &spec.params;
    match spec.kind {
        DistributionKind::Normal => {
            if !params.contains_key("mean") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "normal distribution requires 'mean' param".to_string(),
                });
            }
            if !params.contains_key("std_dev") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "normal distribution requires 'std_dev' param".to_string(),
                });
            } else if let Some(&sd) = params.get("std_dev") {
                if sd <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "normal distribution 'std_dev' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Uniform => {
            if !params.contains_key("min") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "uniform distribution requires 'min' param".to_string(),
                });
            }
            if !params.contains_key("max") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "uniform distribution requires 'max' param".to_string(),
                });
            }
            if let (Some(&min), Some(&max)) = (params.get("min"), params.get("max")) {
                if min >= max {
                    errors.push(BlueprintError::Validation {
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
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "exponential distribution requires 'lambda' param".to_string(),
                });
            } else if let Some(&l) = params.get("lambda") {
                if l <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "exponential distribution 'lambda' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Poisson => {
            if !params.contains_key("lambda") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "poisson distribution requires 'lambda' param".to_string(),
                });
            } else if let Some(&l) = params.get("lambda") {
                if l <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "poisson distribution 'lambda' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Bernoulli => {
            if !params.contains_key("p") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "bernoulli distribution requires 'p' param".to_string(),
                });
            } else if let Some(&p) = params.get("p") {
                if !(0.0..=1.0).contains(&p) {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "bernoulli distribution 'p' must be in [0, 1]".to_string(),
                    });
                }
            }
        }
        DistributionKind::LogNormal => {
            if !params.contains_key("mu") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "lognormal distribution requires 'mu' param".to_string(),
                });
            }
            if !params.contains_key("sigma") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "lognormal distribution requires 'sigma' param".to_string(),
                });
            } else if let Some(&s) = params.get("sigma") {
                if s <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "lognormal distribution 'sigma' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Binomial => {
            if !params.contains_key("n") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "binomial distribution requires 'n' param".to_string(),
                });
            } else if let Some(&n) = params.get("n") {
                if n < 0.0 || n.fract() != 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "binomial distribution 'n' must be >= 0 and integer-valued"
                            .to_string(),
                    });
                }
            }
            if !params.contains_key("p") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "binomial distribution requires 'p' param".to_string(),
                });
            } else if let Some(&p) = params.get("p") {
                if !(0.0..=1.0).contains(&p) {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "binomial distribution 'p' must be in [0, 1]".to_string(),
                    });
                }
            }
        }
        DistributionKind::Geometric => {
            if !params.contains_key("p") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "geometric distribution requires 'p' param".to_string(),
                });
            } else if let Some(&p) = params.get("p") {
                if p <= 0.0 || p > 1.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "geometric distribution 'p' must be in (0, 1]".to_string(),
                    });
                }
            }
        }
        DistributionKind::Pareto => {
            if !params.contains_key("scale") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "pareto distribution requires 'scale' param".to_string(),
                });
            } else if let Some(&v) = params.get("scale") {
                if v <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "pareto distribution 'scale' must be > 0".to_string(),
                    });
                }
            }
            if !params.contains_key("shape") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "pareto distribution requires 'shape' param".to_string(),
                });
            } else if let Some(&v) = params.get("shape") {
                if v <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "pareto distribution 'shape' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Weibull => {
            if !params.contains_key("scale") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "weibull distribution requires 'scale' param".to_string(),
                });
            } else if let Some(&v) = params.get("scale") {
                if v <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "weibull distribution 'scale' must be > 0".to_string(),
                    });
                }
            }
            if !params.contains_key("shape") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "weibull distribution requires 'shape' param".to_string(),
                });
            } else if let Some(&v) = params.get("shape") {
                if v <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "weibull distribution 'shape' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Gamma => {
            if !params.contains_key("shape") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "gamma distribution requires 'shape' param".to_string(),
                });
            } else if let Some(&v) = params.get("shape") {
                if v <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "gamma distribution 'shape' must be > 0".to_string(),
                    });
                }
            }
            if !params.contains_key("scale") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "gamma distribution requires 'scale' param".to_string(),
                });
            } else if let Some(&v) = params.get("scale") {
                if v <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "gamma distribution 'scale' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Beta => {
            if !params.contains_key("alpha") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "beta distribution requires 'alpha' param".to_string(),
                });
            } else if let Some(&v) = params.get("alpha") {
                if v <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "beta distribution 'alpha' must be > 0".to_string(),
                    });
                }
            }
            if !params.contains_key("beta") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "beta distribution requires 'beta' param".to_string(),
                });
            } else if let Some(&v) = params.get("beta") {
                if v <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "beta distribution 'beta' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Cauchy => {
            if !params.contains_key("median") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "cauchy distribution requires 'median' param".to_string(),
                });
            }
            if !params.contains_key("scale") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "cauchy distribution requires 'scale' param".to_string(),
                });
            } else if let Some(&v) = params.get("scale") {
                if v <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "cauchy distribution 'scale' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::ChiSquared => {
            if !params.contains_key("k") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "chi-squared distribution requires 'k' param".to_string(),
                });
            } else if let Some(&v) = params.get("k") {
                if v <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "chi-squared distribution 'k' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::StudentT => {
            if !params.contains_key("n") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "student-t distribution requires 'n' param".to_string(),
                });
            } else if let Some(&v) = params.get("n") {
                if v <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "student-t distribution 'n' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Triangular => {
            if !params.contains_key("min") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "triangular distribution requires 'min' param".to_string(),
                });
            }
            if !params.contains_key("max") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "triangular distribution requires 'max' param".to_string(),
                });
            }
            if !params.contains_key("mode") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "triangular distribution requires 'mode' param".to_string(),
                });
            }
            if let (Some(&min), Some(&max)) = (params.get("min"), params.get("max")) {
                if min >= max {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "triangular distribution requires min < max, got min={}, max={}",
                            min, max
                        ),
                    });
                }
                if let Some(&mode) = params.get("mode") {
                    if mode < min || mode > max {
                        errors.push(BlueprintError::Validation {
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
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "zipf distribution requires 'n' param".to_string(),
                });
            } else if let Some(&n) = params.get("n") {
                if n < 1.0 || n.fract() != 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "zipf distribution 'n' must be >= 1 and integer-valued"
                            .to_string(),
                    });
                }
            }
            if !params.contains_key("s") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "zipf distribution requires 's' param".to_string(),
                });
            } else if let Some(&s) = params.get("s") {
                if s <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "zipf distribution 's' must be > 0".to_string(),
                    });
                }
            }
        }
        DistributionKind::Dirichlet => {
            let alpha = spec.array_params.get("alpha");
            match alpha {
                None => {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "dirichlet distribution requires 'alpha' in array_params"
                            .to_string(),
                    });
                }
                Some(a) => {
                    if a.len() < 2 {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: "dirichlet distribution 'alpha' must have at least 2 elements"
                                .to_string(),
                        });
                    }
                    for (i, &v) in a.iter().enumerate() {
                        if !v.is_finite() || v <= 0.0 {
                            errors.push(BlueprintError::Validation {
                                path: path.to_string(),
                                message: format!(
                                    "dirichlet distribution 'alpha[{}]' must be a finite value > 0, got {}",
                                    i, v
                                ),
                            });
                        }
                    }
                }
            }
            if spec.round {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "dirichlet distribution does not support 'round'".to_string(),
                });
            }
        }
        DistributionKind::Multinomial => {
            if !spec.params.contains_key("n") {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "multinomial distribution requires 'n' param".to_string(),
                });
            } else if let Some(&n) = spec.params.get("n") {
                if !n.is_finite() || n < 1.0 || n.fract() != 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "multinomial distribution 'n' must be a positive integer"
                            .to_string(),
                    });
                }
            }
            let p = spec.array_params.get("p");
            match p {
                None => {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "multinomial distribution requires 'p' in array_params"
                            .to_string(),
                    });
                }
                Some(probs) => {
                    if probs.len() < 2 {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: "multinomial distribution 'p' must have at least 2 elements"
                                .to_string(),
                        });
                    }
                    for (i, &v) in probs.iter().enumerate() {
                        if !v.is_finite() || v < 0.0 {
                            errors.push(BlueprintError::Validation {
                                path: path.to_string(),
                                message: format!(
                                    "multinomial distribution 'p[{}]' must be a finite value >= 0, got {}",
                                    i, v
                                ),
                            });
                        }
                    }
                    let sum: f64 = probs.iter().sum();
                    if (sum - 1.0).abs() > 0.01 {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!(
                                "multinomial distribution 'p' must sum to ~1.0, got {}",
                                sum
                            ),
                        });
                    }
                }
            }
        }
    }
}

const KNOWN_FAKER_METHODS: &[&str] = &[
    // Person
    "first_name",
    "last_name",
    "full_name",
    "name",
    "username",
    "prefix",
    "name_prefix",
    "suffix",
    "name_suffix",
    // Internet
    "email",
    "url",
    "domain",
    "ipv4",
    "ip_address",
    "ipv6",
    "mac_address",
    "mac",
    "user_agent",
    // Address
    "address",
    "street_address",
    "street",
    "city",
    "city_name",
    "state",
    "country",
    "country_code",
    "zip_code",
    "zipcode",
    "postal_code",
    // Company
    "company",
    "industry",
    "catch_phrase",
    "catchphrase",
    "bs",
    "buzzword",
    // Finance
    "credit_card",
    "credit_card_number",
    "iban",
    "bic",
    "swift",
    "currency_code",
    "currency",
    // Phone
    "phone",
    // Lorem
    "word",
    "sentence",
    "paragraph",
    "title",
    // Datetime
    "date",
    "datetime",
    "timestamp",
    "time",
    "month",
    "day_of_week",
    "weekday",
    "timezone",
    "tz",
    // Color
    "color",
    "hex_color",
    // File
    "file_extension",
    "extension",
    "mime_type",
    "content_type",
    "file_name",
    "file_path",
    // Geo
    "latitude",
    "lat",
    "longitude",
    "lon",
    "lng",
    "coordinate",
    "geo",
    // Vehicle
    "license_plate",
    "plate",
    "vin",
    "vehicle_make",
    "make",
    "vehicle_model",
    "model",
    // Medical
    "blood_type",
    // Barcode
    "ean13",
    "isbn13",
    "isbn",
    // Product
    "product_name",
    "product",
    // Other
    "hex_string",
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
        GeneratorSpec::Distribution { spec } => {
            let compatible = match &spec.kind {
                DistributionKind::Dirichlet | DistributionKind::Multinomial => {
                    matches!(data_type, DataType::Array)
                }
                _ => matches!(
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
                ),
            };
            if !compatible {
                let expected = match &spec.kind {
                    DistributionKind::Dirichlet | DistributionKind::Multinomial => {
                        "expected 'array' for vector-valued distribution"
                    }
                    _ => "expected a numeric type (int, int32, float), bool, or temporal type",
                };
                Some(format!(
                    "distribution generator ({}) is not compatible with data_type '{}'; {}",
                    spec.kind, data_type, expected
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
        GeneratorSpec::Sequence { values, start, .. } => {
            if values.is_some() {
                // Cyclic values mode always produces strings
                if *data_type != DataType::String {
                    Some(format!(
                        "sequence with 'values' requires data_type 'string', got '{}'",
                        data_type
                    ))
                } else {
                    None
                }
            } else if start.as_str().is_some() {
                // Temporal sequence — must target a temporal type
                let compatible = matches!(
                    data_type,
                    DataType::Datetime | DataType::DatetimeUs | DataType::Datetimetz
                        | DataType::Date | DataType::Time
                );
                if !compatible {
                    Some(format!(
                        "temporal sequence (string start) is not compatible with data_type '{}'; expected a temporal type",
                        data_type
                    ))
                } else {
                    None
                }
            } else {
                let compatible = matches!(
                    data_type,
                    DataType::Int | DataType::Int32 | DataType::String
                        | DataType::Datetime | DataType::DatetimeUs | DataType::Datetimetz
                        | DataType::Date | DataType::Time
                );
                if !compatible {
                    Some(format!(
                        "sequence generator is not compatible with data_type '{}'; expected 'int', 'int32', 'string', or a temporal type",
                        data_type
                    ))
                } else {
                    None
                }
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
    errors: &mut Vec<BlueprintError>,
) {
    // Check generator ↔ field type compatibility
    if let Some(msg) = check_generator_type_compat(gen, data_type) {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: msg,
        });
    }

    match gen {
        GeneratorSpec::Distribution { spec } => {
            validate_distribution(path, spec, errors);
        }
        GeneratorSpec::Sequence {
            step,
            values,
            cycle,
            prefix,
            start,
            jitter: _,
        } => {
            validate_sequence_params(path, start, step, values, cycle, prefix, errors);
        }
        GeneratorSpec::OneOf { choices } => {
            if choices.is_empty() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "oneOf requires at least one choice".to_string(),
                });
            }
        }
        GeneratorSpec::Faker { method, .. } => {
            let bare = method.split_once('.').map(|(_, m)| m).unwrap_or(method.as_str());
            if !KNOWN_FAKER_METHODS.contains(&bare) {
                errors.push(BlueprintError::Validation {
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
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!("only UUID version 4 is supported, got {}", version),
                });
            }
        }
        GeneratorSpec::BusinessHours {
            start_hour,
            end_hour,
            exclude_weekends,
            timezone,
            timezone_field,
            date_range,
            exclude_dates,
            days,
            ..
        } => {
            if *start_hour > 23 {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "business_hours start_hour must be in [0, 23]".to_string(),
                });
            }
            if *end_hour < 1 || *end_hour > 24 {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "business_hours end_hour must be in [1, 24]".to_string(),
                });
            }
            if *start_hour >= *end_hour {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "business_hours requires start_hour < end_hour".to_string(),
                });
            }
            // timezone and timezone_field are mutually exclusive
            if timezone.is_some() && timezone_field.is_some() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "business_hours: 'timezone' and 'timezone_field' are mutually exclusive".to_string(),
                });
            }
            // Validate timezone is a valid IANA timezone
            if let Some(tz) = timezone {
                if tz.parse::<chrono_tz::Tz>().is_err() {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!("business_hours: invalid timezone '{}'", tz),
                    });
                }
            }
            // timezone_field must reference an existing field in the same entity
            if let Some(ref tz_f) = timezone_field {
                // Path format: "entities.<entity_name>.fields.<field_name>.generator"
                // Extract entity name from path
                if let Some(entity_name) = path.strip_prefix("entities.").and_then(|p| p.split('.').next()) {
                    if let Some(current_entity) = model.entities.iter().find(|e| e.name == entity_name) {
                        let field_names: HashSet<&str> = current_entity
                            .fields.iter().map(|f| f.name.as_str()).collect();
                        if !field_names.contains(tz_f.as_str()) {
                            errors.push(BlueprintError::Validation {
                                path: path.to_string(),
                                message: format!("business_hours: timezone_field '{}' not found in entity", tz_f),
                            });
                        }
                    }
                }
            }
            // days and exclude_weekends=true are mutually exclusive
            if let Some(ref d) = days {
                if *exclude_weekends {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "business_hours: 'days' and 'exclude_weekends=true' are mutually exclusive".to_string(),
                    });
                }
                // Validate day names
                let valid_days = [
                    "monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday",
                    "mon", "tue", "wed", "thu", "fri", "sat", "sun",
                ];
                for day in d {
                    if !valid_days.contains(&day.to_lowercase().as_str()) {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!("business_hours: invalid day name '{}'", day),
                        });
                    }
                }
                if d.is_empty() {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "business_hours: 'days' must not be empty".to_string(),
                    });
                }
            }
            // Validate date_range
            if let Some(ref dr) = date_range {
                let min_parsed = chrono::NaiveDate::parse_from_str(&dr.min, "%Y-%m-%d");
                let max_parsed = chrono::NaiveDate::parse_from_str(&dr.max, "%Y-%m-%d");
                match (min_parsed, max_parsed) {
                    (Err(_), _) => {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!("business_hours: invalid date_range.min '{}'", dr.min),
                        });
                    }
                    (_, Err(_)) => {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!("business_hours: invalid date_range.max '{}'", dr.max),
                        });
                    }
                    (Ok(min_d), Ok(max_d)) => {
                        if min_d > max_d {
                            errors.push(BlueprintError::Validation {
                                path: path.to_string(),
                                message: "business_hours: date_range.min must be before date_range.max".to_string(),
                            });
                        }
                    }
                }
            }
            // Validate exclude_dates are valid ISO dates
            if !exclude_dates.is_empty() {
                for date_str in exclude_dates {
                    if chrono::NaiveDate::parse_from_str(date_str, "%Y-%m-%d").is_err() {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!("business_hours: invalid exclude_date '{}'", date_str),
                        });
                    }
                }
            }
        }
        GeneratorSpec::Lookup {
            entity: ref lookup_entity,
            field: ref lookup_field,
        } => {
            if nested {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "lookup cannot be nested inside Unique, Conditional, or Composite"
                        .to_string(),
                });
            }
            if !entity_names.contains(lookup_entity.as_str()) {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!("lookup references unknown entity '{}'", lookup_entity),
                });
            } else {
                // Entity exists — check field exists on it
                if let Some(target_entity) = model
                    .entities
                    .iter()
                    .find(|e| e.name == *lookup_entity)
                {
                    let target_fields: HashSet<&str> = target_entity
                        .fields
                        .iter()
                        .map(|f| f.name.as_str())
                        .collect();
                    if !target_fields.contains(lookup_field.as_str()) {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!(
                                "lookup references unknown field '{}' on entity '{}'",
                                lookup_field, lookup_entity
                            ),
                        });
                    }
                }
            }
        }
        GeneratorSpec::ExternalLookup {
            source,
            sampling,
            weight_column,
            ..
        } => {
            if nested {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "external_lookup cannot be nested inside Unique, Conditional, or Composite"
                        .to_string(),
                });
            }
            if source.is_empty() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "external_lookup source path must not be empty".to_string(),
                });
            }
            if *sampling == crate::core::SamplingMode::Weighted && weight_column.is_none() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "external_lookup with weighted sampling requires weight_column"
                        .to_string(),
                });
            }
            if *sampling != crate::core::SamplingMode::Weighted && weight_column.is_some() {
                errors.push(BlueprintError::Validation {
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
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "conditional references unknown field '{}' in entity '{}'",
                        field, entity.name
                    ),
                });
            }
            if field == field_name {
                errors.push(BlueprintError::Validation {
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
        GeneratorSpec::Relative { anchor, offset } => {
            let field_names: HashSet<&str> =
                entity.fields.iter().map(|f| f.name.as_str()).collect();
            if !field_names.contains(anchor.as_str()) {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "relative references unknown field '{}' in entity '{}'",
                        anchor, entity.name
                    ),
                });
            }
            if anchor == field_name {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "relative cannot reference itself".to_string(),
                });
            }
            validate_relative_offset(offset, path, errors);
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
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "dictionary generator requires a non-empty 'file' path".to_string(),
                });
            }
            let valid_expansions = ["sample", "combinatorial", "suffix"];
            if !valid_expansions.contains(&expansion.as_str()) {
                errors.push(BlueprintError::Validation {
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
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!("actor_ref references unknown entity '{}'", actor_entity),
                });
            } else if let Some(target) = model.entities.iter().find(|e| e.name == *actor_entity) {
                if !target.actor {
                    errors.push(BlueprintError::Validation {
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
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "relationship_ref requires a non-empty 'relationship' name"
                        .to_string(),
                });
            } else if !model
                .actor_relationships
                .iter()
                .any(|ar| ar.name == *relationship)
            {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "relationship_ref references unknown actor_relationship '{}'",
                        relationship
                    ),
                });
            }
            if let Some(src) = source_field {
                if src.is_empty() {
                    errors.push(BlueprintError::Validation {
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
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "actor_temporal requires a non-empty 'trait' name".to_string(),
                });
            }
            if let Some(ta) = temporal_after {
                // Validate referenced entity exists
                if !entity_names.contains(ta.entity.as_str()) {
                    errors.push(BlueprintError::Validation {
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
                                errors.push(BlueprintError::Validation {
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
                                    errors.push(BlueprintError::Validation {
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
                    errors.push(BlueprintError::Validation {
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
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "burst.avg_events must be a finite number > 0".to_string(),
                    });
                }
                if !b.avg_gap_minutes.is_finite() || b.avg_gap_minutes <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "burst.avg_gap_minutes must be a finite number > 0".to_string(),
                    });
                }
                if !b.avg_idle_hours.is_finite() || b.avg_idle_hours <= 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "burst.avg_idle_hours must be a finite number > 0".to_string(),
                    });
                }
            }
        }
        GeneratorSpec::PersonaField { trait_name } => {
            if trait_name.is_empty() {
                errors.push(BlueprintError::Validation {
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
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "thread_ref cannot be nested inside Unique, Conditional, or Composite"
                        .to_string(),
                });
            }
            if *reply_window == 0 {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "thread_ref reply_window must be at least 1".to_string(),
                });
            }
            if !reply_probability.is_finite()
                || *reply_probability < 0.0
                || *reply_probability > 1.0
            {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "thread_ref reply_probability must be in [0.0, 1.0], got {reply_probability}"
                    ),
                });
            }
            if *max_depth == 0 {
                errors.push(BlueprintError::Validation {
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
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "thread_ref requires entity PK to be 'int' (Int64), but '{}' has data_type '{:?}'",
                            pk.name,
                            pk.data_type
                        ),
                    });
                }
            } else {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "thread_ref requires entity to have a primary_key field".to_string(),
                });
            }
        }
        GeneratorSpec::Plugin { name, .. } => {
            if name.is_empty() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "plugin generator requires a non-empty 'name'".to_string(),
                });
            }
        }
        // ─── Derived expression validation ─────────────────────────────
        GeneratorSpec::Derived { expr: ref expr_str } => {
            // Try parsing as expression; fall back to legacy template check.
            match expr::parser::parse(expr_str) {
                Ok(ast) => {
                    // Expression parsed successfully — validate field references
                    let refs = expr::ast::extract_field_refs(&ast);
                    let field_names: HashSet<&str> =
                        entity.fields.iter().map(|f| f.name.as_str()).collect();

                    for r in &refs {
                        // Skip parameter substitutions (e.g. ${param.prefix})
                        if r.starts_with("param.") {
                            continue;
                        }
                        if !field_names.contains(r.as_str()) {
                            errors.push(BlueprintError::Validation {
                                path: path.to_string(),
                                message: format!(
                                    "derived expression references unknown field '{}' \
                                     in entity '{}'",
                                    r, entity.name
                                ),
                            });
                        }
                    }

                    // Self-reference check
                    if refs.iter().any(|r| r == field_name) {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: "derived expression references itself".to_string(),
                        });
                    }
                }
                Err(_) => {
                    // Parse failed — check if it's a valid legacy template
                    if !expr::parser::is_legacy_template(expr_str) {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!(
                                "derived expression is not a valid expression or \
                                 legacy template: {}",
                                expr_str
                            ),
                        });
                    } else {
                        // Legacy template — still validate field references
                        let field_names: HashSet<&str> =
                            entity.fields.iter().map(|f| f.name.as_str()).collect();
                        let mut template_refs = Vec::new();
                        extract_template_refs(expr_str, &mut template_refs);

                        for r in &template_refs {
                            // Skip parameter substitutions (e.g. ${param.prefix})
                            if r.starts_with("param.") {
                                continue;
                            }
                            if !field_names.contains(*r) {
                                errors.push(BlueprintError::Validation {
                                    path: path.to_string(),
                                    message: format!(
                                        "legacy template references unknown field '{}' \
                                         in entity '{}'",
                                        r, entity.name
                                    ),
                                });
                            }
                        }

                        // Self-reference check for legacy templates
                        if template_refs.contains(&field_name) {
                            errors.push(BlueprintError::Validation {
                                path: path.to_string(),
                                message: "derived expression references itself".to_string(),
                            });
                        }
                    }
                }
            }
        }
        // Pattern, Constant — no additional validation needed
        _ => {}
    }

    // ─── Time series component validation ──────────────────────────
    if let GeneratorSpec::TimeSeries {
        components,
        min,
        max,
        timestamp_field,
        ..
    } = gen
    {
        // Validate min < max
        if let (Some(mn), Some(mx)) = (min, max) {
            if mn >= mx {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!("time_series min ({}) must be less than max ({})", mn, mx),
                });
            }
        }

        // Check that timestamp_field exists in entity if calendar components are used
        let has_calendar = components.iter().any(|c| {
            matches!(
                c,
                crate::core::TimeSeriesComponent::WeekendEffect { .. }
                    | crate::core::TimeSeriesComponent::BusinessHoursEffect { .. }
                    | crate::core::TimeSeriesComponent::HolidayEffect { .. }
            )
        });
        if has_calendar {
            if let Some(ts_name) = timestamp_field {
                if let Some(ts_field) = entity.fields.iter().find(|f| f.name == *ts_name) {
                    // Validate the referenced field is a temporal type
                    match &ts_field.data_type {
                        crate::core::DataType::Datetime
                        | crate::core::DataType::DatetimeUs
                        | crate::core::DataType::Datetimetz
                        | crate::core::DataType::Date => {}
                        other => {
                            errors.push(BlueprintError::Validation {
                                path: path.to_string(),
                                message: format!(
                                    "time_series timestamp_field '{}' has type '{:?}', \
                                     expected a temporal type (datetime, datetime_us, \
                                     datetimetz, date)",
                                    ts_name, other
                                ),
                            });
                        }
                    }
                } else {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "time_series timestamp_field '{}' not found in entity '{}'",
                            ts_name, entity.name
                        ),
                    });
                }
            } else {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "time_series with WeekendEffect, BusinessHoursEffect, or \
                              HolidayEffect requires a timestamp_field"
                        .to_string(),
                });
            }
        }

        // Validate AR coefficients
        for c in components {
            if let crate::core::TimeSeriesComponent::Autoregressive { coefficients } = c {
                if coefficients.is_empty() {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "AR coefficients must not be empty".to_string(),
                    });
                }
                let sum_abs: f64 = coefficients.iter().map(|c| c.abs()).sum();
                if sum_abs >= 1.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "AR coefficients sum(|c|) = {:.3} >= 1.0; \
                             series may be non-stationary (unstable)",
                            sum_abs
                        ),
                    });
                }
            }
            if let crate::core::TimeSeriesComponent::BusinessHoursEffect {
                start_hour,
                end_hour,
                ..
            } = c
            {
                if start_hour >= end_hour {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: format!(
                            "BusinessHoursEffect start_hour ({}) must be < end_hour ({})",
                            start_hour, end_hour
                        ),
                    });
                }
            }
            if let crate::core::TimeSeriesComponent::HolidayEffect {
                dates,
                multiplier,
            } = c
            {
                if dates.is_empty() {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "HolidayEffect dates must not be empty".to_string(),
                    });
                }
                for d in dates {
                    if chrono::NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d").is_err() {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!(
                                "HolidayEffect date '{}' is not a valid YYYY-MM-DD date",
                                d
                            ),
                        });
                    }
                }
                if *multiplier == 0.0 {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "HolidayEffect multiplier must not be zero".to_string(),
                    });
                }
            }
        }
    }

    // ─── Event stream validation ────────────────────────────────────
    if let GeneratorSpec::EventStream {
        start,
        arrival,
        components,
    } = gen
    {
        // Validate start time parses.
        if chrono::DateTime::parse_from_rfc3339(start).is_err()
            && chrono::NaiveDateTime::parse_from_str(start, "%Y-%m-%dT%H:%M:%S").is_err()
        {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: format!(
                    "event_stream start '{}' is not a valid ISO-8601 datetime",
                    start
                ),
            });
        }

        // Validate distribution.
        if arrival.distribution != "exponential" {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: format!(
                    "event_stream arrival distribution '{}' is not supported; \
                     only 'exponential' is currently supported",
                    arrival.distribution
                ),
            });
        }

        // Validate lambda parameter.
        match arrival.params.get("lambda") {
            Some(crate::core::Value::Float(f)) if *f > 0.0 => {}
            Some(crate::core::Value::Int(i)) if *i > 0 => {}
            _ => {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "event_stream arrival requires a positive 'lambda' parameter"
                        .to_string(),
                });
            }
        }

        // Validate unit.
        let valid_units = [
            "millisecond", "milliseconds", "ms", "second", "seconds", "s", "minute", "minutes",
            "m", "hour", "hours", "h", "day", "days", "d",
        ];
        if !valid_units.contains(&arrival.unit.as_str()) {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: format!(
                    "event_stream arrival unit '{}' is not valid; use one of: \
                     second, minute, hour, day, millisecond",
                    arrival.unit
                ),
            });
        }

        // Validate component parameters.
        for comp in components {
            match comp {
                crate::core::EventStreamComponent::BusinessHours {
                    active_hours,
                    ..
                } => {
                    if active_hours[0] >= active_hours[1] {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!(
                                "event_stream business_hours active_hours[0] ({}) must be < active_hours[1] ({})",
                                active_hours[0], active_hours[1]
                            ),
                        });
                    }
                }
                crate::core::EventStreamComponent::Seasonality { amplitude, .. } => {
                    if *amplitude <= 0.0 || *amplitude >= 1.0 {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!(
                                "event_stream seasonality amplitude ({}) should be in (0, 1) \
                                 to keep rate positive",
                                amplitude
                            ),
                        });
                    }
                }
                crate::core::EventStreamComponent::WeekendEffect { multiplier } => {
                    if *multiplier <= 0.0 {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!(
                                "event_stream weekend_effect multiplier ({}) must be positive",
                                multiplier
                            ),
                        });
                    }
                }
                crate::core::EventStreamComponent::HolidayEffect {
                    dates,
                    multiplier,
                } => {
                    if dates.is_empty() {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: "event_stream holiday_effect dates must not be empty"
                                .to_string(),
                        });
                    }
                    for d in dates {
                        if chrono::NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d").is_err() {
                            errors.push(BlueprintError::Validation {
                                path: path.to_string(),
                                message: format!(
                                    "event_stream holiday_effect date '{}' is not a valid \
                                     YYYY-MM-DD date",
                                    d
                                ),
                            });
                        }
                    }
                    if *multiplier <= 0.0 {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!(
                                "event_stream holiday_effect multiplier ({}) must be positive",
                                multiplier
                            ),
                        });
                    }
                }
            }
        }
    }
}

fn validate_relationships(model: &DataModel, errors: &mut Vec<BlueprintError>) {
    let names = entity_names(model);
    let entity_map: HashMap<&str, &crate::core::Entity> = model
        .entities
        .iter()
        .map(|e| (e.name.as_str(), e))
        .collect();
    let mut seen = HashSet::new();
    for rel in &model.relationships {
        let path = format!("relationships.{}", rel.name);
        if !seen.insert(&rel.name) {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!("duplicate relationship name '{}'", rel.name),
            });
        }
        if !names.contains(rel.from.as_str()) {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!("'from' references unknown entity '{}'", rel.from),
            });
        }
        if !names.contains(rel.to.as_str()) {
            errors.push(BlueprintError::Validation {
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
                errors.push(BlueprintError::Validation {
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
                errors.push(BlueprintError::Validation {
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
                            errors.push(BlueprintError::Validation {
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

        // Validate degree distribution parameters
        if let Some(ref degree) = rel.degree {
            let dp = format!("{}.degree", path);
            match degree.kind {
                crate::core::DistributionKind::Zipf => {
                    let exponent = degree
                        .params
                        .get("exponent")
                        .or_else(|| degree.params.get("s"));
                    if let Some(&s) = exponent {
                        if !s.is_finite() || s <= 0.0 {
                            errors.push(BlueprintError::Validation {
                                path: dp.clone(),
                                message: format!(
                                    "Zipf exponent must be a finite value > 0, got {s}"
                                ),
                            });
                        }
                    }
                    // exponent is optional; defaults to 1.0 at runtime
                }
                _ => {
                    // Other distribution kinds are allowed (fall back to uniform at
                    // runtime for unsupported ones) — no extra validation needed.
                }
            }
        }

        // Validate selection strategy
        if let Some(ref sel) = rel.selection {
            let sp = format!("{}.selection", path);

            // Mutual exclusivity with degree
            if rel.degree.is_some() {
                errors.push(BlueprintError::Validation {
                    path: sp.clone(),
                    message: format!(
                        "relationship '{}' specifies both 'degree' and 'selection' — they are mutually exclusive",
                        rel.name
                    ),
                });
            }

            // Validate strategy-specific parameters
            match sel {
                crate::core::SelectionStrategy::Parameterized(
                    crate::core::ParameterizedSelection::Clustered { cluster_size },
                ) => {
                    if *cluster_size == 0 {
                        errors.push(BlueprintError::Validation {
                            path: sp.clone(),
                            message: format!(
                                "relationship '{}': cluster_size must be > 0",
                                rel.name
                            ),
                        });
                    }
                }
                crate::core::SelectionStrategy::Parameterized(
                    crate::core::ParameterizedSelection::Weighted { weight_field },
                ) => {
                    // Weighted selection is not yet implemented
                    errors.push(BlueprintError::Validation {
                        path: sp.clone(),
                        message: format!(
                            "relationship '{}': weighted selection strategy is not yet implemented; use 'degree' with Zipf for non-uniform parent selection",
                            rel.name
                        ),
                    });

                    // Still validate weight_field for forward compatibility
                    if let Some(parent) = entity_map.get(rel.to.as_str()) {
                        let has_field = parent
                            .fields
                            .iter()
                            .any(|f| f.name == *weight_field);
                        if !has_field {
                            errors.push(BlueprintError::Validation {
                                path: sp.clone(),
                                message: format!(
                                    "relationship '{}': weight_field '{}' not found on parent entity '{}'",
                                    rel.name, weight_field, rel.to
                                ),
                            });
                        } else {
                            // Check that the field is numeric
                            let field = parent
                                .fields
                                .iter()
                                .find(|f| f.name == *weight_field)
                                .unwrap();
                            let is_numeric = matches!(
                                field.data_type,
                                crate::core::DataType::Int
                                    | crate::core::DataType::Int32
                                    | crate::core::DataType::Float
                            );
                            if !is_numeric {
                                errors.push(BlueprintError::Validation {
                                    path: sp.clone(),
                                    message: format!(
                                        "relationship '{}': weight_field '{}' must be numeric (int, int32, or float), got {:?}",
                                        rel.name, weight_field, field.data_type
                                    ),
                                });
                            }
                        }
                    }
                }
                _ => {
                    // Simple strategies (uniform, sequential) need no extra validation
                }
            }
        }

        // ── Self-referential hierarchy controls ──────────────────────
        let is_self_ref = rel.from == rel.to;

        if rel.acyclic.is_some() && !is_self_ref {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!(
                    "relationship '{}': 'acyclic' is only valid on self-referential relationships (from == to)",
                    rel.name
                ),
            });
        }

        if rel.root_probability.is_some() && !is_self_ref {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!(
                    "relationship '{}': 'root_probability' is only valid on self-referential relationships (from == to)",
                    rel.name
                ),
            });
        }

        if rel.max_depth.is_some() && !is_self_ref {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!(
                    "relationship '{}': 'max_depth' is only valid on self-referential relationships (from == to)",
                    rel.name
                ),
            });
        }

        if let Some(p) = rel.root_probability {
            if p <= 0.0 || p > 1.0 {
                errors.push(BlueprintError::Validation {
                    path: path.clone(),
                    message: format!(
                        "relationship '{}': root_probability must be in (0.0, 1.0], got {}",
                        rel.name, p
                    ),
                });
            }
        }

        if let Some(d) = rel.max_depth {
            if d < 1 {
                errors.push(BlueprintError::Validation {
                    path: path.clone(),
                    message: format!(
                        "relationship '{}': max_depth must be >= 1, got {}",
                        rel.name, d
                    ),
                });
            }
        }

        // Self-ref with root nodes requires nullable FK
        if is_self_ref {
            let produces_roots = rel.root_probability.map_or(true, |p| p > 0.0);
            if produces_roots && rel.nullable == Some(false) {
                errors.push(BlueprintError::Validation {
                    path: path.clone(),
                    message: format!(
                        "relationship '{}': self-referential relationship with root nodes requires nullable = true (or omit nullable)",
                        rel.name
                    ),
                });
            }
        }

        // ── Edge properties ──────────────────────────────────────────
        if !rel.properties.is_empty() {
            // many_to_many with properties not yet supported
            if rel.kind == crate::core::RelationshipKind::ManyToMany {
                errors.push(BlueprintError::Validation {
                    path: path.clone(),
                    message: format!(
                        "relationship '{}': edge properties on many_to_many relationships are not yet supported; \
                         model an explicit junction entity instead",
                        rel.name
                    ),
                });
            }

            // Uniqueness check within this relationship's properties
            let mut prop_names_seen = HashSet::new();
            for prop in &rel.properties {
                let pp = format!("{}.properties.{}", path, prop.name);
                if !prop_names_seen.insert(&prop.name) {
                    errors.push(BlueprintError::Validation {
                        path: pp.clone(),
                        message: format!(
                            "duplicate edge property name '{}' in relationship '{}'",
                            prop.name, rel.name
                        ),
                    });
                }

                // Reject unsupported complex types
                if matches!(prop.data_type, crate::core::DataType::Map) {
                    errors.push(BlueprintError::Validation {
                        path: pp.clone(),
                        message: format!(
                            "edge property '{}': Map type is not supported as an edge property",
                            prop.name
                        ),
                    });
                }

                // Validate generator spec if present
                if let Some(ref gen) = prop.generator {
                    if let Some(from_entity) = model.entities.iter().find(|e| e.name == rel.from) {
                        validate_generator(
                            &format!("{}.generator", pp),
                            gen,
                            &prop.name,
                            &prop.data_type,
                            from_entity,
                            &names,
                            model,
                            false,
                            errors,
                        );
                    }
                }
            }

            // Check for name conflicts with entity fields and across relationships
            if let Some(from_entity) = model.entities.iter().find(|e| e.name == rel.from) {
                let entity_field_names: HashSet<&str> =
                    from_entity.fields.iter().map(|f| f.name.as_str()).collect();
                for prop in &rel.properties {
                    let pp = format!("{}.properties.{}", path, prop.name);
                    if entity_field_names.contains(prop.name.as_str()) {
                        errors.push(BlueprintError::Validation {
                            path: pp,
                            message: format!(
                                "edge property '{}' conflicts with existing field on entity '{}'",
                                prop.name, rel.from
                            ),
                        });
                    }
                }
            }
        }
    }

    // Cross-relationship edge property name collision: two relationships with
    // the same `from` entity must not have edge properties with the same name.
    let mut from_edge_props: HashMap<&str, HashSet<(&str, &str)>> = HashMap::new();
    for rel in &model.relationships {
        for prop in &rel.properties {
            let entry = from_edge_props.entry(rel.from.as_str()).or_default();
            if !entry.insert((prop.name.as_str(), rel.name.as_str())) {
                // Already caught by within-relationship duplicate check
            }
        }
    }
    for (entity_name, props) in &from_edge_props {
        let mut name_to_rel: HashMap<&str, Vec<&str>> = HashMap::new();
        for (prop_name, rel_name) in props {
            name_to_rel.entry(prop_name).or_default().push(rel_name);
        }
        for (prop_name, rels) in &name_to_rel {
            if rels.len() > 1 {
                errors.push(BlueprintError::Validation {
                    path: format!("relationships"),
                    message: format!(
                        "edge property '{}' appears in multiple relationships ({}) targeting entity '{}'",
                        prop_name,
                        rels.join(", "),
                        entity_name
                    ),
                });
            }
        }
    }
}

fn validate_noise_profiles(model: &DataModel, errors: &mut Vec<BlueprintError>) {
    let names = entity_names(model);
    let mut seen = HashSet::new();
    for noise in &model.noise_profiles {
        let path = format!("noise.{}", noise.name);
        if !seen.insert(&noise.name) {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!("duplicate noise profile name '{}'", noise.name),
            });
        }
        if !names.contains(noise.entity.as_str()) {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!("references unknown entity '{}'", noise.entity),
            });
        } else {
            let fields = entity_field_names(model, &noise.entity);
            for f in &noise.fields {
                if !fields.contains(f.as_str()) {
                    errors.push(BlueprintError::Validation {
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

        // Validate scope expression (if present)
        if let Some(ref scope) = noise.scope {
            match crate::gen::expr::parser::parse(&scope.where_expr) {
                Ok(expr) => {
                    // Validate field refs exist in the target entity
                    if names.contains(noise.entity.as_str()) {
                        let fields = entity_field_names(model, &noise.entity);
                        for field_ref in crate::gen::expr::ast::extract_field_refs(&expr) {
                            if !fields.contains(field_ref.as_str()) {
                                errors.push(BlueprintError::Validation {
                                    path: path.clone(),
                                    message: format!(
                                        "scope expression references unknown field '{}' in entity '{}'",
                                        field_ref, noise.entity
                                    ),
                                });
                            }
                        }
                    }
                }
                Err(e) => {
                    errors.push(BlueprintError::Validation {
                        path: path.clone(),
                        message: format!("invalid scope expression: {}", e),
                    });
                }
            }
        }
    }
}

fn validate_rate(path: &str, name: &str, value: f64, errors: &mut Vec<BlueprintError>) {
    if !(0.0..=1.0).contains(&value) {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: format!("{} must be in [0.0, 1.0], got {}", name, value),
        });
    }
}

fn validate_correlations(model: &DataModel, errors: &mut Vec<BlueprintError>) {
    let names = entity_names(model);
    for (i, corr) in model.correlations.iter().enumerate() {
        let path = format!("correlations[{}]", i);
        if !names.contains(corr.entity.as_str()) {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!("references unknown entity '{}'", corr.entity),
            });
        } else {
            let fields = entity_field_names(model, &corr.entity);
            for f in &corr.fields {
                if !fields.contains(f.as_str()) {
                    errors.push(BlueprintError::Validation {
                        path: path.clone(),
                        message: format!("field '{}' not found in entity '{}'", f, corr.entity),
                    });
                }
            }
        }
        if !corr.matrix.is_empty() {
            let n = corr.fields.len();
            if corr.matrix.len() != n {
                errors.push(BlueprintError::Validation {
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
                        errors.push(BlueprintError::Validation {
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
                // Check diagonal is 1.0 and matrix is symmetric
                for ri in 0..n.min(corr.matrix.len()) {
                    let row = &corr.matrix[ri];
                    if row.len() == n {
                        if (row[ri] - 1.0).abs() > 1e-10 {
                            errors.push(BlueprintError::Validation {
                                path: path.clone(),
                                message: format!(
                                    "correlation matrix diagonal[{}] must be 1.0, got {}",
                                    ri, row[ri]
                                ),
                            });
                        }
                        #[allow(clippy::needless_range_loop)]
                        for ci in (ri + 1)..n {
                            if (row[ci] - corr.matrix[ci][ri]).abs() > 1e-10 {
                                errors.push(BlueprintError::Validation {
                                    path: path.clone(),
                                    message: format!(
                                        "correlation matrix is not symmetric: [{},{}]={} vs [{},{}]={}",
                                        ri, ci, row[ci], ci, ri, corr.matrix[ci][ri]
                                    ),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Copula-specific validation
        if let Some(ref copula) = corr.copula {
            validate_copula(&path, copula, corr, model, errors);
        }

        // Conditional distribution validation (§8.3)
        let is_cond_dist = corr
            .correlation_type
            .as_deref()
            .map(|t| t == "conditional_distribution")
            .unwrap_or(false);
        if is_cond_dist {
            validate_conditional_distribution(&path, corr, model, errors);
        } else if corr.correlation_type.is_some() {
            let ct = corr.correlation_type.as_deref().unwrap();
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!(
                    "unknown correlation type '{}'; expected 'conditional_distribution'",
                    ct
                ),
            });
        }
    }
}

fn validate_copula(
    path: &str,
    copula: &CopulaSpec,
    corr: &Correlation,
    model: &DataModel,
    errors: &mut Vec<BlueprintError>,
) {
    let n = corr.fields.len();

    match copula.family {
        CopulaFamily::Gaussian => {
            // Gaussian copula requires a correlation matrix
            if corr.matrix.is_empty() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "Gaussian copula requires a correlation matrix".to_string(),
                });
            }
            // Check positive semi-definiteness via Cholesky attempt
            if !corr.matrix.is_empty() && corr.matrix.len() == n {
                let all_correct_size = corr.matrix.iter().all(|r| r.len() == n);
                if all_correct_size && !is_positive_semidefinite(&corr.matrix) {
                    errors.push(BlueprintError::Validation {
                        path: path.to_string(),
                        message: "correlation matrix is not positive semi-definite \
                                  (Cholesky decomposition failed)"
                            .to_string(),
                    });
                }
            }
        }
        CopulaFamily::Clayton => {
            if n != 2 {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "Clayton copula requires exactly 2 fields, got {n}"
                    ),
                });
            }
            let theta = copula.params.get("theta").copied().unwrap_or(f64::NAN);
            if theta.is_nan() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "Clayton copula requires 'theta' parameter".to_string(),
                });
            } else if theta <= 0.0 || !theta.is_finite() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "Clayton copula theta must be > 0, got {theta}"
                    ),
                });
            }
        }
        CopulaFamily::Frank => {
            if n != 2 {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "Frank copula requires exactly 2 fields, got {n}"
                    ),
                });
            }
            let theta = copula.params.get("theta").copied().unwrap_or(f64::NAN);
            if theta.is_nan() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "Frank copula requires 'theta' parameter".to_string(),
                });
            } else if theta == 0.0 || !theta.is_finite() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "Frank copula theta must be non-zero and finite, got {theta}"
                    ),
                });
            }
        }
        CopulaFamily::Gumbel => {
            if n != 2 {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "Gumbel copula requires exactly 2 fields, got {n}"
                    ),
                });
            }
            let theta = copula.params.get("theta").copied().unwrap_or(f64::NAN);
            if theta.is_nan() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: "Gumbel copula requires 'theta' parameter".to_string(),
                });
            } else if theta < 1.0 || !theta.is_finite() {
                errors.push(BlueprintError::Validation {
                    path: path.to_string(),
                    message: format!(
                        "Gumbel copula theta must be >= 1.0, got {theta}"
                    ),
                });
            }
        }
    }

    // All copula fields must have distribution generators (for marginal CDFs)
    // and the distribution kind must support inverse CDF
    let supported_marginals = [
        DistributionKind::Normal,
        DistributionKind::LogNormal,
        DistributionKind::Uniform,
        DistributionKind::Exponential,
    ];
    if let Some(entity) = model.entities.iter().find(|e| e.name == corr.entity) {
        for field_name in &corr.fields {
            if let Some(field) = entity.fields.iter().find(|f| &f.name == field_name) {
                match &field.generator {
                    Some(GeneratorSpec::Distribution { spec, .. }) => {
                        if !supported_marginals.contains(&spec.kind) {
                            errors.push(BlueprintError::Validation {
                                path: path.to_string(),
                                message: format!(
                                    "copula field '{}' uses {:?} distribution which does not \
                                     support inverse CDF transform; supported: Normal, \
                                     LogNormal, Uniform, Exponential",
                                    field_name, spec.kind
                                ),
                            });
                        }
                    }
                    _ => {
                        errors.push(BlueprintError::Validation {
                            path: path.to_string(),
                            message: format!(
                                "copula field '{}' must have a distribution generator \
                                 (for marginal CDF transform)",
                                field_name
                            ),
                        });
                    }
                }
            }
        }
    }
}

/// Validate a conditional distribution correlation (§8.3).
fn validate_conditional_distribution(
    path: &str,
    corr: &Correlation,
    model: &DataModel,
    errors: &mut Vec<BlueprintError>,
) {
    // Must have dependent and given
    let dependent = match &corr.dependent {
        Some(d) => d,
        None => {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: "conditional_distribution requires 'dependent' field".to_string(),
            });
            return;
        }
    };
    let given = match &corr.given {
        Some(g) => g,
        None => {
            errors.push(BlueprintError::Validation {
                path: path.to_string(),
                message: "conditional_distribution requires 'given' field".to_string(),
            });
            return;
        }
    };

    // dependent ≠ given
    if dependent == given {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: format!(
                "conditional_distribution 'dependent' and 'given' must differ, both are '{}'",
                dependent
            ),
        });
    }

    // Both fields must exist in the entity
    let field_names = entity_field_names(model, &corr.entity);
    if !field_names.contains(dependent.as_str()) {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: format!(
                "conditional_distribution dependent field '{}' not found in entity '{}'",
                dependent, corr.entity
            ),
        });
    }
    if !field_names.contains(given.as_str()) {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: format!(
                "conditional_distribution given field '{}' not found in entity '{}'",
                given, corr.entity
            ),
        });
    }

    // distributions must be non-empty
    if corr.distributions.is_empty() {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: "conditional_distribution requires at least one distribution branch"
                .to_string(),
        });
    }

    // Check for duplicate when values
    let mut seen_conditions: Vec<String> = Vec::new();
    for (bi, branch) in corr.distributions.iter().enumerate() {
        let key = format!("{:?}", branch.condition);
        if seen_conditions.contains(&key) {
            errors.push(BlueprintError::Validation {
                path: format!("{}.distributions[{}]", path, bi),
                message: format!("duplicate 'when' value: {:?}", branch.condition),
            });
        }
        seen_conditions.push(key);
    }

    // Mutual exclusivity: conditional_distribution should not use matrix/copula fields
    if !corr.fields.is_empty() {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: "conditional_distribution cannot use 'fields' (matrix/copula mode)"
                .to_string(),
        });
    }
    if !corr.matrix.is_empty() {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: "conditional_distribution cannot use 'matrix'".to_string(),
        });
    }
    if corr.copula.is_some() {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: "conditional_distribution cannot use 'copula'".to_string(),
        });
    }
    if !corr.conditional.is_empty() {
        errors.push(BlueprintError::Validation {
            path: path.to_string(),
            message: "conditional_distribution cannot use 'conditional'".to_string(),
        });
    }

    // Validate distribution parameters for each branch
    for (bi, branch) in corr.distributions.iter().enumerate() {
        let branch_path = format!("{}.distributions[{}]", path, bi);
        let spec = DistributionSpec {
            kind: branch.distribution.clone(),
            params: branch.params.clone(),
            array_params: BTreeMap::new(),
            round: branch.round,
        };
        validate_distribution(&branch_path, &spec, errors);
    }

    // Validate default distribution if present
    if let Some(ref default_spec) = corr.default {
        let default_path = format!("{}.default", path);
        validate_distribution(&default_path, default_spec, errors);
    }
}

/// Check if a matrix is positive semi-definite via Cholesky decomposition
/// with small diagonal jitter for numerical stability.
fn is_positive_semidefinite(matrix: &[Vec<f64>]) -> bool {
    let n = matrix.len();
    let mut l = vec![vec![0.0f64; n]; n];
    let jitter = 1e-10;

    for i in 0..n {
        for j in 0..=i {
            let sum: f64 = l[i].iter().zip(l[j].iter()).take(j).map(|(a, b)| a * b).sum();
            if i == j {
                let diag = matrix[i][i] + jitter - sum;
                if diag < 0.0 {
                    return false;
                }
                l[i][j] = diag.sqrt();
            } else if l[j][j].abs() < 1e-15 {
                return false;
            } else {
                l[i][j] = (matrix[i][j] - sum) / l[j][j];
            }
        }
    }
    true
}

fn validate_personas(model: &DataModel, errors: &mut Vec<BlueprintError>) {
    let mut seen = HashSet::new();
    for (i, persona) in model.personas.iter().enumerate() {
        let path = format!("personas[{}]", i);

        // Duplicate name check
        if !seen.insert(&persona.name) {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!("duplicate persona name '{}'", persona.name),
            });
        }

        // Name must be non-empty
        if persona.name.is_empty() {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: "persona name must not be empty".to_string(),
            });
        }

        // Weight must be positive
        if persona.weight <= 0.0 {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!(
                    "persona '{}' has weight {} which must be > 0",
                    persona.name, persona.weight
                ),
            });
        }

        // Weight must be finite
        if !persona.weight.is_finite() {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!("persona '{}' has non-finite weight", persona.name),
            });
        }

        // Traits should not be empty
        if persona.traits.is_empty() {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!(
                    "persona '{}' has empty traits; at least one trait is required",
                    persona.name
                ),
            });
        }
    }
}

fn validate_actor_relationships(model: &DataModel, errors: &mut Vec<BlueprintError>) {
    let names = entity_names(model);
    let mut seen = HashSet::new();
    for (i, ar) in model.actor_relationships.iter().enumerate() {
        let path = format!("actor_relationships[{}]", i);

        // Duplicate name check
        if !seen.insert(&ar.name) {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!("duplicate actor_relationship name '{}'", ar.name),
            });
        }

        // Name must be non-empty
        if ar.name.is_empty() {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: "actor_relationship name must not be empty".to_string(),
            });
        }

        // from_entity must exist and be an actor
        if !names.contains(ar.from_entity.as_str()) {
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!("from_entity '{}' references unknown entity", ar.from_entity),
            });
        } else if let Some(entity) = model.entities.iter().find(|e| e.name == ar.from_entity) {
            if !entity.actor {
                errors.push(BlueprintError::Validation {
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
            errors.push(BlueprintError::Validation {
                path: path.clone(),
                message: format!("to_entity '{}' references unknown entity", ar.to_entity),
            });
        } else if let Some(entity) = model.entities.iter().find(|e| e.name == ar.to_entity) {
            if !entity.actor {
                errors.push(BlueprintError::Validation {
                    path: path.clone(),
                    message: format!("to_entity '{}' is not marked as actor = true", ar.to_entity),
                });
            }
        }

        // avg_degree param (if present) must be positive and finite
        if let Some(&avg_degree) = ar.params.get("avg_degree") {
            if !avg_degree.is_finite() || avg_degree <= 0.0 {
                errors.push(BlueprintError::Validation {
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

/// Detect dependency cycles among derived, relative, and conditional fields
/// within each entity. Reports the cycle path if found.
fn validate_dependency_cycles(model: &DataModel, errors: &mut Vec<BlueprintError>) {
    for entity in &model.entities {
        // Build adjacency: field_name → set of field names it depends on (owned)
        let mut deps: HashMap<String, Vec<String>> = HashMap::new();
        for field in &entity.fields {
            if let Some(ref gen) = field.generator {
                let field_deps = collect_generator_deps_owned(gen);
                if !field_deps.is_empty() {
                    deps.insert(field.name.clone(), field_deps);
                }
            }
        }

        // DFS cycle detection
        let mut visited: HashSet<&str> = HashSet::new();
        let mut stack: HashSet<&str> = HashSet::new();

        for field in &entity.fields {
            let name = field.name.as_str();
            if !visited.contains(name) {
                let mut path = Vec::new();
                if has_cycle(name, &deps, &mut visited, &mut stack, &mut path) {
                    path.reverse();
                    errors.push(BlueprintError::Validation {
                        path: format!("entities.{}", entity.name),
                        message: format!(
                            "dependency cycle detected: {}",
                            path.join(" → ")
                        ),
                    });
                    break; // report one cycle per entity
                }
            }
        }
    }
}

/// Collect field dependencies from a generator spec as owned strings.
///
/// For parsed expressions, uses AST-based field ref extraction (ignores
/// `${...}` inside string literals). Falls back to template scanning
/// only for legacy templates that fail expression parsing.
fn collect_generator_deps_owned(gen: &GeneratorSpec) -> Vec<String> {
    match gen {
        GeneratorSpec::Derived { expr: ref expr_str } => {
            // Try parsing as expression first — AST-based extraction is more
            // accurate (ignores ${...} inside string literals).
            let refs = if let Ok(ast) = expr::parser::parse(expr_str) {
                expr::ast::extract_field_refs(&ast)
            } else {
                // Legacy template: extract ${field} refs from raw string
                let mut deps = Vec::new();
                extract_template_refs(expr_str, &mut deps);
                deps.into_iter().map(|s| s.to_string()).collect()
            };
            // Filter out parameter substitutions (e.g. param.prefix)
            refs.into_iter()
                .filter(|r| !r.starts_with("param."))
                .collect()
        }
        GeneratorSpec::Relative { anchor, .. } => vec![anchor.clone()],
        GeneratorSpec::Conditional { field, .. } => vec![field.clone()],
        _ => Vec::new(),
    }
}

/// Extract `${field}` references from a template/expression string.
fn extract_template_refs<'a>(s: &'a str, deps: &mut Vec<&'a str>) {
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        if let Some(end) = after.find('}') {
            let field_name = &after[..end];
            if !field_name.is_empty() {
                deps.push(field_name);
            }
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
}

/// DFS cycle detection. Returns true if a cycle is found.
fn has_cycle<'a>(
    node: &'a str,
    deps: &'a HashMap<String, Vec<String>>,
    visited: &mut HashSet<&'a str>,
    stack: &mut HashSet<&'a str>,
    path: &mut Vec<String>,
) -> bool {
    visited.insert(node);
    stack.insert(node);

    if let Some(neighbors) = deps.get(node) {
        for dep in neighbors {
            let dep_str = dep.as_str();
            if !visited.contains(dep_str) {
                if has_cycle(dep_str, deps, visited, stack, path) {
                    path.push(node.to_string());
                    return true;
                }
            } else if stack.contains(dep_str) {
                // Found a cycle
                path.push(dep.clone());
                path.push(node.to_string());
                return true;
            }
        }
    }

    stack.remove(node);
    false
}

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
                tags: Vec::new(),
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
                        fields: vec![],
                stats: None,
                traits: None,
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
                        fields: vec![],
                stats: None,
                traits: None,
                    },
                ],
                constraints: vec![],
                topology: None,
                actor: false,
                persona_distribution: None,
                activity_count: None,
                mixin_refs: None,
        output: None,
        stats: None,
            }],
            relationships: vec![],
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            blueprint_version: "1.0".to_string(),
            personas: Vec::new(),
            actor_relationships: Vec::new(),
            custom_types: Vec::new(),
            mixins: Vec::new(),
        companion_files: Vec::new(),
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("duplicate entity"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("duplicate field"))
        }));
    }

    #[test]
    fn test_validate_multiple_primary_keys() {
        let mut model = minimal_model();
        model.entities[0].fields[1].primary_key = Some(true);
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("primary keys"))
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
            degree: None,

            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("unknown entity 'order'"))
        }));
    }

    #[test]
    fn test_validate_relationship_unknown_field() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "order".to_string(),
            description: None,
            tags: Vec::new(),
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
                fields: vec![],
                stats: None,
                traits: None,
            }],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
                mixin_refs: None,
        output: None,
        stats: None,
        });
        model.relationships.push(Relationship {
            name: "user_order".to_string(),
            from: "order".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: Some("user_id".to_string()),
            cardinality: None,
            degree: None,

            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("foreign_key 'user_id'"))
        }));
    }

    #[test]
    fn test_validate_relationship_target_no_pk() {
        let mut model = minimal_model();
        // Add entity with no primary key
        model.entities.push(Entity {
            name: "order".to_string(),
            description: None,
            tags: Vec::new(),
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
                fields: vec![],
                stats: None,
                traits: None,
            }],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
                mixin_refs: None,
        output: None,
        stats: None,
        });
        model.relationships.push(Relationship {
            name: "user_order".to_string(),
            from: "user".to_string(),
            to: "order".to_string(),
            kind: RelationshipKind::OneToMany,
            foreign_key: Some("id".to_string()),
            cardinality: None,
            degree: None,

            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
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
            tags: Vec::new(),
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
                    fields: vec![],
                stats: None,
                traits: None,
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
                    fields: vec![],
                stats: None,
                traits: None,
                },
            ],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
                mixin_refs: None,
        output: None,
        stats: None,
        });
        model.relationships.push(Relationship {
            name: "order_user".to_string(),
            from: "order".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: Some("user_id".to_string()),
            cardinality: None,
            degree: None,

            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
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
            tags: Vec::new(),
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
                    fields: vec![],
                stats: None,
                traits: None,
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
                    fields: vec![],
                stats: None,
                traits: None,
                },
            ],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
                mixin_refs: None,
        output: None,
        stats: None,
        });
        model.relationships.push(Relationship {
            name: "order_user".to_string(),
            from: "order".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: Some("user_id".to_string()),
            cardinality: None,
            degree: None,

            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![],
        });
        let errors = validate(&model);
        // Should produce no relationship-related errors
        assert!(!errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
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
            tags: Vec::new(),
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
                fields: vec![],
                stats: None,
                traits: None,
            }],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
                mixin_refs: None,
        output: None,
        stats: None,
        });
        // Relationship without explicit foreign_key — implicit FK is "order_id"
        model.relationships.push(Relationship {
            name: "user_order".to_string(),
            from: "user".to_string(),
            to: "order".to_string(),
            kind: RelationshipKind::OneToMany,
            foreign_key: None,
            cardinality: None,
            degree: None,

            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
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
                scope: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("unknown entity 'nonexistent'"))
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
                scope: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("null_rate"))
        }));
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("duplicate_rate"))
        }));
    }

    #[test]
    fn test_validate_correlation_unknown_entity() {
        let mut model = minimal_model();
        model.correlations.push(Correlation {
            entity: "nonexistent".to_string(),
            correlation_type: None,
            fields: vec!["a".to_string()],
            matrix: vec![],
            conditional: vec![],
            copula: None,
            dependent: None,
            given: None,
            distributions: vec![],
            default: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("unknown entity 'nonexistent'"))
        }));
    }

    #[test]
    fn test_validate_correlation_matrix_dimensions() {
        let mut model = minimal_model();
        model.correlations.push(Correlation {
            entity: "user".to_string(),
            correlation_type: None,
            fields: vec!["id".to_string(), "email".to_string()],
            matrix: vec![vec![1.0, 0.5]], // 1 row but 2 fields
            conditional: vec![],
            copula: None,
            dependent: None,
            given: None,
            distributions: vec![],
            default: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("matrix"))
        }));
    }

    #[test]
    fn test_validate_duplicate_relationship_names() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "order".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(50),
            fields: vec![],
            stats: None,
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
                mixin_refs: None,
        output: None,
        });
        let rel = Relationship {
            name: "user_order".to_string(),
            from: "user".to_string(),
            to: "order".to_string(),
            kind: RelationshipKind::OneToMany,
            foreign_key: None,
            cardinality: None,
            degree: None,

            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![],
        };
        model.relationships.push(rel.clone());
        model.relationships.push(rel);
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("duplicate relationship"))
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
                scope: None,
        };
        model.noise_profiles.push(noise.clone());
        model.noise_profiles.push(noise);
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("duplicate noise profile"))
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
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("min < max"))
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
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. } if message.contains("binomial"))
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
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("binomial") && message.contains("'p'"))
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
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("triangular") && message.contains("min < max"))
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
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("zipf") && message.contains("'n'"))
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
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. } if message.contains("beta"))
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
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("zipf") && message.contains("integer"))
        }));
    }

    // ── Count spec validation ───────────────────────────────────────

    #[test]
    fn test_validate_count_range_min_gt_max() {
        let mut model = minimal_model();
        model.entities[0].count = CountSpec::Range { min: 100, max: 10 };
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("min <= max"))
        }));
    }

    #[test]
    fn test_validate_count_range_min_zero_valid() {
        let mut model = minimal_model();
        model.entities[0].count = CountSpec::Range { min: 0, max: 10 };
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. } if message.contains("range"))
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
            array_params: BTreeMap::new(),
            round: false,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("normal") && message.contains("mean"))
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
                start: IntOrString::Int(1),
                step: IntOrString::Int(0),
                prefix: None,
            values: None,
            cycle: None,
            jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("sequence step must not be 0"))
        }));
    }

    #[test]
    fn test_validate_sequence_values_empty() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "day".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Sequence {
                start: IntOrString::Int(0),
                step: IntOrString::Int(1),
                prefix: None,
                values: Some(vec![]),
                cycle: None,
            jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("values must not be empty"))
        }));
    }

    #[test]
    fn test_validate_sequence_values_with_start_step() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "day".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Sequence {
                start: IntOrString::Int(5),
                step: IntOrString::Int(2),
                prefix: None,
                values: Some(vec!["A".into(), "B".into()]),
                cycle: None,
            jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("mutually exclusive"))
        }));
    }

    #[test]
    fn test_validate_sequence_cycle_without_values() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "num".to_string(),
            description: None,
            data_type: DataType::Int,
            generator: Some(GeneratorSpec::Sequence {
                start: IntOrString::Int(0),
                step: IntOrString::Int(1),
                prefix: None,
                values: None,
                cycle: Some(true),
                 jitter: None,
             }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("'cycle' requires 'values'"))
        }));
    }

    #[test]
    fn test_validate_sequence_values_valid() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "day".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Sequence {
                start: IntOrString::Int(0),
                step: IntOrString::Int(1),
                prefix: None,
                values: Some(vec!["Mon".into(), "Tue".into(), "Wed".into()]),
                cycle: Some(true),
                 jitter: None,
             }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(!errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("values") || message.contains("cycle"))
        }));
    }

    #[test]
    fn test_validate_sequence_values_on_int_field() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "day".to_string(),
            description: None,
            data_type: DataType::Int, // values produce strings, not ints
            generator: Some(GeneratorSpec::Sequence {
                start: IntOrString::Int(0),
                step: IntOrString::Int(1),
                prefix: None,
                values: Some(vec!["Mon".into(), "Tue".into()]),
                cycle: None,
            jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("requires data_type 'string'"))
        }));
    }

    #[test]
    fn test_validate_temporal_sequence_valid() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::Sequence {
                start: IntOrString::Str("2024-01-01".into()),
                step: IntOrString::Str("1d".into()),
                prefix: None,
                values: None,
                cycle: None,
                jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(!errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("sequence") && message.contains("compatible"))
        }));
    }

    #[test]
    fn test_validate_temporal_sequence_invalid_start() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::Sequence {
                start: IntOrString::Str("not-a-date".into()),
                step: IntOrString::Str("1d".into()),
                prefix: None,
                values: None,
                cycle: None,
                jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("not a valid date"))
        }));
    }

    #[test]
    fn test_validate_temporal_sequence_on_int_field_rejected() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Int, // temporal start on int field
            generator: Some(GeneratorSpec::Sequence {
                start: IntOrString::Str("2024-01-01".into()),
                step: IntOrString::Str("1d".into()),
                prefix: None,
                values: None,
                cycle: None,
                jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("temporal sequence"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("oneOf requires at least one choice"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("unknown faker method 'bogus'"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. } if message.contains("faker"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("only UUID version 4 is supported"))
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
                timezone: None,
                timezone_field: None,
                date_range: None,
                exclude_dates: vec![],
                days: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("start_hour < end_hour"))
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
                timezone: None,
                timezone_field: None,
                date_range: None,
                exclude_dates: vec![],
                days: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. } if message.contains("business_hours"))
            }),
            "expected no business_hours errors, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_business_hours_timezone_and_field_exclusive() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::BusinessHours {
                start_hour: 9,
                end_hour: 17,
                exclude_weekends: false,
                timezone: Some("America/New_York".to_string()),
                timezone_field: Some("tz_col".to_string()),
                date_range: None,
                exclude_dates: vec![],
                days: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("mutually exclusive"))
        }));
    }

    #[test]
    fn test_validate_business_hours_invalid_timezone() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::BusinessHours {
                start_hour: 9,
                end_hour: 17,
                exclude_weekends: false,
                timezone: Some("Not/A/Timezone".to_string()),
                timezone_field: None,
                date_range: None,
                exclude_dates: vec![],
                days: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("invalid timezone"))
        }));
    }

    #[test]
    fn test_validate_business_hours_days_with_exclude_weekends() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::BusinessHours {
                start_hour: 9,
                end_hour: 17,
                exclude_weekends: true,
                timezone: None,
                timezone_field: None,
                date_range: None,
                exclude_dates: vec![],
                days: Some(vec!["Monday".into(), "Wednesday".into()]),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("mutually exclusive"))
        }));
    }

    #[test]
    fn test_validate_business_hours_invalid_day_name() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::BusinessHours {
                start_hour: 9,
                end_hour: 17,
                exclude_weekends: false,
                timezone: None,
                timezone_field: None,
                date_range: None,
                exclude_dates: vec![],
                days: Some(vec!["Funday".into()]),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("invalid day name"))
        }));
    }

    #[test]
    fn test_validate_business_hours_invalid_date_range() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::BusinessHours {
                start_hour: 9,
                end_hour: 17,
                exclude_weekends: false,
                timezone: None,
                timezone_field: None,
                date_range: Some(crate::core::types::BusinessDateRange {
                    min: "2024-12-31".to_string(),
                    max: "2024-01-01".to_string(),
                }),
                exclude_dates: vec![],
                days: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("date_range.min must be before"))
        }));
    }

    #[test]
    fn test_validate_business_hours_single_day_range_valid() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::BusinessHours {
                start_hour: 9,
                end_hour: 17,
                exclude_weekends: false,
                timezone: None,
                timezone_field: None,
                date_range: Some(crate::core::types::BusinessDateRange {
                    min: "2024-06-15".to_string(),
                    max: "2024-06-15".to_string(),
                }),
                exclude_dates: vec![],
                days: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. } if message.contains("date_range"))
            }),
            "single-day date_range should be valid, but got: {:?}",
            errors
        );
    }

    #[test]
    fn test_validate_business_hours_invalid_exclude_date() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::BusinessHours {
                start_hour: 9,
                end_hour: 17,
                exclude_weekends: false,
                timezone: None,
                timezone_field: None,
                date_range: None,
                exclude_dates: vec!["not-a-date".to_string()],
                days: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("invalid exclude_date"))
        }));
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("lookup references unknown entity 'nonexistent'"))
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
                        array_params: BTreeMap::new(),
                        round: false,
                    },
                }),
                max_retries: 100,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("normal") && message.contains("mean"))
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
                anchor: "self_field".to_string(),
                offset: RelativeOffset::Simple(Value::Int(1)),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("relative cannot reference itself"))
        }));
    }

    #[test]
    fn test_validate_relative_distribution_offset() {
        let mut model = minimal_model();
        // Add an anchor field
        model.entities[0].fields.push(Field {
            name: "start_date".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "end_date".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::Relative {
                anchor: "start_date".to_string(),
                offset: RelativeOffset::Distribution {
                    distribution: DistributionKind::LogNormal,
                    params: {
                        let mut p = std::collections::BTreeMap::new();
                        p.insert("mu".into(), 1.5);
                        p.insert("sigma".into(), 0.8);
                        p
                    },
                    min: Some("1d".into()),
                    max: Some("14d".into()),
                    unit: Some("day".into()),
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        // Should have no errors for valid distribution offset
        let rel_errors: Vec<_> = errors
            .iter()
            .filter(|e| matches!(e, BlueprintError::Validation { message, .. } if message.contains("relative")))
            .collect();
        assert!(rel_errors.is_empty(), "unexpected errors: {rel_errors:?}");
    }

    #[test]
    fn test_validate_relative_bad_distribution() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "start_date".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "end_date".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::Relative {
                anchor: "start_date".to_string(),
                offset: RelativeOffset::Distribution {
                    distribution: DistributionKind::Bernoulli, // not allowed
                    params: Default::default(),
                    min: None,
                    max: None,
                    unit: None,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("not supported"))
        }));
    }

    #[test]
    fn test_validate_relative_min_exceeds_max() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "start_date".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "end_date".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::Relative {
                anchor: "start_date".to_string(),
                offset: RelativeOffset::Distribution {
                    distribution: DistributionKind::Normal,
                    params: Default::default(),
                    min: Some("14d".into()),
                    max: Some("1d".into()), // min > max
                    unit: None,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("exceeds max"))
        }));
    }

    #[test]
    fn test_validate_relative_constant_offset() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "start_date".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "expiry".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::Relative {
                anchor: "start_date".to_string(),
                offset: RelativeOffset::Constant {
                    offset_type: "constant".into(),
                    value: "365d".into(),
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        let rel_errors: Vec<_> = errors
            .iter()
            .filter(|e| matches!(e, BlueprintError::Validation { message, .. } if message.contains("relative")))
            .collect();
        assert!(rel_errors.is_empty(), "unexpected errors: {rel_errors:?}");
    }

    #[test]
    fn test_validate_relative_non_numeric_simple_offset() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "start_date".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "end_date".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::Relative {
                anchor: "start_date".to_string(),
                offset: RelativeOffset::Simple(Value::String("not_a_number".into())),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("must be numeric"))
        }));
    }

    #[test]
    fn test_validate_relative_invalid_duration_unit() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "start_date".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "end_date".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::Relative {
                anchor: "start_date".to_string(),
                offset: RelativeOffset::Distribution {
                    distribution: DistributionKind::Normal,
                    params: Default::default(),
                    min: Some("10xyz".into()),
                    max: None,
                    unit: None,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("not a valid duration"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("conditional cannot reference its own field"))
        }));
    }

    #[test]
    fn test_validate_nested_lookup_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "other".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(10),
            fields: vec![Field {
                name: "id".to_string(),
                description: None,
                data_type: DataType::Int,
                generator: Some(GeneratorSpec::Sequence {
                    start: IntOrString::Int(0),
                    step: IntOrString::Int(1),
                    prefix: None,
                values: None,
                cycle: None,
            jitter: None,
            }),
                nullable: NullSpec::Never,
                primary_key: Some(true),
                precision: None,
                actor_column: false,
                fields: vec![],
                stats: None,
                traits: None,
            }],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
                mixin_refs: None,
        output: None,
        stats: None,
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("lookup cannot be nested"))
        }));
    }

    #[test]
    fn test_validate_null_probability_out_of_range() {
        let mut model = minimal_model();
        model.entities[0].fields[0].nullable = NullSpec::Probability(1.5);
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("null probability"))
        }));
    }

    #[test]
    fn test_validate_null_probability_valid() {
        let mut model = minimal_model();
        model.entities[0].fields[0].nullable = NullSpec::Probability(0.3);
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. } if message.contains("null"))
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("every_n must be > 0"))
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
                    array_params: std::collections::BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("is not compatible with data_type"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("faker generator produces strings"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("uuid generator is not compatible"))
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
                start: IntOrString::Int(1),
                step: IntOrString::Int(1),
                prefix: None,
            values: None,
            cycle: None,
            jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(!errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("sequence generator is not compatible"))
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
                start: IntOrString::Int(1),
                step: IntOrString::Int(1),
                prefix: None,
            values: None,
            cycle: None,
            jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("sequence generator is not compatible"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("pattern generator produces strings"))
        }));
    }

    #[test]
    fn test_actor_ref_on_non_actor_entity_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "users".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(10),
            fields: vec![Field {
                name: "id".to_string(),
                description: None,
                data_type: DataType::Int,
                generator: Some(GeneratorSpec::Sequence {
                    start: IntOrString::Int(1),
                    step: IntOrString::Int(1),
                    prefix: None,
                values: None,
                cycle: None,
            jitter: None,
            }),
                nullable: NullSpec::Never,
                primary_key: Some(true),
                precision: None,
                actor_column: false,
                fields: vec![],
                stats: None,
                traits: None,
            }],
            actor: false,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
                mixin_refs: None,
        output: None,
        stats: None,
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("not marked as actor"))
        }));
    }

    #[test]
    fn test_actor_ref_type_compat_on_bool_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "users".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(10),
            fields: vec![],
            stats: None,
            actor: true,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
                mixin_refs: None,
        output: None,
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("actor_ref generator produces key values"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("unknown actor_relationship"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("actor_temporal generator produces temporal values"))
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("duplicate persona name"))
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("weight") && message.contains("must be > 0"))
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("empty traits"))
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("from_entity") && message.contains("unknown entity"))
        }));
    }

    #[test]
    fn test_actor_relationship_non_actor_entity_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "users".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(10),
            fields: vec![],
            stats: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
                mixin_refs: None,
        output: None,
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("not marked as actor"))
        }));
    }

    #[test]
    fn test_actor_relationship_zero_connections_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "people".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(10),
            fields: vec![],
            stats: None,
            actor: true,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
                mixin_refs: None,
        output: None,
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("avg_degree") && message.contains("must be a finite value > 0"))
        }));
    }

    #[test]
    fn test_valid_personas_and_actor_relationships_accepted() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "people".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(50),
            fields: vec![],
            stats: None,
            actor: true,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
                mixin_refs: None,
        output: None,
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
            matches!(e, BlueprintError::Validation { message, .. }
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("name must not be empty"))
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("non-finite weight"))
        }));
    }

    #[test]
    fn test_actor_relationship_duplicate_name_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "people".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(10),
            fields: vec![],
            stats: None,
            actor: true,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
                mixin_refs: None,
        output: None,
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("duplicate actor_relationship name"))
        }));
    }

    #[test]
    fn test_actor_relationship_nan_avg_degree_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "people".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(10),
            fields: vec![],
            stats: None,
            actor: true,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
                mixin_refs: None,
        output: None,
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
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("avg_degree") && message.contains("finite"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("temporal_after references unknown entity"))
        }));
    }

    #[test]
    fn test_temporal_after_unknown_field_rejected() {
        let mut model = minimal_model();
        // Add a "posts" entity with an "id" PK but no "created_at" field
        model.entities.push(Entity {
            name: "posts".to_string(),
            description: None,
            tags: Vec::new(),
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
                fields: vec![],
                stats: None,
                traits: None,
            }],
            actor: false,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
                mixin_refs: None,
        output: None,
        stats: None,
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
            fields: vec![],
                stats: None,
                traits: None,
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("temporal_after.field") && message.contains("not found"))
        }));
    }

    #[test]
    fn test_temporal_after_non_temporal_field_rejected() {
        let mut model = minimal_model();
        // Add "posts" entity with a String "title" field
        model.entities.push(Entity {
            name: "posts".to_string(),
            description: None,
            tags: Vec::new(),
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
                    fields: vec![],
                stats: None,
                traits: None,
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
                    fields: vec![],
                stats: None,
                traits: None,
                },
            ],
            actor: false,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
                mixin_refs: None,
        output: None,
        stats: None,
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
            fields: vec![],
                stats: None,
                traits: None,
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("temporal_after.field") && message.contains("must be a temporal type"))
        }));
    }

    #[test]
    fn test_temporal_after_unknown_fk_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "posts".to_string(),
            description: None,
            tags: Vec::new(),
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
                    fields: vec![],
                stats: None,
                traits: None,
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
                    fields: vec![],
                stats: None,
                traits: None,
                },
            ],
            actor: false,
            persona_distribution: None,
            activity_count: None,
            constraints: vec![],
            topology: None,
                mixin_refs: None,
        output: None,
        stats: None,
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("temporal_after.fk") && message.contains("not found"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("burst.avg_events must be a finite number > 0"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("burst.avg_gap_minutes must be a finite number > 0"))
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
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("burst.avg_events must be a finite number > 0"))
        }));
    }

    // ─── Derived expression validation tests ────────────────────────

    #[test]
    fn test_derived_expr_unknown_field() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "full_name".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Derived {
                expr: "concat(${first_name}, \" \", ${last_name})".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(
            errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. }
                    if message.contains("unknown field") && message.contains("first_name"))
            }),
            "expected error about unknown field 'first_name', got: {errors:?}"
        );
    }

    #[test]
    fn test_derived_expr_self_reference() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "value".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Derived {
                expr: "${value} + 1.0".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(
            errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. }
                    if message.contains("references itself"))
            }),
            "expected self-reference error, got: {errors:?}"
        );
    }

    #[test]
    fn test_derived_legacy_template_accepted() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "greeting".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Derived {
                expr: "Hello ${email}!".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        // Legacy template with valid field should not produce errors
        assert!(
            !errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. }
                    if message.contains("not a valid expression")
                       || message.contains("unknown field"))
            }),
            "legacy template with valid field should be accepted, got: {errors:?}"
        );
    }

    #[test]
    fn test_derived_legacy_template_unknown_field() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "greeting".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::Derived {
                expr: "Hello ${naem}!".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(
            errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. }
                    if message.contains("unknown field") && message.contains("naem"))
            }),
            "legacy template with typo should be flagged, got: {errors:?}"
        );
    }

    #[test]
    fn test_derived_invalid_expression() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "bad".to_string(),
            description: None,
            data_type: DataType::String,
            // Not a valid expression and not a legacy template (no ${...})
            generator: Some(GeneratorSpec::Derived {
                expr: "((( unclosed".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(
            errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. }
                    if message.contains("not a valid expression"))
            }),
            "expected invalid expression error, got: {errors:?}"
        );
    }

    // ─── Dependency cycle detection tests ───────────────────────────

    #[test]
    fn test_dependency_cycle_detected() {
        let mut model = minimal_model();
        // Create a → b → a cycle via Derived expressions
        model.entities[0].fields.push(Field {
            name: "a".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Derived {
                expr: "${b} + 1.0".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "b".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Derived {
                expr: "${a} * 2.0".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(
            errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. }
                    if message.contains("dependency cycle"))
            }),
            "expected cycle detection error, got: {errors:?}"
        );
    }

    #[test]
    fn test_no_false_cycle_for_chain() {
        let mut model = minimal_model();
        // a → b → c is a chain, not a cycle
        model.entities[0].fields.push(Field {
            name: "c".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Distribution {
                spec: DistributionSpec {
                    kind: DistributionKind::Uniform,
                    params: {
                        let mut m = BTreeMap::new();
                        m.insert("min".to_string(), 0.0);
                        m.insert("max".to_string(), 100.0);
                        m
                    },
                    array_params: BTreeMap::new(),
                    round: false,
                },
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "b".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Derived {
                expr: "${c} + 1.0".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "a".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::Derived {
                expr: "${b} * 2.0".to_string(),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. }
                    if message.contains("dependency cycle"))
            }),
            "chain should not be flagged as cycle, got: {errors:?}"
        );
    }

    // ─── extract_template_refs tests ────────────────────────────────

    #[test]
    fn test_extract_template_refs_basic() {
        let mut deps = Vec::new();
        extract_template_refs("Hello ${name}, your age is ${age}", &mut deps);
        assert_eq!(deps, vec!["name", "age"]);
    }

    #[test]
    fn test_extract_template_refs_no_refs() {
        let mut deps = Vec::new();
        extract_template_refs("no refs here", &mut deps);
        assert!(deps.is_empty());
    }

    #[test]
    fn test_extract_template_refs_unclosed() {
        let mut deps = Vec::new();
        extract_template_refs("${good} and ${unclosed", &mut deps);
        assert_eq!(deps, vec!["good"]);
    }

    #[test]
    fn test_validate_noise_scope_invalid_expression() {
        use crate::core::types::NoiseScope;
        let mut model = minimal_model();
        model.noise_profiles.push(NoiseProfile {
            name: "scoped".to_string(),
            entity: "user".to_string(),
            fields: vec![],
            null_rate: 0.1,
            duplicate_rate: 0.0,
            typo_rate: 0.0,
            outlier_rate: 0.0,
            swap_rate: 0.0,
            truncate_rate: 0.0,
            fk_violate_rate: 0.0,
            temporal_spike_rate: 0.0,
            missing_field_rate: 0.0,
            scope: Some(NoiseScope {
                where_expr: "${invalid !!syntax".to_string(),
            }),
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("invalid scope expression"))
        }));
    }

    #[test]
    fn test_validate_noise_scope_unknown_field() {
        use crate::core::types::NoiseScope;
        let mut model = minimal_model();
        model.noise_profiles.push(NoiseProfile {
            name: "scoped".to_string(),
            entity: "user".to_string(),
            fields: vec![],
            null_rate: 0.1,
            duplicate_rate: 0.0,
            typo_rate: 0.0,
            outlier_rate: 0.0,
            swap_rate: 0.0,
            truncate_rate: 0.0,
            fk_violate_rate: 0.0,
            temporal_spike_rate: 0.0,
            missing_field_rate: 0.0,
            scope: Some(NoiseScope {
                where_expr: r#"${nonexistent_field} == "test""#.to_string(),
            }),
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("scope expression references unknown field"))
        }));
    }

    #[test]
    fn test_validate_noise_scope_valid() {
        use crate::core::types::NoiseScope;
        let mut model = minimal_model();
        model.noise_profiles.push(NoiseProfile {
            name: "scoped".to_string(),
            entity: "user".to_string(),
            fields: vec![],
            null_rate: 0.1,
            duplicate_rate: 0.0,
            typo_rate: 0.0,
            outlier_rate: 0.0,
            swap_rate: 0.0,
            truncate_rate: 0.0,
            fk_violate_rate: 0.0,
            temporal_spike_rate: 0.0,
            missing_field_rate: 0.0,
            scope: Some(NoiseScope {
                where_expr: r#"${email} == "admin@test.com""#.to_string(),
            }),
        });
        let errors = validate(&model);
        // Should have no scope-related errors (email is a valid field in minimal model)
        assert!(!errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("scope"))
        }));
    }

    #[test]
    fn test_validate_hierarchy_on_non_self_ref() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "order".to_string(),
            description: None,
            tags: Vec::new(),
            count: CountSpec::Fixed(10),
            fields: vec![],
            stats: None,
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
        output: None,
        });
        model.relationships.push(Relationship {
            name: "order_user".to_string(),
            from: "order".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: None,
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: Some(true),
            root_probability: Some(0.1),
            max_depth: Some(3),
            properties: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("acyclic") && message.contains("self-referential"))
        }));
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("root_probability") && message.contains("self-referential"))
        }));
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("max_depth") && message.contains("self-referential"))
        }));
    }

    #[test]
    fn test_validate_hierarchy_root_probability_range() {
        let mut model = minimal_model();
        model.relationships.push(Relationship {
            name: "self_ref".to_string(),
            from: "user".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: Some("parent_id".to_string()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: Some(0.0),
            max_depth: None,
            properties: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("root_probability") && message.contains("(0.0, 1.0]"))
        }));
    }

    #[test]
    fn test_validate_hierarchy_max_depth_zero() {
        let mut model = minimal_model();
        model.relationships.push(Relationship {
            name: "self_ref".to_string(),
            from: "user".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: Some("parent_id".to_string()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: Some(0),
            properties: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("max_depth") && message.contains(">= 1"))
        }));
    }

    #[test]
    fn test_validate_hierarchy_nullable_false_error() {
        let mut model = minimal_model();
        model.relationships.push(Relationship {
            name: "self_ref".to_string(),
            from: "user".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: Some("parent_id".to_string()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: Some(false),
            acyclic: Some(true),
            root_probability: Some(0.05),
            max_depth: None,
            properties: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("nullable = true"))
        }));
    }

    #[test]
    fn test_validate_hierarchy_valid_self_ref() {
        let mut model = minimal_model();
        model.relationships.push(Relationship {
            name: "self_ref".to_string(),
            from: "user".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: Some("parent_id".to_string()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: Some(true),
            acyclic: Some(true),
            root_probability: Some(0.05),
            max_depth: Some(6),
            properties: vec![],
        });
        let errors = validate(&model);
        assert!(!errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("acyclic") || message.contains("root_probability") || message.contains("max_depth"))
        }));
    }

    // ── Edge property validation tests ──────────────────────────────

    #[test]
    fn test_validate_edge_properties_many_to_many_rejected() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "order".to_string(),
            count: CountSpec::Fixed(10),
            fields: vec![Field {
                name: "id".to_string(),
                data_type: DataType::Int,
                primary_key: Some(true),
                nullable: NullSpec::Never,
                generator: None,
                description: None,
                precision: None,
                actor_column: false,
                fields: vec![],
                stats: None,
                traits: None,
            }],
            constraints: vec![],
            description: None,
            tags: Vec::new(),
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
        output: None,
        stats: None,
        });
        model.relationships.push(Relationship {
            name: "user_order".to_string(),
            from: "user".to_string(),
            to: "order".to_string(),
            kind: RelationshipKind::ManyToMany,
            foreign_key: None,
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![EdgeProperty {
                name: "quantity".to_string(),
                data_type: DataType::Int,
                generator: None,
                nullable: NullSpec::Never,
            }],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("many_to_many") && message.contains("not yet supported"))
        }));
    }

    #[test]
    fn test_validate_edge_property_name_conflict() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "order".to_string(),
            count: CountSpec::Fixed(10),
            fields: vec![Field {
                name: "id".to_string(),
                data_type: DataType::Int,
                primary_key: Some(true),
                nullable: NullSpec::Never,
                generator: None,
                description: None,
                precision: None,
                actor_column: false,
                fields: vec![],
                stats: None,
                traits: None,
            }],
            constraints: vec![],
            description: None,
            tags: Vec::new(),
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
        output: None,
        stats: None,
        });
        model.relationships.push(Relationship {
            name: "order_user".to_string(),
            from: "order".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: None,
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![EdgeProperty {
                name: "id".to_string(), // conflicts with entity field
                data_type: DataType::Int,
                generator: None,
                nullable: NullSpec::Never,
            }],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("conflicts with existing field"))
        }));
    }

    #[test]
    fn test_validate_edge_properties_valid() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "order".to_string(),
            count: CountSpec::Fixed(10),
            fields: vec![
                Field {
                    name: "id".to_string(),
                    data_type: DataType::Int,
                    primary_key: Some(true),
                    nullable: NullSpec::Never,
                    generator: None,
                    description: None,
                    precision: None,
                    actor_column: false,
                    fields: vec![],
                stats: None,
                traits: None,
                },
                Field {
                    name: "user_id".to_string(),
                    data_type: DataType::Int,
                    primary_key: None,
                    nullable: NullSpec::Never,
                    generator: None,
                    description: None,
                    precision: None,
                    actor_column: false,
                    fields: vec![],
                stats: None,
                traits: None,
                },
            ],
            constraints: vec![],
            description: None,
            tags: Vec::new(),
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
        output: None,
        stats: None,
        });
        model.relationships.push(Relationship {
            name: "order_user".to_string(),
            from: "order".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: Some("user_id".to_string()),
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![
                EdgeProperty {
                    name: "priority".to_string(),
                    data_type: DataType::String,
                    generator: None,
                    nullable: NullSpec::Never,
                },
                EdgeProperty {
                    name: "weight".to_string(),
                    data_type: DataType::Float,
                    generator: None,
                    nullable: NullSpec::Never,
                },
            ],
        });
        let errors = validate(&model);
        // No edge-property-related errors
        assert!(!errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("edge property"))
        }));
    }

    #[test]
    fn test_validate_edge_property_duplicate_name() {
        let mut model = minimal_model();
        model.entities.push(Entity {
            name: "order".to_string(),
            count: CountSpec::Fixed(10),
            fields: vec![Field {
                name: "id".to_string(),
                data_type: DataType::Int,
                primary_key: Some(true),
                nullable: NullSpec::Never,
                generator: None,
                description: None,
                precision: None,
                actor_column: false,
                fields: vec![],
                stats: None,
                traits: None,
            }],
            constraints: vec![],
            description: None,
            tags: Vec::new(),
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
        output: None,
        stats: None,
        });
        model.relationships.push(Relationship {
            name: "order_user".to_string(),
            from: "order".to_string(),
            to: "user".to_string(),
            kind: RelationshipKind::ManyToOne,
            foreign_key: None,
            cardinality: None,
            degree: None,
            selection: None,
            nullable: None,
            acyclic: None,
            root_probability: None,
            max_depth: None,
            properties: vec![
                EdgeProperty {
                    name: "weight".to_string(),
                    data_type: DataType::Float,
                    generator: None,
                    nullable: NullSpec::Never,
                },
                EdgeProperty {
                    name: "weight".to_string(), // duplicate
                    data_type: DataType::Int,
                    generator: None,
                    nullable: NullSpec::Never,
                },
            ],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("duplicate edge property name"))
        }));
    }

    // ── Conditional Distribution Validation Tests ─────────────────────

    fn model_with_category_and_amount() -> DataModel {
        let mut model = minimal_model();
        // Add category and amount fields to the user entity
        model.entities[0].fields.push(Field {
            name: "category".to_string(),
            description: None,
            data_type: DataType::String,
            generator: Some(GeneratorSpec::OneOf {
                choices: vec![
                    WeightedChoice { value: Value::String("groceries".into()), weight: 1.0 },
                    WeightedChoice { value: Value::String("electronics".into()), weight: 1.0 },
                ],
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "amount".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: None,
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model
    }

    #[test]
    fn test_validate_conditional_distribution_valid() {
        let mut model = model_with_category_and_amount();
        model.correlations.push(Correlation {
            entity: "user".to_string(),
            correlation_type: Some("conditional_distribution".to_string()),
            fields: vec![],
            matrix: vec![],
            conditional: vec![],
            copula: None,
            dependent: Some("amount".to_string()),
            given: Some("category".to_string()),
            distributions: vec![
                ConditionalDistributionBranch {
                    condition: Value::String("groceries".into()),
                    distribution: DistributionKind::LogNormal,
                    params: [("mu".into(), 3.0), ("sigma".into(), 0.8)].into(),
                    round: false,
                },
                ConditionalDistributionBranch {
                    condition: Value::String("electronics".into()),
                    distribution: DistributionKind::LogNormal,
                    params: [("mu".into(), 5.5), ("sigma".into(), 1.2)].into(),
                    round: false,
                },
            ],
            default: None,
        });
        let errors = validate(&model);
        assert!(!errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("conditional_distribution"))
        }));
    }

    #[test]
    fn test_validate_conditional_distribution_missing_dependent() {
        let mut model = model_with_category_and_amount();
        model.correlations.push(Correlation {
            entity: "user".to_string(),
            correlation_type: Some("conditional_distribution".to_string()),
            fields: vec![],
            matrix: vec![],
            conditional: vec![],
            copula: None,
            dependent: None,
            given: Some("category".to_string()),
            distributions: vec![ConditionalDistributionBranch {
                condition: Value::String("groceries".into()),
                distribution: DistributionKind::Normal,
                params: [("mean".into(), 50.0), ("std_dev".into(), 20.0)].into(),
                round: false,
            }],
            default: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("requires 'dependent'"))
        }));
    }

    #[test]
    fn test_validate_conditional_distribution_missing_given() {
        let mut model = model_with_category_and_amount();
        model.correlations.push(Correlation {
            entity: "user".to_string(),
            correlation_type: Some("conditional_distribution".to_string()),
            fields: vec![],
            matrix: vec![],
            conditional: vec![],
            copula: None,
            dependent: Some("amount".to_string()),
            given: None,
            distributions: vec![ConditionalDistributionBranch {
                condition: Value::String("groceries".into()),
                distribution: DistributionKind::Normal,
                params: [("mean".into(), 50.0), ("std_dev".into(), 20.0)].into(),
                round: false,
            }],
            default: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("requires 'given'"))
        }));
    }

    #[test]
    fn test_validate_conditional_distribution_same_dependent_given() {
        let mut model = model_with_category_and_amount();
        model.correlations.push(Correlation {
            entity: "user".to_string(),
            correlation_type: Some("conditional_distribution".to_string()),
            fields: vec![],
            matrix: vec![],
            conditional: vec![],
            copula: None,
            dependent: Some("amount".to_string()),
            given: Some("amount".to_string()),
            distributions: vec![ConditionalDistributionBranch {
                condition: Value::String("x".into()),
                distribution: DistributionKind::Normal,
                params: [("mean".into(), 50.0), ("std_dev".into(), 20.0)].into(),
                round: false,
            }],
            default: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("must differ"))
        }));
    }

    #[test]
    fn test_validate_conditional_distribution_empty_distributions() {
        let mut model = model_with_category_and_amount();
        model.correlations.push(Correlation {
            entity: "user".to_string(),
            correlation_type: Some("conditional_distribution".to_string()),
            fields: vec![],
            matrix: vec![],
            conditional: vec![],
            copula: None,
            dependent: Some("amount".to_string()),
            given: Some("category".to_string()),
            distributions: vec![],
            default: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("at least one distribution"))
        }));
    }

    #[test]
    fn test_validate_conditional_distribution_unknown_type() {
        let mut model = minimal_model();
        model.correlations.push(Correlation {
            entity: "user".to_string(),
            correlation_type: Some("bogus_type".to_string()),
            fields: vec![],
            matrix: vec![],
            conditional: vec![],
            copula: None,
            dependent: None,
            given: None,
            distributions: vec![],
            default: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("unknown correlation type"))
        }));
    }

    #[test]
    fn test_validate_conditional_distribution_duplicate_when() {
        let mut model = model_with_category_and_amount();
        model.correlations.push(Correlation {
            entity: "user".to_string(),
            correlation_type: Some("conditional_distribution".to_string()),
            fields: vec![],
            matrix: vec![],
            conditional: vec![],
            copula: None,
            dependent: Some("amount".to_string()),
            given: Some("category".to_string()),
            distributions: vec![
                ConditionalDistributionBranch {
                    condition: Value::String("groceries".into()),
                    distribution: DistributionKind::Normal,
                    params: [("mean".into(), 50.0), ("std_dev".into(), 20.0)].into(),
                    round: false,
                },
                ConditionalDistributionBranch {
                    condition: Value::String("groceries".into()),
                    distribution: DistributionKind::LogNormal,
                    params: [("mu".into(), 3.0), ("sigma".into(), 0.8)].into(),
                    round: false,
                },
            ],
            default: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("duplicate 'when'"))
        }));
    }

    #[test]
    fn test_validate_conditional_distribution_with_matrix() {
        let mut model = model_with_category_and_amount();
        model.correlations.push(Correlation {
            entity: "user".to_string(),
            correlation_type: Some("conditional_distribution".to_string()),
            fields: vec!["amount".into()],
            matrix: vec![vec![1.0]],
            conditional: vec![],
            copula: None,
            dependent: Some("amount".to_string()),
            given: Some("category".to_string()),
            distributions: vec![ConditionalDistributionBranch {
                condition: Value::String("groceries".into()),
                distribution: DistributionKind::Normal,
                params: [("mean".into(), 50.0), ("std_dev".into(), 20.0)].into(),
                round: false,
            }],
            default: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. }
                if message.contains("cannot use 'fields'") || message.contains("cannot use 'matrix'"))
        }));
    }

    // ─── Holiday effect validation tests ────────────────────────────

    #[test]
    fn test_validate_holiday_effect_empty_dates() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::Sequence {
                start: IntOrString::Int(0),
                step: IntOrString::Int(1),
                prefix: None,
                values: None,
                cycle: None,
            jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "metric".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::TimeSeries {
                baseline: 100.0,
                components: vec![crate::core::TimeSeriesComponent::HolidayEffect {
                    dates: vec![],
                    multiplier: 2.0,
                }],
                min: None,
                max: None,
                timestamp_field: Some("ts".to_string()),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("dates must not be empty"))
        }));
    }

    #[test]
    fn test_validate_holiday_effect_invalid_date() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::Sequence {
                start: IntOrString::Int(0),
                step: IntOrString::Int(1),
                prefix: None,
                values: None,
                cycle: None,
            jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "metric".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::TimeSeries {
                baseline: 100.0,
                components: vec![crate::core::TimeSeriesComponent::HolidayEffect {
                    dates: vec!["not-a-date".to_string()],
                    multiplier: 2.0,
                }],
                min: None,
                max: None,
                timestamp_field: Some("ts".to_string()),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("not a valid YYYY-MM-DD"))
        }));
    }

    #[test]
    fn test_validate_holiday_effect_requires_timestamp_field() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "metric".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::TimeSeries {
                baseline: 100.0,
                components: vec![crate::core::TimeSeriesComponent::HolidayEffect {
                    dates: vec!["2024-12-25".to_string()],
                    multiplier: 2.0,
                }],
                min: None,
                max: None,
                timestamp_field: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("requires a timestamp_field"))
        }));
    }

    #[test]
    fn test_validate_holiday_effect_zero_multiplier() {
        let mut model = minimal_model();
        model.entities[0].fields.push(Field {
            name: "ts".to_string(),
            description: None,
            data_type: DataType::Datetime,
            generator: Some(GeneratorSpec::Sequence {
                start: IntOrString::Int(0),
                step: IntOrString::Int(1),
                prefix: None,
                values: None,
                cycle: None,
            jitter: None,
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        model.entities[0].fields.push(Field {
            name: "metric".to_string(),
            description: None,
            data_type: DataType::Float,
            generator: Some(GeneratorSpec::TimeSeries {
                baseline: 100.0,
                components: vec![crate::core::TimeSeriesComponent::HolidayEffect {
                    dates: vec!["2024-12-25".to_string()],
                    multiplier: 0.0,
                }],
                min: None,
                max: None,
                timestamp_field: Some("ts".to_string()),
            }),
            nullable: NullSpec::Never,
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
                stats: None,
                traits: None,
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("multiplier must not be zero"))
        }));
    }

    #[test]
    fn test_not_null_constraint_valid() {
        let mut model = minimal_model();
        model.entities[0].constraints.push(Constraint::NotNull {
            fields: vec!["id".to_string()],
        });
        let errors = validate(&model);
        assert!(
            !errors.iter().any(|e| {
                matches!(e, BlueprintError::Validation { message, .. } if message.contains("constraint"))
            }),
            "expected no constraint errors, got: {:?}",
            errors
        );
    }

    #[test]
    fn test_not_null_constraint_unknown_field() {
        let mut model = minimal_model();
        model.entities[0].constraints.push(Constraint::NotNull {
            fields: vec!["nonexistent".to_string()],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("unknown field 'nonexistent'"))
        }));
    }

    #[test]
    fn test_not_null_constraint_empty_fields() {
        let mut model = minimal_model();
        model.entities[0].constraints.push(Constraint::NotNull {
            fields: vec![],
        });
        let errors = validate(&model);
        assert!(errors.iter().any(|e| {
            matches!(e, BlueprintError::Validation { message, .. } if message.contains("must not be empty"))
        }));
    }
}