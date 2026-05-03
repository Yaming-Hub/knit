//! Relationship detection — discover foreign key and referential relationships
//! between tables by analyzing column names, value overlaps, and cardinality.

use std::collections::{HashMap, HashSet};

use tracing::{debug, info};

/// Profile of a table for relationship detection.
#[derive(Debug, Clone)]
pub struct TableProfile {
    /// Table / entity name.
    pub name: String,
    /// Column profiles relevant for relationship detection.
    pub columns: Vec<RelColumn>,
}

/// Column metadata needed for relationship analysis.
#[derive(Debug, Clone)]
pub struct RelColumn {
    /// Column name.
    pub name: String,
    /// Whether this column is a primary key (or unique).
    pub is_primary_key: bool,
    /// Distinct values (capped for memory safety, e.g., first 10 000).
    pub distinct_values: HashSet<String>,
    /// Total non-null row count.
    pub row_count: u64,
    /// Number of distinct values.
    pub distinct_count: u64,
}

/// Kind of relationship inferred from cardinality.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelationshipKind {
    /// One-to-one: both sides have near-unique values.
    OneToOne,
    /// One-to-many: referencing side has duplicates.
    OneToMany,
    /// Many-to-many: both sides have duplicates.
    ManyToMany,
}

/// A detected relationship candidate.
#[derive(Debug, Clone)]
pub struct RelationshipCandidate {
    /// Source table name.
    pub from_table: String,
    /// Source column name.
    pub from_column: String,
    /// Target table name.
    pub to_table: String,
    /// Target column name (typically a PK).
    pub to_column: String,
    /// Inferred relationship kind.
    pub kind: RelationshipKind,
    /// Confidence score (0.0–1.0).
    pub confidence: f64,
    /// Whether this is a self-referential relationship.
    pub is_self_ref: bool,
}

/// Suffix patterns that suggest a foreign key column.
const FK_SUFFIXES: &[&str] = &["_id", "_key", "_fk", "Id", "Key", "_ref"];

/// Detect relationships across a set of table profiles.
///
/// Analyzes column names for FK-like suffixes, checks value overlap with
/// candidate primary keys, and infers cardinality.
///
/// # Arguments
///
/// * `tables` — Profiles of all tables to analyze.
///
/// # Returns
///
/// A vector of relationship candidates, sorted by confidence descending.
pub fn detect_relationships(tables: &[TableProfile]) -> Vec<RelationshipCandidate> {
    if tables.is_empty() {
        return Vec::new();
    }

    // Build index of table names → PK columns
    let _pk_index = build_pk_index(tables);
    // Build index of entity names (table names, lowered)
    let _entity_names: HashSet<String> = tables.iter().map(|t| t.name.to_lowercase()).collect();

    let mut candidates = Vec::new();

    for table in tables {
        for col in &table.columns {
            if col.is_primary_key {
                continue; // Skip PKs as source
            }

            let stripped = strip_fk_suffix(&col.name);

            // Try to match against known entity names
            for target_table in tables {
                // Check name match
                let name_score = name_match_score(&stripped, &target_table.name);
                if name_score < 0.3 {
                    continue;
                }

                // Find target PK columns
                let target_pks: Vec<&RelColumn> = target_table
                    .columns
                    .iter()
                    .filter(|c| c.is_primary_key)
                    .collect();

                for target_pk in &target_pks {
                    let overlap = value_overlap_ratio(&col.distinct_values, &target_pk.distinct_values);
                    if overlap < 0.1 {
                        continue;
                    }

                    let kind = infer_cardinality(col, target_pk);
                    let is_self_ref = table.name == target_table.name;

                    let confidence = compute_confidence(name_score, overlap, &kind);
                    debug!(
                        from = %table.name, from_col = %col.name,
                        to = %target_table.name, to_col = %target_pk.name,
                        confidence, "relationship candidate"
                    );

                    candidates.push(RelationshipCandidate {
                        from_table: table.name.clone(),
                        from_column: col.name.clone(),
                        to_table: target_table.name.clone(),
                        to_column: target_pk.name.clone(),
                        kind,
                        confidence,
                        is_self_ref,
                    });
                }
            }
        }
    }

    // Also check composite keys (pairs)
    detect_composite_keys(tables, &mut candidates);

    // Sort by confidence descending
    candidates.sort_by(|a, b| b.confidence.partial_cmp(&a.confidence).unwrap_or(std::cmp::Ordering::Equal));

    // Cap results
    candidates.truncate(1000);

    info!(count = candidates.len(), "detected relationship candidates");
    candidates
}

