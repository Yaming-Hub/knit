//! Cross-column correlation detection — Pearson, Spearman, and Cramér's V.
//!
//! Analyzes pairs of columns for statistical relationships and returns
//! significant correlations filtered by p-value and effect size.

use std::collections::HashMap;

use arrow::array::{Array, Float64Array, StringArray};
use arrow::record_batch::RecordBatch;
use tracing::{debug, info_span, trace};

use super::fitting::Distribution;

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

/// A detected tuple: a group of co-occurring string columns where values form
/// near-functional dependencies (e.g., city+state, first_name+last_name).
#[derive(Debug, Clone)]
pub struct TupleGroup {
    /// Column names in the tuple, ordered by cardinality (highest first = primary).
    pub columns: Vec<String>,
    /// Unique tuples as rows of string values (same order as `columns`).
    pub tuples: Vec<Vec<String>>,
}

/// Detect co-occurring string column tuples.
///
/// Finds pairs of string columns with high Cramér's V (≥ 0.8) that exhibit
/// near-functional dependencies: knowing one column's value nearly determines
/// the other's. Returns groups of columns whose values should be sampled
/// together from a joint dictionary.
pub fn detect_tuple_columns(
    profiles: &[ColumnProfile],
    batches: &[RecordBatch],
) -> Vec<TupleGroup> {
    let _span = info_span!("tuple_detection").entered();
    if profiles.is_empty() || batches.is_empty() {
        return Vec::new();
    }

    let string_cols = collect_string_columns(profiles, batches);
    let str_names: Vec<&String> = string_cols.keys().collect();
    let mut results = Vec::new();

    for i in 0..str_names.len() {
        for j in (i + 1)..str_names.len() {
            let a = &string_cols[str_names[i]];
            let b = &string_cols[str_names[j]];
            let n = a.len().min(b.len());

            // Collect aligned non-null pairs
            let mut paired: Vec<(String, String)> = Vec::new();
            for idx in 0..n {
                if let (Some(av), Some(bv)) = (&a[idx], &b[idx]) {
                    paired.push((av.clone(), bv.clone()));
                }
            }
            if paired.len() < 5 {
                continue;
            }

            // Check Cramér's V for strong association
            let col_a: Vec<String> = paired.iter().map(|(a, _)| a.clone()).collect();
            let col_b: Vec<String> = paired.iter().map(|(_, b)| b.clone()).collect();
            let v = cramers_v(&col_a, &col_b);
            if v < 0.8 {
                continue;
            }

            // Check functional dependency ratio: A→B means each unique A maps
            // to (nearly) one unique B
            let mut a_to_b: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();
            for (av, bv) in &paired {
                a_to_b.entry(av.as_str()).or_default().insert(bv.as_str());
            }
            let func_ratio_ab =
                a_to_b.values().filter(|s| s.len() == 1).count() as f64 / a_to_b.len() as f64;

            let mut b_to_a: HashMap<&str, std::collections::HashSet<&str>> = HashMap::new();
            for (av, bv) in &paired {
                b_to_a.entry(bv.as_str()).or_default().insert(av.as_str());
            }
            let func_ratio_ba =
                b_to_a.values().filter(|s| s.len() == 1).count() as f64 / b_to_a.len() as f64;

            // At least one direction should be >90% functional
            if func_ratio_ab < 0.9 && func_ratio_ba < 0.9 {
                continue;
            }

            // Determine primary (higher cardinality) and extract unique tuples
            let card_a = a_to_b.len();
            let card_b = b_to_a.len();
            let (primary, secondary) = if card_a >= card_b {
                (str_names[i].clone(), str_names[j].clone())
            } else {
                (str_names[j].clone(), str_names[i].clone())
            };

            // Collect unique tuples (dedup by primary value)
            let mut seen = std::collections::HashSet::new();
            let mut tuples = Vec::new();
            for (av, bv) in &paired {
                let (pv, sv) = if card_a >= card_b {
                    (av.clone(), bv.clone())
                } else {
                    (bv.clone(), av.clone())
                };
                if seen.insert(pv.clone()) {
                    tuples.push(vec![pv, sv]);
                }
            }

            debug!(
                a = %primary, b = %secondary, cramers_v = v,
                func_ab = func_ratio_ab, func_ba = func_ratio_ba,
                tuples = tuples.len(),
                "detected tuple columns"
            );

            results.push(TupleGroup {
                columns: vec![primary, secondary],
                tuples,
            });
        }
    }

    results.truncate(20);
    results
}

