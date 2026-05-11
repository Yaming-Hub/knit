//! Dimension detection from a DataModel.

use std::collections::BTreeMap;

use crate::core::types::{CountSpec, DataModel, GeneratorSpec, RelationshipKind};

use super::{ActorDimension, CustomDimension, ScalingAnalysis, TimeDimension};

/// Analyze a DataModel to discover scaling dimensions.
pub fn analyze(model: &DataModel) -> ScalingAnalysis {
    let entity_counts = collect_entity_counts(model);
    let actor = detect_actor(model, &entity_counts);
    let time = detect_time(model);
    let custom = detect_custom_dimensions(model, &entity_counts);

    ScalingAnalysis {
        actor,
        time,
        custom,
        entity_counts,
    }
}

/// Collect resolved entity counts.
fn collect_entity_counts(model: &DataModel) -> BTreeMap<String, u64> {
    model
        .entities
        .iter()
        .map(|e| {
            let count = match &e.count {
                CountSpec::Fixed(n) => *n,
                CountSpec::Range { min, max } => (min + max) / 2,
                CountSpec::Distribution(_) => 1000,
                CountSpec::Expression { .. } => {
                    crate::plan::partition::resolve_count(&e.count, &model.params)
                        .unwrap_or(1000)
                }
            };
            (e.name.clone(), count)
        })
        .collect()
}

/// Detect the actor (root) entity.
///
/// Priority: explicit `actor = true` > FK-root heuristic (most referenced entity).
fn detect_actor(
    model: &DataModel,
    entity_counts: &BTreeMap<String, u64>,
) -> Option<ActorDimension> {
    // 1. Explicit actor entity
    if let Some(actor_entity) = model.entities.iter().find(|e| e.actor) {
        let dependents = find_dependents(model, &actor_entity.name, entity_counts);
        let count = entity_counts
            .get(&actor_entity.name)
            .copied()
            .unwrap_or(1);
        return Some(ActorDimension {
            entity_name: actor_entity.name.clone(),
            current_count: count,
            dependents,
            confidence: 1.0,
        });
    }

    // 2. FK-root heuristic: entity referenced most by other entities
    let mut ref_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for rel in &model.relationships {
        *ref_counts.entry(rel.to.as_str()).or_insert(0) += 1;
    }

    // Also check actor_column FK fields
    for entity in &model.entities {
        for field in &entity.fields {
            if field.actor_column {
                // Find the FK target
                if let Some(ref gen) = field.generator {
                    if let GeneratorSpec::Lookup { entity: target, .. } = gen {
                        *ref_counts.entry(target.as_str()).or_insert(0) += 1;
                    }
                }
            }
        }
    }

    if let Some((&root_name, &count)) = ref_counts.iter().max_by_key(|(_, c)| *c) {
        if count >= 1 {
            let dependents = find_dependents(model, root_name, entity_counts);
            let entity_count = entity_counts.get(root_name).copied().unwrap_or(1);
            return Some(ActorDimension {
                entity_name: root_name.to_string(),
                current_count: entity_count,
                dependents,
                confidence: 0.6 + 0.1 * count.min(4) as f64,
            });
        }
    }

    None
}

/// Find entities that depend on the given entity via FK relationships.
fn find_dependents(
    model: &DataModel,
    entity_name: &str,
    entity_counts: &BTreeMap<String, u64>,
) -> Vec<(String, f64)> {
    let actor_count = entity_counts.get(entity_name).copied().unwrap_or(1) as f64;
    let mut dependents = Vec::new();

    for rel in &model.relationships {
        if rel.to == entity_name && rel.from != entity_name {
            let dep_count = entity_counts.get(&rel.from).copied().unwrap_or(1) as f64;
            let ratio = dep_count / actor_count.max(1.0);
            dependents.push((rel.from.clone(), ratio));
        }
    }

    dependents
}

/// Detect time dimension from partition-based entities.
fn detect_time(model: &DataModel) -> Option<TimeDimension> {
    for entity in &model.entities {
        if let Some(ref output) = entity.output {
            if let Some(ref partition_field) = output.partition_by {
                if output.partition_values.is_empty() {
                    continue;
                }

                // Check if partition values look like dates
                let values: Vec<String> = output
                    .partition_values
                    .iter()
                    .map(|pv| pv.value.clone())
                    .collect();

                if values.iter().all(|v| is_date_like(v)) {
                    let (cadence, confidence) = detect_cadence(&values);
                    return Some(TimeDimension {
                        entity_name: entity.name.clone(),
                        partition_field: partition_field.clone(),
                        partition_values: values,
                        cadence,
                        cadence_confidence: confidence,
                    });
                }
            }
        }
    }
    None
}

/// Check if a string looks like a date (YYYY-MM-DD or similar).
fn is_date_like(s: &str) -> bool {
    // Try common date formats
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
        || chrono::NaiveDate::parse_from_str(s, "%Y/%m/%d").is_ok()
        || chrono::NaiveDate::parse_from_str(s, "%Y%m%d").is_ok()
}

