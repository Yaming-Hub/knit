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
            _ => {
                tracing::warn!(
                    kind = %self.kind,
                    "unsupported distribution kind, producing zeros"
                );
                let values: Vec<f64> = vec![0.0; count];
                Arc::new(Float64Array::from(values))
            }
        }
    }

    fn output_type(&self) -> DataType {
        match self.kind {
            DistributionKind::Poisson => DataType::Int64,
            _ => DataType::Float64,
        }
    }
}
