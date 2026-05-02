//! Distribution-based generator supporting multiple statistical distributions.
//!
//! This is the most commonly-used generator type, covering Uniform, Normal,
//! LogNormal, Exponential, and Poisson distributions. Additional distributions
//! (Zipf, Binomial, etc.) will be added in future PRs.
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