/// Detect cadence (gap between sorted dates) and confidence.
///
/// Recognizes monthly patterns (28–31 day gaps) and returns `Cadence::Months(1)`.
/// Otherwise returns `Cadence::Days(median_gap)`.
fn detect_cadence(values: &[String]) -> (Option<super::Cadence>, f64) {
    let mut dates: Vec<chrono::NaiveDate> = values
        .iter()
        .filter_map(|v| {
            chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d")
                .or_else(|_| chrono::NaiveDate::parse_from_str(v, "%Y/%m/%d"))
                .ok()
        })
        .collect();

    if dates.len() < 2 {
        return (None, 0.0);
    }

    dates.sort();

    let gaps: Vec<i64> = dates.windows(2).map(|w| (w[1] - w[0]).num_days()).collect();
    let median_gap = {
        let mut sorted_gaps = gaps.clone();
        sorted_gaps.sort();
        sorted_gaps[sorted_gaps.len() / 2]
    };

    if median_gap <= 0 {
        return (None, 0.0);
    }

    // Check if the pattern looks monthly:
    // 1. Gaps must be in 28–31 range
    // 2. Calendar alignment: same day-of-month across dates, OR all end-of-month
    let gaps_in_range = gaps.iter().all(|&g| (28..=31).contains(&g));
    let is_monthly = if gaps_in_range && dates.len() >= 3 {
        use chrono::Datelike;
        let all_same_day = dates.windows(2).all(|w| w[0].day() == w[1].day());
        let all_eom = dates.iter().all(|d| {
            d.day() == super::time::days_in_month(d.year(), d.month())
        });
        all_same_day || all_eom
    } else {
        false
    };

    // Confidence based on gap variance
    let variance: f64 = gaps
        .iter()
        .map(|g| (*g as f64 - median_gap as f64).powi(2))
        .sum::<f64>()
        / gaps.len() as f64;
    let cv = variance.sqrt() / median_gap as f64; // coefficient of variation

    let confidence = if is_monthly {
        // Monthly patterns naturally have variance (28–31 day gaps) — high confidence
        1.0
    } else if cv < 0.1 {
        1.0
    } else if cv < 0.5 {
        1.0 - cv
    } else {
        0.3
    };

    if is_monthly {
        (Some(super::Cadence::Months(1)), confidence)
    } else {
        (Some(super::Cadence::Days(median_gap as u32)), confidence)
    }
}

