//! Cross-column correlation detection — Pearson, Spearman, and Cramér's V.
//!
//! Analyzes pairs of columns for statistical relationships and returns
//! significant correlations filtered by p-value and effect size.

use std::collections::HashMap;

use arrow::array::{Array, Float64Array, StringArray};
use arrow::record_batch::RecordBatch;
use tracing::{debug, info_span, trace};

use crate::learn::profile::ColumnProfile;

/// A detected correlation between two columns.
#[derive(Debug, Clone)]
pub struct Correlation {
    /// First column name.
    pub column_a: String,
    /// Second column name.
    pub column_b: String,
    /// Correlation method used.
    pub method: CorrelationMethod,
    /// Correlation coefficient (or Cramér's V).
    pub coefficient: f64,
    /// Approximate p-value.
    pub p_value: f64,
}

/// Method used to compute the correlation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrelationMethod {
    /// Pearson product-moment correlation.
    Pearson,
    /// Spearman rank correlation.
    Spearman,
    /// Cramér's V for categorical associations.
    CramersV,
}

/// Detect significant correlations between columns in record batches.
///
/// For numeric column pairs, computes Pearson and Spearman correlations.
/// For string/categorical pairs, computes Cramér's V.
///
/// Filters results to |r| ≥ 0.3 and approximate p-value < 0.05.
///
/// # Arguments
///
/// * `profiles` — Column profiles (used to determine column types).
/// * `batches` — The actual data as Arrow record batches.
pub fn detect_correlations(
    profiles: &[ColumnProfile],
    batches: &[RecordBatch],
) -> Vec<Correlation> {
    let _span = info_span!("correlations", columns = profiles.len()).entered();
    if profiles.is_empty() || batches.is_empty() {
        return Vec::new();
    }

    let _schema = batches[0].schema();
    let mut results = Vec::new();

    // Collect column data by name
    let numeric_cols = collect_numeric_columns(profiles, batches);
    let string_cols = collect_string_columns(profiles, batches);

    // Numeric pairs: Pearson + Spearman
    let num_names: Vec<&String> = numeric_cols.keys().collect();
    for i in 0..num_names.len() {
        for j in (i + 1)..num_names.len() {
            let a = &numeric_cols[num_names[i]];
            let b = &numeric_cols[num_names[j]];
            let (paired_a, paired_b) = paired_finite(a, b);
            if paired_a.len() < 5 {
                continue;
            }

            // Pearson
            let pearson = pearson_correlation(&paired_a, &paired_b);
            let p_val = pearson_p_value(pearson, paired_a.len());
            if pearson.abs() >= 0.3 && p_val < 0.05 {
                debug!(a = %num_names[i], b = %num_names[j], r = pearson, "Pearson correlation");
                results.push(Correlation {
                    column_a: num_names[i].clone(),
                    column_b: num_names[j].clone(),
                    method: CorrelationMethod::Pearson,
                    coefficient: pearson,
                    p_value: p_val,
                });
            } else {
                trace!(a = %num_names[i], b = %num_names[j], r = pearson, p = p_val,
                       "Pearson correlation below threshold, skipped");
            }

            // Spearman
            let spearman = spearman_correlation(&paired_a, &paired_b);
            let sp_p_val = pearson_p_value(spearman, paired_a.len());
            if spearman.abs() >= 0.3 && sp_p_val < 0.05 {
                debug!(a = %num_names[i], b = %num_names[j], rho = spearman, "Spearman correlation");
                results.push(Correlation {
                    column_a: num_names[i].clone(),
                    column_b: num_names[j].clone(),
                    method: CorrelationMethod::Spearman,
                    coefficient: spearman,
                    p_value: sp_p_val,
                });
            } else {
                trace!(a = %num_names[i], b = %num_names[j], rho = spearman, p = sp_p_val,
                       "Spearman correlation below threshold, skipped");
            }
        }
    }

    // Categorical pairs: Cramér's V (row-aligned, skip nulls)
    let str_names: Vec<&String> = string_cols.keys().collect();
    for i in 0..str_names.len() {
        for j in (i + 1)..str_names.len() {
            let a = &string_cols[str_names[i]];
            let b = &string_cols[str_names[j]];
            let n = a.len().min(b.len());
            // Collect aligned non-null pairs
            let mut paired_a = Vec::new();
            let mut paired_b = Vec::new();
            for idx in 0..n {
                if let (Some(av), Some(bv)) = (&a[idx], &b[idx]) {
                    paired_a.push(av.clone());
                    paired_b.push(bv.clone());
                }
            }
            if paired_a.len() < 5 {
                continue;
            }

            let v = cramers_v(&paired_a, &paired_b);
            if v >= 0.3 {
                debug!(a = %str_names[i], b = %str_names[j], v, "Cramér's V");
                results.push(Correlation {
                    column_a: str_names[i].clone(),
                    column_b: str_names[j].clone(),
                    method: CorrelationMethod::CramersV,
                    coefficient: v,
                    p_value: 0.0, // Cramér's V doesn't have a simple p-value
                });
            } else {
                trace!(a = %str_names[i], b = %str_names[j], v, "Cramér's V below threshold, skipped");
            }
        }
    }

    results.truncate(500);
    results
}

