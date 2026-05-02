//! Distribution fitting — fit statistical distributions to observed data.
//!
//! Supports MLE-based fitting for common distributions (normal, log-normal,
//! exponential, uniform, Poisson, Zipf, beta, gamma, Pareto) with KS-test
//! goodness-of-fit scoring and AIC/BIC model selection.

use std::collections::HashMap;
use std::f64::consts::{E, PI};

use statrs::distribution::{Beta, ContinuousCDF, Exp, Gamma, LogNormal, Normal, Uniform};
use tracing::{debug, warn};

/// A named distribution with its fitted parameters.
#[derive(Debug, Clone)]
pub enum Distribution {
    /// Uniform(min, max)
    Uniform(f64, f64),
    /// Normal(mean, std_dev)
    Normal(f64, f64),
    /// LogNormal(mu, sigma) — parameters of the underlying normal
    LogNormal(f64, f64),
    /// Exponential(lambda)
    Exponential(f64),
    /// Poisson(lambda)
    Poisson(f64),
    /// Zipf(n, s)
    Zipf(u64, f64),
    /// Beta(alpha, beta)
    Beta(f64, f64),
    /// Gamma(shape, rate)
    Gamma(f64, f64),
    /// Pareto(x_m, alpha)
    Pareto(f64, f64),
}

impl Distribution {
    /// Human-readable name of the distribution.
    pub fn name(&self) -> &'static str {
        match self {
            Distribution::Uniform(..) => "uniform",
            Distribution::Normal(..) => "normal",
            Distribution::LogNormal(..) => "log_normal",
            Distribution::Exponential(..) => "exponential",
            Distribution::Poisson(..) => "poisson",
            Distribution::Zipf(..) => "zipf",
            Distribution::Beta(..) => "beta",
            Distribution::Gamma(..) => "gamma",
            Distribution::Pareto(..) => "pareto",
        }
    }

    /// Number of free parameters (for AIC/BIC).
    pub fn k(&self) -> usize {
        match self {
            Distribution::Exponential(_) | Distribution::Poisson(_) => 1,
            _ => 2,
        }
    }
}

/// A single candidate fit result.
#[derive(Debug, Clone)]
pub struct CandidateFit {
    /// The fitted distribution.
    pub distribution: Distribution,
    /// KS-test statistic (lower is better).
    pub ks_stat: f64,
    /// Approximate p-value from KS-test.
    pub p_value: f64,
    /// Akaike Information Criterion.
    pub aic: f64,
    /// Bayesian Information Criterion.
    pub bic: f64,
}

/// Result of distribution fitting.
#[derive(Debug, Clone)]
pub struct FitResult {
    /// Best-fitting distribution (lowest AIC).
    pub best: CandidateFit,
    /// All candidates, ranked by AIC ascending.
    pub alternatives: Vec<CandidateFit>,
}

/// Result of categorical frequency analysis.
#[derive(Debug, Clone)]
pub struct CategoricalFit {
    /// Category → relative frequency weight.
    pub weights: HashMap<String, f64>,
    /// Number of distinct categories.
    pub cardinality: usize,
}

