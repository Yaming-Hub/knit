//! Correlated field generator — produces values with a target Pearson correlation
//! to an already-generated column.
//!
//! Uses the identity `y = r·x_norm + √(1−r²)·z` where `x_norm` is the
//! standardised (mean-centred, unit-variance) version of the target column and
//! `z` is independent standard-normal noise. The result is a Float64 column
//! with the specified linear correlation to the target field.

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array};
use arrow::datatypes::DataType;
use rand::RngCore;
use rand_distr::{Distribution, Normal};

use crate::gen::context::GenContext;
use crate::gen::traits::FieldGenerator;

/// Generates Float64 values correlated with an existing column.
///
/// # Algorithm
///
/// 1. Read the target field from [`GenContext::batch_columns`].
/// 2. Standardise it: `x_norm = (x − mean) / std`.
/// 3. Sample independent standard-normal noise `z`.
/// 4. Produce `y = r·x_norm + √(1 − r²)·z`.
///
/// # Output
///
/// `DataType::Float64`
pub struct CorrelatedGenerator {
    /// Name of the field to correlate with (must already exist in batch_columns).
    target_field: String,
    /// Target Pearson correlation coefficient (−1.0 to 1.0).
    correlation: f64,
}

impl CorrelatedGenerator {
    /// Create a new correlated generator.
    pub fn new(target_field: String, correlation: f64) -> Self {
        let r = correlation.clamp(-1.0, 1.0);
        Self {
            target_field,
            correlation: r,
        }
    }
}

/// Extract Float64 values from an ArrayRef (supports Float64 and Int64 source columns).
fn extract_f64_values(arr: &ArrayRef, count: usize) -> Vec<f64> {
    if let Some(f) = arr.as_any().downcast_ref::<Float64Array>() {
        (0..f.len().min(count)).map(|i| f.value(i)).collect()
    } else if let Some(i) = arr.as_any().downcast_ref::<Int64Array>() {
        (0..i.len().min(count)).map(|j| i.value(j) as f64).collect()
    } else {
        vec![0.0; count]
    }
}

