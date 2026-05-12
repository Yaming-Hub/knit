//! Model enrichment from reference samples.
//!
//! Extracts statistical knowledge from reference data and merges it into
//! a base model, improving generated data quality without storing actual values.

pub mod extract;
pub mod interactive;
pub mod mapper;
pub mod merge;
pub mod quality;

use std::path::Path;

use anyhow::{Context, Result};
use tracing::{info, warn};

use crate::core::DataModel;
use crate::learn::ingest::read_auto_with_limit;
use crate::learn::profile::compute_profiles;

use self::mapper::{ColumnMapping, map_columns};
use self::merge::merge_enrichment;

/// Configuration for an enrichment run.
#[derive(Debug, Clone)]
pub struct EnrichConfig {
    /// Minimum confidence for auto-accepting a column mapping (0.0–1.0).
    pub min_confidence: f64,
    /// Maximum row count to read from reference (None = all).
    pub max_rows: Option<usize>,
    /// If true, show what would be enriched without modifying the model.
    pub dry_run: bool,
    /// Only enrich fields in this entity (None = all entities).
    pub entity_filter: Option<String>,
    /// If true, interactively confirm each column mapping.
    pub interactive: bool,
}

impl Default for EnrichConfig {
    fn default() -> Self {
        Self {
            min_confidence: 0.7,
            max_rows: Some(100_000),
            dry_run: false,
            entity_filter: None,
            interactive: false,
        }
    }
}

/// Result of an enrichment run.
#[derive(Debug)]
pub struct EnrichResult {
    /// Number of reference columns processed.
    pub ref_columns: usize,
    /// Number of columns successfully mapped.
    pub mapped_columns: usize,
    /// Number of fields actually enriched (had compatible generators).
    pub enriched_fields: usize,
    /// Number of fields skipped (incompatible generator kind).
    pub skipped_fields: usize,
    /// Number of columns with no mapping found.
    pub unmapped_columns: usize,
    /// The mappings used.
    pub mappings: Vec<ColumnMapping>,
    /// Quality report (present when enrichment ran, not dry-run).
    pub quality_report: Option<quality::QualityReport>,
}

/// Run the enrichment pipeline: load reference → profile → map → extract → merge.
pub fn enrich(
    model: &mut DataModel,
    ref_path: &Path,
    config: &EnrichConfig,
) -> Result<EnrichResult> {
    // Phase 1: Ingest reference data
    info!(path = %ref_path.display(), "loading reference sample");
    let batches = read_auto_with_limit(ref_path, config.max_rows)
        .with_context(|| format!("reading reference {}", ref_path.display()))?;

    if batches.is_empty() {
        anyhow::bail!("reference sample is empty: {}", ref_path.display());
    }

    // Phase 2: Profile reference columns
    let profiles = compute_profiles(&batches)
        .with_context(|| "profiling reference data")?;
    info!(columns = profiles.len(), "reference profiled");

    // Phase 3: Determine target entity
    let target_entity_name = resolve_target_entity(model, ref_path, config)?;
    info!(entity = %target_entity_name, "target entity for enrichment");

    let entity = model.entities.iter()
        .find(|e| e.name == target_entity_name)
        .ok_or_else(|| anyhow::anyhow!("entity '{}' not found in model", target_entity_name))?
        .clone();

    // Phase 4: Map reference columns to entity fields
    let mappings = map_columns(&profiles, &entity, config.min_confidence);
    let mapped_count = mappings.iter().filter(|m| m.confidence >= config.min_confidence).count();
    let unmapped_count = profiles.len() - mapped_count;

    info!(
        mapped = mapped_count,
        unmapped = unmapped_count,
        "column mapping complete"
    );

    // Phase 4b: Interactive confirmation
    let mappings = if config.interactive && !config.dry_run {
        interactive::confirm_mappings(mappings)?
    } else {
        mappings
    };
    let mapped_count = mappings.iter().filter(|m| m.confidence >= config.min_confidence).count();
    let unmapped_count = profiles.len() - mapped_count;

    if config.dry_run {
        return Ok(EnrichResult {
            ref_columns: profiles.len(),
            mapped_columns: mapped_count,
            enriched_fields: 0,
            skipped_fields: 0,
            unmapped_columns: unmapped_count,
            mappings,
            quality_report: None,
        });
    }

    // Phase 5: Extract and merge
    let ref_row_count = batches.iter().map(|b| b.num_rows()).sum::<usize>() as u64;
    let mut enriched_fields = 0;
    let mut skipped_fields = 0;
    let mut field_scores = Vec::new();

    let accepted_mappings: Vec<&ColumnMapping> = mappings.iter()
        .filter(|m| m.confidence >= config.min_confidence)
        .collect();

    // Extract numeric values for fitting from the batches
    let schema = batches[0].schema();

    for mapping in &accepted_mappings {
        let profile = &profiles[mapping.ref_col_index];

        // Find the field in the entity
        let entity_mut = model.entities.iter_mut()
            .find(|e| e.name == target_entity_name)
            .unwrap();

        let field = entity_mut.fields.iter_mut()
            .find(|f| f.name == mapping.target_field);

        let Some(field) = field else {
            warn!(field = %mapping.target_field, "field not found in entity");
            continue;
        };

        // Extract enrichment from reference profile
        let enrichment = extract::extract_field_enrichment(
            profile,
            &batches,
            &schema,
            mapping.ref_col_index,
        );

        // Merge into the field's generator
        let merged = merge_enrichment(field, &enrichment, ref_row_count);

        // Score the field enrichment quality
        field_scores.push(quality::score_field(mapping, &enrichment, merged));

        if merged {
            enriched_fields += 1;
        } else {
            skipped_fields += 1;
        }
    }

    let quality_report = Some(quality::build_report(field_scores));

    Ok(EnrichResult {
        ref_columns: profiles.len(),
        mapped_columns: mapped_count,
        enriched_fields,
        skipped_fields,
        unmapped_columns: unmapped_count,
        mappings,
        quality_report,
    })
}

