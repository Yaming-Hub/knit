//! Actor identity resolution — cross-entity actor unification.
//!
//! When multiple entities share actor columns that reference the same
//! population (e.g. `orders.customer_id` and `support_tickets.user_id`
//! both FK to `users.id`), those columns should share a single actor
//! namespace for behavioral profiling.
//!
//! This module implements the resolution rules from the design doc §5:
//!
//! 1. Actor columns within the same entity that reference the same FK target
//!    → same actor namespace
//! 2. Actor columns in different entities linked by FK to a common actor entity
//!    → same actor namespace
//! 3. Columns with no linkage → separate actor namespaces (independent populations)

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::learn::relationships::RelationshipCandidate;

/// A named actor population grouping columns that reference the same actors.
#[derive(Debug, Clone)]
pub struct ActorNamespace {
    /// Namespace name (derived from FK target entity, e.g. `"users"`).
    pub name: String,
    /// All columns that reference this namespace: `(entity_name, field_name)`.
    pub columns: Vec<(String, String)>,
    /// Source entity name (the FK target, if one was identified).
    pub source_entity: Option<String>,
}

/// Registry of actor namespaces discovered via FK-based unification.
#[derive(Debug, Clone)]
pub struct ActorRegistry {
    /// Named actor populations.
    pub namespaces: BTreeMap<String, ActorNamespace>,
    /// Warnings generated during resolution (ambiguous linkages, etc.).
    pub warnings: Vec<String>,
}

/// Build an [`ActorRegistry`] from detected actor columns and FK relationships.
///
/// # Arguments
///
/// * `actor_columns` — actor columns per entity: `[(entity_name, [column_names])]`
/// * `relationships` — FK relationships detected across all tables
///
/// # Returns
///
/// An `ActorRegistry` with namespaces grouping columns that share the same
/// actor population, plus any warnings about ambiguous linkages.
pub fn build_actor_registry(
    actor_columns: &[(String, Vec<String>)],
    relationships: &[RelationshipCandidate],
) -> ActorRegistry {
    let mut warnings = Vec::new();

    // Step 1: Build a lookup from (entity, column) → FK target entity.
    // Keep only the highest-confidence target when multiple candidates exist.
    let mut fk_target: HashMap<(String, String), String> = HashMap::new();
    for rel in relationships {
        let key = (rel.from_table.clone(), rel.from_column.clone());
        // detect_relationships() sorts descending by confidence, so the first
        // insert for each key is the highest-confidence candidate. Skip later ones.
        fk_target.entry(key).or_insert_with(|| rel.to_table.clone());
    }

    // Step 2: Group actor columns by their FK target entity.
    // Columns pointing to the same target → same namespace.
    let mut target_groups: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut unlinked: Vec<(String, String)> = Vec::new();

    for (entity, cols) in actor_columns {
        for col in cols {
            if let Some(target) = fk_target.get(&(entity.clone(), col.clone())) {
                target_groups
                    .entry(target.clone())
                    .or_default()
                    .push((entity.clone(), col.clone()));
            } else {
                // No FK detected — try name-based inference
                // e.g. "customer_id" might reference "customers" entity
                let inferred = infer_target_from_name(col, actor_columns);
                if let Some(target) = inferred {
                    target_groups
                        .entry(target)
                        .or_default()
                        .push((entity.clone(), col.clone()));
                } else {
                    unlinked.push((entity.clone(), col.clone()));
                }
            }
        }
    }

    // Step 3: Build namespaces from groups
    let mut namespaces = BTreeMap::new();

    for (target, columns) in &target_groups {
        namespaces.insert(
            target.clone(),
            ActorNamespace {
                name: target.clone(),
                columns: columns.clone(),
                source_entity: Some(target.clone()),
            },
        );
    }

    // Step 4: Handle unlinked columns — each becomes its own namespace
    for (entity, col) in &unlinked {
        let ns_name = format!("{}_{}", entity, col);
        warnings.push(format!(
            "actor column {}.{} has no FK linkage — assigned to independent namespace '{}'",
            entity, col, ns_name,
        ));
        namespaces.insert(
            ns_name.clone(),
            ActorNamespace {
                name: ns_name,
                columns: vec![(entity.clone(), col.clone())],
                source_entity: None,
            },
        );
    }

    // Step 5: Detect potential unification misses (columns that might be same
    // population but couldn't be proven via FK).
    // Look for columns in different entities with same name but no FK link.
    let mut name_groups: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for (entity, col) in &unlinked {
        name_groups
            .entry(col.clone())
            .or_default()
            .push((entity.clone(), col.clone()));
    }
    for (col_name, entries) in &name_groups {
        if entries.len() > 1 {
            let locations: Vec<String> = entries.iter().map(|(e, _)| e.clone()).collect();
            warnings.push(format!(
                "actor column '{}' appears in {} entities ({}) but no FK link found — \
                 treated as separate populations. Use --actor-column to override.",
                col_name,
                locations.len(),
                locations.join(", "),
            ));
        }
    }

    ActorRegistry {
        namespaces,
        warnings,
    }
}

