//! Incremental relationship and correlation detection.
//!
//! This module provides cross-table relationship detection using HLL sketches
//! and naming heuristics, suitable for streaming/chunked data processing.
//! It also provides running Pearson correlation for numeric column pairs.

use serde::{Deserialize, Serialize};

use crate::learn::streaming::HyperLogLog;

// ─── Relationship Evidence ──────────────────────────────────────────────────

/// Suffix patterns that suggest a foreign key column.
const FK_SUFFIXES: &[&str] = &["_id", "_key", "_fk", "Id", "ID", "Key", "_ref"];

/// Maximum number of tracked relationship candidates.
const MAX_RELATIONSHIP_CANDIDATES: usize = 500;

/// Minimum naming score to consider a candidate.
const MIN_NAMING_SCORE: f64 = 0.3;

/// Minimum coverage ratio to confirm a relationship at finalize.
const MIN_COVERAGE_RATIO: f64 = 0.5;

/// Evidence accumulator for a potential FK→PK relationship.
///
/// Maintains HLL sketches for both sides to estimate coverage ratio
/// without storing full value sets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipEvidence {
    /// Source (FK) table name.
    pub from_table: String,
    /// Source (FK) column name.
    pub from_column: String,
    /// Target (PK) table name.
    pub to_table: String,
    /// Target (PK) column name.
    pub to_column: String,
    /// HLL sketch for FK column values.
    pub from_hll: HyperLogLog,
    /// HLL sketch for PK column values.
    pub to_hll: HyperLogLog,
    /// Naming heuristic confidence (0.0–1.0).
    pub naming_score: f64,
    /// Number of chunks that contributed evidence.
    pub chunks_observed: u64,
}

impl RelationshipEvidence {
    /// Create new evidence for a candidate relationship.
    pub fn new(
        from_table: String,
        from_column: String,
        to_table: String,
        to_column: String,
        naming_score: f64,
    ) -> Self {
        Self {
            from_table,
            from_column,
            to_table,
            to_column,
            from_hll: HyperLogLog::new(14), // p=14 to match column HLLs for direct merge
            to_hll: HyperLogLog::new(14),
            naming_score,
            chunks_observed: 0,
        }
    }

    /// Estimate coverage ratio: |FK ∩ PK| / |FK|.
    ///
    /// Values close to 1.0 indicate a strong FK relationship.
    pub fn coverage_ratio(&self) -> f64 {
        let fk_card = self.from_hll.cardinality();
        if fk_card < 1.0 {
            return 0.0;
        }
        let intersection = self.from_hll.intersection_cardinality(&self.to_hll);
        (intersection / fk_card).clamp(0.0, 1.0)
    }

    /// Combined confidence score considering naming + overlap evidence.
    pub fn confidence(&self) -> f64 {
        let coverage = self.coverage_ratio();
        // Weight: 40% naming, 60% coverage
        (self.naming_score * 0.4 + coverage * 0.6).clamp(0.0, 1.0)
    }

    /// Merge another evidence into this one (union of HLL sketches).
    pub fn merge(&mut self, other: &RelationshipEvidence) {
        if self.from_hll.precision() == other.from_hll.precision() {
            self.from_hll.merge(&other.from_hll);
        }
        if self.to_hll.precision() == other.to_hll.precision() {
            self.to_hll.merge(&other.to_hll);
        }
        self.chunks_observed = self.chunks_observed.saturating_add(other.chunks_observed);
    }
}

// ─── Running Pearson Correlation ────────────────────────────────────────────

/// Streaming co-moment tracker for Pearson correlation between two numeric columns.
///
/// Uses the online covariance algorithm (parallel to Welford's for single variables).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairwiseCorrelation {
    /// First column name.
    pub col_a: String,
    /// Second column name.
    pub col_b: String,
    /// Number of paired observations.
    pub count: u64,
    /// Running mean of column A.
    pub mean_a: f64,
    /// Running mean of column B.
    pub mean_b: f64,
    /// Running M2 for column A (sum of squared deviations from mean).
    pub m2_a: f64,
    /// Running M2 for column B.
    pub m2_b: f64,
    /// Running co-moment (sum of (x-mean_x)(y-mean_y)).
    pub co_moment: f64,
}

impl PairwiseCorrelation {
    /// Create a new tracker for a pair of columns.
    pub fn new(col_a: String, col_b: String) -> Self {
        Self {
            col_a,
            col_b,
            count: 0,
            mean_a: 0.0,
            mean_b: 0.0,
            m2_a: 0.0,
            m2_b: 0.0,
            co_moment: 0.0,
        }
    }