/// Fit distributions to a slice of f64 values, returning ranked candidates.
///
/// Filters out NaN and Infinity values. Returns `None` if fewer than 2 valid
/// values remain.
pub fn fit_distribution(values: &[f64]) -> Option<FitResult> {
    let mut clean: Vec<f64> = values
        .iter()
        .copied()
        .filter(|v| v.is_finite())
        .collect();

    if clean.len() < 2 {
        warn!("fit_distribution: fewer than 2 finite values");
        return None;
    }

    clean.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = clean.len();

    let mean = clean.iter().sum::<f64>() / n as f64;
    // MLE variance uses /n (not /(n-1) which is sample variance)
    let var = clean.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
    let std_dev = var.sqrt();
    let min_val = clean[0];
    let max_val = clean[n - 1];

    debug!(n, mean, std_dev, min_val, max_val, "fitting distributions");

    let mut candidates: Vec<CandidateFit> = Vec::new();

    // Uniform
    if max_val > min_val {
        let dist = Uniform::new(min_val, max_val).ok();
        if let Some(d) = dist {
            let ks = ks_stat_continuous(&clean, |x| d.cdf(x));
            let ll = -(n as f64) * (max_val - min_val).ln();
            push_candidate(&mut candidates, Distribution::Uniform(min_val, max_val), ks, ll, n);
        }
    }

    // Normal
    if std_dev > 0.0 {
        if let Some(d) = Normal::new(mean, std_dev).ok() {
            let ks = ks_stat_continuous(&clean, |x| d.cdf(x));
            let ll = normal_log_likelihood(&clean, mean, std_dev);
            push_candidate(&mut candidates, Distribution::Normal(mean, std_dev), ks, ll, n);
        }
    }

    // LogNormal — only for strictly positive values
    if min_val > 0.0 {
        let log_vals: Vec<f64> = clean.iter().map(|v| v.ln()).collect();
        let mu = log_vals.iter().sum::<f64>() / n as f64;
        // MLE variance uses /n
        let sigma2 = log_vals.iter().map(|v| (v - mu).powi(2)).sum::<f64>() / n as f64;
        let sigma = sigma2.sqrt();
        if sigma > 0.0 {
            if let Some(d) = LogNormal::new(mu, sigma).ok() {
                let ks = ks_stat_continuous(&clean, |x| d.cdf(x));
                let ll: f64 = clean
                    .iter()
                    .map(|&x| {
                        -x.ln() - 0.5 * ((x.ln() - mu) / sigma).powi(2)
                            - sigma.ln()
                            - (2.0 * PI).sqrt().ln()
                    })
                    .sum();
                push_candidate(&mut candidates, Distribution::LogNormal(mu, sigma), ks, ll, n);
            }
        }
    }

    // Exponential — positive values only
    if min_val >= 0.0 && mean > 0.0 {
        let lambda = 1.0 / mean;
        if let Some(d) = Exp::new(lambda).ok() {
            let ks = ks_stat_continuous(&clean, |x| d.cdf(x));
            let ll = n as f64 * lambda.ln() - lambda * clean.iter().sum::<f64>();
            push_candidate(&mut candidates, Distribution::Exponential(lambda), ks, ll, n);
        }
    }

    // Gamma — positive values only
    if min_val > 0.0 && var > 0.0 {
        let shape = mean * mean / var;
        let rate = mean / var;
        if shape > 0.0 && rate > 0.0 {
            if let Some(d) = Gamma::new(shape, 1.0 / rate).ok() {
                let ks = ks_stat_continuous(&clean, |x| d.cdf(x));
                let ll = gamma_log_likelihood(&clean, shape, rate);
                push_candidate(&mut candidates, Distribution::Gamma(shape, rate), ks, ll, n);
            }
        }
    }

    // Beta — values in (0,1)
    if min_val > 0.0 && max_val < 1.0 {
        let m = mean;
        let v = var;
        let common = m * (1.0 - m) / v - 1.0;
        if common > 0.0 {
            let alpha = m * common;
            let beta_param = (1.0 - m) * common;
            if alpha > 0.0 && beta_param > 0.0 {
                if let Some(d) = Beta::new(alpha, beta_param).ok() {
                    let ks = ks_stat_continuous(&clean, |x| d.cdf(x));
                    let ll = beta_log_likelihood(&clean, alpha, beta_param);
                    push_candidate(
                        &mut candidates,
                        Distribution::Beta(alpha, beta_param),
                        ks,
                        ll,
                        n,
                    );
                }
            }
        }
    }

    // Pareto — positive values with min > 0
    if min_val > 0.0 {
        let x_m = min_val;
        let alpha_hat = n as f64 / clean.iter().map(|v| (v / x_m).ln()).sum::<f64>();
        if alpha_hat > 0.0 && alpha_hat.is_finite() {
            let ks = ks_stat_continuous(&clean, |x| {
                if x < x_m {
                    0.0
                } else {
                    1.0 - (x_m / x).powf(alpha_hat)
                }
            });
            let ll = n as f64 * alpha_hat.ln() + n as f64 * alpha_hat * x_m.ln()
                - (alpha_hat + 1.0) * clean.iter().map(|v| v.ln()).sum::<f64>();
            push_candidate(&mut candidates, Distribution::Pareto(x_m, alpha_hat), ks, ll, n);
        }
    }

    // Poisson — non-negative integer-like values
    if min_val >= 0.0 && mean > 0.0 {
        // Check if values are approximately integer-valued
        let is_integer_like = clean.iter().all(|v| (v - v.round()).abs() < 1e-6);
        if is_integer_like {
            let lambda = mean; // MLE for Poisson is sample mean
            // Compute KS using Poisson CDF approximation (normal approximation for large lambda)
            let ks = ks_stat_continuous(&clean, |x| {
                // Use normal approximation to Poisson CDF
                if lambda > 0.0 {
                    let z = (x + 0.5 - lambda) / lambda.sqrt();
                    normal_cdf(z)
                } else {
                    1.0
                }
            });
            // Poisson log-likelihood: sum(x_i * ln(lambda) - lambda - ln(x_i!))
            let ll = clean.iter().map(|&x| {
                x * lambda.ln() - lambda - ln_gamma(x + 1.0)
            }).sum::<f64>();
            push_candidate(&mut candidates, Distribution::Poisson(lambda), ks, ll, n);
        }
    }

    // Zipf — positive integer-valued data with heavy tail
    if min_val >= 1.0 && mean > 0.0 {
        let is_integer_like = clean.iter().all(|v| (v - v.round()).abs() < 1e-6);
        if is_integer_like {
            let max_rank = max_val as u64;
            if max_rank >= 2 && max_rank <= 100_000 {
                // MLE for Zipf: solve numerically via Newton's method
                // s_hat ≈ 1 + n / (sum(ln(x_i)))  (approximate)
                let sum_ln: f64 = clean.iter().map(|v| v.ln()).sum();
                if sum_ln > 0.0 {
                    let s_hat = 1.0 + n as f64 / sum_ln;
                    if s_hat > 1.0 && s_hat.is_finite() && s_hat < 10.0 {
                        // Zipf log-likelihood: -s * sum(ln(x_i)) - n * ln(H(N,s))
                        let h_n_s: f64 = (1..=max_rank).map(|k| (k as f64).powf(-s_hat)).sum();
                        if h_n_s > 0.0 {
                            let ll = -s_hat * sum_ln - n as f64 * h_n_s.ln();
                            // KS with Zipf CDF
                            let ks = ks_stat_continuous(&clean, |x| {
                                let k = x.floor() as u64;
                                if k < 1 { return 0.0; }
                                let k = k.min(max_rank);
                                let partial: f64 = (1..=k).map(|i| (i as f64).powf(-s_hat)).sum();
                                (partial / h_n_s).clamp(0.0, 1.0)
                            });
                            push_candidate(&mut candidates, Distribution::Zipf(max_rank, s_hat), ks, ll, n);
                        }
                    }
                }
            }
        }
    }

    if candidates.is_empty() {
        return None;
    }

    candidates.sort_by(|a, b| a.aic.partial_cmp(&b.aic).unwrap_or(std::cmp::Ordering::Equal));

    let best = candidates[0].clone();
    debug!(best_dist = best.distribution.name(), ks = best.ks_stat, aic = best.aic, "best fit");

    Some(FitResult {
        best: candidates[0].clone(),
        alternatives: candidates,
    })
}