/// Detect custom dimensions (low-cardinality OneOf fields).
fn detect_custom_dimensions(
    model: &DataModel,
    entity_counts: &BTreeMap<String, u64>,
) -> Vec<CustomDimension> {
    let mut dimensions = Vec::new();

    // Collect FK field names and partition field names for exclusion
    let mut fk_fields: std::collections::HashSet<(String, String)> =
        std::collections::HashSet::new();
    for rel in &model.relationships {
        let fk_col = rel
            .foreign_key
            .clone()
            .unwrap_or_else(|| format!("{}_id", rel.to));
        fk_fields.insert((rel.from.clone(), fk_col));
    }

    for entity in &model.entities {
        let partition_field = entity
            .output
            .as_ref()
            .and_then(|o| o.partition_by.as_ref());

        for field in &entity.fields {
            // Skip PK, FK, partition, and actor fields
            if field.primary_key.unwrap_or(false) {
                continue;
            }
            if field.actor_column {
                continue;
            }
            if fk_fields.contains(&(entity.name.clone(), field.name.clone())) {
                continue;
            }
            if partition_field == Some(&field.name) {
                continue;
            }

            // Check if this field uses OneOf with low cardinality
            if let Some(GeneratorSpec::OneOf { ref choices }) = field.generator {
                if choices.len() >= 2 && choices.len() <= 50 {
                    let entity_count =
                        entity_counts.get(&entity.name).copied().unwrap_or(1);
                    let ratio = choices.len() as f64 / entity_count as f64;
                    if ratio < 0.1 || choices.len() <= 20 {
                        // Check if this field is used as a condition key
                        let is_condition_key = entity.fields.iter().any(|f| {
                            matches!(
                                &f.generator,
                                Some(GeneratorSpec::Conditional { field: ref cond_field, .. })
                                if cond_field == &field.name
                            )
                        });

                        let values: Vec<(String, f64)> = choices
                            .iter()
                            .map(|c| {
                                let v = match &c.value {
                                    crate::core::Value::String(s) => s.clone(),
                                    other => format!("{:?}", other),
                                };
                                (v, c.weight)
                            })
                            .collect();

                        dimensions.push(CustomDimension {
                            entity_name: entity.name.clone(),
                            field_name: field.name.clone(),
                            current_values: values,
                            is_condition_key,
                        });
                    }
                }
            }
        }
    }

    dimensions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::*;

    fn make_test_model() -> DataModel {
        DataModel {
            name: "test".into(),
            description: None,
            seed: 42,
            locale: "en_US".into(),
            timezone: "UTC".into(),
            blueprint_version: "1.0".into(),
            params: BTreeMap::new(),
            noise_profiles: vec![],
            correlations: vec![],
            personas: vec![],
            actor_relationships: vec![],
            custom_types: vec![],
            mixins: vec![],
            companion_files: vec![],
            entities: vec![
                Entity {
                    name: "Users".into(),
                    description: None,
                    tags: vec![],
                    count: CountSpec::Fixed(10),
                    fields: vec![Field {
                        name: "id".into(),
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
                    constraints: vec![],
                    topology: None,
                    actor: true,
                    persona_distribution: None,
                    activity_count: None,
                    mixin_refs: None,
                    output: None,
                    stats: None,
                },
                Entity {
                    name: "Events".into(),
                    description: None,
                    tags: vec![],
                    count: CountSpec::Fixed(100),
                    fields: vec![
                        Field {
                            name: "id".into(),
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
                        },
                        Field {
                            name: "user_id".into(),
                            description: None,
                            data_type: DataType::Int,
                            generator: Some(GeneratorSpec::Lookup {
                                entity: "Users".into(),
                                field: "id".into(),
                            }),
                            nullable: NullSpec::Never,
                            primary_key: None,
                            precision: None,
                            actor_column: false,
                            fields: vec![],
                stats: None,
                traits: None,
                        },
                        Field {
                            name: "Region".into(),
                            description: None,
                            data_type: DataType::String,
                            generator: Some(GeneratorSpec::OneOf {
                                choices: vec![
                                    WeightedChoice {
                                        value: Value::String("US".into()),
                                        weight: 0.5,
                                    },
                                    WeightedChoice {
                                        value: Value::String("EU".into()),
                                        weight: 0.3,
                                    },
                                    WeightedChoice {
                                        value: Value::String("APAC".into()),
                                        weight: 0.2,
                                    },
                                ],
                            }),
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
                },
            ],
            relationships: vec![Relationship {
                name: "events_users".into(),
                from: "Events".into(),
                to: "Users".into(),
                kind: RelationshipKind::ManyToOne,
                foreign_key: Some("user_id".into()),
                cardinality: None,
                degree: None,
                selection: None,
                nullable: None,
                acyclic: None,
                root_probability: None,
                max_depth: None,
                properties: vec![],
            }],
        }
    }

    #[test]
    fn test_detect_actor_explicit() {
        let model = make_test_model();
        let counts = collect_entity_counts(&model);
        let actor = detect_actor(&model, &counts);
        assert!(actor.is_some());
        let a = actor.unwrap();
        assert_eq!(a.entity_name, "Users");
        assert_eq!(a.current_count, 10);
        assert_eq!(a.confidence, 1.0);
        assert_eq!(a.dependents.len(), 1);
        assert_eq!(a.dependents[0].0, "Events");
    }

    #[test]
    fn test_detect_custom_dimension() {
        let model = make_test_model();
        let counts = collect_entity_counts(&model);
        let custom = detect_custom_dimensions(&model, &counts);
        assert_eq!(custom.len(), 1);
        assert_eq!(custom[0].entity_name, "Events");
        assert_eq!(custom[0].field_name, "Region");
        assert_eq!(custom[0].current_values.len(), 3);
    }

    #[test]
    fn test_detect_time_no_partitions() {
        let model = make_test_model();
        let time = detect_time(&model);
        assert!(time.is_none());
    }

    #[test]
    fn test_cadence_detection_weekly() {
        let values: Vec<String> = vec![
            "2024-01-01", "2024-01-08", "2024-01-15", "2024-01-22", "2024-01-29",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let (cadence, confidence) = detect_cadence(&values);
        assert_eq!(cadence, Some(crate::scale::Cadence::Days(7)));
        assert!(confidence > 0.9);
    }

    #[test]
    fn test_cadence_detection_monthly() {
        let values: Vec<String> = vec![
            "2024-01-01", "2024-02-01", "2024-03-01", "2024-04-01", "2024-05-01",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let (cadence, confidence) = detect_cadence(&values);
        assert_eq!(cadence, Some(crate::scale::Cadence::Months(1)));
        assert!(confidence > 0.9);
    }

    #[test]
    fn test_cadence_detection_4week_not_monthly() {
        // Every 28 days, but NOT same day-of-month or EOM — should be Days(28)
        let values: Vec<String> = vec![
            "2024-01-01", "2024-01-29", "2024-02-26", "2024-03-25",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let (cadence, _confidence) = detect_cadence(&values);
        assert_eq!(cadence, Some(crate::scale::Cadence::Days(28)));
    }

    #[test]
    fn test_cadence_detection_monthly_eom() {
        // End-of-month dates: 31, 29, 31, 30, 31 — should detect as monthly
        let values: Vec<String> = vec![
            "2024-01-31", "2024-02-29", "2024-03-31", "2024-04-30", "2024-05-31",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let (cadence, confidence) = detect_cadence(&values);
        assert_eq!(cadence, Some(crate::scale::Cadence::Months(1)));
        assert!(confidence > 0.9);
    }

    #[test]
    fn test_is_date_like() {
        assert!(is_date_like("2024-01-01"));
        assert!(is_date_like("2024/06/15"));
        assert!(!is_date_like("hello"));
        assert!(!is_date_like("123"));
    }
}