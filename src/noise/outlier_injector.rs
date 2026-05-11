//! Extreme-value replacement perturbator.
//!
//! [`OutlierInjector`] replaces selected numeric cells with extreme values
//! (very large or very small). It breaks the
//! [`TYPE_RANGE`](crate::noise::InvariantSet::TYPE_RANGE) invariant.

use arrow::array::*;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use rand::Rng;
use rand::RngCore;
use std::sync::Arc;
use tracing::trace;

use crate::noise::error::NoiseError;
use crate::noise::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

/// Replace random numeric cells with extreme outlier values.
///
/// The `multiplier` controls how extreme the outlier is relative to the
/// column's observed range.
#[derive(Debug, Clone)]
pub struct OutlierInjector {
    /// How many times the column range to use for outlier magnitude.
    pub multiplier: f64,
}

impl Default for OutlierInjector {
    fn default() -> Self {
        Self { multiplier: 10.0 }
    }
}

impl OutlierInjector {
    /// Create an outlier injector with the given multiplier.
    pub fn new(multiplier: f64) -> Self {
        Self { multiplier }
    }
}

impl Perturbator for OutlierInjector {
    fn name(&self) -> &str {
        "OutlierInjector"
    }

    fn breaks(&self) -> InvariantSet {
        InvariantSet::TYPE_RANGE
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

            if !is_numeric(field.data_type()) || !should_apply(field.name(), &config.columns) {
                columns.push(Arc::clone(col));
                continue;
            }

            let outlied = inject_outliers(col.as_ref(), rng, config, self.multiplier)?;
            trace!(column = field.name(), "injected outliers");
            columns.push(outlied);
        }

        Ok(RecordBatch::try_new(schema, columns)?)
    }
}

fn is_numeric(dt: &DataType) -> bool {
    matches!(dt, DataType::Int32 | DataType::Int64 | DataType::Float64)
}

fn should_apply(name: &str, filter: &ColumnFilter) -> bool {
    match filter {
        ColumnFilter::All => true,
        ColumnFilter::ByName(names) => names.iter().any(|n| n == name),
    }
}

fn inject_outliers(
    array: &dyn Array,
    rng: &mut dyn RngCore,
    config: &PerturbConfig,
    multiplier: f64,
) -> Result<Arc<dyn Array>, NoiseError> {
    let probability = config.probability;
    match array.data_type() {
        DataType::Float64 => {
            let a = array.as_any().downcast_ref::<Float64Array>().unwrap();
            let (min, max) = float_range(a);
            let range = (max - min).max(1.0);
            let vals: Vec<Option<f64>> = (0..a.len())
                .map(|i| {
                    if !a.is_valid(i) {
                        return None;
                    }
                    if config.in_scope(i) && rng.gen::<f64>() < probability {
                        let sign: f64 = if rng.gen_bool(0.5) { 1.0 } else { -1.0 };
                        Some(a.value(i) + sign * range * multiplier)
                    } else {
                        Some(a.value(i))
                    }
                })
                .collect();
            Ok(Arc::new(Float64Array::from(vals)))
        }
        DataType::Int32 => {
            let a = array.as_any().downcast_ref::<Int32Array>().unwrap();
            let (min, max) = int32_range(a);
            let range = ((max - min) as f64).max(1.0);
            let vals: Vec<Option<i32>> = (0..a.len())
                .map(|i| {
                    if !a.is_valid(i) {
                        return None;
                    }
                    if config.in_scope(i) && rng.gen::<f64>() < probability {
                        let sign: f64 = if rng.gen_bool(0.5) { 1.0 } else { -1.0 };
                        Some((a.value(i) as f64 + sign * range * multiplier) as i32)
                    } else {
                        Some(a.value(i))
                    }
                })
                .collect();
            Ok(Arc::new(Int32Array::from(vals)))
        }
        DataType::Int64 => {
            let a = array.as_any().downcast_ref::<Int64Array>().unwrap();
            let (min, max) = int64_range(a);
            let range = ((max - min) as f64).max(1.0);
            let vals: Vec<Option<i64>> = (0..a.len())
                .map(|i| {
                    if !a.is_valid(i) {
                        return None;
                    }
                    if config.in_scope(i) && rng.gen::<f64>() < probability {
                        let sign: f64 = if rng.gen_bool(0.5) { 1.0 } else { -1.0 };
                        Some((a.value(i) as f64 + sign * range * multiplier) as i64)
                    } else {
                        Some(a.value(i))
                    }
                })
                .collect();
            Ok(Arc::new(Int64Array::from(vals)))
        }
        _ => Ok(arrow::array::make_array(array.to_data())),
    }
}