/// Compute frequency weights for a categorical column.
pub fn fit_categorical(values: &[String]) -> CategoricalFit {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for v in values {
        *counts.entry(v.clone()).or_insert(0) += 1;
    }
    let total = values.len() as f64;
    let weights: HashMap<String, f64> = counts
        .iter()
        .map(|(k, &v)| (k.clone(), v as f64 / total))
        .collect();
    let cardinality = weights.len();
    debug!(cardinality, "categorical fit");
    CategoricalFit {
        weights,
        cardinality,
    }
}

// ─── internal helpers ───────────────────────────────────────────────────────

fn push_candidate(
    candidates: &mut Vec<CandidateFit>,
    dist: Distribution,
    ks: f64,
    log_likelihood: f64,
    n: usize,
) {
    let k = dist.k() as f64;
    let nf = n as f64;
    let aic = 2.0 * k - 2.0 * log_likelihood;
    let bic = k * nf.ln() - 2.0 * log_likelihood;
    let p_value = ks_p_value(ks, n);
    candidates.push(CandidateFit {
        distribution: dist,
        ks_stat: ks,
        p_value,
        aic,
        bic,
    });
}

/// Kolmogorov–Smirnov statistic for a sorted sample against a CDF.
fn ks_stat_continuous<F: Fn(f64) -> f64>(sorted: &[f64], cdf: F) -> f64 {
    let n = sorted.len() as f64;
    let mut max_d = 0.0_f64;
    for (i, &x) in sorted.iter().enumerate() {
        let ecdf = (i + 1) as f64 / n;
        let ecdf_prev = i as f64 / n;
        let f_x = cdf(x).clamp(0.0, 1.0);
        let d = (ecdf - f_x).abs().max((ecdf_prev - f_x).abs());
        if d > max_d {
            max_d = d;
        }
    }
    max_d
}

/// Approximate KS-test p-value using the asymptotic formula.
fn ks_p_value(d: f64, n: usize) -> f64 {
    let sqrt_n = (n as f64).sqrt();
    let lambda = (sqrt_n + 0.12 + 0.11 / sqrt_n) * d;
    if lambda <= 0.0 {
        return 1.0;
    }
    // Kolmogorov's limiting distribution (first few terms)
    let mut p = 0.0;
    for j in 1..=100 {
        let sign = if j % 2 == 0 { -1.0 } else { 1.0 };
        let term = sign * (-2.0 * (j as f64 * lambda).powi(2)).exp();
        p += term;
    }
    (2.0 * p).clamp(0.0, 1.0)
}

fn normal_log_likelihood(data: &[f64], mean: f64, std_dev: f64) -> f64 {
    let n = data.len() as f64;
    -0.5 * n * (2.0 * PI * std_dev * std_dev).ln()
        - data
            .iter()
            .map(|x| (x - mean).powi(2) / (2.0 * std_dev * std_dev))
            .sum::<f64>()
}

