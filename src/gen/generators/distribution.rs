//! Distribution-based generator supporting 19 statistical distributions.
//!
//! Covers continuous (Uniform, Normal, LogNormal, Exponential, etc.), discrete
//! (Poisson, Bernoulli, Binomial, Geometric, Zipf), shape-parameterised
//! (Pareto, Weibull, Gamma, Beta, Cauchy, ChiSquared, StudentT, Triangular),
//! and vector-valued (Dirichlet, Multinomial) families.
//!
//! Invalid user-supplied parameters (negative std_dev, zero lambda) are handled
//! gracefully — the generator logs a warning via `tracing` and falls back to
//! safe defaults rather than panicking.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, ListArray};
use arrow::buffer::OffsetBuffer;
use arrow::datatypes::{DataType, Field};
use rand::RngCore;
use rand_distr::Distribution;

use crate::core::DistributionKind;

use crate::gen::context::GenContext;
use crate::gen::traits::FieldGenerator;

/// Generate values drawn from a configurable statistical distribution.
///
/// Created by [`create_generator`](crate::gen::create_generator) when the plan
/// contains a [`GeneratorPlan::Distribution`](crate::plan::GeneratorPlan::Distribution).
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
    array_params: BTreeMap<String, Vec<f64>>,
    clamp_min: Option<f64>,
    clamp_max: Option<f64>,
    round: bool,
}

