//! Correlation-preserving rank reordering (Iman-Conover method).
//!
//! Applies the Iman-Conover (1982) algorithm to reorder independently
//! generated column values so that their rank correlations match a target
//! correlation matrix.  Unlike inverse-CDF copula transforms, this method
//! preserves **exact** marginal distributions — only row positions change.
//!
//! For non-Gaussian copula families (Clayton, Frank, Gumbel) the legacy
//! inverse-CDF path is kept as a fallback for bivariate plans.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array};
use rand::RngExt;
use rand::rngs::ChaCha8Rng;
use statrs::distribution::{ContinuousCDF, LogNormal, Normal};

use crate::core::{CopulaFamily, DistributionKind};
use crate::plan::types::{CopulaPlan, MarginalInfo};

/// Generate a uniform f64 in [0, 1).
#[inline]
fn uniform01(rng: &mut ChaCha8Rng) -> f64 {
    rng.random::<f64>()
}

/// Apply copula plans to a batch, reordering independently generated columns
/// to induce target rank correlations (Iman-Conover) or, for non-Gaussian
/// families, replacing them via inverse-CDF transform.
pub fn apply_copula_plans(
    copula_plans: &[CopulaPlan],
    batch_columns: &mut HashMap<String, ArrayRef>,
    rng: &mut ChaCha8Rng,
    count: usize,
) {
    for plan in copula_plans {
        apply_single_copula(plan, batch_columns, rng, count);
    }
}

fn apply_single_copula(
    plan: &CopulaPlan,
    batch_columns: &mut HashMap<String, ArrayRef>,
    rng: &mut ChaCha8Rng,
    count: usize,
) {
    let n = plan.fields.len();
    if n == 0 || count == 0 {
        return;
    }

    // For Gaussian copula: use Iman-Conover rank reordering to preserve exact
    // marginal distributions while inducing target rank correlations.
    if plan.family == CopulaFamily::Gaussian {
        apply_iman_conover(plan, batch_columns, rng, count);
        return;
    }

    // Legacy path for non-Gaussian (Clayton/Frank/Gumbel): inverse CDF.
    let uniforms = match plan.family {
        CopulaFamily::Gaussian => unreachable!(),
        CopulaFamily::Clayton => generate_clayton_copula(plan, rng, count),
        CopulaFamily::Frank => generate_frank_copula(plan, rng, count),
        CopulaFamily::Gumbel => generate_gumbel_copula(plan, rng, count),
    };

    for (field_idx, field_name) in plan.fields.iter().enumerate() {
        let marginal = &plan.marginals[field_idx];
        let values: Vec<f64> = uniforms[field_idx]
            .iter()
            .map(|&u| inverse_cdf(u, marginal))
            .collect();

        let arr: ArrayRef = Arc::new(Float64Array::from(values));
        batch_columns.insert(field_name.clone(), arr);
    }
}

