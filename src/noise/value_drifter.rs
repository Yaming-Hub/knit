//! Gradual numeric drift perturbator.
//!
//! [`ValueDrifter`] adds a monotonically increasing (or decreasing) bias to
//! numeric columns, simulating sensor drift or calibration errors over the
//! length of the batch. It breaks the
//! [`TYPE_RANGE`](crate::noise::InvariantSet::TYPE_RANGE) invariant when the
//! accumulated drift pushes values outside expected bounds.

use arrow::array::*;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use rand::Rng;
use rand::RngCore;
use std::sync::Arc;
use tracing::trace;

use crate::noise::error::NoiseError;
use crate::noise::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

/// Add gradual drift to numeric columns over the row axis.
///
/// `drift_per_row` is the amount of bias added per row. A positive value
/// produces upward drift; negative produces downward drift.
#[derive(Debug, Clone)]
pub struct ValueDrifter {
    /// Drift amount per row.
    pub drift_per_row: f64,
}

impl Default for ValueDrifter {
    fn default() -> Self {
        Self {
            drift_per_row: 0.01,
        }
    }
}

impl ValueDrifter {
    /// Create a value drifter with the specified per-row drift.
    pub fn new(drift_per_row: f64) -> Self {
        Self { drift_per_row }
    }
}

impl Perturbator for ValueDrifter {
    fn name(&self) -> &str {
        "ValueDrifter"
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

            let drifted = apply_drift(col.as_ref(), self.drift_per_row, config.probability, rng)?;
            trace!(
                column = field.name(),
                drift = self.drift_per_row,
                "applied value drift"
            );
            columns.push(drifted);
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

fn apply_drift(
    array: &dyn Array,
    drift_per_row: f64,
    probability: f64,
    rng: &mut dyn RngCore,
) -> Result<Arc<dyn Array>, NoiseError> {
    match array.data_type() {
        DataType::Float64 => {
            let a = array.as_any().downcast_ref::<Float64Array>().unwrap();
            let vals: Vec<Option<f64>> = (0..a.len())
                .map(|i| {
                    if !a.is_valid(i) {
                        return None;
                    }
                    if rng.gen::<f64>() >= probability {
                        return Some(a.value(i));
                    }
                    Some(a.value(i) + drift_per_row * i as f64)
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
                    if rng.gen::<f64>() >= probability {
                        return Some(a.value(i));
                    }
                    Some((a.value(i) as f64 + drift_per_row * i as f64).round() as i32)
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
                    if rng.gen::<f64>() >= probability {
                        return Some(a.value(i));
                    }
                    Some((a.value(i) as f64 + drift_per_row * i as f64).round() as i64)
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
        RecordBatch::try_new(schema, vec![Arc::new(Float64Array::from(vec![100.0; 5]))]).unwrap()
    }

    #[test]
    fn drift_increases_over_rows() {
        let d = ValueDrifter::new(1.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = d.perturb(float_batch(), &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        // Formula: value + drift_per_row * row_index
        for i in 0..5 {
            let expected = 100.0 + 1.0 * i as f64;
            assert!(
                (arr.value(i) - expected).abs() < 1e-10,
                "row {i}: expected {expected}, got {}",
                arr.value(i)
            );
        }
    }

    #[test]
    fn negative_drift_decreases_over_rows() {
        let d = ValueDrifter::new(-2.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = d.perturb(float_batch(), &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        for i in 0..5 {
            let expected = 100.0 + (-2.0) * i as f64;
            assert!(
                (arr.value(i) - expected).abs() < 1e-10,
                "row {i}: expected {expected}, got {}",
                arr.value(i)
            );
        }
    }

    #[test]
    fn drift_on_int32() {
        let d = ValueDrifter::new(1.5);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![Field::new("val", DataType::Int32, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(vec![10, 10, 10, 10]))],
        )
        .unwrap();
        let result = d.perturb(batch, &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        // Row 0: 10+0=10, Row 1: round(10+1.5)=12, Row 2: 10+3=13, Row 3: round(10+4.5)=15
        assert_eq!(arr.value(0), 10);
        assert_eq!(arr.value(1), 12); // round(11.5) = 12
        assert_eq!(arr.value(2), 13); // round(13.0) = 13
        assert_eq!(arr.value(3), 15); // round(14.5) = 15 (Rust rounds .5 away from zero)
    }

    #[test]
    fn drift_on_int64() {
        let d = ValueDrifter::new(10.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![Field::new("val", DataType::Int64, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int64Array::from(vec![1000i64, 1000, 1000]))],
        )
        .unwrap();
        let result = d.perturb(batch, &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(arr.value(0), 1000); // row 0: no drift
        assert_eq!(arr.value(1), 1010); // row 1: +10
        assert_eq!(arr.value(2), 1020); // row 2: +20
    }

    #[test]
    fn zero_probability_leaves_values_unchanged() {
        let d = ValueDrifter::new(100.0);
        let config = PerturbConfig::default().with_probability(0.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = d.perturb(float_batch(), &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        for i in 0..5 {
            assert!(
                (arr.value(i) - 100.0).abs() < 1e-10,
                "row {i} should be unchanged"
            );
        }
    }

    #[test]
    fn null_values_preserved() {
        let d = ValueDrifter::new(1.0);
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
        let result = d.perturb(batch, &mut rng, &config).unwrap();
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
        let d = ValueDrifter::new(1.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![
            Field::new("name", DataType::Utf8, false),
            Field::new("val", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["a", "b", "c"])),
                Arc::new(Float64Array::from(vec![1.0, 1.0, 1.0])),
            ],
        )
        .unwrap();
        let result = d.perturb(batch, &mut rng, &config).unwrap();
        let names = result
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(names.value(0), "a");
        assert_eq!(names.value(1), "b");
    }

    #[test]
    fn column_filter_by_name() {
        let d = ValueDrifter::new(100.0);
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
                Arc::new(Float64Array::from(vec![1.0, 1.0, 1.0])),
                Arc::new(Float64Array::from(vec![1.0, 1.0, 1.0])),
            ],
        )
        .unwrap();
        let result = d.perturb(batch, &mut rng, &config).unwrap();
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
        // Row 2: targeted should have drift 100*2 = 200 added
        assert!(
            (targeted.value(2) - 201.0).abs() < 1e-10,
            "targeted should drift"
        );
        assert!(
            (safe.value(2) - 1.0).abs() < 1e-10,
            "safe should be unchanged"
        );
    }

    #[test]
    fn breaks_type_range_invariant() {
        let d = ValueDrifter::default();
        assert_eq!(d.breaks(), InvariantSet::TYPE_RANGE);
        assert_eq!(d.name(), "ValueDrifter");
        assert!(
            (d.drift_per_row - 0.01).abs() < 1e-10,
            "default drift should be 0.01"
        );
    }
}