// ─── correlation implementations ────────────────────────────────────────────

/// Pearson product-moment correlation coefficient.
pub fn pearson_correlation(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len() as f64;
    if n < 2.0 {
        return 0.0;
    }
    let x_mean = xs.iter().sum::<f64>() / n;
    let y_mean = ys.iter().sum::<f64>() / n;

    let mut num = 0.0;
    let mut den_x = 0.0;
    let mut den_y = 0.0;
    for (&x, &y) in xs.iter().zip(ys) {
        let dx = x - x_mean;
        let dy = y - y_mean;
        num += dx * dy;
        den_x += dx * dx;
        den_y += dy * dy;
    }
    let den = (den_x * den_y).sqrt();
    if den < f64::EPSILON {
        0.0
    } else {
        (num / den).clamp(-1.0, 1.0)
    }
}

/// Spearman rank correlation coefficient.
pub fn spearman_correlation(xs: &[f64], ys: &[f64]) -> f64 {
    let rank_x = ranks(xs);
    let rank_y = ranks(ys);
    pearson_correlation(&rank_x, &rank_y)
}

/// Cramér's V for two categorical columns.
pub fn cramers_v(a: &[String], b: &[String]) -> f64 {
    let n = a.len();
    if n == 0 {
        return 0.0;
    }

    // Build contingency table
    let mut contingency: HashMap<(&str, &str), u64> = HashMap::new();
    let mut row_cats: HashMap<&str, u64> = HashMap::new();
    let mut col_cats: HashMap<&str, u64> = HashMap::new();
    for (ai, bi) in a.iter().zip(b.iter()) {
        *contingency.entry((ai.as_str(), bi.as_str())).or_insert(0) += 1;
        *row_cats.entry(ai.as_str()).or_insert(0) += 1;
        *col_cats.entry(bi.as_str()).or_insert(0) += 1;
    }

    let r = row_cats.len();
    let k = col_cats.len();
    if r <= 1 || k <= 1 {
        return 0.0;
    }

    // Chi-squared statistic — iterate full Cartesian product of categories
    let nf = n as f64;
    let mut chi2 = 0.0;
    for (ri, &row_count) in &row_cats {
        for (ci, &col_count) in &col_cats {
            let expected = (row_count as f64) * (col_count as f64) / nf;
            if expected > 0.0 {
                let obs = contingency.get(&(*ri, *ci)).copied().unwrap_or(0) as f64;
                chi2 += (obs - expected).powi(2) / expected;
            }
        }
    }

    let min_dim = (r - 1).min(k - 1) as f64;
    if min_dim <= 0.0 {
        return 0.0;
    }

    (chi2 / (nf * min_dim)).sqrt().clamp(0.0, 1.0)
}

