//! Dimension annotation: populates `Entity.scaling` fields from scaling analysis.
//!
//! Called during `knit learn` after schema assembly and before writing the
//! blueprint, so that `knit scale` can read dimension metadata directly
//! without re-running analysis.

use crate::core::types::{
    ActorAnnotation, CustomDimensionAnnotation, DataModel, DimensionAnnotation, TimeAnnotation,
};
use crate::scale::{Cadence, ScalingAnalysis};

/// Annotate entities in `model` with dimension metadata from `analysis`.
///
/// For each scaling dimension (actor, time, custom), the corresponding
/// entity's `scaling` field is populated with the annotation.
pub fn annotate_dimensions(model: &mut DataModel, analysis: &ScalingAnalysis) {
    // Actor dimension
    if let Some(actor) = &analysis.actor {
        // Mark the actor root entity
        if let Some(entity) = model
            .entities
            .iter_mut()
            .find(|e| e.name == actor.entity_name)
        {
            let ann = entity.scaling.get_or_insert_with(|| DimensionAnnotation {
                actor: None,
                time: None,
                custom: vec![],
            });
            ann.actor = Some(ActorAnnotation {
                is_root: true,
                root_entity: None,
                rows_per_actor: None,
            });
        }

        // Mark dependent entities
        for (dep_name, ratio) in &actor.dependents {
            if let Some(entity) = model.entities.iter_mut().find(|e| e.name == *dep_name) {
                let ann = entity.scaling.get_or_insert_with(|| DimensionAnnotation {
                    actor: None,
                    time: None,
                    custom: vec![],
                });
                ann.actor = Some(ActorAnnotation {
                    is_root: false,
                    root_entity: Some(actor.entity_name.clone()),
                    rows_per_actor: Some(*ratio),
                });
            }
        }
    }

    // Time dimension
    if let Some(time) = &analysis.time
        && let Some(entity) = model
            .entities
            .iter_mut()
            .find(|e| e.name == time.entity_name)
        {
            let ann = entity.scaling.get_or_insert_with(|| DimensionAnnotation {
                actor: None,
                time: None,
                custom: vec![],
            });
            ann.time = Some(TimeAnnotation {
                partition_column: time.partition_field.clone(),
                cadence: time.cadence.map(format_cadence),
                partition_count: time.partition_values.len(),
                partition_values: time.partition_values.clone(),
            });
        }

    // Custom dimensions
    for dim in &analysis.custom {
        if let Some(entity) = model
            .entities
            .iter_mut()
            .find(|e| e.name == dim.entity_name)
        {
            let ann = entity.scaling.get_or_insert_with(|| DimensionAnnotation {
                actor: None,
                time: None,
                custom: vec![],
            });
            ann.custom.push(CustomDimensionAnnotation {
                name: dim.field_name.clone(),
                field: dim.field_name.clone(),
                cardinality: dim.current_values.len(),
            });
        }
    }
}

