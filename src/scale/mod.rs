//! Multi-dimensional dataset scaling.
//!
//! Analyzes a learned schema to discover scaling dimensions (actors, time,
//! custom categorical fields), computes a scaling plan, and rewrites the
//! DataModel so the generate pipeline produces the scaled output.

pub mod analyze;
pub mod time;

use std::collections::BTreeMap;

use crate::core::types::{CountSpec, DataModel, GeneratorSpec, Value, WeightedChoice};

// ── Analysis types ──────────────────────────────────────────────────

/// Full analysis of a schema's scaling dimensions.
#[derive(Debug, Clone)]
pub struct ScalingAnalysis {
    /// Detected actor (people/root) entity.
    pub actor: Option<ActorDimension>,
    /// Detected time (partition-based) dimension.
    pub time: Option<TimeDimension>,
    /// Detected custom categorical dimensions.
    pub custom: Vec<CustomDimension>,
    /// Current row count per entity.
    pub entity_counts: BTreeMap<String, u64>,
}

/// Actor/root entity dimension.
#[derive(Debug, Clone)]
pub struct ActorDimension {
    pub entity_name: String,
    pub current_count: u64,
    /// Dependent entities with their cardinality ratio (child_rows / actor_rows).
    pub dependents: Vec<(String, f64)>,
    pub confidence: f64,
}

/// Time (partition-based) dimension.
#[derive(Debug, Clone)]
pub struct TimeDimension {
    pub entity_name: String,
    pub partition_field: String,
    pub partition_values: Vec<String>,
    pub cadence_days: Option<u32>,
    pub cadence_confidence: f64,
}

/// Custom categorical dimension.
#[derive(Debug, Clone)]
pub struct CustomDimension {
    pub entity_name: String,
    pub field_name: String,
    pub current_values: Vec<(String, f64)>,
    pub is_condition_key: bool,
}

// ── Scaling plan ────────────────────────────────────────────────────

/// Computed plan describing how to transform the DataModel.
#[derive(Debug, Clone)]
pub struct ScalingPlan {
    /// New entity counts (entity_name → new_count).
    pub entity_overrides: BTreeMap<String, u64>,
    /// New partition values (if time dimension scaled).
    pub new_partitions: Option<NewPartitions>,
    /// Expanded values for custom dimensions.
    pub dim_overrides: Vec<DimOverride>,
}

/// New partition values for time scaling.
#[derive(Debug, Clone)]
pub struct NewPartitions {
    pub entity_name: String,
    pub field_name: String,
    pub values: Vec<crate::core::types::PartitionValue>,
}

/// Override for a custom dimension's OneOf values.
#[derive(Debug, Clone)]
pub struct DimOverride {
    pub entity_name: String,
    pub field_name: String,
    pub new_values: Vec<(String, f64)>,
}

// ── Plan computation ────────────────────────────────────────────────

/// User-specified scaling targets.
#[derive(Debug, Clone, Default)]
pub struct ScaleTargets {
    /// Target actor count.
    pub actors: Option<u64>,
    /// Target time spec (duration like "52w" or range like "2024-01-01..2025-12-31").
    pub time: Option<String>,
    /// Custom dimension targets: (dimension_name, target_cardinality).
    pub dims: Vec<(String, u64)>,
    /// Additional uniform count multiplier.
    pub count: Option<f64>,
}

