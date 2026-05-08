//! Gaussian noise perturbator for numeric columns.
//!
//! [`GaussianNoise`] adds normally distributed noise to numeric columns.
//! By default it breaks no invariants — the noise is additive and stays
//! within typical bounds.

use arrow::array::*;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use rand::RngCore;
use rand_distr::{Distribution, Normal};
use std::sync::Arc;
use tracing::trace;

use crate::noise::error::NoiseError;
use crate::noise::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

/// Add Gaussian noise to numeric columns.
///
/// `stddev` controls the standard deviation of the additive noise.
/// If `relative` is true, the stddev is treated as a fraction of each
/// cell's absolute value.
#[derive(Debug, Clone)]
pub struct GaussianNoise {
    /// Absolute standard deviation of noise, or relative fraction.
    pub stddev: f64,
    /// When true, `stddev` is multiplied by each cell's value.
    pub relative: bool,
}

impl Default for GaussianNoise {
    fn default() -> Self {
        Self {
            stddev: 0.1,
            relative: false,
        }
    }
}

impl GaussianNoise {
    /// Create a Gaussian noise perturbator with absolute stddev.
    pub fn absolute(stddev: f64) -> Self {
        Self {
            stddev,
            relative: false,
        }
    }

    /// Create a Gaussian noise perturbator with relative stddev.
    pub fn relative(fraction: f64) -> Self {
        Self {
            stddev: fraction,
            relative: true,
        }
    }
}

impl Perturbator for GaussianNoise {
    fn name(&self) -> &str {
        "GaussianNoise"
    }

    fn breaks(&self) -> InvariantSet {
        InvariantSet::empty()
    }

    fn perturb(
        &self,
        batch: RecordBatch,
        rng: &mut dyn RngCore,
        config: &PerturbConfig,
    ) -> Result<RecordBatch, NoiseError> {
        let schema = batch.schema();
        let mut columns: Vec<Arc<dyn Array>> = Vec::with_capacity(batch.num_columns());

        for (col_idx, field) in schema.fields().iter().enumerate() {
            let col = batch.column(col_idx);

            if !should_apply_numeric(field.name(), field.data_type(), &config.columns) {
                columns.push(Arc::clone(col));
                continue;
            }

            let noisy = add_noise(
                col.as_ref(),
                rng,
                config.probability,
                self.stddev,
                self.relative,
            )?;
            trace!(
                column = field.name(),
                stddev = self.stddev,
                "added gaussian noise"
            );
            columns.push(noisy);
        }

        Ok(RecordBatch::try_new(schema, columns)?)
    }
}

fn should_apply_numeric(name: &str, dt: &DataType, filter: &ColumnFilter) -> bool {
    let is_numeric = matches!(
        dt,
        DataType::Int32 | DataType::Int64 | DataType::Float32 | DataType::Float64
    );
    if !is_numeric {
        return false;
    }
    match filter {
        ColumnFilter::All => true,
        ColumnFilter::ByName(names) => names.iter().any(|n| n == name),
    }
}