    /// Update with a paired observation (both non-null).
    pub fn update(&mut self, a: f64, b: f64) {
        self.count += 1;
        let n = self.count as f64;
        let delta_a = a - self.mean_a;
        let delta_b = b - self.mean_b;
        self.mean_a += delta_a / n;
        self.mean_b += delta_b / n;
        let delta_a2 = a - self.mean_a;
        let delta_b2 = b - self.mean_b;
        self.m2_a += delta_a * delta_a2;
        self.m2_b += delta_b * delta_b2;
        self.co_moment += delta_a * delta_b2;
    }

    /// Compute Pearson correlation coefficient.
    ///
    /// Returns `None` if insufficient data or zero variance.
    pub fn pearson_r(&self) -> Option<f64> {
        if self.count < 3 {
            return None;
        }
        let var_a = self.m2_a / (self.count as f64 - 1.0);
        let var_b = self.m2_b / (self.count as f64 - 1.0);
        if var_a < 1e-15 || var_b < 1e-15 {
            return None;
        }
        let r = self.co_moment / (self.m2_a.sqrt() * self.m2_b.sqrt());
        Some(r.clamp(-1.0, 1.0))
    }

    /// Merge two PairwiseCorrelation trackers (parallel merge formula).
    pub fn merge(&mut self, other: &PairwiseCorrelation) {
        if other.count == 0 {
            return;
        }
        if self.count == 0 {
            *self = other.clone();
            return;
        }
        let n_a = self.count as f64;
        let n_b = other.count as f64;
        let n = n_a + n_b;

        let delta_mean_a = other.mean_a - self.mean_a;
        let delta_mean_b = other.mean_b - self.mean_b;

        self.co_moment += other.co_moment + delta_mean_a * delta_mean_b * n_a * n_b / n;
        self.m2_a += other.m2_a + delta_mean_a * delta_mean_a * n_a * n_b / n;
        self.m2_b += other.m2_b + delta_mean_b * delta_mean_b * n_a * n_b / n;
        self.mean_a = (self.mean_a * n_a + other.mean_a * n_b) / n;
        self.mean_b = (self.mean_b * n_a + other.mean_b * n_b) / n;
        self.count += other.count;
    }
}

// ─── Candidate Selection (Stage 1) ─────────────────────────────────────────

/// Column metadata needed for relationship candidate selection.
pub struct IncrementalRelColumn {
    /// Column name.
    pub name: String,
    /// Whether this column is likely a primary key (high uniqueness ratio).
    pub is_likely_pk: bool,
    /// Table this column belongs to.
    pub table_name: String,
}

/// Detect relationship candidates using naming heuristics.
///
/// This is Stage 1: cheap, runs per-chunk, returns candidate pairs.
/// Only columns with FK-like suffixes are considered as sources.
pub fn detect_candidates(
    columns: &[IncrementalRelColumn],
    existing: &[RelationshipEvidence],
) -> Vec<RelationshipEvidence> {
    let mut new_candidates = Vec::new();

    // Build set of existing pairs to avoid duplicates
    let existing_pairs: std::collections::HashSet<(String, String, String, String)> = existing
        .iter()
        .map(|e| {
            (
                e.from_table.clone(),
                e.from_column.clone(),
                e.to_table.clone(),
                e.to_column.clone(),
            )
        })
        .collect();

    // Find FK candidate columns (those with FK-like suffixes)
    let fk_candidates: Vec<&IncrementalRelColumn> = columns
        .iter()
        .filter(|c| !c.is_likely_pk && has_fk_suffix(&c.name))
        .collect();

    // Find PK columns
    let pk_columns: Vec<&IncrementalRelColumn> =
        columns.iter().filter(|c| c.is_likely_pk).collect();

    for fk_col in &fk_candidates {
        let stripped = strip_fk_suffix(&fk_col.name);

        for pk_col in &pk_columns {
            // Don't create self-referencing relationships on same column
            if fk_col.table_name == pk_col.table_name && fk_col.name == pk_col.name {
                continue;
            }

            let name_score = name_match_score(&stripped, &pk_col.table_name);
            if name_score < MIN_NAMING_SCORE {
                continue;
            }

            let key = (
                fk_col.table_name.clone(),
                fk_col.name.clone(),
                pk_col.table_name.clone(),
                pk_col.name.clone(),
            );
            if existing_pairs.contains(&key) {
                continue;
            }

            // Check capacity
            if existing.len() + new_candidates.len() >= MAX_RELATIONSHIP_CANDIDATES {
                break;
            }

            new_candidates.push(RelationshipEvidence::new(
                fk_col.table_name.clone(),
                fk_col.name.clone(),
                pk_col.table_name.clone(),
                pk_col.name.clone(),
                name_score,
            ));
        }
    }

    new_candidates
}