/// Compute a scaling plan from the analysis and user targets.
pub fn compute_plan(
    analysis: &ScalingAnalysis,
    targets: &ScaleTargets,
) -> anyhow::Result<ScalingPlan> {
    let mut entity_overrides = BTreeMap::new();

    // Actor scaling: set actor count, scale dependents proportionally.
    if let (Some(target_actors), Some(actor)) = (targets.actors, &analysis.actor) {
        let ratio = target_actors as f64 / actor.current_count.max(1) as f64;
        entity_overrides.insert(actor.entity_name.clone(), target_actors);

        for (dep_name, _dep_ratio) in &actor.dependents {
            let current = analysis.entity_counts.get(dep_name).copied().unwrap_or(1);
            let new_count = (current as f64 * ratio).round() as u64;
            entity_overrides.insert(dep_name.clone(), new_count.max(1));
        }
    }

    // Time scaling
    let new_partitions = if let (Some(ref time_spec), Some(time_dim)) =
        (&targets.time, &analysis.time)
    {
        let new_values = time::compute_new_partitions(time_dim, time_spec)?;
        let time_ratio = new_values.len() as f64
            / time_dim.partition_values.len().max(1) as f64;

        // Scale entities that use this partition proportionally
        let current = analysis
            .entity_counts
            .get(&time_dim.entity_name)
            .copied()
            .unwrap_or(1);
        let base = entity_overrides
            .get(&time_dim.entity_name)
            .copied()
            .unwrap_or(current);
        entity_overrides.insert(
            time_dim.entity_name.clone(),
            (base as f64 * time_ratio).round() as u64,
        );

        Some(NewPartitions {
            entity_name: time_dim.entity_name.clone(),
            field_name: time_dim.partition_field.clone(),
            values: new_values,
        })
    } else {
        None
    };

    // Custom dimension scaling
    let mut dim_overrides = Vec::new();
    for (dim_name, target_card) in &targets.dims {
        let dim = analysis
            .custom
            .iter()
            .find(|d| d.field_name.eq_ignore_ascii_case(dim_name))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "dimension '{}' not found; run --analyze to see available dimensions",
                    dim_name
                )
            })?;

        let old_card = dim.current_values.len() as u64;
        let dim_ratio = *target_card as f64 / old_card.max(1) as f64;

        // Expand values
        let new_values = expand_oneof_values(&dim.current_values, *target_card);
        dim_overrides.push(DimOverride {
            entity_name: dim.entity_name.clone(),
            field_name: dim.field_name.clone(),
            new_values,
        });

        // Scale entity row count proportionally
        let current = analysis
            .entity_counts
            .get(&dim.entity_name)
            .copied()
            .unwrap_or(1);
        let base = entity_overrides
            .get(&dim.entity_name)
            .copied()
            .unwrap_or(current);
        entity_overrides.insert(
            dim.entity_name.clone(),
            (base as f64 * dim_ratio).round().max(1.0) as u64,
        );
    }

    // Uniform count multiplier (applied on top of other scaling)
    if let Some(factor) = targets.count {
        for (_, count) in entity_overrides.iter_mut() {
            *count = (*count as f64 * factor).round().max(1.0) as u64;
        }
        // Also scale entities not yet overridden
        for (name, &current) in &analysis.entity_counts {
            entity_overrides
                .entry(name.clone())
                .or_insert_with(|| (current as f64 * factor).round().max(1.0) as u64);
        }
    }

    Ok(ScalingPlan {
        entity_overrides,
        new_partitions,
        dim_overrides,
    })
}

// ── Schema rewrite ──────────────────────────────────────────────────

