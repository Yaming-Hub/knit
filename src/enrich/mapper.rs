//! Column mapping — match reference columns to base model fields.

use crate::core::{Entity, Field};
use crate::learn::profile::ColumnProfile;
use tracing::debug;

/// A mapping from a reference column to a model field.
#[derive(Debug, Clone)]
pub struct ColumnMapping {
    /// Index of the reference column in the profiles array.
    pub ref_col_index: usize,
    /// Name of the reference column.
    pub ref_col_name: String,
    /// Name of the target field in the model entity.
    pub target_field: String,
    /// Confidence score (0.0–1.0).
    pub confidence: f64,
    /// Whether types are compatible.
    pub type_compatible: bool,
}

/// Map reference columns to entity fields using name similarity and type compatibility.
///
/// Uses greedy best-match assignment: each model field can be matched at most once.
pub fn map_columns(
    profiles: &[ColumnProfile],
    entity: &Entity,
    min_confidence: f64,
) -> Vec<ColumnMapping> {
    let mut mappings = Vec::new();
    let mut used_fields: Vec<bool> = vec![false; entity.fields.len()];

    // Score all pairs
    let mut candidates: Vec<(usize, usize, f64, bool)> = Vec::new(); // (ref_idx, field_idx, score, type_ok)

    for (ref_idx, profile) in profiles.iter().enumerate() {
        for (field_idx, field) in entity.fields.iter().enumerate() {
            let name_score = name_similarity(&profile.name, &field.name);
            let type_ok = type_compatible(profile, field);
            let type_score = if type_ok { 1.0 } else { 0.0 };
            let score = 0.7 * name_score + 0.3 * type_score;

            if score >= min_confidence * 0.5 {
                candidates.push((ref_idx, field_idx, score, type_ok));
            }
        }
    }

    // Sort by score descending for greedy assignment
    candidates.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));

    let mut used_refs: Vec<bool> = vec![false; profiles.len()];

    for (ref_idx, field_idx, score, type_ok) in candidates {
        if used_refs[ref_idx] || used_fields[field_idx] {
            continue;
        }
        if score < min_confidence {
            continue;
        }

        used_refs[ref_idx] = true;
        used_fields[field_idx] = true;

        debug!(
            ref_col = %profiles[ref_idx].name,
            target = %entity.fields[field_idx].name,
            score = score,
            "mapped column"
        );

        mappings.push(ColumnMapping {
            ref_col_index: ref_idx,
            ref_col_name: profiles[ref_idx].name.clone(),
            target_field: entity.fields[field_idx].name.clone(),
            confidence: score,
            type_compatible: type_ok,
        });
    }

    mappings
}

/// Compute name similarity between a reference column name and a model field name.
/// Uses normalized Levenshtein distance with preprocessing.
fn name_similarity(ref_name: &str, field_name: &str) -> f64 {
    let a = normalize_name(ref_name);
    let b = normalize_name(field_name);

    if a == b {
        return 1.0;
    }

    // Check common abbreviation expansions
    let a_expanded = expand_abbreviations(&a);
    let b_expanded = expand_abbreviations(&b);
    if a_expanded == b_expanded {
        return 0.95;
    }

    // Normalized Levenshtein
    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 1.0;
    }
    let dist = levenshtein(&a, &b);
    let similarity = 1.0 - (dist as f64 / max_len as f64);

    // Also check if one contains the other (partial match)
    let containment = if a.contains(&b) || b.contains(&a) {
        let shorter = a.len().min(b.len()) as f64;
        let longer = a.len().max(b.len()) as f64;
        shorter / longer
    } else {
        0.0
    };

    similarity.max(containment)
}

/// Normalize a column/field name for comparison.
fn normalize_name(name: &str) -> String {
    name.to_lowercase()
        .replace(['-', '_', ' ', '.'], "")
}

/// Expand common abbreviations found as components in the name.
fn expand_abbreviations(name: &str) -> String {
    let mut s = name.to_string();
    let abbrevs = [
        ("msg", "message"),
        ("addr", "address"),
        ("qty", "quantity"),
        ("num", "number"),
        ("amt", "amount"),
        ("desc", "description"),
        ("dept", "department"),
        ("org", "organization"),
        ("ts", "timestamp"),
        ("dt", "date"),
        ("cnt", "count"),
    ];
    for (short, long) in abbrevs {
        if s.contains(short) {
            s = s.replace(short, long);
        }
    }
    s
}

/// Check if reference column type is compatible with model field type.
fn type_compatible(profile: &ColumnProfile, field: &Field) -> bool {
    use crate::core::types::DataType;
    let is_ref_numeric = profile.numeric.is_some();
    let is_ref_string = profile.string.is_some();
    let is_ref_temporal = profile.temporal.is_some();

    match &field.data_type {
        DataType::String | DataType::Uuid => is_ref_string || !is_ref_numeric,
        DataType::Int | DataType::Int32 | DataType::Float => is_ref_numeric,
        DataType::Date | DataType::Datetime | DataType::DatetimeUs | DataType::Datetimetz | DataType::Time => {
            is_ref_temporal || is_ref_string
        }
        DataType::Bool => true,
        _ => true,
    }
}

/// Compute Levenshtein distance between two strings.
fn levenshtein(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    let mut dp = vec![vec![0usize; n + 1]; m + 1];
    for i in 0..=m {
        dp[i][0] = i;
    }
    for j in 0..=n {
        dp[0][j] = j;
    }
    for i in 1..=m {
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] { 0 } else { 1 };
            dp[i][j] = (dp[i - 1][j] + 1)
                .min(dp[i][j - 1] + 1)
                .min(dp[i - 1][j - 1] + cost);
        }
    }
    dp[m][n]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name_similarity_exact() {
        assert_eq!(name_similarity("UserName", "username"), 1.0);
        assert_eq!(name_similarity("user_name", "UserName"), 1.0);
    }

    #[test]
    fn test_name_similarity_abbreviation() {
        let score = name_similarity("msg_count", "message_count");
        assert!(score > 0.7, "abbreviation similarity should be high, got {}", score);
    }

    #[test]
    fn test_name_similarity_partial() {
        let score = name_similarity("email", "email_address");
        assert!(score > 0.4);
    }

    #[test]
    fn test_name_similarity_unrelated() {
        let score = name_similarity("age", "department");
        assert!(score < 0.5);
    }

    #[test]
    fn test_normalize_name() {
        assert_eq!(normalize_name("User_Name"), "username");
        assert_eq!(normalize_name("user-name"), "username");
        assert_eq!(normalize_name("UserName"), "username");
    }

    #[test]
    fn test_levenshtein() {
        assert_eq!(levenshtein("kitten", "sitting"), 3);
        assert_eq!(levenshtein("", "abc"), 3);
        assert_eq!(levenshtein("abc", "abc"), 0);
    }
}