/// Finalize relationships from accumulated evidence.
///
/// Applies coverage threshold and returns confirmed relationships with
/// the same structure as batch-mode detection.
pub fn finalize_relationships(evidence: &[RelationshipEvidence]) -> Vec<FinalizedRelationship> {
    let mut results: Vec<FinalizedRelationship> = evidence
        .iter()
        .filter(|e| e.confidence() >= 0.4)
        .filter(|e| e.coverage_ratio() >= MIN_COVERAGE_RATIO || e.naming_score >= 0.8)
        .map(|e| {
            let coverage = e.coverage_ratio();
            let fk_card = e.from_hll.cardinality();
            let pk_card = e.to_hll.cardinality();

            // Infer cardinality kind
            let kind = if fk_card > 0.0 && pk_card > 0.0 {
                let ratio = fk_card / pk_card;
                if ratio < 1.5 && coverage > 0.8 {
                    RelKind::OneToOne
                } else {
                    RelKind::OneToMany
                }
            } else {
                RelKind::OneToMany
            };

            FinalizedRelationship {
                from_table: e.from_table.clone(),
                from_column: e.from_column.clone(),
                to_table: e.to_table.clone(),
                to_column: e.to_column.clone(),
                kind,
                confidence: e.confidence(),
                is_self_ref: e.from_table == e.to_table,
            }
        })
        .collect();

    results.sort_by(|a, b| {
        b.confidence
            .partial_cmp(&a.confidence)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    results.truncate(100);
    results
}

/// A confirmed relationship after finalization.
#[derive(Debug, Clone)]
pub struct FinalizedRelationship {
    /// Source (FK) table.
    pub from_table: String,
    /// Source (FK) column.
    pub from_column: String,
    /// Target (PK) table.
    pub to_table: String,
    /// Target (PK) column.
    pub to_column: String,
    /// Relationship cardinality kind.
    pub kind: RelKind,
    /// Confidence score (0.0–1.0).
    pub confidence: f64,
    /// Whether this is a self-referential relationship.
    pub is_self_ref: bool,
}

/// Relationship cardinality kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RelKind {
    /// One-to-one.
    OneToOne,
    /// One-to-many.
    OneToMany,
}

// ─── Naming Heuristic Helpers ───────────────────────────────────────────────

/// Check if a column name has a FK-like suffix.
fn has_fk_suffix(name: &str) -> bool {
    let lower = name.to_lowercase();
    FK_SUFFIXES
        .iter()
        .any(|s| lower.ends_with(&s.to_lowercase()))
}

/// Strip FK-like suffix to extract the referenced entity name.
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

/// Score how well a stripped FK name matches a table name.
///
/// Returns 0.0–1.0 where 1.0 is exact match.
fn name_match_score(stripped: &str, table_name: &str) -> f64 {
    let table_lower = table_name.to_lowercase();

    // Exact match
    if stripped == table_lower {
        return 1.0;
    }

    // Singular/plural match (simple: add/remove 's')
    if stripped.ends_with('s') && stripped[..stripped.len() - 1] == table_lower {
        return 0.9;
    }
    if table_lower.ends_with('s') && &table_lower[..table_lower.len() - 1] == stripped {
        return 0.9;
    }

    // Prefix match (e.g., "user" matches "users_extended")
    if table_lower.starts_with(stripped) && stripped.len() >= 3 {
        return 0.7;
    }

    // Substring match
    if table_lower.contains(stripped) && stripped.len() >= 3 {
        return 0.5;
    }

    0.0
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_strip_fk_suffix() {
        assert_eq!(strip_fk_suffix("user_id"), "user");
        assert_eq!(strip_fk_suffix("customerId"), "customer");
        assert_eq!(strip_fk_suffix("order_key"), "order");
        assert_eq!(strip_fk_suffix("name"), "name"); // no suffix
    }

    #[test]
    fn test_has_fk_suffix() {
        assert!(has_fk_suffix("user_id"));
        assert!(has_fk_suffix("customerId"));
        assert!(has_fk_suffix("order_key"));
        assert!(!has_fk_suffix("name"));
        assert!(!has_fk_suffix("email"));
    }

    #[test]
    fn test_name_match_score() {
        assert_eq!(name_match_score("user", "user"), 1.0);
        assert_eq!(name_match_score("user", "users"), 0.9);
        assert_eq!(name_match_score("users", "user"), 0.9);
        assert!(name_match_score("user", "user_profiles") >= 0.7);
        assert_eq!(name_match_score("xyz", "abc"), 0.0);
    }

    #[test]
    fn test_relationship_evidence_coverage() {
        let mut ev = RelationshipEvidence::new(
            "orders".into(),
            "user_id".into(),
            "users".into(),
            "id".into(),
            0.9,
        );

        // Add FK values (subset of PK values)
        for i in 0..100 {
            ev.from_hll.add(&format!("user_{i}"));
        }
        // Add PK values (superset)
        for i in 0..200 {
            ev.to_hll.add(&format!("user_{i}"));
        }

        let coverage = ev.coverage_ratio();
        // All FK values exist in PK, so coverage should be close to 1.0
        assert!(coverage > 0.8, "coverage should be high, got {coverage}");
    }

    #[test]
    fn test_relationship_evidence_low_coverage() {
        let mut ev = RelationshipEvidence::new(
            "orders".into(),
            "product_id".into(),
            "users".into(),
            "id".into(),
            0.3,
        );

        // FK values are completely different from PK values
        for i in 0..100 {
            ev.from_hll.add(&format!("product_{i}"));
        }
        for i in 0..100 {
            ev.to_hll.add(&format!("user_{i}"));
        }

        let coverage = ev.coverage_ratio();
        assert!(coverage < 0.2, "coverage should be low, got {coverage}");
    }

    #[test]
    fn test_detect_candidates() {
        let columns = vec![
            IncrementalRelColumn {
                name: "id".into(),
                is_likely_pk: true,
                table_name: "users".into(),
            },
            IncrementalRelColumn {
                name: "user_id".into(),
                is_likely_pk: false,
                table_name: "orders".into(),
            },
            IncrementalRelColumn {
                name: "order_id".into(),
                is_likely_pk: true,
                table_name: "orders".into(),
            },
            IncrementalRelColumn {
                name: "name".into(),
                is_likely_pk: false,
                table_name: "users".into(),
            },
        ];

        let candidates = detect_candidates(&columns, &[]);
        assert!(!candidates.is_empty());
        assert!(candidates
            .iter()
            .any(|c| c.from_column == "user_id" && c.to_table == "users"));
    }

    #[test]
    fn test_pairwise_correlation_update() {
        let mut pc = PairwiseCorrelation::new("x".into(), "y".into());

        // Perfect positive correlation: y = 2x
        for i in 0..100 {
            let x = i as f64;
            let y = 2.0 * x;
            pc.update(x, y);
        }

        let r = pc.pearson_r().unwrap();
        assert!((r - 1.0).abs() < 1e-10, "expected r≈1.0, got {r}");
    }

    #[test]
    fn test_pairwise_correlation_negative() {
        let mut pc = PairwiseCorrelation::new("x".into(), "y".into());

        // Perfect negative correlation: y = -x
        for i in 0..100 {
            let x = i as f64;
            let y = -x;
            pc.update(x, y);
        }

        let r = pc.pearson_r().unwrap();
        assert!((r + 1.0).abs() < 1e-10, "expected r≈-1.0, got {r}");
    }

    #[test]
    fn test_pairwise_correlation_merge() {
        let mut pc1 = PairwiseCorrelation::new("x".into(), "y".into());
        let mut pc2 = PairwiseCorrelation::new("x".into(), "y".into());

        // Split data: first half in pc1, second in pc2
        for i in 0..50 {
            let x = i as f64;
            let y = 3.0 * x + 1.0;
            pc1.update(x, y);
        }
        for i in 50..100 {
            let x = i as f64;
            let y = 3.0 * x + 1.0;
            pc2.update(x, y);
        }

        // Merge
        pc1.merge(&pc2);

        let r = pc1.pearson_r().unwrap();
        assert!((r - 1.0).abs() < 1e-10, "merged r should be ≈1.0, got {r}");
        assert_eq!(pc1.count, 100);
    }

    #[test]
    fn test_finalize_relationships_filters() {
        let mut ev = RelationshipEvidence::new(
            "orders".into(),
            "user_id".into(),
            "users".into(),
            "id".into(),
            0.9,
        );
        // High coverage
        for i in 0..100 {
            let s = format!("u{i}");
            ev.from_hll.add(&s);
            ev.to_hll.add(&s);
        }
        // Extra PK values
        for i in 100..200 {
            ev.to_hll.add(&format!("u{i}"));
        }
        ev.chunks_observed = 3;

        let results = finalize_relationships(&[ev]);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].from_table, "orders");
        assert_eq!(results[0].to_table, "users");
        assert!(results[0].confidence > 0.5);
    }
}