impl FieldGenerator for CorrelatedGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let target = ctx.batch_columns.get(&self.target_field);

        let x_raw = match target {
            Some(arr) => extract_f64_values(arr, count),
            None => {
                tracing::warn!(
                    field = %self.target_field,
                    entity = %ctx.entity_name,
                    "target field not found, producing uncorrelated noise"
                );
                vec![0.0; count]
            }
        };

        // Pad or truncate to `count`.
        let x: Vec<f64> = if x_raw.len() >= count {
            x_raw[..count].to_vec()
        } else {
            let mut v = x_raw;
            v.resize(count, 0.0);
            v
        };

        // Standardise x.
        let n = count as f64;
        let mean = x.iter().sum::<f64>() / n.max(1.0);
        let var = x.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n.max(1.0);
        let std = var.sqrt().max(1e-12);

        let x_norm: Vec<f64> = x.iter().map(|v| (v - mean) / std).collect();

        let r = self.correlation;
        let complement = (1.0 - r * r).max(0.0).sqrt();
        let noise = Normal::new(0.0, 1.0).unwrap();

        let values: Vec<f64> = x_norm
            .iter()
            .map(|&xn| r * xn + complement * noise.sample(rng))
            .collect();

        Arc::new(Float64Array::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Float64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    #[test]
    fn correlation_within_tolerance() {
        let n = 10_000usize;
        // Generate a "source" column: simple linear ramp + noise.
        let mut src_rng = ChaCha8Rng::seed_from_u64(99);
        let noise = Normal::new(0.0, 1.0).unwrap();
        let source: Vec<f64> = (0..n)
            .map(|i| i as f64 + noise.sample(&mut src_rng) * 5.0)
            .collect();
        let source_arr: ArrayRef = Arc::new(Float64Array::from(source.clone()));

        let target_r = 0.8;
        let gen = CorrelatedGenerator::new("x".into(), target_r);

        let mut cols = HashMap::new();
        cols.insert("x".into(), source_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, n, &ctx);
        let y = arr.as_any().downcast_ref::<Float64Array>().unwrap();

        // Compute Pearson correlation.
        let y_vals: Vec<f64> = (0..n).map(|i| y.value(i)).collect();
        let r = pearson(&source, &y_vals);
        // With n=10000, sampling error ~0.004 for r=0.8, use ±0.03
        assert!(
            (r - target_r).abs() < 0.03,
            "correlation {r:.4} not within ±0.03 of target {target_r}"
        );
    }

    #[test]
    fn negative_correlation() {
        let n = 10_000usize;
        let source: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let source_arr: ArrayRef = Arc::new(Float64Array::from(source.clone()));

        let target_r = -0.7;
        let gen = CorrelatedGenerator::new("x".into(), target_r);

        let mut cols = HashMap::new();
        cols.insert("x".into(), source_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let arr = gen.generate(&mut rng, n, &ctx);
        let y = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        let y_vals: Vec<f64> = (0..n).map(|i| y.value(i)).collect();
        let r = pearson(&source, &y_vals);
        // With n=10000, sampling error ~0.005 for r=-0.7, use ±0.03
        assert!(
            (r - target_r).abs() < 0.03,
            "correlation {r:.4} not within ±0.03 of target {target_r}"
        );
    }

    #[test]
    fn perfect_positive_correlation() {
        // r=1.0 → complement=0, output = exactly x_norm (no noise)
        let n = 1_000usize;
        let source: Vec<f64> = (0..n).map(|i| i as f64 * 3.0 + 5.0).collect();
        let source_arr: ArrayRef = Arc::new(Float64Array::from(source.clone()));

        let gen = CorrelatedGenerator::new("x".into(), 1.0);
        let mut cols = HashMap::new();
        cols.insert("x".into(), source_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, n, &ctx);
        let y = arr.as_any().downcast_ref::<Float64Array>().unwrap();

        // With r=1.0, output should be exactly x_norm (complement=0, noise eliminated)
        // Verify directly: each y[i] should equal (source[i] - mean) / std
        let mean = source.iter().sum::<f64>() / n as f64;
        let var = source.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
        let std = var.sqrt();
        for i in 0..n {
            let expected = (source[i] - mean) / std;
            assert!(
                (y.value(i) - expected).abs() < 1e-10,
                "row {i}: expected exactly x_norm={expected}, got {}",
                y.value(i)
            );
        }
    }

    #[test]
    fn perfect_negative_correlation() {
        let n = 1_000usize;
        let source: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let source_arr: ArrayRef = Arc::new(Float64Array::from(source.clone()));

        let gen = CorrelatedGenerator::new("x".into(), -1.0);
        let mut cols = HashMap::new();
        cols.insert("x".into(), source_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, n, &ctx);
        let y = arr.as_any().downcast_ref::<Float64Array>().unwrap();

        // With r=-1.0, output should be exactly -x_norm
        let mean = source.iter().sum::<f64>() / n as f64;
        let var = source.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / n as f64;
        let std = var.sqrt();
        for i in 0..n {
            let expected = -1.0 * (source[i] - mean) / std;
            assert!(
                (y.value(i) - expected).abs() < 1e-10,
                "row {i}: expected exactly -x_norm={expected}, got {}",
                y.value(i)
            );
        }
    }

    #[test]
    fn zero_correlation_produces_uncorrelated_output() {
        let n = 10_000usize;
        let source: Vec<f64> = (0..n).map(|i| i as f64).collect();
        let source_arr: ArrayRef = Arc::new(Float64Array::from(source.clone()));

        let gen = CorrelatedGenerator::new("x".into(), 0.0);
        let mut cols = HashMap::new();
        cols.insert("x".into(), source_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, n, &ctx);
        let y = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        let y_vals: Vec<f64> = (0..n).map(|i| y.value(i)).collect();
        let r = pearson(&source, &y_vals);
        // With n=10000, sampling error ~0.01, use ±0.03
        assert!(
            r.abs() < 0.03,
            "zero target correlation should yield ~0 actual, got {r:.4}"
        );
    }

    #[test]
    fn int64_source_column() {
        let n = 5_000usize;
        let source_i64: Vec<i64> = (0..n as i64).collect();
        let source_arr: ArrayRef = Arc::new(Int64Array::from(source_i64));
        let source_f64: Vec<f64> = (0..n).map(|i| i as f64).collect();

        let gen = CorrelatedGenerator::new("x".into(), 0.9);
        let mut cols = HashMap::new();
        cols.insert("x".into(), source_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, n, &ctx);
        let y = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        let y_vals: Vec<f64> = (0..n).map(|i| y.value(i)).collect();
        let r = pearson(&source_f64, &y_vals);
        assert!(
            (r - 0.9).abs() < 0.1,
            "int64 source should work, got r={r:.4}"
        );
    }

    #[test]
    fn missing_target_field_produces_output() {
        // Target field not in batch_columns → produces uncorrelated noise
        let gen = CorrelatedGenerator::new("nonexistent".into(), 0.8);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 100, &ctx);
        assert_eq!(arr.len(), 100);
        assert_eq!(gen.output_type(), DataType::Float64);
    }

    #[test]
    fn constant_source_column() {
        // All same values → std=0 → falls back to max(std, 1e-12)
        let n = 100usize;
        let source: Vec<f64> = vec![42.0; n];
        let source_arr: ArrayRef = Arc::new(Float64Array::from(source));

        let gen = CorrelatedGenerator::new("x".into(), 0.5);
        let mut cols = HashMap::new();
        cols.insert("x".into(), source_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, n, &ctx);
        assert_eq!(arr.len(), n);
        // Should not panic/NaN — all values should be finite
        let y = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        for i in 0..n {
            assert!(y.value(i).is_finite(), "row {i}: value should be finite");
        }
    }

    #[test]
    fn correlation_clamped_to_bounds() {
        // r > 1.0 should be clamped to 1.0
        let gen = CorrelatedGenerator::new("x".into(), 5.0);
        assert_eq!(gen.correlation, 1.0);
        // r < -1.0 should be clamped to -1.0
        let gen2 = CorrelatedGenerator::new("x".into(), -3.0);
        assert_eq!(gen2.correlation, -1.0);
    }

    fn pearson(x: &[f64], y: &[f64]) -> f64 {
        let n = x.len() as f64;
        let mx = x.iter().sum::<f64>() / n;
        let my = y.iter().sum::<f64>() / n;
        let cov: f64 = x
            .iter()
            .zip(y)
            .map(|(a, b)| (a - mx) * (b - my))
            .sum::<f64>()
            / n;
        let sx = (x.iter().map(|a| (a - mx).powi(2)).sum::<f64>() / n).sqrt();
        let sy = (y.iter().map(|b| (b - my).powi(2)).sum::<f64>() / n).sqrt();
        cov / (sx * sy).max(1e-12)
    }
}