/// Format a `Cadence` value as a human-readable string (e.g. `"7d"`, `"1m"`).
fn format_cadence(c: Cadence) -> String {
    match c {
        Cadence::Days(n) => format!("{n}d"),
        Cadence::Months(n) => format!("{n}m"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::types::{CountSpec, Entity};
    use crate::scale::{ActorDimension, CustomDimension, TimeDimension};
    use std::collections::BTreeMap;

    fn make_entity(name: &str) -> Entity {
        Entity {
            name: name.to_string(),
            description: None,
            tags: vec![],
            count: CountSpec::Fixed(10),
            fields: vec![],
            constraints: vec![],
            topology: None,
            actor: false,
            persona_distribution: None,
            activity_count: None,
            mixin_refs: None,
            output: None,
            stats: None,
            scaling: None,
        }
    }

    fn empty_analysis() -> ScalingAnalysis {
        ScalingAnalysis {
            actor: None,
            time: None,
            custom: vec![],
            entity_counts: BTreeMap::new(),
        }
    }

    #[test]
    fn empty_analysis_no_annotations() {
        let mut model = DataModel {
            name: "test".into(),
            description: None,
            blueprint_version: "2.0".into(),
            entities: vec![make_entity("Users")],
            relationships: vec![],
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            seed: 42,
            locale: "en_US".into(),
            timezone: "UTC".into(),
            personas: vec![],
            actor_relationships: vec![],
            custom_types: vec![],
            mixins: vec![],
            companion_files: vec![],
        };

        annotate_dimensions(&mut model, &empty_analysis());
        assert!(model.entities[0].scaling.is_none());
    }

    #[test]
    fn actor_root_annotated() {
        let mut model = DataModel {
            name: "test".into(),
            description: None,
            blueprint_version: "2.0".into(),
            entities: vec![make_entity("Users"), make_entity("Events")],
            relationships: vec![],
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            seed: 42,
            locale: "en_US".into(),
            timezone: "UTC".into(),
            personas: vec![],
            actor_relationships: vec![],
            custom_types: vec![],
            mixins: vec![],
            companion_files: vec![],
        };

        let analysis = ScalingAnalysis {
            actor: Some(ActorDimension {
                entity_name: "Users".into(),
                current_count: 10,
                dependents: vec![("Events".into(), 5.0)],
                confidence: 0.9,
            }),
            ..empty_analysis()
        };

        annotate_dimensions(&mut model, &analysis);

        let users = &model.entities[0].scaling.as_ref().unwrap();
        let actor = users.actor.as_ref().unwrap();
        assert!(actor.is_root);
        assert!(actor.root_entity.is_none());

        let events = &model.entities[1].scaling.as_ref().unwrap();
        let actor = events.actor.as_ref().unwrap();
        assert!(!actor.is_root);
        assert_eq!(actor.root_entity.as_deref(), Some("Users"));
        assert_eq!(actor.rows_per_actor, Some(5.0));
    }

    #[test]
    fn time_dimension_annotated() {
        let mut model = DataModel {
            name: "test".into(),
            description: None,
            blueprint_version: "2.0".into(),
            entities: vec![make_entity("Events")],
            relationships: vec![],
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            seed: 42,
            locale: "en_US".into(),
            timezone: "UTC".into(),
            personas: vec![],
            actor_relationships: vec![],
            custom_types: vec![],
            mixins: vec![],
            companion_files: vec![],
        };

        let analysis = ScalingAnalysis {
            time: Some(TimeDimension {
                entity_name: "Events".into(),
                partition_field: "date".into(),
                partition_values: vec!["2024-01-01".into(), "2024-01-08".into()],
                cadence: Some(Cadence::Days(7)),
                cadence_confidence: 1.0,
            }),
            ..empty_analysis()
        };

        annotate_dimensions(&mut model, &analysis);

        let time = model.entities[0]
            .scaling
            .as_ref()
            .unwrap()
            .time
            .as_ref()
            .unwrap();
        assert_eq!(time.partition_column, "date");
        assert_eq!(time.cadence.as_deref(), Some("7d"));
        assert_eq!(time.partition_count, 2);
        assert_eq!(
            time.partition_values,
            vec!["2024-01-01".to_string(), "2024-01-08".to_string()]
        );
    }

    #[test]
    fn custom_dimension_annotated() {
        let mut model = DataModel {
            name: "test".into(),
            description: None,
            blueprint_version: "2.0".into(),
            entities: vec![make_entity("Products")],
            relationships: vec![],
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            seed: 42,
            locale: "en_US".into(),
            timezone: "UTC".into(),
            personas: vec![],
            actor_relationships: vec![],
            custom_types: vec![],
            mixins: vec![],
            companion_files: vec![],
        };

        let analysis = ScalingAnalysis {
            custom: vec![CustomDimension {
                entity_name: "Products".into(),
                field_name: "category".into(),
                current_values: vec![("A".into(), 0.5), ("B".into(), 0.3), ("C".into(), 0.2)],
                is_condition_key: false,
            }],
            ..empty_analysis()
        };

        annotate_dimensions(&mut model, &analysis);

        let custom = &model.entities[0].scaling.as_ref().unwrap().custom;
        assert_eq!(custom.len(), 1);
        assert_eq!(custom[0].name, "category");
        assert_eq!(custom[0].cardinality, 3);
    }

    #[test]
    fn combined_dimensions_on_same_entity() {
        let mut model = DataModel {
            name: "test".into(),
            description: None,
            blueprint_version: "2.0".into(),
            entities: vec![make_entity("Events")],
            relationships: vec![],
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            seed: 42,
            locale: "en_US".into(),
            timezone: "UTC".into(),
            personas: vec![],
            actor_relationships: vec![],
            custom_types: vec![],
            mixins: vec![],
            companion_files: vec![],
        };

        let analysis = ScalingAnalysis {
            actor: Some(ActorDimension {
                entity_name: "Events".into(),
                current_count: 10,
                dependents: vec![],
                confidence: 0.8,
            }),
            time: Some(TimeDimension {
                entity_name: "Events".into(),
                partition_field: "dt".into(),
                partition_values: vec!["2024-01".into(), "2024-02".into(), "2024-03".into()],
                cadence: Some(Cadence::Months(1)),
                cadence_confidence: 0.95,
            }),
            ..empty_analysis()
        };

        annotate_dimensions(&mut model, &analysis);

        let ann = model.entities[0].scaling.as_ref().unwrap();
        assert!(ann.actor.is_some());
        assert!(ann.time.is_some());
        assert_eq!(ann.time.as_ref().unwrap().cadence.as_deref(), Some("1m"));
    }

    #[test]
    fn format_cadence_days_and_months() {
        assert_eq!(format_cadence(Cadence::Days(1)), "1d");
        assert_eq!(format_cadence(Cadence::Days(7)), "7d");
        assert_eq!(format_cadence(Cadence::Months(1)), "1m");
        assert_eq!(format_cadence(Cadence::Months(3)), "3m");
    }
}