// ─── internal helpers ───────────────────────────────────────────────────────

fn build_pk_index(tables: &[TableProfile]) -> HashMap<String, Vec<(String, String)>> {
    let mut index: HashMap<String, Vec<(String, String)>> = HashMap::new();
    for table in tables {
        for col in &table.columns {
            if col.is_primary_key {
                index
                    .entry(table.name.to_lowercase())
                    .or_default()
                    .push((table.name.clone(), col.name.clone()));
            }
        }
    }
    index
}

/// Strip FK-like suffixes to get the base entity reference name.
fn strip_fk_suffix(name: &str) -> String {
    let lower = name.to_lowercase();
    for suffix in FK_SUFFIXES {
        let s = suffix.to_lowercase();
        if lower.ends_with(&s) && lower.len() > s.len() {
            return lower[..lower.len() - s.len()].to_string();
        }
    }
    lower
}

/// Score how well a stripped column name matches a table name (0.0–1.0).
fn name_match_score(stripped: &str, table_name: &str) -> f64 {
    let a = stripped.to_lowercase();
    let b = table_name.to_lowercase();

    if a == b {
        return 1.0;
    }

    // Singular/plural: simple heuristic
    if a.ends_with('s') && a[..a.len() - 1] == b {
        return 0.9;
    }
    if b.ends_with('s') && b[..b.len() - 1] == a {
        return 0.9;
    }

    // Substring containment
    if b.contains(&a) || a.contains(&b) {
        return 0.6;
    }

    0.0
}

/// Fraction of values in `source` that appear in `target`.
fn value_overlap_ratio(source: &HashSet<String>, target: &HashSet<String>) -> f64 {
    if source.is_empty() || target.is_empty() {
        return 0.0;
    }
    let overlap = source.intersection(target).count();
    overlap as f64 / source.len() as f64
}

/// Infer relationship kind from unique ratios.
fn infer_cardinality(source: &RelColumn, target: &RelColumn) -> RelationshipKind {
    let source_ratio = if source.row_count > 0 {
        source.distinct_count as f64 / source.row_count as f64
    } else {
        0.0
    };
    let target_ratio = if target.row_count > 0 {
        target.distinct_count as f64 / target.row_count as f64
    } else {
        0.0
    };

    if source_ratio > 0.95 && target_ratio > 0.95 {
        RelationshipKind::OneToOne
    } else if target_ratio > 0.95 {
        RelationshipKind::OneToMany
    } else {
        RelationshipKind::ManyToMany
    }
}

/// Weighted confidence score.
fn compute_confidence(name_score: f64, overlap: f64, kind: &RelationshipKind) -> f64 {
    let kind_bonus = match kind {
        RelationshipKind::OneToMany => 0.1,
        RelationshipKind::OneToOne => 0.05,
        RelationshipKind::ManyToMany => 0.0,
    };
    let raw = 0.4 * name_score + 0.4 * overlap + 0.1 * kind_bonus + 0.1;
    raw.clamp(0.0, 1.0)
}

/// Detect composite key candidates by checking pairs of columns.
fn detect_composite_keys(tables: &[TableProfile], _candidates: &mut Vec<RelationshipCandidate>) {
    // Simple heuristic: if two columns both have FK-like names referencing
    // different tables, flag as potential composite key / junction table
    for table in tables {
        let fk_cols: Vec<&RelColumn> = table
            .columns
            .iter()
            .filter(|c| !c.is_primary_key && has_fk_suffix(&c.name))
            .collect();

        if fk_cols.len() >= 2 {
            debug!(
                table = %table.name,
                fk_count = fk_cols.len(),
                "potential junction table with composite keys"
            );
        }
    }
}

