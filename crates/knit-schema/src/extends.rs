//! Schema inheritance via `extends` directives.
//!
//! Allows a child schema to inherit from a parent, overriding or adding
//! entities, fields, relationships, and scalar properties. The merge is
//! key-based (by name) so child additions are additive and child overrides
//! replace parent values.

use knit_core::{CountSpec, DataModel, Entity};

use crate::error::SchemaError;

/// Merge a child model on top of a parent model.
///
/// Semantics:
/// - Entities merge by `name` (keyed merge)
/// - Fields within an entity merge by `name`
/// - Relationships merge by `name`
/// - Noise profiles merge by `name`
/// - Correlations merge by `entity`
/// - Scalar properties: child overrides parent
/// - Vec properties: child replaces parent only when non-empty (within merged element)
pub fn merge_models(parent: &DataModel, child: &DataModel) -> DataModel {
    let mut result = parent.clone();

    // Override scalar model properties if child specifies non-default
    if child.name != "unnamed" {
        result.name = child.name.clone();
    }
    if child.description.is_some() {
        result.description = child.description.clone();
    }
    if child.seed != 42 {
        result.seed = child.seed;
    }
    if child.locale != "en_US" {
        result.locale = child.locale.clone();
    }
    if child.timezone != "UTC" {
        result.timezone = child.timezone.clone();
    }
    if !child.params.is_empty() {
        result.params = child.params.clone();
    }

    // Merge entities by name
    for child_entity in &child.entities {
        if let Some(parent_entity) = result.entities.iter_mut().find(|e| e.name == child_entity.name) {
            merge_entity(parent_entity, child_entity);
        } else {
            result.entities.push(child_entity.clone());
        }
    }

    // Merge relationships by name
    for child_rel in &child.relationships {
        if let Some(parent_rel) = result.relationships.iter_mut().find(|r| r.name == child_rel.name) {
            *parent_rel = child_rel.clone();
        } else {
            result.relationships.push(child_rel.clone());
        }
    }

    // Merge noise profiles by name
    for child_noise in &child.noise_profiles {
        if let Some(parent_noise) = result.noise_profiles.iter_mut().find(|n| n.name == child_noise.name) {
            *parent_noise = child_noise.clone();
        } else {
            result.noise_profiles.push(child_noise.clone());
        }
    }

    // Merge correlations by entity
    for child_corr in &child.correlations {
        if let Some(parent_corr) = result.correlations.iter_mut().find(|c| c.entity == child_corr.entity) {
            *parent_corr = child_corr.clone();
        } else {
            result.correlations.push(child_corr.clone());
        }
    }

    result
}

fn merge_entity(parent: &mut Entity, child: &Entity) {
    if child.count != CountSpec::default() {
        parent.count = child.count.clone();
    }
    if child.description.is_some() {
        parent.description = child.description.clone();
    }
    if child.topology.is_some() {
        parent.topology = child.topology.clone();
    }
    if !child.constraints.is_empty() {
        parent.constraints = child.constraints.clone();
    }

    // Merge fields by name
    for child_field in &child.fields {
        if let Some(parent_field) = parent.fields.iter_mut().find(|f| f.name == child_field.name) {
            *parent_field = child_field.clone();
        } else {
            parent.fields.push(child_field.clone());
        }
    }
}