impl DistributionGenerator {
    /// Create a new distribution generator.
    pub fn new(
        kind: DistributionKind,
        params: BTreeMap<String, f64>,
        array_params: BTreeMap<String, Vec<f64>>,
        clamp_min: Option<f64>,
        clamp_max: Option<f64>,
        round: bool,
    ) -> Self {
        Self {
            kind,
            params,
            array_params,
            clamp_min,
            clamp_max,
            round,
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

    /// Convert sampled f64 values to an Arrow array, rounding to Int64 when configured.
    fn to_array(&self, values: Vec<f64>) -> ArrayRef {
        if self.round {
            let ints: Vec<i64> = values.iter().map(|v| v.round() as i64).collect();
            Arc::new(Int64Array::from(ints))
        } else {
            Arc::new(Float64Array::from(values))
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
                self.to_array(values)
            }
            DistributionKind::Normal => {
                let mean = self.param("mean", 0.0);
                let std_dev = self.param("std_dev", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Normal::new(mean, std_dev).unwrap_or_else(|_| {
                    tracing::warn!(
                        mean,
                        std_dev,
                        "invalid Normal params, falling back to N(0,1)"
                    );
                    rand_distr::Normal::new(0.0, 1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                self.to_array(values)
            }
            DistributionKind::LogNormal => {
                let mu = self.param("mu", 0.0);
                let sigma = self.param("sigma", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::LogNormal::new(mu, sigma).unwrap_or_else(|_| {
                    tracing::warn!(
                        mu,
                        sigma,
                        "invalid LogNormal params, falling back to LN(0,1)"
                    );
                    rand_distr::LogNormal::new(0.0, 1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                self.to_array(values)
            }
            DistributionKind::Exponential => {
                let lambda = self.param("lambda", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Exp::new(lambda).unwrap_or_else(|_| {
                    tracing::warn!(lambda, "invalid Exponential params, falling back to Exp(1)");
                    rand_distr::Exp::new(1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                self.to_array(values)
            }
            DistributionKind::Poisson => {
                let lambda = self.param("lambda", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Poisson::new(lambda).unwrap_or_else(|_| {
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
                    tracing::warn!(
                        scale,
                        shape,
                        "invalid Pareto params, falling back to Pareto(1,1)"
                    );
                    rand_distr::Pareto::new(1.0, 1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                self.to_array(values)
            }
            DistributionKind::Weibull => {
                let scale = self.param("scale", 1.0).abs().max(f64::EPSILON);
                let shape = self.param("shape", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Weibull::new(scale, shape).unwrap_or_else(|_| {
                    tracing::warn!(
                        scale,
                        shape,
                        "invalid Weibull params, falling back to Weibull(1,1)"
                    );
                    rand_distr::Weibull::new(1.0, 1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                self.to_array(values)
            }
            DistributionKind::Gamma => {
                let shape = self.param("shape", 1.0).abs().max(f64::EPSILON);
                let scale = self.param("scale", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Gamma::new(shape, scale).unwrap_or_else(|_| {
                    tracing::warn!(
                        shape,
                        scale,
                        "invalid Gamma params, falling back to Gamma(1,1)"
                    );
                    rand_distr::Gamma::new(1.0, 1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                self.to_array(values)
            }
            DistributionKind::Beta => {
                let alpha = self.param("alpha", 2.0).abs().max(f64::EPSILON);
                let beta = self.param("beta", 2.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Beta::new(alpha, beta).unwrap_or_else(|_| {
                    tracing::warn!(
                        alpha,
                        beta,
                        "invalid Beta params, falling back to Beta(2,2)"
                    );
                    rand_distr::Beta::new(2.0, 2.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                self.to_array(values)
            }
            DistributionKind::Cauchy => {
                let median = self.param("median", 0.0);
                let scale = self.param("scale", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::Cauchy::new(median, scale).unwrap_or_else(|_| {
                    tracing::warn!(
                        median,
                        scale,
                        "invalid Cauchy params, falling back to Cauchy(0,1)"
                    );
                    rand_distr::Cauchy::new(0.0, 1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                self.to_array(values)
            }
            DistributionKind::ChiSquared => {
                let k = self.param("k", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::ChiSquared::new(k).unwrap_or_else(|_| {
                    tracing::warn!(
                        k,
                        "invalid ChiSquared params, falling back to ChiSquared(1)"
                    );
                    rand_distr::ChiSquared::new(1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                self.to_array(values)
            }
            DistributionKind::StudentT => {
                let n = self.param("n", 1.0).abs().max(f64::EPSILON);
                let dist = rand_distr::StudentT::new(n).unwrap_or_else(|_| {
                    tracing::warn!(n, "invalid StudentT params, falling back to StudentT(1)");
                    rand_distr::StudentT::new(1.0).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                self.to_array(values)
            }
            DistributionKind::Triangular => {
                let min = self.param("min", 0.0);
                let max = self.param("max", 1.0);
                let mode = self.param("mode", (min + max) / 2.0);
                let (min, max) = if min >= max { (0.0, 1.0) } else { (min, max) };
                let mode = mode.clamp(min, max);
                let dist = rand_distr::Triangular::new(min, max, mode).unwrap_or_else(|_| {
                    tracing::warn!(
                        min,
                        max,
                        mode,
                        "invalid Triangular params, falling back to Tri(0,1,0.5)"
                    );
                    rand_distr::Triangular::new(0.0, 1.0, 0.5).unwrap()
                });
                let values: Vec<f64> = (0..count).map(|_| self.clamp(dist.sample(rng))).collect();
                self.to_array(values)
            }
            DistributionKind::Zipf => {
                let n = self.param("n", 100.0).max(1.0) as u64;
                let s = self.param("s", 1.0).max(f64::EPSILON);
                let dist = rand_distr::Zipf::new(n, s).unwrap_or_else(|_| {
                    tracing::warn!(n, s, "invalid Zipf params, falling back to Zipf(100,1)");
                    rand_distr::Zipf::new(100, 1.0).unwrap()
                });
                let values: Vec<i64> = (0..count)
                    .map(|_| {
                        let v: f64 = dist.sample(rng);
                        self.clamp(v) as i64
                    })
                    .collect();
                Arc::new(Int64Array::from(values))
            }
            DistributionKind::Dirichlet => {
                let alpha = self
                    .array_params
                    .get("alpha")
                    .cloned()
                    .unwrap_or_else(|| vec![1.0, 1.0]);
                let k = alpha.len();
                let dist = rand_distr::Dirichlet::new(&alpha).unwrap_or_else(|_| {
                    tracing::warn!(
                        ?alpha,
                        "invalid Dirichlet alpha, falling back to symmetric(1.0, k={})",
                        k
                    );
                    rand_distr::Dirichlet::new(&vec![1.0; k.max(2)]).unwrap()
                });
                // Sample k floats per row, flatten into a single values array.
                let mut flat_values = Vec::with_capacity(count * k);
                for _ in 0..count {
                    let sample: Vec<f64> = dist.sample(rng);
                    flat_values.extend_from_slice(&sample);
                }
                let values_array = Arc::new(Float64Array::from(flat_values));
                let offsets: Vec<i32> = (0..=count).map(|i| (i * k) as i32).collect();
                let field = Arc::new(Field::new("item", DataType::Float64, false));
                Arc::new(ListArray::new(
                    field,
                    OffsetBuffer::new(offsets.into()),
                    values_array,
                    None,
                ))
            }
            DistributionKind::Multinomial => {
                let p = self
                    .array_params
                    .get("p")
                    .cloned()
                    .unwrap_or_else(|| vec![0.5, 0.5]);
                let n = self.param("n", 10.0).max(0.0) as i64;
                let k = p.len();
                // Sequential-binomial method: O(k) per row.
                let mut flat_values = Vec::with_capacity(count * k);
                for _ in 0..count {
                    let mut remaining = n;
                    let mut p_remaining = 1.0;
                    for j in 0..k {
                        if j == k - 1 {
                            // Last bucket gets the remainder.
                            flat_values.push(remaining);
                        } else if p_remaining <= 0.0 || remaining <= 0 {
                            flat_values.push(0);
                        } else {
                            let p_cond = (p[j] / p_remaining).clamp(0.0, 1.0);
                            let binom = rand_distr::Binomial::new(remaining as u64, p_cond)
                                .unwrap_or_else(|_| {
                                    rand_distr::Binomial::new(remaining as u64, 0.5).unwrap()
                                });
                            let x = binom.sample(rng) as i64;
                            flat_values.push(x);
                            remaining -= x;
                            p_remaining -= p[j];
                        }
                    }
                }
                let values_array = Arc::new(Int64Array::from(flat_values));
                let offsets: Vec<i32> = (0..=count).map(|i| (i * k) as i32).collect();
                let field = Arc::new(Field::new("item", DataType::Int64, false));
                Arc::new(ListArray::new(
                    field,
                    OffsetBuffer::new(offsets.into()),
                    values_array,
                    None,
                ))
            }
        }
    }

    fn output_type(&self) -> DataType {
        match self.kind {
            DistributionKind::Dirichlet => {
                let field = Arc::new(Field::new("item", DataType::Float64, false));
                DataType::List(field)
            }
            DistributionKind::Multinomial => {
                let field = Arc::new(Field::new("item", DataType::Int64, false));
                DataType::List(field)
            }
            _ => {
                if self.round {
                    return DataType::Int64;
                }
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
        let g = DistributionGenerator::new(kind, p, BTreeMap::new(), None, None, false);
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = g.generate(&mut rng, count, &ctx);
        let fa = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        (0..fa.len()).map(|i| fa.value(i)).collect()
    }

    fn gen_i64(kind: DistributionKind, params: &[(&str, f64)], count: usize) -> Vec<i64> {
        let p: BTreeMap<String, f64> = params.iter().map(|(k, v)| (k.to_string(), *v)).collect();
        let g = DistributionGenerator::new(kind, p, BTreeMap::new(), None, None, false);
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
        let g = DistributionGenerator::new(kind, p, BTreeMap::new(), clamp_min, clamp_max, false);
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = g.generate(&mut rng, count, &ctx);
        let fa = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        (0..fa.len()).map(|i| fa.value(i)).collect()
    }

    #[test]
    fn uniform_in_range() {
        let vals = gen_f64(
            DistributionKind::Uniform,
            &[("min", 10.0), ("max", 20.0)],
            500,
        );
        assert_eq!(vals.len(), 500);
        for v in &vals {
            assert!(*v >= 10.0 && *v < 20.0, "uniform value out of range: {v}");
        }
    }

    #[test]
    fn uniform_invalid_params_fallback() {
        // min >= max should fallback to (0, 1)
        let vals = gen_f64(
            DistributionKind::Uniform,
            &[("min", 5.0), ("max", 5.0)],
            100,
        );
        for v in &vals {
            assert!(*v >= 0.0 && *v < 1.0, "fallback uniform out of (0,1): {v}");
        }
    }

    #[test]
    fn normal_produces_float64() {
        let vals = gen_f64(
            DistributionKind::Normal,
            &[("mean", 100.0), ("std_dev", 5.0)],
            1000,
        );
        assert_eq!(vals.len(), 1000);
        let mean: f64 = vals.iter().sum::<f64>() / vals.len() as f64;
        // With 1000 samples, mean should be roughly near 100
        assert!(
            (mean - 100.0).abs() < 5.0,
            "normal mean too far from 100: {mean}"
        );
    }

    #[test]
    fn lognormal_positive() {
        let vals = gen_f64(
            DistributionKind::LogNormal,
            &[("mu", 0.0), ("sigma", 0.5)],
            500,
        );
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
        assert!(
            vals_zero.iter().all(|v| *v == 0),
            "p=0 should produce all zeros"
        );
        let vals_one = gen_i64(DistributionKind::Bernoulli, &[("p", 1.0)], 100);
        assert!(
            vals_one.iter().all(|v| *v == 1),
            "p=1 should produce all ones"
        );
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
        let vals = gen_f64(
            DistributionKind::Pareto,
            &[("scale", 2.0), ("shape", 3.0)],
            500,
        );
        for v in &vals {
            assert!(*v >= 2.0, "pareto should be >= scale: {v}");
        }
    }

    #[test]
    fn weibull_positive() {
        let vals = gen_f64(
            DistributionKind::Weibull,
            &[("scale", 1.0), ("shape", 2.0)],
            500,
        );
        for v in &vals {
            assert!(*v >= 0.0, "weibull should be non-negative: {v}");
        }
    }

    #[test]
    fn gamma_positive() {
        let vals = gen_f64(
            DistributionKind::Gamma,
            &[("shape", 2.0), ("scale", 1.0)],
            500,
        );
        for v in &vals {
            assert!(*v > 0.0, "gamma should be positive: {v}");
        }
    }

    #[test]
    fn beta_in_unit_interval() {
        let vals = gen_f64(
            DistributionKind::Beta,
            &[("alpha", 2.0), ("beta", 5.0)],
            1000,
        );
        for v in &vals {
            assert!(*v >= 0.0 && *v <= 1.0, "beta should be in [0,1]: {v}");
        }
        // Beta(2,5) has expected mean = 2/(2+5) ≈ 0.286
        let mean: f64 = vals.iter().sum::<f64>() / vals.len() as f64;
        assert!(
            (mean - 0.286).abs() < 0.05,
            "beta(2,5) mean should be near 0.286: {mean}"
        );
    }

    #[test]
    fn cauchy_centered_near_median() {
        let vals = gen_f64(
            DistributionKind::Cauchy,
            &[("median", 50.0), ("scale", 1.0)],
            1000,
        );
        assert_eq!(vals.len(), 1000);
        // Cauchy has no mean, but median should be near 50. Sort and check.
        let mut sorted = vals.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let sample_median = sorted[sorted.len() / 2];
        assert!(
            (sample_median - 50.0).abs() < 5.0,
            "cauchy median should be near 50: got {sample_median}"
        );
    }

    #[test]
    fn chi_squared_positive() {
        let vals = gen_f64(DistributionKind::ChiSquared, &[("k", 3.0)], 500);
        for v in &vals {
            assert!(*v >= 0.0, "chi-squared should be non-negative: {v}");
        }
    }

    #[test]
    fn student_t_symmetric_around_zero() {
        let vals = gen_f64(DistributionKind::StudentT, &[("n", 5.0)], 2000);
        assert_eq!(vals.len(), 2000);
        let mean: f64 = vals.iter().sum::<f64>() / vals.len() as f64;
        // StudentT(5) has mean 0 and finite variance; sample mean should be near 0
        assert!(mean.abs() < 1.0, "studentT mean should be near 0: {mean}");
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
    fn zipf_bounded_by_n() {
        let vals = gen_i64(DistributionKind::Zipf, &[("n", 50.0), ("s", 1.0)], 500);
        for v in &vals {
            assert!(*v >= 1 && *v <= 50, "zipf(n=50) should be in [1,50]: {v}");
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
            let g = DistributionGenerator::new(kind.clone(), BTreeMap::new(), BTreeMap::new(), None, None, false);
            assert_eq!(
                g.output_type(),
                DataType::Float64,
                "expected Float64 for {kind:?}"
            );
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
            let g = DistributionGenerator::new(kind.clone(), BTreeMap::new(), BTreeMap::new(), None, None, false);
            assert_eq!(
                g.output_type(),
                DataType::Int64,
                "expected Int64 for {kind:?}"
            );
        }
    }

    #[test]
    fn deterministic_with_same_seed() {
        let a = gen_f64(
            DistributionKind::Normal,
            &[("mean", 0.0), ("std_dev", 1.0)],
            50,
        );
        let b = gen_f64(
            DistributionKind::Normal,
            &[("mean", 0.0), ("std_dev", 1.0)],
            50,
        );
        assert_eq!(a, b, "same seed must produce same output");
    }

    #[test]
    fn zero_count_returns_empty() {
        let vals = gen_f64(DistributionKind::Uniform, &[("min", 0.0), ("max", 1.0)], 0);
        assert!(vals.is_empty());
    }

    // ── Invalid parameter fallback tests ──────────────────────────

    #[test]
    fn normal_negative_std_dev_fallback() {
        // Negative std_dev is abs'd then clamped to epsilon
        let vals = gen_f64(
            DistributionKind::Normal,
            &[("mean", 0.0), ("std_dev", -5.0)],
            100,
        );
        assert_eq!(vals.len(), 100);
    }

    #[test]
    fn exponential_zero_lambda_fallback() {
        let vals = gen_f64(DistributionKind::Exponential, &[("lambda", 0.0)], 100);
        assert_eq!(vals.len(), 100);
        for v in &vals {
            assert!(
                *v >= 0.0,
                "exponential fallback should be non-negative: {v}"
            );
        }
    }

    #[test]
    fn poisson_zero_lambda_fallback() {
        let vals = gen_i64(DistributionKind::Poisson, &[("lambda", 0.0)], 100);
        assert_eq!(vals.len(), 100);
        for v in &vals {
            assert!(*v >= 0, "poisson fallback should be non-negative: {v}");
        }
    }

    #[test]
    fn bernoulli_out_of_range_clamped() {
        // p > 1 should be clamped to 1.0
        let vals = gen_i64(DistributionKind::Bernoulli, &[("p", 5.0)], 50);
        assert!(
            vals.iter().all(|v| *v == 1),
            "p=5 clamped to 1 should produce all ones"
        );
        // p < 0 should be clamped to 0.0
        let vals = gen_i64(DistributionKind::Bernoulli, &[("p", -1.0)], 50);
        assert!(
            vals.iter().all(|v| *v == 0),
            "p=-1 clamped to 0 should produce all zeros"
        );
    }

    #[test]
    fn pareto_zero_params_fallback() {
        let vals = gen_f64(
            DistributionKind::Pareto,
            &[("scale", 0.0), ("shape", 0.0)],
            100,
        );
        assert_eq!(vals.len(), 100);
        for v in &vals {
            assert!(*v > 0.0, "pareto fallback should be positive: {v}");
        }
    }

    #[test]
    fn gamma_zero_shape_fallback() {
        // shape=0 is abs'd then clamped to epsilon, so produces valid output
        let vals = gen_f64(
            DistributionKind::Gamma,
            &[("shape", 0.0), ("scale", 0.0)],
            100,
        );
        assert_eq!(vals.len(), 100);
        for v in &vals {
            assert!(*v >= 0.0, "gamma fallback should be non-negative: {v}");
        }
    }

    #[test]
    fn triangular_invalid_range_fallback() {
        // min >= max should fallback to (0, 1, 0.5)
        let vals = gen_f64(
            DistributionKind::Triangular,
            &[("min", 5.0), ("max", 5.0), ("mode", 5.0)],
            100,
        );
        for v in &vals {
            assert!(
                *v >= 0.0 && *v <= 1.0,
                "triangular fallback out of [0,1]: {v}"
            );
        }
    }

    #[test]
    fn lognormal_zero_sigma_fallback() {
        let vals = gen_f64(
            DistributionKind::LogNormal,
            &[("mu", 0.0), ("sigma", 0.0)],
            100,
        );
        assert_eq!(vals.len(), 100);
        for v in &vals {
            assert!(*v > 0.0, "lognormal should be positive: {v}");
        }
    }

    // ─── Dirichlet tests ────────────────────────────────────────────

    #[test]
    fn dirichlet_output_is_list_summing_to_one() {
        let mut array_params = BTreeMap::new();
        array_params.insert("alpha".to_string(), vec![2.0, 3.0, 5.0]);
        let g = DistributionGenerator::new(
            DistributionKind::Dirichlet,
            BTreeMap::new(),
            array_params,
            None,
            None,
            false,
        );
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = g.generate(&mut rng, 100, &ctx);
        let list = arr.as_any().downcast_ref::<ListArray>().unwrap();
        assert_eq!(list.len(), 100);
        for i in 0..list.len() {
            let inner = list.value(i);
            let floats = inner.as_any().downcast_ref::<Float64Array>().unwrap();
            assert_eq!(floats.len(), 3);
            let sum: f64 = (0..3).map(|j| floats.value(j)).sum();
            assert!(
                (sum - 1.0).abs() < 1e-9,
                "row {i}: Dirichlet sample should sum to 1.0, got {sum}"
            );
            for j in 0..3 {
                assert!(
                    floats.value(j) > 0.0,
                    "row {i}: all elements must be > 0"
                );
            }
        }
    }

    #[test]
    fn dirichlet_output_type_is_list() {
        let mut array_params = BTreeMap::new();
        array_params.insert("alpha".to_string(), vec![1.0, 1.0]);
        let g = DistributionGenerator::new(
            DistributionKind::Dirichlet,
            BTreeMap::new(),
            array_params,
            None,
            None,
            false,
        );
        assert!(matches!(g.output_type(), DataType::List(_)));
    }

    // ─── Multinomial tests ──────────────────────────────────────────

    #[test]
    fn multinomial_counts_sum_to_n() {
        let mut params = BTreeMap::new();
        params.insert("n".to_string(), 50.0);
        let mut array_params = BTreeMap::new();
        array_params.insert("p".to_string(), vec![0.2, 0.3, 0.5]);
        let g = DistributionGenerator::new(
            DistributionKind::Multinomial,
            params,
            array_params,
            None,
            None,
            false,
        );
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = g.generate(&mut rng, 100, &ctx);
        let list = arr.as_any().downcast_ref::<ListArray>().unwrap();
        assert_eq!(list.len(), 100);
        for i in 0..list.len() {
            let inner = list.value(i);
            let ints = inner.as_any().downcast_ref::<Int64Array>().unwrap();
            assert_eq!(ints.len(), 3);
            let sum: i64 = (0..3).map(|j| ints.value(j)).sum();
            assert_eq!(sum, 50, "row {i}: multinomial counts must sum to n=50, got {sum}");
            for j in 0..3 {
                assert!(
                    ints.value(j) >= 0,
                    "row {i}: counts must be non-negative"
                );
            }
        }
    }

    #[test]
    fn multinomial_output_type_is_list() {
        let mut params = BTreeMap::new();
        params.insert("n".to_string(), 10.0);
        let mut array_params = BTreeMap::new();
        array_params.insert("p".to_string(), vec![0.5, 0.5]);
        let g = DistributionGenerator::new(
            DistributionKind::Multinomial,
            params,
            array_params,
            None,
            None,
            false,
        );
        assert!(matches!(g.output_type(), DataType::List(_)));
    }

    #[test]
    fn dirichlet_deterministic_with_seed() {
        let mut array_params = BTreeMap::new();
        array_params.insert("alpha".to_string(), vec![1.0, 1.0, 1.0]);
        let g = DistributionGenerator::new(
            DistributionKind::Dirichlet,
            BTreeMap::new(),
            array_params,
            None,
            None,
            false,
        );
        let ctx = make_ctx();
        let mut rng1 = ChaCha8Rng::seed_from_u64(99);
        let mut rng2 = ChaCha8Rng::seed_from_u64(99);
        let a = g.generate(&mut rng1, 10, &ctx);
        let b = g.generate(&mut rng2, 10, &ctx);
        let la = a.as_any().downcast_ref::<ListArray>().unwrap();
        let lb = b.as_any().downcast_ref::<ListArray>().unwrap();
        for i in 0..10 {
            let va = la.value(i);
            let vb = lb.value(i);
            let fa = va.as_any().downcast_ref::<Float64Array>().unwrap();
            let fb = vb.as_any().downcast_ref::<Float64Array>().unwrap();
            for j in 0..3 {
                assert_eq!(fa.value(j), fb.value(j), "determinism check row {i} col {j}");
            }
        }
    }
}
