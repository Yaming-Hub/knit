//! Multi-dimensional dataset scaling.
//!
//! Analyzes a learned schema to discover scaling dimensions (actors, time,
//! custom categorical fields), computes a scaling plan, and rewrites the
//! DataModel so the generate pipeline produces the scaled output.

pub mod analyze;
pub mod naming;
pub mod time;

use std::collections::BTreeMap;

use crate::core::types::{CountSpec, DataModel, GeneratorSpec, Value, WeightedChoice};

// ── Cadence type ────────────────────────────────────────────────────

/// Cadence (stepping interval) for time-based partitions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cadence {
    /// Fixed number of days (e.g., 1 for daily, 7 for weekly).
    Days(u32),
    /// Calendar months (properly handles variable-length months).
    Months(u32),
}

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
    pub cadence: Option<Cadence>,
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
    /// User-specified cadence override (e.g. 7d for weekly, 1m for monthly).
    pub cadence: Option<Cadence>,
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
        // Apply cadence override or warn on low confidence
        let effective_dim = if let Some(override_cadence) = targets.cadence {
            tracing::debug!(
                ?override_cadence,
                detected = ?time_dim.cadence,
                "using cadence override"
            );
            let mut dim = time_dim.clone();
            dim.cadence = Some(override_cadence);
            dim.cadence_confidence = 1.0;
            dim
        } else {
            if time_dim.cadence_confidence < 0.5 {
                tracing::warn!(
                    confidence = time_dim.cadence_confidence,
                    detected = ?time_dim.cadence,
                    "low cadence confidence; consider using --cadence to specify explicitly (e.g. --cadence 7d)"
                );
            }
            time_dim.clone()
        };

        let new_values = time::compute_new_partitions(&effective_dim, time_spec)?;
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

    // Upscale: keep existing, add new with smart naming
    let existing_total: f64 = existing.iter().map(|(_, w)| w).sum();
    let avg_weight = if existing.is_empty() {
        1.0
    } else {
        existing_total / existing.len() as f64
    };

    let new_count = target - existing.len();
    let strategy = naming::detect_strategy(existing);
    let new_names = naming::generate_values(&strategy, existing, new_count);

    let mut result: Vec<(String, f64)> = existing.to_vec();
    for name in new_names {
        result.push((name, avg_weight));
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

// ── Size estimation ─────────────────────────────────────────────────

/// Estimated output size per entity.
#[derive(Debug, Clone)]
pub struct EntitySizeEstimate {
    pub entity_name: String,
    pub rows: u64,
    /// Estimated bytes in CSV format.
    pub csv_bytes: u64,
    /// Estimated bytes in JSON/JSONL format.
    pub json_bytes: u64,
}

/// Estimate output sizes for a scaling plan.
///
/// Returns per-entity estimates and total CSV bytes.
/// Uses heuristic bytes-per-field based on data type.
/// Parquet is typically 30-50% smaller due to compression; callers estimate 40% of CSV.
pub fn estimate_output_size(
    model: &DataModel,
    analysis: &ScalingAnalysis,
    plan: &ScalingPlan,
) -> (Vec<EntitySizeEstimate>, u64) {
    let mut estimates = Vec::new();
    let mut total_csv = 0u64;

    for entity in &model.entities {
        let rows = plan
            .entity_overrides
            .get(&entity.name)
            .copied()
            .unwrap_or_else(|| {
                analysis
                    .entity_counts
                    .get(&entity.name)
                    .copied()
                    .unwrap_or(0)
            });

        let csv_per_row = estimate_row_bytes(&entity.fields);
        let json_per_row = estimate_json_row_bytes(&entity.fields);
        let csv_bytes = rows * csv_per_row;
        let json_bytes = rows * json_per_row;

        estimates.push(EntitySizeEstimate {
            entity_name: entity.name.clone(),
            rows,
            csv_bytes,
            json_bytes,
        });
        total_csv += csv_bytes;
    }

    (estimates, total_csv)
}

/// Estimate bytes per row for a list of fields (CSV-like format).
fn estimate_row_bytes(fields: &[crate::core::types::Field]) -> u64 {
    use crate::core::types::DataType;

    let field_bytes: u64 = fields
        .iter()
        .map(|f| match f.data_type {
            DataType::Bool => 5,
            DataType::Int | DataType::Int32 => 8,
            DataType::Float => 12,
            DataType::String => 20,
            DataType::Uuid => 36,
            DataType::Date => 10,
            DataType::Time => 8,
            DataType::Datetime | DataType::DatetimeUs | DataType::Datetimetz => 24,
            DataType::Duration => 12,
            DataType::Bytes => 24,
            DataType::Array | DataType::Map | DataType::Object => 40,
            _ => 16,
        })
        .sum();

    let delimiters = fields.len().saturating_sub(1) as u64;
    field_bytes + delimiters + 1 // +delimiters between fields, +1 newline
}

/// Estimate JSON bytes per row: field names + values + syntax overhead.
fn estimate_json_row_bytes(fields: &[crate::core::types::Field]) -> u64 {
    use crate::core::types::DataType;

    let mut total: u64 = 2; // { and }
    for (i, f) in fields.iter().enumerate() {
        // "field_name": + value
        let name_overhead = f.name.len() as u64 + 4; // quotes, colon, space
        let value_bytes: u64 = match f.data_type {
            DataType::Bool => 5,
            DataType::Int | DataType::Int32 => 8,
            DataType::Float => 12,
            DataType::String => 22, // quotes + content
            DataType::Uuid => 38,   // quotes + 36 chars
            DataType::Date => 12,
            DataType::Time => 10,
            DataType::Datetime | DataType::DatetimeUs | DataType::Datetimetz => 26,
            DataType::Duration => 14,
            DataType::Bytes => 26,
            DataType::Array | DataType::Map | DataType::Object => 50,
            _ => 18,
        };
        total += name_overhead + value_bytes;
        if i < fields.len() - 1 {
            total += 2; // ", "
        }
    }
    total + 1 // newline
}

/// Format a byte count as a human-readable string (e.g., "1.2 MB").
pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;

    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} B", bytes)
    }
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
        // Smart naming: mixed uppercase codes → Generic strategy → "value_N"
        assert_eq!(result[3].0, "value_1");
        assert_eq!(result[4].0, "value_2");
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

    #[test]
    fn test_expand_oneof_country_codes() {
        // 2-letter country codes should use the country code pool
        let existing = vec![
            ("US".to_string(), 0.5),
            ("GB".to_string(), 0.3),
            ("DE".to_string(), 0.2),
        ];
        let result = expand_oneof_values(&existing, 8);
        assert_eq!(result.len(), 8);
        // First 3 are original
        assert_eq!(result[0].0, "US");
        assert_eq!(result[1].0, "GB");
        assert_eq!(result[2].0, "DE");
        // New values should be real country codes, not "value_N"
        for (name, _) in &result[3..] {
            assert_eq!(name.len(), 2);
            assert!(name.chars().all(|c| c.is_ascii_uppercase()));
            assert_ne!(name, "US");
            assert_ne!(name, "GB");
            assert_ne!(name, "DE");
        }
    }

    #[test]
    fn test_expand_oneof_capitalized_words() {
        let existing = vec![
            ("Electronics".to_string(), 0.6),
            ("Clothing".to_string(), 0.4),
        ];
        let result = expand_oneof_values(&existing, 5);
        assert_eq!(result.len(), 5);
        assert_eq!(result[0].0, "Electronics");
        assert_eq!(result[1].0, "Clothing");
        // New values should be capitalized words from the pool
        for (name, _) in &result[2..] {
            assert!(name.chars().next().unwrap().is_ascii_uppercase());
            assert_ne!(name, "Electronics");
            assert_ne!(name, "Clothing");
        }
    }

    #[test]
    fn test_format_bytes() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(1024), "1.0 KB");
        assert_eq!(format_bytes(1536), "1.5 KB");
        assert_eq!(format_bytes(1_048_576), "1.0 MB");
        assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
    }

    #[test]
    fn test_estimate_row_bytes() {
        use crate::core::types::{DataType, Field, NullSpec};

        let fields = vec![
            Field {
                name: "id".into(),
                data_type: DataType::Uuid,
                description: None,
                generator: None,
                nullable: NullSpec::default(),
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
                stats: None,
                traits: None,
            },
            Field {
                name: "name".into(),
                data_type: DataType::String,
                description: None,
                generator: None,
                nullable: NullSpec::default(),
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
                stats: None,
                traits: None,
            },
            Field {
                name: "age".into(),
                data_type: DataType::Int,
                description: None,
                generator: None,
                nullable: NullSpec::default(),
                primary_key: None,
                precision: None,
                actor_column: false,
                fields: vec![],
                stats: None,
                traits: None,
            },
        ];
        let bytes = estimate_row_bytes(&fields);
        // UUID(36) + String(20) + Int(8) + 2 delimiters + 1 newline = 67
        assert_eq!(bytes, 67);
    }

    #[test]
    fn test_estimate_output_size() {
        use crate::core::types::{CountSpec, DataModel, DataType, Entity, Field, NullSpec};

        let make_field = |name: &str, dt: DataType| Field {
            name: name.into(),
            data_type: dt,
            description: None,
            generator: None,
            nullable: NullSpec::default(),
            primary_key: None,
            precision: None,
            actor_column: false,
            fields: vec![],
            stats: None,
            traits: None,
        };

        let model = DataModel {
            entities: vec![Entity {
                name: "Users".into(),
                fields: vec![
                    make_field("id", DataType::Uuid),
                    make_field("name", DataType::String),
                ],
                count: CountSpec::Fixed(100),
                description: None,
                tags: vec![],
                constraints: vec![],
                topology: None,
                actor: false,
                persona_distribution: None,
                activity_count: None,
                mixin_refs: None,
                output: None,
                stats: None,
            }],
            ..DataModel::default()
        };

        let mut entity_counts = BTreeMap::new();
        entity_counts.insert("Users".to_string(), 100);

        let analysis = ScalingAnalysis {
            actor: None,
            time: None,
            custom: vec![],
            entity_counts,
        };

        let mut overrides = BTreeMap::new();
        overrides.insert("Users".to_string(), 1000);

        let plan = ScalingPlan {
            entity_overrides: overrides,
            new_partitions: None,
            dim_overrides: vec![],
        };

        let (estimates, total) = estimate_output_size(&model, &analysis, &plan);
        assert_eq!(estimates.len(), 1);
        assert_eq!(estimates[0].rows, 1000);
        // UUID(36) + String(20) + 1 delimiter + 1 newline = 58 bytes/row
        assert_eq!(estimates[0].csv_bytes, 58_000);
        assert_eq!(total, 58_000);
        // JSON: {"id": UUID(38) + "name": String(22)} = 2 + (4+2+4+38) + (6+2+4+22) + 2 + 1 = 85
        assert!(estimates[0].json_bytes > estimates[0].csv_bytes);
    }
}