/// Apply a scaling plan to a DataModel, modifying it in place.
pub fn rewrite(model: &mut DataModel, plan: &ScalingPlan) {
    // Override entity counts
    for entity in &mut model.entities {
        if let Some(&new_count) = plan.entity_overrides.get(&entity.name) {
            tracing::debug!(
                entity = %entity.name,
                old_count = ?entity.count,
                new_count,
                "scaling entity count"
            );
            entity.count = CountSpec::Fixed(new_count);
        }
    }

    // Replace partition values and sync partition field generator
    if let Some(ref np) = plan.new_partitions {
        for entity in &mut model.entities {
            if entity.name == np.entity_name {
                if let Some(ref mut output) = entity.output {
                    tracing::debug!(
                        entity = %entity.name,
                        old_partitions = output.partition_values.len(),
                        new_partitions = np.values.len(),
                        "scaling partitions"
                    );
                    output.partition_values = np.values.clone();

                    // Also update the partition field's generator to produce
                    // values matching the new partition set.
                    if let Some(partition_field) = &output.partition_by {
                        for field in &mut entity.fields {
                            if field.name == *partition_field {
                                let choices: Vec<WeightedChoice> = np
                                    .values
                                    .iter()
                                    .map(|pv| WeightedChoice {
                                        value: Value::String(pv.value.clone()),
                                        weight: pv.weight,
                                    })
                                    .collect();
                                field.generator =
                                    Some(GeneratorSpec::OneOf { choices });
                                tracing::debug!(
                                    entity = %entity.name,
                                    field = %field.name,
                                    values = np.values.len(),
                                    "synced partition field generator"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // Replace OneOf choices for custom dimensions
    for dim_override in &plan.dim_overrides {
        for entity in &mut model.entities {
            if entity.name == dim_override.entity_name {
                for field in &mut entity.fields {
                    if field.name == dim_override.field_name {
                        let choices: Vec<WeightedChoice> = dim_override
                            .new_values
                            .iter()
                            .map(|(v, w)| WeightedChoice {
                                value: Value::String(v.clone()),
                                weight: *w,
                            })
                            .collect();
                        tracing::debug!(
                            entity = %entity.name,
                            field = %field.name,
                            old_values = ?field.generator,
                            new_count = choices.len(),
                            "scaling custom dimension"
                        );
                        field.generator = Some(GeneratorSpec::OneOf { choices });
                    }
                }
            }
        }
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Expand a OneOf value set to the target cardinality.
/// Keeps existing values, adds indexed suffixes for new ones.
/// Weights follow the existing distribution shape (normalized).
fn expand_oneof_values(
    existing: &[(String, f64)],
    target_count: u64,
) -> Vec<(String, f64)> {
    let target = target_count as usize;
    if target <= existing.len() {
        // Downscale: keep top-N by weight
        let mut sorted: Vec<_> = existing.to_vec();
        sorted.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        sorted.truncate(target);
        let total: f64 = sorted.iter().map(|(_, w)| w).sum();
        if total > 0.0 {
            for item in &mut sorted {
                item.1 /= total;
            }
        }
        return sorted;
    }

    // Upscale: keep existing, add new with average weight
    let existing_total: f64 = existing.iter().map(|(_, w)| w).sum();
    let avg_weight = if existing.is_empty() {
        1.0
    } else {
        existing_total / existing.len() as f64
    };

    let mut result: Vec<(String, f64)> = existing.to_vec();
    let prefix = if let Some((first, _)) = existing.first() {
        // Try to extract a base name (e.g., "US" → "value")
        if first.chars().all(|c| c.is_uppercase() || c.is_ascii_digit()) {
            "value".to_string()
        } else {
            first.clone()
        }
    } else {
        "value".to_string()
    };

    for i in existing.len()..target {
        result.push((format!("{}_{}", prefix, i + 1), avg_weight));
    }

    // Normalize weights
    let total: f64 = result.iter().map(|(_, w)| w).sum();
    if total > 0.0 {
        for item in &mut result {
            item.1 /= total;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_oneof_upscale() {
        let existing = vec![
            ("US".to_string(), 0.5),
            ("EU".to_string(), 0.3),
            ("APAC".to_string(), 0.2),
        ];
        let result = expand_oneof_values(&existing, 5);
        assert_eq!(result.len(), 5);
        assert_eq!(result[0].0, "US");
        assert_eq!(result[3].0, "value_4");
        assert_eq!(result[4].0, "value_5");
        let total: f64 = result.iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_expand_oneof_downscale() {
        let existing = vec![
            ("A".to_string(), 0.5),
            ("B".to_string(), 0.3),
            ("C".to_string(), 0.2),
        ];
        let result = expand_oneof_values(&existing, 2);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].0, "A");
        assert_eq!(result[1].0, "B");
        let total: f64 = result.iter().map(|(_, w)| w).sum();
        assert!((total - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_expand_oneof_same_size() {
        let existing = vec![("X".to_string(), 0.5), ("Y".to_string(), 0.5)];
        let result = expand_oneof_values(&existing, 2);
        assert_eq!(result.len(), 2);
    }
}