/// A detected conditional distribution: a categorical column conditions a
/// numeric column's distribution.
#[derive(Debug, Clone)]
pub struct ConditionalDistribution {
    /// The categorical column (conditioning field).
    pub given: String,
    /// The numeric column (dependent field).
    pub dependent: String,
    /// Per-category distribution branches.
    pub branches: Vec<ConditionalBranch>,
    /// Overall (unconditional) distribution for fallback.
    pub default_distribution: Distribution,
    /// Overall mean of the dependent column.
    pub default_mean: f64,
    /// Overall standard deviation of the dependent column.
    pub default_std: f64,
}

/// A single branch mapping a category value to a fitted distribution.
#[derive(Debug, Clone)]
pub struct ConditionalBranch {
    /// Category value.
    pub condition: String,
    /// Fitted distribution for this category.
    pub distribution: Distribution,
    /// Whether values are integer-only.
    pub is_integer: bool,
}

/// Detect categorical→numeric conditional distributions.
///
/// For each (categorical, numeric) column pair, groups numeric values by
/// category and fits distributions per group. Emits a conditional distribution
/// when the per-category means differ significantly (coefficient of variation
/// of group means > 0.15).
pub fn detect_conditional_distributions(
    profiles: &[ColumnProfile],
    batches: &[RecordBatch],
) -> Vec<ConditionalDistribution> {
    let _span = info_span!("conditional_distributions").entered();
    if profiles.is_empty() || batches.is_empty() {
        return Vec::new();
    }

    let numeric_cols = collect_numeric_columns(profiles, batches);
    let string_cols = collect_string_columns(profiles, batches);
    let mut results = Vec::new();

    // For each (categorical, numeric) pair
    for (cat_name, cat_vals) in &string_cols {
        // Skip high-cardinality categoricals (>50 unique values)
        let unique_cats: std::collections::HashSet<&str> = cat_vals
            .iter()
            .filter_map(|v| v.as_deref())
            .collect();
        if unique_cats.len() > 50 || unique_cats.len() < 2 {
            continue;
        }

        for (num_name, num_vals) in &numeric_cols {
            let len = cat_vals.len().min(num_vals.len());
            if len < 10 {
                continue;
            }

            // Group numeric values by category
            let mut groups: HashMap<&str, Vec<f64>> = HashMap::new();
            for i in 0..len {
                if let Some(cat) = cat_vals[i].as_deref() {
                    let val = num_vals[i];
                    if val.is_finite() {
                        groups.entry(cat).or_default().push(val);
                    }
                }
            }

            // Need at least 2 groups with 5+ values each
            let valid_groups: Vec<(&str, &Vec<f64>)> = groups
                .iter()
                .filter(|(_, v)| v.len() >= 5)
                .map(|(k, v)| (*k, v))
                .collect();
            if valid_groups.len() < 2 {
                continue;
            }

            // Check if group means differ significantly
            let group_means: Vec<f64> = valid_groups
                .iter()
                .map(|(_, v)| v.iter().sum::<f64>() / v.len() as f64)
                .collect();
            let overall_mean = group_means.iter().sum::<f64>() / group_means.len() as f64;
            if overall_mean.abs() < f64::EPSILON {
                continue;
            }
            let mean_std = (group_means
                .iter()
                .map(|m| (m - overall_mean).powi(2))
                .sum::<f64>()
                / group_means.len() as f64)
                .sqrt();
            let cv = mean_std / overall_mean.abs();

            // Only emit if means vary meaningfully (CV > 0.15)
            if cv < 0.15 {
                trace!(
                    cat = %cat_name, num = %num_name, cv,
                    "conditional distribution CV below threshold"
                );
                continue;
            }

            // Fit distribution per group
            let mut branches = Vec::new();
            let all_finite: Vec<f64> = num_vals.iter().filter(|v| v.is_finite()).copied().collect();
            // True overall mean (weighted by actual values, not group means)
            let true_overall_mean =
                all_finite.iter().sum::<f64>() / all_finite.len().max(1) as f64;
            let is_integer = all_finite.iter().all(|v| (*v - v.round()).abs() < 1e-9);

            for (cat, vals) in &valid_groups {
                if let Some(fit) = super::fitting::fit_distribution(vals) {
                    branches.push(ConditionalBranch {
                        condition: cat.to_string(),
                        distribution: fit.best.distribution.clone(),
                        is_integer,
                    });
                }
            }

            if branches.len() < 2 {
                continue;
            }

            // Fit overall default distribution
            let default_dist = super::fitting::fit_distribution(&all_finite)
                .map(|f| f.best.distribution.clone())
                .unwrap_or_else(|| {
                    let mean = all_finite.iter().sum::<f64>() / all_finite.len() as f64;
                    let std = (all_finite
                        .iter()
                        .map(|v| (v - mean).powi(2))
                        .sum::<f64>()
                        / all_finite.len() as f64)
                        .sqrt();
                    Distribution::Normal(mean, std.max(0.01))
                });

            let overall_std = (all_finite
                .iter()
                .map(|v| (v - true_overall_mean).powi(2))
                .sum::<f64>()
                / all_finite.len() as f64)
                .sqrt();

            debug!(
                cat = %cat_name, num = %num_name, cv,
                groups = branches.len(),
                "detected conditional distribution"
            );

            results.push(ConditionalDistribution {
                given: cat_name.clone(),
                dependent: num_name.clone(),
                branches,
                default_distribution: default_dist,
                default_mean: true_overall_mean,
                default_std: overall_std.max(0.01),
            });
        }
    }

    // Limit to avoid blueprint bloat
    results.truncate(50);
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
pub fn pearson_p_value(r: f64, n: usize) -> f64 {
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
    use arrow::array::{Float64Array, Int64Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    // ─── helper builders ────────────────────────────────────────────────

    /// Build a minimal numeric `ColumnProfile`.
    fn numeric_profile(name: &str) -> ColumnProfile {
        ColumnProfile {
            name: name.to_string(),
            data_type: DataType::Float64,
            count: 0,
            null_count: 0,
            null_rate: 0.0,
            empty_string_rate: 0.0,
            distinct_count: None,
            cardinality_ratio: None,
            numeric: Some(crate::learn::profile::NumericProfile {
                min: 0.0,
                max: 0.0,
                mean: 0.0,
                std_dev: 0.0,
                median: 0.0,
                skewness: 0.0,
                kurtosis: 0.0,
                max_decimal_places: Some(0),
                percentiles: crate::learn::profile::Percentiles {
                    p1: 0.0,
                    p5: 0.0,
                    p10: 0.0,
                    p25: 0.0,
                    p50: 0.0,
                    p75: 0.0,
                    p90: 0.0,
                    p95: 0.0,
                    p99: 0.0,
                },
            }),
            string: None,
            temporal: None,
        }
    }

    /// Build a minimal string `ColumnProfile`.
    fn string_profile(name: &str) -> ColumnProfile {
        ColumnProfile {
            name: name.to_string(),
            data_type: DataType::Utf8,
            count: 0,
            null_count: 0,
            null_rate: 0.0,
            empty_string_rate: 0.0,
            distinct_count: None,
            cardinality_ratio: None,
            numeric: None,
            string: None,
            temporal: None,
        }
    }

    /// Build a `RecordBatch` from f64 column arrays.
    fn numeric_batch(columns: &[(&str, Vec<f64>)]) -> RecordBatch {
        let fields: Vec<Field> = columns
            .iter()
            .map(|(name, _)| Field::new(*name, DataType::Float64, true))
            .collect();
        let schema = Arc::new(Schema::new(fields));
        let arrays: Vec<Arc<dyn Array>> = columns
            .iter()
            .map(|(_, values)| Arc::new(Float64Array::from(values.clone())) as Arc<dyn Array>)
            .collect();
        RecordBatch::try_new(schema, arrays).unwrap()
    }

    /// Build a `RecordBatch` from string column arrays.
    fn string_batch(columns: &[(&str, Vec<Option<&str>>)]) -> RecordBatch {
        let fields: Vec<Field> = columns
            .iter()
            .map(|(name, _)| Field::new(*name, DataType::Utf8, true))
            .collect();
        let schema = Arc::new(Schema::new(fields));
        let arrays: Vec<Arc<dyn Array>> = columns
            .iter()
            .map(|(_, values)| Arc::new(StringArray::from(values.clone())) as Arc<dyn Array>)
            .collect();
        RecordBatch::try_new(schema, arrays).unwrap()
    }

    // ─── pearson_correlation ────────────────────────────────────────────

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
    fn pearson_uncorrelated() {
        // sin and cos over full cycles are uncorrelated
        let n = 1000;
        let x: Vec<f64> = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * i as f64 / n as f64).sin())
            .collect();
        let y: Vec<f64> = (0..n)
            .map(|i| (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos())
            .collect();
        let r = pearson_correlation(&x, &y);
        assert!(r.abs() < 0.05, "sin/cos should be uncorrelated, got {r}");
    }

    #[test]
    fn pearson_constant_x_returns_zero() {
        let x = vec![5.0; 50];
        let y: Vec<f64> = (0..50).map(|i| i as f64).collect();
        assert_eq!(pearson_correlation(&x, &y), 0.0);
    }

    #[test]
    fn pearson_single_element() {
        assert_eq!(pearson_correlation(&[1.0], &[2.0]), 0.0);
    }

    #[test]
    fn pearson_two_elements() {
        let r = pearson_correlation(&[1.0, 2.0], &[3.0, 4.0]);
        assert!((r - 1.0).abs() < 0.001, "two co-increasing points → r≈1.0");
    }

    // ─── spearman_correlation ───────────────────────────────────────────

    #[test]
    fn spearman_monotone() {
        let x: Vec<f64> = (0..50).map(|i| i as f64).collect();
        let y: Vec<f64> = x.iter().map(|v| v.powi(3)).collect();
        let rho = spearman_correlation(&x, &y);
        assert!(rho > 0.99, "monotone should have rho≈1.0, got {rho}");
    }

    #[test]
    fn spearman_reverse() {
        let x: Vec<f64> = (0..50).map(|i| i as f64).collect();
        let y: Vec<f64> = x.iter().map(|v| -v).collect();
        let rho = spearman_correlation(&x, &y);
        assert!(
            (rho + 1.0).abs() < 0.01,
            "reverse should have rho≈-1.0, got {rho}"
        );
    }

    #[test]
    fn spearman_with_ties() {
        let x = vec![1.0, 2.0, 2.0, 3.0, 4.0, 4.0, 5.0];
        let y = vec![10.0, 20.0, 20.0, 30.0, 40.0, 40.0, 50.0];
        let rho = spearman_correlation(&x, &y);
        assert!(rho > 0.95, "tied but monotone → high rho, got {rho}");
    }

    // ─── cramers_v ──────────────────────────────────────────────────────

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
        let b: Vec<String> = (0..200)
            .map(|i| if i % 3 == 0 { "X".into() } else { "Y".into() })
            .collect();
        let v = cramers_v(&a, &b);
        assert!(v < 0.3, "no association should have low V, got {v}");
    }

    #[test]
    fn cramers_v_single_category_a() {
        let a = vec!["only".to_string(); 20];
        let b: Vec<String> = (0..20)
            .map(|i| if i % 2 == 0 { "X".into() } else { "Y".into() })
            .collect();
        let v = cramers_v(&a, &b);
        assert_eq!(v, 0.0, "single category in a → V=0");
    }

    #[test]
    fn cramers_v_single_category_b() {
        let a: Vec<String> = (0..20)
            .map(|i| if i % 2 == 0 { "A".into() } else { "B".into() })
            .collect();
        let b = vec!["only".to_string(); 20];
        let v = cramers_v(&a, &b);
        assert_eq!(v, 0.0, "single category in b → V=0");
    }

    #[test]
    fn cramers_v_multi_category() {
        // 3×3 contingency with perfect diagonal → high V
        let mut a = Vec::new();
        let mut b = Vec::new();
        for _ in 0..50 {
            a.push("A".to_string());
            b.push("X".to_string());
        }
        for _ in 0..50 {
            a.push("B".to_string());
            b.push("Y".to_string());
        }
        for _ in 0..50 {
            a.push("C".to_string());
            b.push("Z".to_string());
        }
        let v = cramers_v(&a, &b);
        assert!(v > 0.8, "perfect 3×3 diagonal → high V, got {v}");
    }

    // ─── ranks ──────────────────────────────────────────────────────────

    #[test]
    fn ranks_with_ties() {
        let vals = vec![3.0, 1.0, 4.0, 1.0, 5.0];
        let r = ranks(&vals);
        assert!((r[1] - 1.5).abs() < 0.01);
        assert!((r[3] - 1.5).abs() < 0.01);
    }

    #[test]
    fn ranks_already_sorted() {
        let vals = vec![10.0, 20.0, 30.0, 40.0];
        let r = ranks(&vals);
        assert_eq!(r, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn ranks_all_tied() {
        let vals = vec![7.0; 5];
        let r = ranks(&vals);
        // All tied → average rank = (1+2+3+4+5)/5 = 3.0
        for rank in &r {
            assert!((rank - 3.0).abs() < 0.01, "all tied should be 3.0, got {rank}");
        }
    }

    #[test]
    fn ranks_reverse_order() {
        let vals = vec![40.0, 30.0, 20.0, 10.0];
        let r = ranks(&vals);
        assert_eq!(r, vec![4.0, 3.0, 2.0, 1.0]);
    }

    // ─── pearson_p_value ────────────────────────────────────────────────

    #[test]
    fn p_value_perfect_correlation() {
        let p = pearson_p_value(1.0, 100);
        assert_eq!(p, 0.0, "|r|=1 → p=0");
    }

    #[test]
    fn p_value_zero_correlation() {
        let p = pearson_p_value(0.0, 100);
        assert!(
            (p - 1.0).abs() < 0.1,
            "r=0 with large n → p≈1.0, got {p}"
        );
    }

    #[test]
    fn p_value_strong_correlation_small_n() {
        let p = pearson_p_value(0.95, 10);
        assert!(p < 0.05, "strong r with n=10 → p<0.05, got {p}");
    }

    #[test]
    fn p_value_weak_correlation_small_n() {
        let p = pearson_p_value(0.1, 5);
        assert!(p > 0.05, "weak r with n=5 → p>0.05, got {p}");
    }

    #[test]
    fn p_value_too_few_points() {
        assert_eq!(pearson_p_value(0.5, 2), 1.0);
        assert_eq!(pearson_p_value(0.5, 1), 1.0);
    }

    #[test]
    fn p_value_large_df_uses_normal() {
        // df > 30 triggers normal approximation path
        let p = pearson_p_value(0.5, 100);
        assert!(p < 0.001, "r=0.5 at n=100 should be very significant, got {p}");
    }

    // ─── normal_cdf ─────────────────────────────────────────────────────

    #[test]
    fn normal_cdf_symmetry() {
        let mid = normal_cdf(0.0);
        assert!(
            (mid - 0.5).abs() < 0.001,
            "Φ(0) should be 0.5, got {mid}"
        );
    }

    #[test]
    fn normal_cdf_tails() {
        let left = normal_cdf(-3.0);
        let right = normal_cdf(3.0);
        assert!(left < 0.01, "Φ(-3) should be small, got {left}");
        assert!(right > 0.99, "Φ(3) should be near 1, got {right}");
        assert!(
            (left + right - 1.0).abs() < 0.001,
            "Φ(-3) + Φ(3) ≈ 1"
        );
    }

    // ─── paired_finite ──────────────────────────────────────────────────

    #[test]
    fn paired_finite_filters_nan() {
        let a = vec![1.0, f64::NAN, 3.0, 4.0];
        let b = vec![10.0, 20.0, f64::NAN, 40.0];
        let (pa, pb) = paired_finite(&a, &b);
        assert_eq!(pa, vec![1.0, 4.0]);
        assert_eq!(pb, vec![10.0, 40.0]);
    }

    #[test]
    fn paired_finite_filters_inf() {
        let a = vec![1.0, f64::INFINITY, 3.0];
        let b = vec![10.0, 20.0, 30.0];
        let (pa, pb) = paired_finite(&a, &b);
        assert_eq!(pa, vec![1.0, 3.0]);
        assert_eq!(pb, vec![10.0, 30.0]);
    }

    #[test]
    fn paired_finite_different_lengths() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![10.0, 20.0];
        let (pa, pb) = paired_finite(&a, &b);
        assert_eq!(pa.len(), 2);
        assert_eq!(pb.len(), 2);
    }

    #[test]
    fn paired_finite_empty() {
        let (pa, pb) = paired_finite(&[], &[]);
        assert!(pa.is_empty());
        assert!(pb.is_empty());
    }

    // ─── detect_correlations (end-to-end) ───────────────────────────────

    #[test]
    fn detect_correlations_empty_profiles() {
        let result = detect_correlations(&[], &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn detect_correlations_empty_batches() {
        let profiles = vec![numeric_profile("a")];
        let result = detect_correlations(&profiles, &[]);
        assert!(result.is_empty());
    }

    #[test]
    fn detect_correlations_finds_numeric_pair() {
        let n = 200;
        let x: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let y: Vec<f64> = x.iter().map(|v| v * 2.0 + 5.0).collect();
        let batch = numeric_batch(&[("x", x), ("y", y)]);
        let profiles = vec![numeric_profile("x"), numeric_profile("y")];

        let results = detect_correlations(&profiles, &[batch]);
        let pearson_hits: Vec<_> = results
            .iter()
            .filter(|c| c.method == CorrelationMethod::Pearson)
            .collect();
        assert!(
            !pearson_hits.is_empty(),
            "should detect Pearson correlation for perfectly linear data"
        );
        assert!(
            pearson_hits[0].coefficient > 0.99,
            "coefficient should be ~1.0, got {}",
            pearson_hits[0].coefficient
        );
    }

    #[test]
    fn detect_correlations_skips_weak_numeric() {
        // Two uncorrelated columns — should produce no results at all
        let n = 200;
        let x: Vec<f64> = (0..n)
            .map(|i| (i as f64 * 0.1).sin())
            .collect();
        let y: Vec<f64> = (0..n)
            .map(|i| (i as f64 * 0.1).cos())
            .collect();
        let batch = numeric_batch(&[("x", x), ("y", y)]);
        let profiles = vec![numeric_profile("x"), numeric_profile("y")];

        let results = detect_correlations(&profiles, &[batch]);
        assert!(
            results.is_empty(),
            "uncorrelated sin/cos should produce no results, got {} hits",
            results.len()
        );
    }

    #[test]
    fn detect_correlations_fewer_than_5_rows_skipped() {
        let batch = numeric_batch(&[("a", vec![1.0, 2.0, 3.0]), ("b", vec![4.0, 5.0, 6.0])]);
        let profiles = vec![numeric_profile("a"), numeric_profile("b")];
        let results = detect_correlations(&profiles, &[batch]);
        assert!(
            results.is_empty(),
            "fewer than 5 paired values should be skipped"
        );
    }

    #[test]
    fn detect_correlations_finds_categorical_pair() {
        // Perfect categorical association
        let n = 100;
        let a: Vec<Option<&str>> = (0..n)
            .map(|i| Some(if i % 2 == 0 { "A" } else { "B" }))
            .collect();
        let b: Vec<Option<&str>> = (0..n)
            .map(|i| Some(if i % 2 == 0 { "X" } else { "Y" }))
            .collect();
        let batch = string_batch(&[("cat_a", a), ("cat_b", b)]);
        let profiles = vec![string_profile("cat_a"), string_profile("cat_b")];

        let results = detect_correlations(&profiles, &[batch]);
        let cramers: Vec<_> = results
            .iter()
            .filter(|c| c.method == CorrelationMethod::CramersV)
            .collect();
        assert!(
            !cramers.is_empty(),
            "should detect Cramér's V for perfectly associated categories"
        );
        assert!(cramers[0].coefficient > 0.5);
    }

    #[test]
    fn detect_correlations_skips_null_pairs() {
        // Mix of null and non-null rows — only 3 non-null pairs remain
        // (fewer than 5), so no correlation should be detected
        let a: Vec<Option<&str>> = vec![
            Some("A"), None, Some("B"), None, Some("A"),
            None, None, None, None, None,
            None, None, None, None, None,
            None, None, None, None, None,
        ];
        let b: Vec<Option<&str>> = vec![
            Some("X"), Some("Y"), Some("Y"), Some("X"), Some("X"),
            Some("Y"), Some("X"), Some("Y"), Some("X"), Some("Y"),
            Some("X"), Some("Y"), Some("X"), Some("Y"), Some("X"),
            Some("Y"), Some("X"), Some("Y"), Some("X"), Some("Y"),
        ];
        let batch = string_batch(&[("cat_a", a), ("cat_b", b)]);
        let profiles = vec![string_profile("cat_a"), string_profile("cat_b")];

        let results = detect_correlations(&profiles, &[batch]);
        assert!(
            results.is_empty(),
            "mostly-null column should produce no correlations (too few pairs)"
        );
    }

    #[test]
    fn detect_correlations_multiple_batches() {
        // Each batch alone has only 4 rows (below the 5-row minimum).
        // Only by combining both batches do we get 8 rows, enough
        // to detect the correlation.
        let x1: Vec<f64> = (0..4).map(|i| i as f64).collect();
        let y1: Vec<f64> = x1.iter().map(|v| v * 3.0).collect();
        let x2: Vec<f64> = (4..8).map(|i| i as f64).collect();
        let y2: Vec<f64> = x2.iter().map(|v| v * 3.0).collect();

        let batch1 = numeric_batch(&[("x", x1), ("y", y1)]);
        let batch2 = numeric_batch(&[("x", x2), ("y", y2)]);
        let profiles = vec![numeric_profile("x"), numeric_profile("y")];

        // Single batch alone should be insufficient
        let single = detect_correlations(&profiles, std::slice::from_ref(&batch1));
        assert!(
            single.is_empty(),
            "single 4-row batch should be insufficient"
        );

        // Both batches combined should detect the correlation
        let results = detect_correlations(&profiles, &[batch1, batch2]);
        assert!(
            !results.is_empty(),
            "combined batches should detect correlation"
        );
    }

    #[test]
    fn detect_correlations_three_numeric_columns() {
        // a ↔ b correlated, a ↔ c uncorrelated
        let n = 200;
        let a: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let b: Vec<f64> = a.iter().map(|v| v * 2.0).collect();
        let c: Vec<f64> = (0..n).map(|i| ((i * 7 + 13) % 100) as f64).collect();

        let batch = numeric_batch(&[("a", a), ("b", b), ("c", c)]);
        let profiles = vec![
            numeric_profile("a"),
            numeric_profile("b"),
            numeric_profile("c"),
        ];

        let results = detect_correlations(&profiles, &[batch]);
        let ab: Vec<_> = results
            .iter()
            .filter(|c| {
                (c.column_a == "a" && c.column_b == "b")
                    || (c.column_a == "b" && c.column_b == "a")
            })
            .collect();
        assert!(!ab.is_empty(), "should detect a↔b correlation");
    }

    // ─── append_numeric_values_aligned ───────────────────────────────────

    #[test]
    fn append_numeric_f64_with_nulls() {
        let arr = Float64Array::from(vec![Some(1.0), None, Some(3.0)]);
        let mut values = Vec::new();
        append_numeric_values_aligned(&arr, &mut values);
        assert_eq!(values.len(), 3);
        assert_eq!(values[0], 1.0);
        assert!(values[1].is_nan());
        assert_eq!(values[2], 3.0);
    }

    #[test]
    fn append_numeric_i64_with_nulls() {
        let arr = Int64Array::from(vec![Some(10), None, Some(30)]);
        let mut values = Vec::new();
        append_numeric_values_aligned(&arr, &mut values);
        assert_eq!(values.len(), 3);
        assert_eq!(values[0], 10.0);
        assert!(values[1].is_nan());
        assert_eq!(values[2], 30.0);
    }

    // ─── empty / edge cases ─────────────────────────────────────────────

    #[test]
    fn empty_inputs() {
        assert_eq!(pearson_correlation(&[], &[]), 0.0);
        assert_eq!(cramers_v(&[], &[]), 0.0);
        assert_eq!(spearman_correlation(&[], &[]), 0.0);
    }

    #[test]
    fn ranks_empty() {
        assert!(ranks(&[]).is_empty());
    }

    #[test]
    fn ranks_single() {
        assert_eq!(ranks(&[42.0]), vec![1.0]);
    }

    // ─── conditional distribution tests ────────────────────────────────

    /// Build a batch with one string column and one numeric column.
    fn cond_batch(cat: &[&str], nums: &[f64]) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("category", DataType::Utf8, false),
            Field::new("value", DataType::Float64, false),
        ]));
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(cat.to_vec())),
                Arc::new(Float64Array::from(nums.to_vec())),
            ],
        )
        .unwrap()
    }

    #[test]
    fn cond_dist_detects_distinct_groups() {
        // Two categories with very different means → should detect
        let mut cat = Vec::new();
        let mut vals = Vec::new();
        for _ in 0..50 {
            cat.push("A");
            vals.push(10.0);
        }
        for _ in 0..50 {
            cat.push("B");
            vals.push(100.0);
        }
        let batch = cond_batch(&cat, &vals);
        let profiles = vec![string_profile("category"), numeric_profile("value")];
        let result = detect_conditional_distributions(&profiles, &[batch]);
        assert!(!result.is_empty(), "should detect conditional distribution");
        assert_eq!(result[0].given, "category");
        assert_eq!(result[0].dependent, "value");
        assert_eq!(result[0].branches.len(), 2);
    }

    #[test]
    fn cond_dist_skips_similar_groups() {
        // Two categories with identical distributions → CV ≈ 0 → no detection
        let mut cat = Vec::new();
        let mut vals = Vec::new();
        for _ in 0..50 {
            cat.push("A");
            vals.push(50.0);
        }
        for _ in 0..50 {
            cat.push("B");
            vals.push(50.0);
        }
        let batch = cond_batch(&cat, &vals);
        let profiles = vec![string_profile("category"), numeric_profile("value")];
        let result = detect_conditional_distributions(&profiles, &[batch]);
        assert!(
            result.is_empty(),
            "should NOT detect when groups are identical"
        );
    }

    #[test]
    fn cond_dist_empty_input() {
        let result = detect_conditional_distributions(&[], &[]);
        assert!(result.is_empty());
    }
}
