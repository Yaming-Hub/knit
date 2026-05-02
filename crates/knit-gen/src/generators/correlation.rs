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

use crate::context::GenContext;
use crate::traits::FieldGenerator;

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
        let ctx = GenContext {
            batch_columns: cols,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test",
        };

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, n, &ctx);
        let y = arr.as_any().downcast_ref::<Float64Array>().unwrap();

        // Compute Pearson correlation.
        let y_vals: Vec<f64> = (0..n).map(|i| y.value(i)).collect();
        let r = pearson(&source, &y_vals);
        assert!(
            (r - target_r).abs() < 0.1,
            "correlation {r:.4} not within ±0.1 of target {target_r}"
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
        let ctx = GenContext {
            batch_columns: cols,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test",
        };

        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let arr = gen.generate(&mut rng, n, &ctx);
        let y = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        let y_vals: Vec<f64> = (0..n).map(|i| y.value(i)).collect();
        let r = pearson(&source, &y_vals);
        assert!(
            (r - target_r).abs() < 0.1,
            "correlation {r:.4} not within ±0.1 of target {target_r}"
        );
    }

    fn pearson(x: &[f64], y: &[f64]) -> f64 {
        let n = x.len() as f64;
        let mx = x.iter().sum::<f64>() / n;
        let my = y.iter().sum::<f64>() / n;
        let cov: f64 = x.iter().zip(y).map(|(a, b)| (a - mx) * (b - my)).sum::<f64>() / n;
        let sx = (x.iter().map(|a| (a - mx).powi(2)).sum::<f64>() / n).sqrt();
        let sy = (y.iter().map(|b| (b - my).powi(2)).sum::<f64>() / n).sqrt();
        cov / (sx * sy).max(1e-12)
    }
}