fn gamma_log_likelihood(data: &[f64], shape: f64, rate: f64) -> f64 {
    let n = data.len() as f64;
    n * (shape * rate.ln() - ln_gamma(shape))
        + (shape - 1.0) * data.iter().map(|x| x.ln()).sum::<f64>()
        - rate * data.iter().sum::<f64>()
}

fn beta_log_likelihood(data: &[f64], alpha: f64, beta: f64) -> f64 {
    let n = data.len() as f64;
    n * (ln_gamma(alpha + beta) - ln_gamma(alpha) - ln_gamma(beta))
        + (alpha - 1.0) * data.iter().map(|x| x.ln()).sum::<f64>()
        + (beta - 1.0) * data.iter().map(|x| (1.0 - x).ln()).sum::<f64>()
}

/// Stirling's approximation for ln(Gamma(x)).
fn ln_gamma(x: f64) -> f64 {
    statrs::function::gamma::ln_gamma(x)
}

/// Standard normal CDF (used for Poisson approximation).
fn normal_cdf(x: f64) -> f64 {
    0.5 * (1.0 + erf(x / std::f64::consts::SQRT_2))
}

/// Error function approximation (Abramowitz & Stegun).
fn erf(x: f64) -> f64 {
    let a1 = 0.254829592;
    let a2 = -0.284496736;
    let a3 = 1.421413741;
    let a4 = -1.453152027;
    let a5 = 1.061405429;
    let p = 0.3275911;
    let sign = if x < 0.0 { -1.0 } else { 1.0 };
    let x = x.abs();
    let t = 1.0 / (1.0 + p * x);
    let y = 1.0 - (((((a5 * t + a4) * t) + a3) * t + a2) * t + a1) * t * (-x * x).exp();
    sign * y
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::rngs::StdRng;
    use rand::SeedableRng;
    use rand_distr::{Distribution as RandDist, Exp as ExpDist, LogNormal as LnDist, Normal as NormDist};

    #[test]
    fn fit_normal_distribution() {
        let mut rng = StdRng::seed_from_u64(42);
        let dist = NormDist::new(50.0, 10.0).unwrap();
        let samples: Vec<f64> = (0..1000).map(|_| dist.sample(&mut rng)).collect();
        let result = fit_distribution(&samples).unwrap();
        // Should identify normal or similar
        let names: Vec<&str> = result.alternatives.iter().map(|c| c.distribution.name()).collect();
        assert!(names.contains(&"normal"), "normal should be a candidate");
        // The best fit params should be close to (50, 10)
        if let Distribution::Normal(m, s) = &result.best.distribution {
            assert!((m - 50.0).abs() < 3.0, "mean should be close to 50");
            assert!((s - 10.0).abs() < 3.0, "std should be close to 10");
        }
    }

    #[test]
    fn fit_exponential_distribution() {
        let mut rng = StdRng::seed_from_u64(99);
        let dist = ExpDist::new(2.0).unwrap();
        let samples: Vec<f64> = (0..500).map(|_| dist.sample(&mut rng)).collect();
        let result = fit_distribution(&samples).unwrap();
        let names: Vec<&str> = result.alternatives.iter().map(|c| c.distribution.name()).collect();
        assert!(names.contains(&"exponential"));
    }

    #[test]
    fn fit_lognormal_distribution() {
        let mut rng = StdRng::seed_from_u64(7);
        let dist = LnDist::new(0.0, 0.5).unwrap();
        let samples: Vec<f64> = (0..500).map(|_| dist.sample(&mut rng)).collect();
        let result = fit_distribution(&samples).unwrap();
        let names: Vec<&str> = result.alternatives.iter().map(|c| c.distribution.name()).collect();
        assert!(names.contains(&"log_normal"));
    }

    #[test]
    fn fit_empty_returns_none() {
        assert!(fit_distribution(&[]).is_none());
    }

    #[test]
    fn fit_single_value_returns_none() {
        assert!(fit_distribution(&[5.0]).is_none());
    }

    #[test]
    fn fit_with_nans_filtered() {
        let vals = vec![1.0, 2.0, f64::NAN, 3.0, f64::INFINITY, 4.0, 5.0];
        let result = fit_distribution(&vals);
        assert!(result.is_some());
    }

    #[test]
    fn categorical_fit_weights() {
        let vals: Vec<String> = vec!["a", "b", "a", "a", "b", "c"]
            .into_iter()
            .map(String::from)
            .collect();
        let fit = fit_categorical(&vals);
        assert_eq!(fit.cardinality, 3);
        assert!((fit.weights["a"] - 0.5).abs() < 0.01);
        assert!((fit.weights["b"] - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn ks_p_value_zero_d_is_one() {
        assert!((ks_p_value(0.0, 100) - 1.0).abs() < 0.01);
    }
}
