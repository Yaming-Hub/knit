//! Distribution-based generator supporting 17 statistical distributions.
//!
//! Covers continuous (Uniform, Normal, LogNormal, Exponential, etc.), discrete
//! (Poisson, Bernoulli, Binomial, Geometric, Zipf), and shape-parameterised
//! (Pareto, Weibull, Gamma, Beta, Cauchy, ChiSquared, StudentT, Triangular)
//! families.
//!
//! Invalid user-supplied parameters (negative std_dev, zero lambda) are handled
//! gracefully — the generator logs a warning via `tracing` and falls back to
//! safe defaults rather than panicking.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use rand::RngCore;
use rand_distr::Distribution;

use knit_core::DistributionKind;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Generate values drawn from a configurable statistical distribution.
///
/// Created by [`create_generator`](crate::create_generator) when the plan
/// contains a [`GeneratorPlan::Distribution`](knit_plan::GeneratorPlan::Distribution).
///
/// # Parameters
///
/// Distribution parameters are stored in a `BTreeMap<String, f64>`:
/// - **Uniform**: `min`, `max`
/// - **Normal**: `mean`, `std_dev`
/// - **LogNormal**: `mu`, `sigma`
/// - **Exponential**: `lambda`
/// - **Poisson**: `lambda`
/// - **Bernoulli**: `p`
/// - **Binomial**: `n`, `p`
/// - **Geometric**: `p`
/// - **Pareto**: `scale`, `shape`
/// - **Weibull**: `scale`, `shape`
/// - **Gamma**: `shape`, `scale`
/// - **Beta**: `alpha`, `beta`
/// - **Cauchy**: `median`, `scale`
/// - **ChiSquared**: `k`
/// - **StudentT**: `n`
/// - **Triangular**: `min`, `max`, `mode`
/// - **Zipf**: `n`, `s`
///
/// Optional `clamp_min` / `clamp_max` bounds are applied after sampling to
/// truncate extreme values (useful for ensuring realistic ranges).
pub struct DistributionGenerator {
    kind: DistributionKind,
    params: BTreeMap<String, f64>,
    clamp_min: Option<f64>,
    clamp_max: Option<f64>,
}

impl DistributionGenerator {
    /// Create a new distribution generator.
    pub fn new(
        kind: DistributionKind,
        params: BTreeMap<String, f64>,
        clamp_min: Option<f64>,
        clamp_max: Option<f64>,
    ) -> Self {
        Self {
            kind,
            params,
            clamp_min,
            clamp_max,
        }
    }

    /// Helper to get a named parameter, with a default fallback.
    fn param(&self, name: &str, default: f64) -> f64 {
        self.params.get(name).copied().unwrap_or(default)
    }

    /// Clamp a value to the configured bounds.
    fn clamp(&self, v: f64) -> f64 {
        let v = match self.clamp_min {
            Some(lo) => v.max(lo),
            None => v,
        };
        match self.clamp_max {
            Some(hi) => v.min(hi),
            None => v,
        }
    }
}