// ─── helpers ────────────────────────────────────────────────────────────────

/// Compute ranks for a slice (average ranks for ties).
fn ranks(values: &[f64]) -> Vec<f64> {
    let n = values.len();
    let mut indexed: Vec<(usize, f64)> = values.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

    let mut result = vec![0.0; n];
    let mut i = 0;
    while i < n {
        let mut j = i;
        while j < n && (indexed[j].1 - indexed[i].1).abs() < f64::EPSILON {
            j += 1;
        }
        let avg_rank = (i + j + 1) as f64 / 2.0; // 1-based average
        for k in i..j {
            result[indexed[k].0] = avg_rank;
        }
        i = j;
    }
    result
}

/// Approximate p-value for Pearson r using t-distribution approximation.
fn pearson_p_value(r: f64, n: usize) -> f64 {
    if n < 3 || r.abs() >= 1.0 {
        return if r.abs() >= 1.0 { 0.0 } else { 1.0 };
    }
    let t = r * ((n - 2) as f64 / (1.0 - r * r)).sqrt();
    let df = (n - 2) as f64;
    // Use the incomplete beta function approximation for the t-distribution
    // For large df, use normal approximation
    let p = if df > 30.0 {
        2.0 * normal_cdf(-t.abs())
    } else {
        // Simple approximation
        2.0 * t_distribution_tail(t.abs(), df)
    };
    p.clamp(0.0, 1.0)
}

/// Very rough approximation of the upper tail of the t-distribution.
fn t_distribution_tail(t: f64, df: f64) -> f64 {
    // Use the normal approximation as a fallback
    let x = t * (1.0 - 1.0 / (4.0 * df)).max(0.0);
    normal_cdf(-x)
}

/// Standard normal CDF approximation (Abramowitz and Stegun).
fn normal_cdf(x: f64) -> f64 {
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;

    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs() / std::f64::consts::SQRT_2;
    let t = 1.0 / (1.0 + p * x);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x * x).exp();
    0.5 * (1.0 + sign * y)
}

fn paired_finite(a: &[f64], b: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let mut ra = Vec::new();
    let mut rb = Vec::new();
    for (&av, &bv) in a.iter().zip(b.iter()) {
        if av.is_finite() && bv.is_finite() {
            ra.push(av);
            rb.push(bv);
        }
    }
    (ra, rb)
}

fn collect_numeric_columns(
    profiles: &[ColumnProfile],
    batches: &[RecordBatch],
) -> HashMap<String, Vec<f64>> {
    let mut result: HashMap<String, Vec<f64>> = HashMap::new();
    for profile in profiles {
        if profile.numeric.is_none() {
            continue;
        }
        let mut values = Vec::new();
        for batch in batches {
            if let Ok(col_idx) = batch.schema().index_of(&profile.name) {
                let col = batch.column(col_idx);
                // Preserve row alignment: use NaN for null slots
                append_numeric_values_aligned(col, &mut values);
            }
        }
        if !values.is_empty() {
            // Cap at 100k values
            values.truncate(100_000);
            result.insert(profile.name.clone(), values);
        }
    }
    result
}

fn collect_string_columns(
    profiles: &[ColumnProfile],
    batches: &[RecordBatch],
) -> HashMap<String, Vec<Option<String>>> {
    let mut result: HashMap<String, Vec<Option<String>>> = HashMap::new();
    for profile in profiles {
        if profile.numeric.is_some() || profile.temporal.is_some() {
            continue;
        }
        let mut values = Vec::new();
        for batch in batches {
            if let Ok(col_idx) = batch.schema().index_of(&profile.name) {
                let col = batch.column(col_idx);
                if let Some(arr) = col.as_any().downcast_ref::<StringArray>() {
                    for i in 0..arr.len() {
                        if arr.is_null(i) {
                            values.push(None);
                        } else {
                            values.push(Some(arr.value(i).to_string()));
                        }
                    }
                }
            }
        }
        if !values.is_empty() {
            values.truncate(100_000);
            result.insert(profile.name.clone(), values);
        }
    }
    result
}