/// Iman-Conover rank reordering: reorder existing column values so that their
/// rank correlations approximate the target correlation matrix.
///
/// Algorithm:
/// 1. Read independently generated values from batch_columns.
/// 2. Generate correlated standard normals via Cholesky of target matrix.
/// 3. Compute the rank ordering of both the normals and original values.
/// 4. For each column, sort original values and assign them to positions
///    dictated by the correlated-normal ranks.
fn apply_iman_conover(
    plan: &CopulaPlan,
    batch_columns: &mut HashMap<String, ArrayRef>,
    rng: &mut ChaCha8Rng,
    count: usize,
) {
    let n = plan.fields.len();

    let chol = match &plan.cholesky_l {
        Some(l) => l,
        None => return, // No Cholesky → can't correlate, leave independent
    };

    // Step 1: Generate correlated standard normals via Cholesky
    let normal =
        Normal::new(0.0, 1.0).expect("standard normal distribution uses valid parameters");
    let mut correlated_normals = vec![vec![0.0f64; count]; n];

    for row_idx in 0..count {
        let z: Vec<f64> = (0..n).map(|_| normal.inverse_cdf(uniform01(rng))).collect();
        // x = L · z
        for i in 0..n {
            let mut x = 0.0;
            for j in 0..=i {
                x += chol[i][j] * z[j];
            }
            correlated_normals[i][row_idx] = x;
        }
    }

    // Step 2: For each field, compute target rank order from correlated normals
    // and reorder the independently generated values accordingly.
    for (field_idx, field_name) in plan.fields.iter().enumerate() {
        let col = match batch_columns.get(field_name) {
            Some(c) => c,
            None => continue,
        };

        // Extract current values as f64
        let orig_values: Vec<f64> = if let Some(f64_arr) =
            col.as_any().downcast_ref::<Float64Array>()
        {
            f64_arr.values().iter().copied().collect()
        } else if let Some(i64_arr) = col
            .as_any()
            .downcast_ref::<arrow::array::Int64Array>()
        {
            i64_arr.values().iter().map(|&v| v as f64).collect()
        } else if let Some(f32_arr) = col
            .as_any()
            .downcast_ref::<arrow::array::Float32Array>()
        {
            f32_arr.values().iter().map(|&v| v as f64).collect()
        } else if let Some(i32_arr) = col
            .as_any()
            .downcast_ref::<arrow::array::Int32Array>()
        {
            i32_arr.values().iter().map(|&v| v as f64).collect()
        } else {
            // Non-numeric column — skip
            continue;
        };

        if orig_values.len() != count {
            continue;
        }

        // Compute target rank order: argsort correlated normals for this field
        let target_order = argsort(&correlated_normals[field_idx]);

        // Sort original values
        let mut sorted_values = orig_values.clone();
        sorted_values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

        // Assign sorted values to target rank positions:
        // target_order[rank] = row_idx means the rank-th smallest value goes
        // to row target_order[rank].
        let mut reordered = vec![0.0f64; count];
        for (rank, &row_idx) in target_order.iter().enumerate() {
            reordered[row_idx] = sorted_values[rank];
        }

        let arr: ArrayRef = Arc::new(Float64Array::from(reordered));
        batch_columns.insert(field_name.clone(), arr);
    }
}