impl FieldGenerator for DistributionGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        match self.kind {
            DistributionKind::Uniform => {
                let lo = self.param("min", 0.0);
                let hi = self.param("max", 1.0);
                let (lo, hi) = if lo >= hi {
                    tracing::warn!(lo, hi, "Uniform min >= max, swapping to fallback (0,1)");
                    (0.0, 1.0)
                } else {
                    (lo, hi)
                };
                let dist = rand::distributions::Uniform::new(lo, hi);
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::Normal => {
                let mean = self.param("mean", 0.0);
                let std_dev = self.param("std_dev", 1.0).abs().max(f64::EPSILON);
                let dist =
                    rand_distr::Normal::new(mean, std_dev).unwrap_or_else(|_| {
                        tracing::warn!(mean, std_dev, "invalid Normal params, falling back to N(0,1)");
                        rand_distr::Normal::new(0.0, 1.0).unwrap()
                    });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::LogNormal => {
                let mu = self.param("mu", 0.0);
                let sigma = self.param("sigma", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::LogNormal::new(mu, sigma)
                    .unwrap_or_else(|_| {
                        tracing::warn!(mu, sigma, "invalid LogNormal params, falling back to LN(0,1)");
                        rand_distr::LogNormal::new(0.0, 1.0).unwrap()
                    });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::Exponential => {
                let lambda = self.param("lambda", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Exp::new(lambda).unwrap_or_else(|_| {
                    tracing::warn!(lambda, "invalid Exponential params, falling back to Exp(1)");
                    rand_distr::Exp::new(1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::Poisson => {
                let lambda = self.param("lambda", 1.0).abs().max(f64::EPSILON);
                let dist =
                    rand_distr::Poisson::new(lambda).unwrap_or_else(|_| {
                        tracing::warn!(lambda, "invalid Poisson params, falling back to Poisson(1)");
                        rand_distr::Poisson::new(1.0).unwrap()
                    });
                let values: Vec<i64> = (0..count)
                    .map(|_| {
                        let v: f64 = dist.sample(rng);
                        let v = self.clamp(v);
                        v as i64
                    })
                    .collect();
                Arc::new(Int64Array::from(values))
            }
            DistributionKind::Bernoulli => {
                let p = self.param("p", 0.5).clamp(0.0, 1.0);
                let dist = rand_distr::Bernoulli::new(p).unwrap_or_else(|_| {
                    tracing::warn!(p, "invalid Bernoulli params, falling back to p=0.5");
                    rand_distr::Bernoulli::new(0.5).unwrap()
                });
                let values: Vec<i64> = (0..count)
                    .map(|_| if dist.sample(rng) { 1 } else { 0 })
                    .collect();
                Arc::new(Int64Array::from(values))
            }
            DistributionKind::Binomial => {
                let n = self.param("n", 10.0).max(0.0) as u64;
                let p = self.param("p", 0.5).clamp(0.0, 1.0);
                let dist = rand_distr::Binomial::new(n, p).unwrap_or_else(|_| {
                    tracing::warn!(n, p, "invalid Binomial params, falling back to B(10,0.5)");
                    rand_distr::Binomial::new(10, 0.5).unwrap()
                });
                let values: Vec<i64> = (0..count)
                    .map(|_| {
                        let v = dist.sample(rng) as f64;
                        self.clamp(v) as i64
                    })
                    .collect();
                Arc::new(Int64Array::from(values))
            }
            DistributionKind::Geometric => {
                let p = self.param("p", 0.5).clamp(f64::EPSILON, 1.0);
                let dist = rand_distr::Geometric::new(p).unwrap_or_else(|_| {
                    tracing::warn!(p, "invalid Geometric params, falling back to p=0.5");
                    rand_distr::Geometric::new(0.5).unwrap()
                });
                let values: Vec<i64> = (0..count)
                    .map(|_| {
                        let v = dist.sample(rng) as f64;
                        self.clamp(v) as i64
                    })
                    .collect();
                Arc::new(Int64Array::from(values))
            }
            DistributionKind::Pareto => {
                let scale = self.param("scale", 1.0).abs().max(f64::EPSILON);
                let shape = self.param("shape", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Pareto::new(scale, shape).unwrap_or_else(|_| {
                    tracing::warn!(scale, shape, "invalid Pareto params, falling back to Pareto(1,1)");
                    rand_distr::Pareto::new(1.0, 1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::Weibull => {
                let scale = self.param("scale", 1.0).abs().max(f64::EPSILON);
                let shape = self.param("shape", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Weibull::new(scale, shape).unwrap_or_else(|_| {
                    tracing::warn!(scale, shape, "invalid Weibull params, falling back to Weibull(1,1)");
                    rand_distr::Weibull::new(1.0, 1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::Gamma => {
                let shape = self.param("shape", 1.0).abs().max(f64::EPSILON);
                let scale = self.param("scale", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Gamma::new(shape, scale).unwrap_or_else(|_| {
                    tracing::warn!(shape, scale, "invalid Gamma params, falling back to Gamma(1,1)");
                    rand_distr::Gamma::new(1.0, 1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::Beta => {
                let alpha = self.param("alpha", 2.0).abs().max(f64::EPSILON);
                let beta = self.param("beta", 2.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Beta::new(alpha, beta).unwrap_or_else(|_| {
                    tracing::warn!(alpha, beta, "invalid Beta params, falling back to Beta(2,2)");
                    rand_distr::Beta::new(2.0, 2.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::Cauchy => {
                let median = self.param("median", 0.0);
                let scale = self.param("scale", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Cauchy::new(median, scale).unwrap_or_else(|_| {
                    tracing::warn!(median, scale, "invalid Cauchy params, falling back to Cauchy(0,1)");
                    rand_distr::Cauchy::new(0.0, 1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::ChiSquared => {
                let k = self.param("k", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::ChiSquared::new(k).unwrap_or_else(|_| {
                    tracing::warn!(k, "invalid ChiSquared params, falling back to ChiSquared(1)");
                    rand_distr::ChiSquared::new(1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::StudentT => {
                let n = self.param("n", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::StudentT::new(n).unwrap_or_else(|_| {
                    tracing::warn!(n, "invalid StudentT params, falling back to StudentT(1)");
                    rand_distr::StudentT::new(1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::Triangular => {
                let min = self.param("min", 0.0);
                let max = self.param("max", 1.0);
                let mode = self.param("mode", (min + max) / 2.0);
                let (min, max) = if min >= max { (0.0, 1.0) } else { (min, max) };
                let mode = mode.clamp(min, max);
                let dist = rand_distr::Triangular::new(min, max, mode).unwrap_or_else(|_| {
                    tracing::warn!(min, max, mode, "invalid Triangular params, falling back to Tri(0,1,0.5)");
                    rand_distr::Triangular::new(0.0, 1.0, 0.5).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                Arc::new(Float64Array::from(values))
            }
            DistributionKind::Zipf => {
                let n = self.param("n", 100.0).max(1.0) as u64;
                let s = self.param("s", 1.0).max(f64::EPSILON);
                let dist = rand_distr::Zipf::new(n, s).unwrap_or_else(|_| {
                    tracing::warn!(n, s, "invalid Zipf params, falling back to Zipf(100,1)");
                    rand_distr::Zipf::new(100, 1.0).unwrap()
                });
                let values: Vec<i64> = (0..count).map(|_| {
                    let v: f64 = dist.sample(rng);
                    self.clamp(v) as i64
                }).collect();
                Arc::new(Int64Array::from(values))
            }
        }
    }

    fn output_type(&self) -> DataType {
        match self.kind {
            DistributionKind::Poisson
            | DistributionKind::Bernoulli
            | DistributionKind::Binomial
            | DistributionKind::Geometric
            | DistributionKind::Zipf => DataType::Int64,
            _ => DataType::Float64,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, 0, 0, 1, "test")
    }

    fn gen_f64(kind: DistributionKind, params: &[(&str, f64)], count: usize) -> Vec<f64> {
        let p: BTreeMap<String, f64> = params.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let g = DistributionGenerator::new(kind, p, None, None);
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = g.generate(&mut rng, count, &ctx);
        let fa = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        (0..fa.len()).map(|i| fa.value(i)).collect()
    }

    fn gen_i64(kind: DistributionKind, params: &[(&str, f64)], count: usize) -> Vec<i64> {
        let p: BTreeMap<String, f64> = params.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let g = DistributionGenerator::new(kind, p, None, None);
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = g.generate(&mut rng, count, &ctx);
        let ia = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        (0..ia.len()).map(|i| ia.value(i)).collect()
    }

    fn gen_clamped(
        kind: DistributionKind,
        params: &[(&str, f64)],
        clamp_min: Option<f64>,
        clamp_max: Option<f64>,
        count: usize,
    ) -> Vec<f64> {
        let p: BTreeMap<String, f64> = params.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let g = DistributionGenerator::new(kind, p, clamp_min, clamp_max);
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = g.generate(&mut rng, count, &ctx);
        let fa = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        (0..fa.len()).map(|i| fa.value(i)).collect()
    }

    #[test]
    fn uniform_in_range() {
        let vals = gen_f64(DistributionKind::Uniform, &[("min", 10.0), ("max", 20.0)], 500);
        assert_eq!(vals.len(), 500);
        for v in &vals {
            assert!(*v >= 10.0 && *v < 20.0, "uniform value out of range: {v}");
        }
    }

    #[test]
    fn uniform_invalid_params_fallback() {
        // min >= max should fallback to (0, 1)
        let vals = gen_f64(DistributionKind::Uniform, &[("min", 5.0), ("max", 5.0)], 100);
        for v in &vals {
            assert!(*v >= 0.0 && *v < 1.0, "fallback uniform out of (0,1): {v}");
        }
    }

    #[test]
    fn normal_produces_float64() {
        let vals = gen_f64(DistributionKind::Normal, &[("mean", 100.0), ("std_dev", 5.0)], 1000);
        assert_eq!(vals.len(), 1000);
        let mean: f64 = vals.iter().sum::<f64>() / vals.len() as f64;
        // With 1000 samples, mean should be roughly near 100
        assert!((mean - 100.0).abs() < 5.0, "normal mean too far from 100: {mean}");
    }

    #[test]
    fn lognormal_positive() {
        let vals = gen_f64(DistributionKind::LogNormal, &[("mu", 0.0), ("sigma", 0.5)], 500);
        for v in &vals {
            assert!(*v > 0.0, "lognormal should be positive: {v}");
        }
    }

    #[test]
    fn exponential_positive() {
        let vals = gen_f64(DistributionKind::Exponential, &[("lambda", 2.0)], 500);
        for v in &vals {
            assert!(*v >= 0.0, "exponential should be non-negative: {v}");
        }
    }

    #[test]
    fn poisson_non_negative_int() {
        let vals = gen_i64(DistributionKind::Poisson, &[("lambda", 5.0)], 500);
        assert_eq!(vals.len(), 500);
        for v in &vals {
            assert!(*v >= 0, "poisson should be non-negative: {v}");
        }
    }

    #[test]
    fn bernoulli_zero_or_one() {
        let vals = gen_i64(DistributionKind::Bernoulli, &[("p", 0.5)], 500);
        for v in &vals {
            assert!(*v == 0 || *v == 1, "bernoulli should be 0 or 1: {v}");
        }
    }

    #[test]
    fn bernoulli_extreme_p() {
        // p=0 → all zeros; p=1 → all ones
        let vals_zero = gen_i64(DistributionKind::Bernoulli, &[("p", 0.0)], 100);
        assert!(vals_zero.iter().all(|v| *v == 0), "p=0 should produce all zeros");
        let vals_one = gen_i64(DistributionKind::Bernoulli, &[("p", 1.0)], 100);
        assert!(vals_one.iter().all(|v| *v == 1), "p=1 should produce all ones");
    }

    #[test]
    fn binomial_in_range() {
        let vals = gen_i64(DistributionKind::Binomial, &[("n", 10.0), ("p", 0.5)], 500);
        for v in &vals {
            assert!(*v >= 0 && *v <= 10, "binomial(10,0.5) out of [0,10]: {v}");
        }
    }

    #[test]
    fn geometric_positive() {
        let vals = gen_i64(DistributionKind::Geometric, &[("p", 0.3)], 500);
        for v in &vals {
            assert!(*v >= 0, "geometric should be non-negative: {v}");
        }
    }

    #[test]
    fn pareto_above_scale() {
        let vals = gen_f64(DistributionKind::Pareto, &[("scale", 2.0), ("shape", 3.0)], 500);
        for v in &vals {
            assert!(*v >= 2.0, "pareto should be >= scale: {v}");
        }
    }

    #[test]
    fn weibull_positive() {
        let vals = gen_f64(DistributionKind::Weibull, &[("scale", 1.0), ("shape", 2.0)], 500);
        for v in &vals {
            assert!(*v >= 0.0, "weibull should be non-negative: {v}");
        }
    }

    #[test]
    fn gamma_positive() {
        let vals = gen_f64(DistributionKind::Gamma, &[("shape", 2.0), ("scale", 1.0)], 500);
        for v in &vals {
            assert!(*v > 0.0, "gamma should be positive: {v}");
        }
    }

    #[test]
    fn beta_in_unit_interval() {
        let vals = gen_f64(DistributionKind::Beta, &[("alpha", 2.0), ("beta", 5.0)], 500);
        for v in &vals {
            assert!(*v >= 0.0 && *v <= 1.0, "beta should be in [0,1]: {v}");
        }
    }

    #[test]
    fn cauchy_produces_values() {
        let vals = gen_f64(DistributionKind::Cauchy, &[("median", 0.0), ("scale", 1.0)], 100);
        assert_eq!(vals.len(), 100);
        // Cauchy has heavy tails, just verify it produces finite values mostly
        let finite_count = vals.iter().filter(|v| v.is_finite()).count();
        assert!(finite_count > 90, "cauchy should produce mostly finite values");
    }

    #[test]
    fn chi_squared_positive() {
        let vals = gen_f64(DistributionKind::ChiSquared, &[("k", 3.0)], 500);
        for v in &vals {
            assert!(*v >= 0.0, "chi-squared should be non-negative: {v}");
        }
    }

    #[test]
    fn student_t_produces_values() {
        let vals = gen_f64(DistributionKind::StudentT, &[("n", 5.0)], 500);
        assert_eq!(vals.len(), 500);
    }

    #[test]
    fn triangular_in_range() {
        let vals = gen_f64(
            DistributionKind::Triangular,
            &[("min", 1.0), ("max", 10.0), ("mode", 5.0)],
            500,
        );
        for v in &vals {
            assert!(*v >= 1.0 && *v <= 10.0, "triangular out of [1,10]: {v}");
        }
    }

    #[test]
    fn zipf_positive_int() {
        let vals = gen_i64(DistributionKind::Zipf, &[("n", 100.0), ("s", 1.0)], 500);
        for v in &vals {
            assert!(*v >= 1, "zipf should be >= 1: {v}");
        }
    }

    #[test]
    fn clamp_min_max() {
        let vals = gen_clamped(
            DistributionKind::Normal,
            &[("mean", 0.0), ("std_dev", 100.0)],
            Some(-5.0),
            Some(5.0),
            500,
        );
        for v in &vals {
            assert!(*v >= -5.0 && *v <= 5.0, "clamped value out of [-5,5]: {v}");
        }
    }

    #[test]
    fn output_type_float_for_continuous() {
        let continuous = [
            DistributionKind::Uniform,
            DistributionKind::Normal,
            DistributionKind::LogNormal,
            DistributionKind::Exponential,
            DistributionKind::Pareto,
            DistributionKind::Weibull,
            DistributionKind::Gamma,
            DistributionKind::Beta,
            DistributionKind::Cauchy,
            DistributionKind::ChiSquared,
            DistributionKind::StudentT,
            DistributionKind::Triangular,
        ];
        for kind in &continuous {
            let g = DistributionGenerator::new(kind.clone(), BTreeMap::new(), None, None);
            assert_eq!(g.output_type(), DataType::Float64, "expected Float64 for {kind:?}");
        }
    }

    #[test]
    fn output_type_int_for_discrete() {
        let discrete = [
            DistributionKind::Poisson,
            DistributionKind::Bernoulli,
            DistributionKind::Binomial,
            DistributionKind::Geometric,
            DistributionKind::Zipf,
        ];
        for kind in &discrete {
            let g = DistributionGenerator::new(kind.clone(), BTreeMap::new(), None, None);
            assert_eq!(g.output_type(), DataType::Int64, "expected Int64 for {kind:?}");
        }
    }

    #[test]
    fn deterministic_with_same_seed() {
        let a = gen_f64(DistributionKind::Normal, &[("mean", 0.0), ("std_dev", 1.0)], 50);
        let b = gen_f64(DistributionKind::Normal, &[("mean", 0.0), ("std_dev", 1.0)], 50);
        assert_eq!(a, b, "same seed must produce same output");
    }

    #[test]
    fn zero_count_returns_empty() {
        let vals = gen_f64(DistributionKind::Uniform, &[("min", 0.0), ("max", 1.0)], 0);
        assert!(vals.is_empty());
    }
}