/// Try to infer a FK target entity from a column name.
///
/// If a column is named `"customer_id"`, look for an entity named `"customers"`
/// or `"customer"` among the known entities.
fn infer_target_from_name(
    col_name: &str,
    actor_columns: &[(String, Vec<String>)],
) -> Option<String> {
    let entity_names: HashSet<&str> = actor_columns.iter().map(|(e, _)| e.as_str()).collect();

    // Strip common suffixes to get candidate entity name
    let base = col_name
        .strip_suffix("_id")
        .or_else(|| col_name.strip_suffix("_key"))
        .or_else(|| col_name.strip_suffix("_fk"))
        .or_else(|| col_name.strip_suffix("Id"))
        .or_else(|| col_name.strip_suffix("_ref"))?;

    // Try exact match, then pluralized
    if entity_names.contains(base) {
        return Some(base.to_string());
    }
    let plural = format!("{}s", base);
    if entity_names.contains(plural.as_str()) {
        return Some(plural);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::relationships::{RelationshipCandidate, RelationshipKind};

    fn make_rel(from_table: &str, from_col: &str, to_table: &str) -> RelationshipCandidate {
        RelationshipCandidate {
            from_table: from_table.to_string(),
            from_column: from_col.to_string(),
            to_table: to_table.to_string(),
            to_column: "id".to_string(),
            kind: RelationshipKind::OneToMany,
            confidence: 0.95,
            is_self_ref: false,
        }
    }

    #[test]
    fn same_fk_target_unifies_columns() {
        let actor_cols = vec![
            ("orders".to_string(), vec!["customer_id".to_string()]),
            ("support_tickets".to_string(), vec!["user_id".to_string()]),
            ("users".to_string(), vec![]),
        ];
        let rels = vec![
            make_rel("orders", "customer_id", "users"),
            make_rel("support_tickets", "user_id", "users"),
        ];

        let registry = build_actor_registry(&actor_cols, &rels);

        assert_eq!(registry.namespaces.len(), 1);
        let ns = registry.namespaces.get("users").unwrap();
        assert_eq!(ns.columns.len(), 2);
        assert_eq!(ns.source_entity, Some("users".to_string()));
        assert!(registry.warnings.is_empty());
    }

    #[test]
    fn same_entity_different_targets() {
        // emails.sender_id → users, emails.org_id → orgs
        let actor_cols = vec![
            (
                "emails".to_string(),
                vec!["sender_id".to_string(), "org_id".to_string()],
            ),
            ("users".to_string(), vec![]),
            ("orgs".to_string(), vec![]),
        ];
        let rels = vec![
            make_rel("emails", "sender_id", "users"),
            make_rel("emails", "org_id", "orgs"),
        ];

        let registry = build_actor_registry(&actor_cols, &rels);

        assert_eq!(registry.namespaces.len(), 2);
        assert!(registry.namespaces.contains_key("users"));
        assert!(registry.namespaces.contains_key("orgs"));
    }

    #[test]
    fn unlinked_columns_get_separate_namespaces() {
        let actor_cols = vec![("events".to_string(), vec!["actor_id".to_string()])];
        let rels = vec![];

        let registry = build_actor_registry(&actor_cols, &rels);

        assert_eq!(registry.namespaces.len(), 1);
        assert!(registry.namespaces.contains_key("events_actor_id"));
        assert_eq!(registry.warnings.len(), 1);
        assert!(registry.warnings[0].contains("no FK linkage"));
    }

    #[test]
    fn name_based_inference_finds_entity() {
        // user_id should infer "users" entity via pluralization
        let actor_cols = vec![
            ("orders".to_string(), vec!["user_id".to_string()]),
            ("users".to_string(), vec![]),
        ];
        let rels = vec![]; // No FK detected, but name inference should work

        let registry = build_actor_registry(&actor_cols, &rels);

        assert_eq!(registry.namespaces.len(), 1);
        assert!(registry.namespaces.contains_key("users"));
    }

    #[test]
    fn ambiguous_same_name_different_entities_warns() {
        // Two entities have "manager_id" but no FK link
        let actor_cols = vec![
            ("dept_a".to_string(), vec!["manager_id".to_string()]),
            ("dept_b".to_string(), vec!["manager_id".to_string()]),
        ];
        let rels = vec![];

        let registry = build_actor_registry(&actor_cols, &rels);

        // Each gets its own namespace
        assert_eq!(registry.namespaces.len(), 2);
        // Should warn about potential missed unification
        assert!(
            registry
                .warnings
                .iter()
                .any(|w| w.contains("appears in 2 entities"))
        );
    }
}