/// Determine which entity the reference sample should enrich.
fn resolve_target_entity(
    model: &DataModel,
    ref_path: &Path,
    config: &EnrichConfig,
) -> Result<String> {
    // If user specified --entity, use that
    if let Some(ref name) = config.entity_filter {
        if model.entities.iter().any(|e| e.name == *name) {
            return Ok(name.clone());
        }
        anyhow::bail!("entity '{}' not found in model", name);
    }

    // Try matching by filename stem
    let stem = ref_path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("");

    let stem_lower = stem.to_lowercase();
    for entity in &model.entities {
        if entity.name.to_lowercase() == stem_lower {
            return Ok(entity.name.clone());
        }
    }

    // Default to the entity with the most fields (likely the main data entity)
    model.entities.iter()
        .max_by_key(|e| e.fields.len())
        .map(|e| e.name.clone())
        .ok_or_else(|| anyhow::anyhow!("model has no entities"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::*;
    use std::collections::BTreeMap;

    fn make_test_model() -> DataModel {
        DataModel {
            name: "test".to_string(),
            description: None,
            seed: 42,
            locale: "en_US".to_string(),
            timezone: "UTC".to_string(),
            entities: vec![Entity {
                name: "Users".to_string(),
                description: None,
                tags: vec![],
                count: CountSpec::Fixed(10),
                fields: vec![
                    Field {
                        name: "name".to_string(),
                        description: None,
                        data_type: DataType::String,
                        generator: Some(GeneratorSpec::OneOf {
                            choices: vec![
                                WeightedChoice { value: Value::String("Alice".into()), weight: 0.5 },
                                WeightedChoice { value: Value::String("Bob".into()), weight: 0.5 },
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
                    Field {
                        name: "score".to_string(),
                        description: None,
                        data_type: DataType::Float,
                        generator: Some(GeneratorSpec::Distribution {
                            spec: DistributionSpec {
                                kind: DistributionKind::Normal,
                                params: BTreeMap::from([
                                    ("mean".to_string(), 50.0),
                                    ("std_dev".to_string(), 10.0),
                                ]),
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
            personas: vec![],
            actor_relationships: vec![],
            custom_types: vec![],
            mixins: vec![],
            companion_files: vec![],
        }
    }

    #[test]
    fn test_resolve_target_entity_by_filter() {
        let model = make_test_model();
        let config = EnrichConfig {
            entity_filter: Some("Users".to_string()),
            ..Default::default()
        };
        let result = resolve_target_entity(&model, Path::new("anything.csv"), &config).unwrap();
        assert_eq!(result, "Users");
    }

    #[test]
    fn test_resolve_target_entity_by_filename() {
        let model = make_test_model();
        let config = EnrichConfig::default();
        let result = resolve_target_entity(&model, Path::new("users.csv"), &config).unwrap();
        assert_eq!(result, "Users");
    }

    #[test]
    fn test_resolve_target_entity_fallback() {
        let model = make_test_model();
        let config = EnrichConfig::default();
        let result = resolve_target_entity(&model, Path::new("unknown.csv"), &config).unwrap();
        assert_eq!(result, "Users"); // Only entity, so it gets picked
    }
}