/// Return the indices that would sort the slice in ascending order.
/// argsort([30, 10, 20]) → [1, 2, 0] (index 1 is smallest, index 0 is largest)
fn argsort(data: &[f64]) -> Vec<usize> {
    let mut indices: Vec<usize> = (0..data.len()).collect();
    indices.sort_by(|&a, &b| {
        data[a]
            .partial_cmp(&data[b])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    indices
}

/// Generate bivariate Clayton copula samples.
/// Clayton(θ): C(u,v) = (u^{-θ} + v^{-θ} - 1)^{-1/θ}, θ > 0
fn generate_clayton_copula(plan: &CopulaPlan, rng: &mut ChaCha8Rng, count: usize) -> Vec<Vec<f64>> {
    let theta = plan.theta.unwrap_or(1.0);
    let mut u1 = Vec::with_capacity(count);
    let mut u2 = Vec::with_capacity(count);

    for _ in 0..count {
        let u: f64 = uniform01(rng);
        let t: f64 = uniform01(rng); // uniform for conditional inverse

        // Conditional inverse: u2 = (u1^{-θ} (t^{-θ/(1+θ)} - 1) + 1)^{-1/θ}
        let u_neg_theta = u.powf(-theta);
        let t_exp = t.powf(-theta / (1.0 + theta));
        let v = (u_neg_theta * (t_exp - 1.0) + 1.0).powf(-1.0 / theta);

        u1.push(u.clamp(1e-10, 1.0 - 1e-10));
        u2.push(v.clamp(1e-10, 1.0 - 1e-10));
    }

    vec![u1, u2]
}

/// Generate bivariate Frank copula samples.
/// Frank(θ): C(u,v) = -1/θ · ln(1 + (e^{-θu}-1)(e^{-θv}-1)/(e^{-θ}-1))
fn generate_frank_copula(plan: &CopulaPlan, rng: &mut ChaCha8Rng, count: usize) -> Vec<Vec<f64>> {
    let theta = plan.theta.unwrap_or(1.0);
    let mut u1 = Vec::with_capacity(count);
    let mut u2 = Vec::with_capacity(count);

    for _ in 0..count {
        let u: f64 = uniform01(rng);
        let t: f64 = uniform01(rng); // uniform for conditional inverse

        // Conditional inverse of Frank copula
        // v = -1/θ · ln(1 + t·(e^{-θ}-1) / (t·(e^{-θu}-1) - (e^{-θ}-1)))
        // Simplified: avoid overflow by computing in log space when needed
        let exp_neg_theta = (-theta).exp();
        let exp_neg_theta_u = (-theta * u).exp();

        let numerator = t * (exp_neg_theta - 1.0);
        let denominator = t * (exp_neg_theta_u - 1.0) - (exp_neg_theta - 1.0);

        let v = if denominator.abs() < 1e-15 {
            uniform01(rng) // degenerate case
        } else {
            (-1.0 / theta) * (1.0 + numerator / denominator).ln()
        };

        u1.push(u.clamp(1e-10, 1.0 - 1e-10));
        u2.push(v.clamp(1e-10, 1.0 - 1e-10));
    }

    vec![u1, u2]
}

/// Generate bivariate Gumbel copula samples.
/// Uses the Marshall-Olkin algorithm with stable distribution.
fn generate_gumbel_copula(plan: &CopulaPlan, rng: &mut ChaCha8Rng, count: usize) -> Vec<Vec<f64>> {
    let theta = plan.theta.unwrap_or(1.0);
    let mut u1_out = Vec::with_capacity(count);
    let mut u2_out = Vec::with_capacity(count);

    let alpha = 1.0 / theta;

    for _ in 0..count {
        // Generate a positive stable random variable with index alpha
        // Using Chambers-Mallows-Stuck algorithm
        let s = sample_positive_stable(alpha, rng);

        // Generate two independent exponentials
        let e1: f64 = -uniform01(rng).ln();
        let e2: f64 = -uniform01(rng).ln();

        // Gumbel copula via Marshall-Olkin:
        // u = exp(-(e1/s)^alpha), v = exp(-(e2/s)^alpha)
        // But alpha = 1/theta, so (e/s)^(1/theta)
        let u = (-(e1 / s).powf(alpha)).exp();
        let v = (-(e2 / s).powf(alpha)).exp();

        u1_out.push(u.clamp(1e-10, 1.0 - 1e-10));
        u2_out.push(v.clamp(1e-10, 1.0 - 1e-10));
    }

    vec![u1_out, u2_out]
}

/// Sample from positive stable distribution with index alpha ∈ (0,1].
/// Uses Chambers-Mallows-Stuck algorithm.
fn sample_positive_stable(alpha: f64, rng: &mut ChaCha8Rng) -> f64 {
    if (alpha - 1.0).abs() < 1e-10 {
        return 1.0; // Degenerate case: theta = 1 means independence
    }

    let u: f64 = uniform01(rng) * std::f64::consts::PI;
    let e: f64 = -uniform01(rng).ln(); // Exp(1)

    let t = alpha * u;
    let s = t.sin() / u.sin().powf(1.0 / alpha);
    let r = ((u - t).sin() / e).powf((1.0 - alpha) / alpha);

    s * r
}

/// Compute the inverse CDF (quantile function) for a marginal distribution.
fn inverse_cdf(u: f64, marginal: &MarginalInfo) -> f64 {
    // Clamp to avoid infinities at boundaries
    let u = u.clamp(1e-10, 1.0 - 1e-10);

    let val = match marginal.kind {
        DistributionKind::Normal => {
            let mean = marginal.params.get("mean").copied().unwrap_or(0.0);
            let std_dev = marginal.params.get("std_dev").copied().unwrap_or(1.0);
            let d = Normal::new(mean, std_dev).unwrap_or_else(|_| {
                Normal::new(0.0, 1.0).expect("fallback normal distribution uses valid parameters")
            });
            d.inverse_cdf(u)
        }
        DistributionKind::LogNormal => {
            let mu = marginal.params.get("mu").copied().unwrap_or(0.0);
            let sigma = marginal.params.get("sigma").copied().unwrap_or(1.0);
            let d = LogNormal::new(mu, sigma).unwrap_or_else(|_| {
                LogNormal::new(0.0, 1.0)
                    .expect("fallback log-normal distribution uses valid parameters")
            });
            d.inverse_cdf(u)
        }
        DistributionKind::Uniform => {
            let min = marginal.params.get("min").copied().unwrap_or(0.0);
            let max = marginal.params.get("max").copied().unwrap_or(1.0);
            min + u * (max - min)
        }
        DistributionKind::Exponential => {
            let lambda = marginal.params.get("lambda").copied().unwrap_or(1.0);
            -(1.0 - u).ln() / lambda
        }
        // For distributions without easy inverse CDF, use normal approximation
        _ => {
            let mean = marginal.params.get("mean").copied().unwrap_or(0.0);
            let std_dev = marginal.params.get("std_dev").copied().unwrap_or(1.0);
            let d = Normal::new(mean, std_dev.max(0.01)).unwrap_or_else(|_| {
                Normal::new(0.0, 1.0).expect("fallback normal approximation uses valid parameters")
            });
            d.inverse_cdf(u)
        }
    };

    if marginal.round { val.round() } else { val }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use std::collections::BTreeMap;

    #[test]
    fn test_iman_conover_preserves_marginals_and_induces_correlation() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let plan = CopulaPlan {
            fields: vec!["x".to_string(), "y".to_string()],
            family: CopulaFamily::Gaussian,
            cholesky_l: Some(vec![
                vec![1.0, 0.0],
                vec![0.8, 0.6], // target correlation = 0.8
            ]),
            theta: None,
            marginals: vec![
                MarginalInfo {
                    kind: DistributionKind::Normal,
                    params: {
                        let mut m = BTreeMap::new();
                        m.insert("mean".to_string(), 100.0);
                        m.insert("std_dev".to_string(), 15.0);
                        m
                    },
                    round: false,
                },
                MarginalInfo {
                    kind: DistributionKind::Normal,
                    params: {
                        let mut m = BTreeMap::new();
                        m.insert("mean".to_string(), 50.0);
                        m.insert("std_dev".to_string(), 10.0);
                        m
                    },
                    round: false,
                },
            ],
        };

        let n = 10_000;
        // Generate independent column values
        let normal_x =
            Normal::new(100.0, 15.0).expect("normal distribution uses valid parameters");
        let normal_y =
            Normal::new(50.0, 10.0).expect("normal distribution uses valid parameters");
        let x_vals: Vec<f64> = (0..n)
            .map(|_| normal_x.inverse_cdf(uniform01(&mut rng)))
            .collect();
        let y_vals: Vec<f64> = (0..n)
            .map(|_| normal_y.inverse_cdf(uniform01(&mut rng)))
            .collect();

        // Record original marginal stats
        let x_mean_orig: f64 = x_vals.iter().sum::<f64>() / n as f64;
        let y_mean_orig: f64 = y_vals.iter().sum::<f64>() / n as f64;
        let mut x_sorted = x_vals.clone();
        x_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut y_sorted = y_vals.clone();
        y_sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());

        let mut batch_columns = HashMap::new();
        batch_columns.insert(
            "x".to_string(),
            Arc::new(Float64Array::from(x_vals)) as ArrayRef,
        );
        batch_columns.insert(
            "y".to_string(),
            Arc::new(Float64Array::from(y_vals)) as ArrayRef,
        );

        apply_copula_plans(&[plan], &mut batch_columns, &mut rng, n);

        let x = batch_columns["x"]
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let y = batch_columns["y"]
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();

        // Check marginals preserved: same sorted values (Iman-Conover only reorders)
        let mut x_after: Vec<f64> = x.values().iter().copied().collect();
        x_after.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mut y_after: Vec<f64> = y.values().iter().copied().collect();
        y_after.sort_by(|a, b| a.partial_cmp(b).unwrap());

        for i in 0..n {
            assert!(
                (x_sorted[i] - x_after[i]).abs() < 1e-10,
                "Iman-Conover should preserve exact marginal values for x"
            );
            assert!(
                (y_sorted[i] - y_after[i]).abs() < 1e-10,
                "Iman-Conover should preserve exact marginal values for y"
            );
        }

        // Check correlation induced
        let n_f = n as f64;
        let x_mean: f64 = x.values().iter().sum::<f64>() / n_f;
        let y_mean: f64 = y.values().iter().sum::<f64>() / n_f;
        let mut cov = 0.0;
        let mut var_x = 0.0;
        let mut var_y = 0.0;
        for i in 0..n {
            let dx = x.value(i) - x_mean;
            let dy = y.value(i) - y_mean;
            cov += dx * dy;
            var_x += dx * dx;
            var_y += dy * dy;
        }
        let r = cov / (var_x.sqrt() * var_y.sqrt());

        // Iman-Conover should produce rank correlation close to target (0.8)
        assert!(
            (r - 0.8).abs() < 0.1,
            "Iman-Conover should induce correlation ~0.8, got {r}"
        );
    }

    #[test]
    fn test_clayton_copula_positive_dependence() {
        use rand::SeedableRng;
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let plan = CopulaPlan {
            fields: vec!["a".to_string(), "b".to_string()],
            family: CopulaFamily::Clayton,
            cholesky_l: None,
            theta: Some(5.0), // strong lower-tail dependence
            marginals: vec![
                MarginalInfo {
                    kind: DistributionKind::Uniform,
                    params: {
                        let mut m = BTreeMap::new();
                        m.insert("min".to_string(), 0.0);
                        m.insert("max".to_string(), 1.0);
                        m
                    },
                    round: false,
                },
                MarginalInfo {
                    kind: DistributionKind::Uniform,
                    params: {
                        let mut m = BTreeMap::new();
                        m.insert("min".to_string(), 0.0);
                        m.insert("max".to_string(), 1.0);
                        m
                    },
                    round: false,
                },
            ],
        };

        let n = 5_000;
        let mut batch_columns = HashMap::new();
        batch_columns.insert(
            "a".to_string(),
            Arc::new(Float64Array::from(vec![0.0; n])) as ArrayRef,
        );
        batch_columns.insert(
            "b".to_string(),
            Arc::new(Float64Array::from(vec![0.0; n])) as ArrayRef,
        );

        apply_copula_plans(&[plan], &mut batch_columns, &mut rng, n);

        let a = batch_columns["a"]
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let b = batch_columns["b"]
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();

        // Compute rank correlation (Spearman) — should be positive
        let n_f = n as f64;
        let a_mean: f64 = a.values().iter().sum::<f64>() / n_f;
        let b_mean: f64 = b.values().iter().sum::<f64>() / n_f;
        let mut cov = 0.0;
        let mut var_a = 0.0;
        let mut var_b = 0.0;
        for i in 0..n {
            let da = a.value(i) - a_mean;
            let db = b.value(i) - b_mean;
            cov += da * db;
            var_a += da * da;
            var_b += db * db;
        }
        let r = cov / (var_a.sqrt() * var_b.sqrt());

        assert!(
            r > 0.3,
            "Clayton copula with theta=5 should show positive correlation, got {r}"
        );
    }

    #[test]
    fn test_inverse_cdf_normal() {
        let m = MarginalInfo {
            kind: DistributionKind::Normal,
            params: {
                let mut p = BTreeMap::new();
                p.insert("mean".to_string(), 100.0);
                p.insert("std_dev".to_string(), 15.0);
                p
            },
            round: false,
        };
        let val = inverse_cdf(0.5, &m);
        assert!(
            (val - 100.0).abs() < 0.01,
            "median of Normal(100,15) should be 100, got {val}"
        );
    }

    #[test]
    fn test_inverse_cdf_uniform() {
        let m = MarginalInfo {
            kind: DistributionKind::Uniform,
            params: {
                let mut p = BTreeMap::new();
                p.insert("min".to_string(), 10.0);
                p.insert("max".to_string(), 20.0);
                p
            },
            round: false,
        };
        let val = inverse_cdf(0.5, &m);
        assert!(
            (val - 15.0).abs() < 0.01,
            "median of Uniform(10,20) should be 15, got {val}"
        );
    }
}