fn float_range(a: &Float64Array) -> (f64, f64) {
    let mut min = f64::MAX;
    let mut max = f64::MIN;
    let mut found = false;
    for i in 0..a.len() {
        if a.is_valid(i) {
            min = min.min(a.value(i));
            max = max.max(a.value(i));
            found = true;
        }
    }
    if !found {
        return (0.0, 0.0);
    }
    (min, max)
}

fn int32_range(a: &Int32Array) -> (i32, i32) {
    let mut min = i32::MAX;
    let mut max = i32::MIN;
    let mut found = false;
    for i in 0..a.len() {
        if a.is_valid(i) {
            min = min.min(a.value(i));
            max = max.max(a.value(i));
            found = true;
        }
    }
    if !found {
        return (0, 0);
    }
    (min, max)
}

fn int64_range(a: &Int64Array) -> (i64, i64) {
    let mut min = i64::MAX;
    let mut max = i64::MIN;
    let mut found = false;
    for i in 0..a.len() {
        if a.is_valid(i) {
            min = min.min(a.value(i));
            max = max.max(a.value(i));
            found = true;
        }
    }
    if !found {
        return (0, 0);
    }
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::{Field, Schema};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn num_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "val",
            DataType::Float64,
            true,
        )]));
        RecordBatch::try_new(
            schema,
            vec![Arc::new(Float64Array::from(vec![
                10.0, 20.0, 30.0, 40.0, 50.0,
            ]))],
        )
        .unwrap()
    }

    #[test]
    fn outlier_injection_produces_extreme_values() {
        let o = OutlierInjector::new(100.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = o.perturb(num_batch(), &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        // All values should be far from original range [10, 50]
        let extreme = (0..arr.len())
            .filter(|&i| arr.value(i) < 0.0 || arr.value(i) > 100.0)
            .count();
        assert!(extreme > 0, "expected extreme values");
    }

    #[test]
    fn outlier_magnitude_scales_with_multiplier() {
        // range = 50-10 = 40, multiplier=100 → offset = ±4000
        let o = OutlierInjector::new(100.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = o.perturb(num_batch(), &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        for i in 0..arr.len() {
            let deviation = (arr.value(i) - 30.0).abs(); // ~center of [10,50]
            assert!(
                deviation > 100.0,
                "row {i}: value {} should be far from center",
                arr.value(i)
            );
        }
    }

    #[test]
    fn outlier_on_int32() {
        let o = OutlierInjector::new(50.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![Field::new("val", DataType::Int32, false)]));
        let batch =
            RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(vec![10, 20, 30]))])
                .unwrap();
        let result = o.perturb(batch, &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        // range=20, multiplier=50 → offset=±1000, so values far from [10,30]
        let any_extreme = (0..arr.len()).any(|i| arr.value(i) < -100 || arr.value(i) > 200);
        assert!(any_extreme, "expected extreme int32 values");
    }

    #[test]
    fn outlier_on_int64() {
        let o = OutlierInjector::new(50.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![Field::new("val", DataType::Int64, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int64Array::from(vec![100i64, 200, 300]))],
        )
        .unwrap();
        let result = o.perturb(batch, &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        let any_extreme = (0..arr.len()).any(|i| arr.value(i) < -500 || arr.value(i) > 1000);
        assert!(any_extreme, "expected extreme int64 values");
    }

    #[test]
    fn zero_probability_leaves_values_unchanged() {
        let o = OutlierInjector::new(100.0);
        let config = PerturbConfig::default().with_probability(0.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = o.perturb(num_batch(), &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let originals = [10.0, 20.0, 30.0, 40.0, 50.0];
        for i in 0..5 {
            assert!(
                (arr.value(i) - originals[i]).abs() < 1e-10,
                "row {i} should be unchanged"
            );
        }
    }

    #[test]
    fn null_values_preserved() {
        let o = OutlierInjector::new(100.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![Field::new(
            "val",
            DataType::Float64,
            true,
        )]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Float64Array::from(vec![
                Some(10.0),
                None,
                Some(30.0),
            ]))],
        )
        .unwrap();
        let result = o.perturb(batch, &mut rng, &config).unwrap();
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
    fn non_numeric_columns_skipped() {
        let o = OutlierInjector::new(100.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![
            Field::new("name", DataType::Utf8, false),
            Field::new("val", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["a", "b"])),
                Arc::new(Float64Array::from(vec![10.0, 20.0])),
            ],
        )
        .unwrap();
        let result = o.perturb(batch, &mut rng, &config).unwrap();
        let names = result
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(names.value(0), "a", "string column should be untouched");
        assert_eq!(names.value(1), "b");
    }

    #[test]
    fn column_filter_by_name() {
        let o = OutlierInjector::new(100.0);
        let config = PerturbConfig::default()
            .with_probability(1.0)
            .with_columns(vec!["targeted".to_string()]);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![
            Field::new("targeted", DataType::Float64, false),
            Field::new("safe", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Float64Array::from(vec![10.0, 20.0])),
                Arc::new(Float64Array::from(vec![10.0, 20.0])),
            ],
        )
        .unwrap();
        let result = o.perturb(batch, &mut rng, &config).unwrap();
        let targeted = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let safe = result
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        // Targeted column should have extreme outliers
        let any_changed =
            (0..targeted.len()).any(|i| (targeted.value(i) - [10.0, 20.0][i]).abs() > 1.0);
        assert!(any_changed, "targeted column should have outliers injected");
        // Safe column should be untouched
        assert!(
            (safe.value(0) - 10.0).abs() < 1e-10,
            "safe col should be untouched"
        );
        assert!(
            (safe.value(1) - 20.0).abs() < 1e-10,
            "safe col should be untouched"
        );
    }

    #[test]
    fn constant_array_uses_min_range_of_one() {
        // All same values → range=0, but code uses max(range, 1.0)
        // Expected offset = max(0,1.0) * 10.0 = 10.0, so values = 5 ± 10
        let o = OutlierInjector::new(10.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![Field::new(
            "val",
            DataType::Float64,
            false,
        )]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Float64Array::from(vec![5.0, 5.0, 5.0]))],
        )
        .unwrap();
        let result = o.perturb(batch, &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        let expected_offset = 1.0 * 10.0; // max(range=0, 1.0) * multiplier
        for i in 0..arr.len() {
            let deviation = (arr.value(i) - 5.0).abs();
            assert!(
                (deviation - expected_offset).abs() < 1e-10,
                "row {i}: expected deviation {expected_offset}, got {deviation} (value={})",
                arr.value(i)
            );
        }
    }

    #[test]
    fn breaks_type_range_invariant() {
        let o = OutlierInjector::default();
        assert_eq!(o.breaks(), InvariantSet::TYPE_RANGE);
        assert_eq!(o.name(), "OutlierInjector");
        assert!(
            (o.multiplier - 10.0).abs() < 1e-10,
            "default multiplier should be 10.0"
        );
    }
}