fn has_fk_suffix(name: &str) -> bool {
    let lower = name.to_lowercase();
    FK_SUFFIXES
        .iter()
        .any(|s| lower.ends_with(&s.to_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_pk_column(name: &str, values: &[&str]) -> RelColumn {
        RelColumn {
            name: name.to_string(),
            is_primary_key: true,
            distinct_values: values.iter().map(|s| s.to_string()).collect(),
            row_count: values.len() as u64,
            distinct_count: values.len() as u64,
        }
    }

    fn make_fk_column(name: &str, values: &[&str], row_count: u64) -> RelColumn {
        let distinct: HashSet<String> = values.iter().map(|s| s.to_string()).collect();
        RelColumn {
            name: name.to_string(),
            is_primary_key: false,
            distinct_values: distinct.clone(),
            row_count,
            distinct_count: distinct.len() as u64,
        }
    }

    #[test]
    fn detect_simple_fk() {
        let users = TableProfile {
            name: "user".to_string(),
            columns: vec![make_pk_column("id", &["1", "2", "3", "4", "5"])],
        };
        let orders = TableProfile {
            name: "order".to_string(),
            columns: vec![
                make_pk_column("id", &["10", "20", "30"]),
                make_fk_column("user_id", &["1", "2", "3"], 10),
            ],
        };

        let rels = detect_relationships(&[users, orders]);
        assert!(!rels.is_empty(), "should detect user_id → user.id");
        let best = &rels[0];
        assert_eq!(best.from_column, "user_id");
        assert_eq!(best.to_table, "user");
        assert!(best.confidence > 0.5);
    }

    #[test]
    fn detect_self_referential() {
        let employees = TableProfile {
            name: "employee".to_string(),
            columns: vec![
                make_pk_column("id", &["1", "2", "3"]),
                make_fk_column("employee_id", &["1", "2"], 5),
            ],
        };

        let rels = detect_relationships(&[employees]);
        let self_refs: Vec<_> = rels.iter().filter(|r| r.is_self_ref).collect();
        assert!(!self_refs.is_empty(), "should detect self-referential FK");
    }

    #[test]
    fn no_relationships_in_empty() {
        let rels = detect_relationships(&[]);
        assert!(rels.is_empty());
    }

    #[test]
    fn strip_fk_suffix_variants() {
        assert_eq!(strip_fk_suffix("user_id"), "user");
        assert_eq!(strip_fk_suffix("customer_key"), "customer");
        assert_eq!(strip_fk_suffix("order_fk"), "order");
        assert_eq!(strip_fk_suffix("parentId"), "parent");
        assert_eq!(strip_fk_suffix("name"), "name"); // no suffix
    }

    #[test]
    fn name_match_score_exact() {
        assert!((name_match_score("user", "user") - 1.0).abs() < 0.01);
    }

    #[test]
    fn name_match_score_plural() {
        assert!(name_match_score("users", "user") > 0.8);
    }

    #[test]
    fn value_overlap_full() {
        let a: HashSet<String> = ["1", "2", "3"].iter().map(|s| s.to_string()).collect();
        let b: HashSet<String> = ["1", "2", "3", "4", "5"].iter().map(|s| s.to_string()).collect();
        let overlap = value_overlap_ratio(&a, &b);
        assert!((overlap - 1.0).abs() < 0.01);
    }

    #[test]
    fn cardinality_one_to_many() {
        let source = RelColumn {
            name: "fk".into(),
            is_primary_key: false,
            distinct_values: HashSet::new(),
            row_count: 100,
            distinct_count: 10,
        };
        let target = RelColumn {
            name: "pk".into(),
            is_primary_key: true,
            distinct_values: HashSet::new(),
            row_count: 10,
            distinct_count: 10,
        };
        assert_eq!(infer_cardinality(&source, &target), RelationshipKind::OneToMany);
    }
}