fn add_noise(
    array: &dyn Array,
    rng: &mut dyn RngCore,
    probability: f64,
    stddev: f64,
    relative: bool,
) -> Result<Arc<dyn Array>, NoiseError> {
    match array.data_type() {
        DataType::Float64 => {
            let a = array.as_any().downcast_ref::<Float64Array>().unwrap();
            let vals: Vec<Option<f64>> = (0..a.len())
                .map(|i| {
                    if !a.is_valid(i) {
                        return None;
                    }
                    let v = a.value(i);
                    if rand::Rng::gen::<f64>(rng) >= probability {
                        return Some(v);
                    }
                    let sd = if relative { stddev * v.abs() } else { stddev };
                    let dist = Normal::new(0.0, sd.max(1e-15)).unwrap();
                    Some(v + dist.sample(rng))
                })
                .collect();
            Ok(Arc::new(Float64Array::from(vals)))
        }
        DataType::Int32 => {
            let a = array.as_any().downcast_ref::<Int32Array>().unwrap();
            let vals: Vec<Option<i32>> = (0..a.len())
                .map(|i| {
                    if !a.is_valid(i) {
                        return None;
                    }
                    let v = a.value(i) as f64;
                    if rand::Rng::gen::<f64>(rng) >= probability {
                        return Some(v as i32);
                    }
                    let sd = if relative { stddev * v.abs() } else { stddev };
                    let dist = Normal::new(0.0, sd.max(1e-15)).unwrap();
                    Some((v + dist.sample(rng)).round() as i32)
                })
                .collect();
            Ok(Arc::new(Int32Array::from(vals)))
        }
        DataType::Int64 => {
            let a = array.as_any().downcast_ref::<Int64Array>().unwrap();
            let vals: Vec<Option<i64>> = (0..a.len())
                .map(|i| {
                    if !a.is_valid(i) {
                        return None;
                    }
                    let v = a.value(i) as f64;
                    if rand::Rng::gen::<f64>(rng) >= probability {
                        return Some(v as i64);
                    }
                    let sd = if relative { stddev * v.abs() } else { stddev };
                    let dist = Normal::new(0.0, sd.max(1e-15)).unwrap();
                    Some((v + dist.sample(rng)).round() as i64)
                })
                .collect();
            Ok(Arc::new(Int64Array::from(vals)))
        }
        _ => Ok(arrow::array::make_array(array.to_data())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::{Field, Schema};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn float_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "val",
            DataType::Float64,
            true,
        )]));
        RecordBatch::try_new(schema, vec![Arc::new(Float64Array::from(vec![100.0; 50]))]).unwrap()
    }

    #[test]
    fn gaussian_noise_modifies_values() {
        let g = GaussianNoise::absolute(5.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(99);
        let result = g.perturb(float_batch(), &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        // At least some values should differ from 100.0
        let changed = (0..arr.len())
            .filter(|&i| (arr.value(i) - 100.0).abs() > 0.01)
            .count();
        assert!(changed > 0, "expected some values to change");
    }

    #[test]
    fn zero_probability_leaves_unchanged() {
        let g = GaussianNoise::absolute(5.0);
        let config = PerturbConfig::default().with_probability(0.0);
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let result = g.perturb(float_batch(), &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        for i in 0..arr.len() {
            assert!((arr.value(i) - 100.0).abs() < 1e-10);
        }
    }

    #[test]
    fn relative_noise_scales_with_value() {
        let g = GaussianNoise::relative(0.1); // 10% of value
        let config = PerturbConfig::default().with_probability(1.0);
        let schema = Arc::new(Schema::new(vec![Field::new(
            "val",
            DataType::Float64,
            true,
        )]));
        // Two groups: small values (1.0) and large values (1000.0)
        let vals: Vec<f64> = (0..50).map(|i| if i < 25 { 1.0 } else { 1000.0 }).collect();
        let batch = RecordBatch::try_new(schema, vec![Arc::new(Float64Array::from(vals))]).unwrap();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = g.perturb(batch, &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();

        // Large values should have larger deviations
        let small_dev: f64 = (0..25).map(|i| (arr.value(i) - 1.0).abs()).sum::<f64>() / 25.0;
        let large_dev: f64 = (25..50).map(|i| (arr.value(i) - 1000.0).abs()).sum::<f64>() / 25.0;
        assert!(
            large_dev > small_dev * 50.0,
            "relative noise should scale: small_dev={small_dev:.3}, large_dev={large_dev:.3}"
        );
    }

    #[test]
    fn gaussian_noise_int32_column() {
        let g = GaussianNoise::absolute(10.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let schema = Arc::new(Schema::new(vec![Field::new(
            "count",
            DataType::Int32,
            true,
        )]));
        let batch =
            RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(vec![100; 50]))]).unwrap();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = g.perturb(batch, &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        let changed = (0..arr.len()).filter(|&i| arr.value(i) != 100).count();
        assert!(changed > 0, "expected some int values to change");
    }

    #[test]
    fn gaussian_noise_skips_string_columns() {
        let g = GaussianNoise::absolute(5.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let schema = Arc::new(Schema::new(vec![
            Field::new("name", DataType::Utf8, true),
            Field::new("val", DataType::Float64, true),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["a", "b", "c"])),
                Arc::new(Float64Array::from(vec![10.0, 20.0, 30.0])),
            ],
        )
        .unwrap();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = g.perturb(batch, &mut rng, &config).unwrap();
        // String column unchanged
        let strs = result
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(strs.value(0), "a");
        assert_eq!(strs.value(1), "b");
        assert_eq!(strs.value(2), "c");
        // Float column SHOULD be noised
        let floats = result
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let changed = (0..3)
            .filter(|&i| {
                let orig = [10.0, 20.0, 30.0];
                (floats.value(i) - orig[i]).abs() > 0.01
            })
            .count();
        assert!(
            changed > 0,
            "numeric column should be noised in mixed-type batch"
        );
    }

    #[test]
    fn gaussian_noise_preserves_nulls() {
        let g = GaussianNoise::absolute(5.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let schema = Arc::new(Schema::new(vec![Field::new(
            "val",
            DataType::Float64,
            true,
        )]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Float64Array::from(vec![
                Some(1.0),
                None,
                Some(3.0),
            ]))],
        )
        .unwrap();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = g.perturb(batch, &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert!(arr.is_valid(0));
        assert!(!arr.is_valid(1), "null should remain null");
        assert!(arr.is_valid(2));
    }

    #[test]
    fn gaussian_noise_name_and_breaks() {
        let g = GaussianNoise::default();
        assert_eq!(g.name(), "GaussianNoise");
        assert_eq!(g.breaks(), InvariantSet::empty());
    }
}