/// Resolve an extends chain by parsing the parent file and merging.
///
/// The `extends` path is resolved relative to `schema_path`'s parent directory.
/// Absolute paths and path traversal (`..`) are rejected for safety.
pub fn resolve_extends(
    schema_path: &std::path::Path,
    child: &DataModel,
    extends: &str,
) -> Result<DataModel, SchemaError> {
    // Reject absolute paths and path traversal
    let extends_path = std::path::Path::new(extends);
    if extends_path.is_absolute() {
        return Err(SchemaError::Validation {
            path: "extends".to_string(),
            message: "absolute paths are not allowed in extends".to_string(),
        });
    }
    if extends_path.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(SchemaError::Validation {
            path: "extends".to_string(),
            message: "path traversal ('..') is not allowed in extends".to_string(),
        });
    }

    let parent_path = schema_path
        .parent()
        .unwrap_or(std::path::Path::new("."))
        .join(extends);
    let parent = crate::parse_toml_file(&parent_path)?;
    Ok(merge_models(&parent, child))
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use knit_core::*;
    use std::collections::BTreeMap;

    fn parent_model() -> DataModel {
        DataModel {
            name: "parent".to_string(),
            description: Some("parent desc".to_string()),
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
            relationships: vec![Relationship {
                name: "user_order".to_string(),
                from: "user".to_string(),
                to: "order".to_string(),
                kind: RelationshipKind::OneToMany,
                foreign_key: None,
                cardinality: None,
            }],
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            schema_version: "1.0".to_string(),
        }
    }

    fn child_model() -> DataModel {
        DataModel {
            name: "child".to_string(),
            description: None,
            seed: 99,
            locale: "en_US".to_string(),
            timezone: "UTC".to_string(),
            entities: vec![],
            relationships: vec![],
            noise_profiles: vec![],
            correlations: vec![],
            params: BTreeMap::new(),
            schema_version: "1.0".to_string(),
        }
    }

    #[test]
    fn test_merge_override_model_properties() {
        let parent = parent_model();
        let child = child_model();
        let merged = merge_models(&parent, &child);
        assert_eq!(merged.name, "child");
        assert_eq!(merged.seed, 99);
        // parent description retained since child is None
        assert_eq!(merged.description, Some("parent desc".to_string()));
    }

    #[test]
    fn test_merge_add_entity() {
        let parent = parent_model();
        let mut child = child_model();
        child.entities.push(Entity {
            name: "order".to_string(),
            description: None,
            count: CountSpec::Fixed(500),
            fields: vec![],
            constraints: vec![],
            topology: None,
        });
        let merged = merge_models(&parent, &child);
        assert_eq!(merged.entities.len(), 2);
        assert!(merged.entities.iter().any(|e| e.name == "order"));
    }

    #[test]
    fn test_merge_override_entity_count() {
        let parent = parent_model();
        let mut child = child_model();
        child.entities.push(Entity {
            name: "user".to_string(),
            description: None,
            count: CountSpec::Fixed(5000),
            fields: vec![],
            constraints: vec![],
            topology: None,
        });
        let merged = merge_models(&parent, &child);
        assert_eq!(merged.entities.len(), 1);
        assert_eq!(merged.entities[0].count, CountSpec::Fixed(5000));
    }

    #[test]
    fn test_merge_add_field_to_entity() {
        let parent = parent_model();
        let mut child = child_model();
        child.entities.push(Entity {
            name: "user".to_string(),
            description: None,
            count: CountSpec::default(), // default => don't override
            fields: vec![Field {
                name: "age".to_string(),
                description: None,
                data_type: DataType::Int,
                generator: None,
                nullable: NullSpec::Never,
                primary_key: None,
            }],
            constraints: vec![],
            topology: None,
        });
        let merged = merge_models(&parent, &child);
        assert_eq!(merged.entities[0].fields.len(), 3);
        assert!(merged.entities[0].fields.iter().any(|f| f.name == "age"));
    }

    #[test]
    fn test_merge_override_field() {
        let parent = parent_model();
        let mut child = child_model();
        child.entities.push(Entity {
            name: "user".to_string(),
            description: None,
            count: CountSpec::default(),
            fields: vec![Field {
                name: "email".to_string(),
                description: Some("overridden".to_string()),
                data_type: DataType::String,
                generator: None,
                nullable: NullSpec::Always,
                primary_key: None,
            }],
            constraints: vec![],
            topology: None,
        });
        let merged = merge_models(&parent, &child);
        let email = merged.entities[0]
            .fields
            .iter()
            .find(|f| f.name == "email")
            .unwrap();
        assert_eq!(email.description, Some("overridden".to_string()));
        assert_eq!(email.nullable, NullSpec::Always);
    }

    #[test]
    fn test_merge_relationships() {
        let parent = parent_model();
        let mut child = child_model();
        // Override existing relationship
        child.relationships.push(Relationship {
            name: "user_order".to_string(),
            from: "user".to_string(),
            to: "order".to_string(),
            kind: RelationshipKind::ManyToMany,
            foreign_key: None,
            cardinality: None,
        });
        // Add new relationship
        child.relationships.push(Relationship {
            name: "order_item".to_string(),
            from: "order".to_string(),
            to: "item".to_string(),
            kind: RelationshipKind::OneToMany,
            foreign_key: None,
            cardinality: None,
        });
        let merged = merge_models(&parent, &child);
        assert_eq!(merged.relationships.len(), 2);
        let user_order = merged
            .relationships
            .iter()
            .find(|r| r.name == "user_order")
            .unwrap();
        assert_eq!(user_order.kind, RelationshipKind::ManyToMany);
        assert!(merged.relationships.iter().any(|r| r.name == "order_item"));
    }

    #[test]
    fn test_merge_entity_preserves_parent_constraints_and_topology() {
        let mut parent = parent_model();
        parent.entities[0].constraints = vec![Constraint::Unique {
            fields: vec!["email".to_string()],
        }];
        parent.entities[0].topology = Some(TopologySpec::Tree { max_depth: 5, branching_factor: 3 });
        // Child overrides entity with no constraints/topology
        let mut child = child_model();
        child.entities.push(Entity {
            name: "user".to_string(),
            description: Some("updated".to_string()),
            count: CountSpec::default(),
            fields: vec![],
            constraints: vec![],
            topology: None,
        });
        let merged = merge_models(&parent, &child);
        // Parent constraints & topology preserved since child has empty/None
        assert_eq!(merged.entities[0].constraints.len(), 1);
        assert!(merged.entities[0].topology.is_some());
        assert_eq!(merged.entities[0].description, Some("updated".to_string()));
    }

    #[test]
    fn test_resolve_extends_rejects_path_traversal() {
        let result = crate::resolve_extends(
            std::path::Path::new("schema.toml"),
            &child_model(),
            "../../../etc/passwd",
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            format!("{}", err).contains("path traversal"),
            "expected path traversal error, got: {}",
            err
        );
    }

    #[test]
    fn test_resolve_extends_rejects_absolute_path() {
        // Use a Windows-style absolute path for cross-platform test
        let abs_path = if cfg!(windows) {
            "C:\\schema.toml"
        } else {
            "/etc/passwd"
        };
        let result = crate::resolve_extends(
            std::path::Path::new("schema.toml"),
            &child_model(),
            abs_path,
        );
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            format!("{}", err).contains("absolute"),
            "expected absolute path error, got: {}",
            err
        );
    }
}