/// Append numeric values preserving row alignment — NaN for null slots.
fn append_numeric_values_aligned(col: &dyn Array, values: &mut Vec<f64>) {
    if let Some(arr) = col.as_any().downcast_ref::<Float64Array>() {
        for i in 0..arr.len() {
            values.push(if arr.is_null(i) {
                f64::NAN
            } else {
                arr.value(i)
            });
        }
    } else if let Some(arr) = col.as_any().downcast_ref::<arrow::array::Int64Array>() {
        for i in 0..arr.len() {
            values.push(if arr.is_null(i) {
                f64::NAN
            } else {
                arr.value(i) as f64
            });
        }
    } else if let Some(arr) = col.as_any().downcast_ref::<arrow::array::Int32Array>() {
        for i in 0..arr.len() {
            values.push(if arr.is_null(i) {
                f64::NAN
            } else {
                arr.value(i) as f64
            });
        }
    } else if let Some(arr) = col.as_any().downcast_ref::<arrow::array::Float32Array>() {
        for i in 0..arr.len() {
            values.push(if arr.is_null(i) {
                f64::NAN
            } else {
                arr.value(i) as f64
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pearson_perfect_positive() {
        let x: Vec<f64> = (0..100).map(|i| i as f64).collect();
        let y: Vec<f64> = x.iter().map(|v| v * 2.0 + 1.0).collect();
        let r = pearson_correlation(&x, &y);
        assert!(
            (r - 1.0).abs() < 0.001,
            "perfect linear should be r≈1.0, got {r}"
        );
    }

    #[test]
    fn pearson_perfect_negative() {
        let x: Vec<f64> = (0..100).map(|i| i as f64).collect();
        let y: Vec<f64> = x.iter().map(|v| -v * 3.0 + 50.0).collect();
        let r = pearson_correlation(&x, &y);
        assert!(
            (r + 1.0).abs() < 0.001,
            "perfect negative should be r≈-1.0, got {r}"
        );
    }

    #[test]
    fn spearman_monotone() {
        let x: Vec<f64> = (0..50).map(|i| i as f64).collect();
        let y: Vec<f64> = x.iter().map(|v| v.powi(3)).collect(); // monotone increasing
        let rho = spearman_correlation(&x, &y);
        assert!(rho > 0.99, "monotone should have rho≈1.0, got {rho}");
    }

    #[test]
    fn cramers_v_perfect_association() {
        let a: Vec<String> = (0..100)
            .map(|i| if i % 2 == 0 { "A".into() } else { "B".into() })
            .collect();
        let b: Vec<String> = (0..100)
            .map(|i| if i % 2 == 0 { "X".into() } else { "Y".into() })
            .collect();
        let v = cramers_v(&a, &b);
        assert!(v > 0.6, "perfect association should have high V, got {v}");
    }

    #[test]
    fn cramers_v_no_association() {
        let a: Vec<String> = (0..200)
            .map(|i| if i % 2 == 0 { "A".into() } else { "B".into() })
            .collect();
        // b is independent of a
        let b: Vec<String> = (0..200)
            .map(|i| if i % 3 == 0 { "X".into() } else { "Y".into() })
            .collect();
        let v = cramers_v(&a, &b);
        assert!(v < 0.3, "no association should have low V, got {v}");
    }

    #[test]
    fn ranks_with_ties() {
        let vals = vec![3.0, 1.0, 4.0, 1.0, 5.0];
        let r = ranks(&vals);
        // 1.0 appears at indices 1, 3 → ranks 1, 2 → avg 1.5
        assert!((r[1] - 1.5).abs() < 0.01);
        assert!((r[3] - 1.5).abs() < 0.01);
    }

    #[test]
    fn empty_inputs() {
        assert_eq!(pearson_correlation(&[], &[]), 0.0);
        assert_eq!(cramers_v(&[], &[]), 0.0);
    }
}