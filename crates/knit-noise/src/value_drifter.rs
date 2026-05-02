//! Gradual numeric drift perturbator.
//!
//! [`ValueDrifter`] adds a monotonically increasing (or decreasing) bias to
//! numeric columns, simulating sensor drift or calibration errors over the
//! length of the batch. It breaks the
//! [`TYPE_RANGE`](crate::InvariantSet::TYPE_RANGE) invariant when the
//! accumulated drift pushes values outside expected bounds.

use arrow::array::*;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use rand::Rng;
use rand::RngCore;
use std::sync::Arc;
use tracing::trace;

use crate::error::NoiseError;
use crate::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

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
        Self { drift_per_row: 0.01 }
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

            if !is_numeric(field.data_type())
                || !should_apply(field.name(), &config.columns)
            {
                columns.push(Arc::clone(col));
                continue;
            }

            let drifted = apply_drift(col.as_ref(), self.drift_per_row, config.probability, rng)?;
            trace!(column = field.name(), drift = self.drift_per_row, "applied value drift");
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

fn apply_drift(array: &dyn Array, drift_per_row: f64, probability: f64, rng: &mut dyn RngCore) -> Result<Arc<dyn Array>, NoiseError> {
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
        let schema = Arc::new(Schema::new(vec![
            Field::new("val", DataType::Float64, true),
        ]));
        RecordBatch::try_new(
            schema,
            vec![Arc::new(Float64Array::from(vec![100.0; 5]))],
        )
        .unwrap()
    }

    #[test]
    fn drift_increases_over_rows() {
        let d = ValueDrifter::new(1.0);
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = d.perturb(float_batch(), &mut rng, &config).unwrap();
        let arr = result.column(0).as_any().downcast_ref::<Float64Array>().unwrap();
        // Row 0: 100 + 0 = 100, Row 4: 100 + 4 = 104
        assert!((arr.value(0) - 100.0).abs() < 1e-10);
        assert!((arr.value(4) - 104.0).abs() < 1e-10);
    }
}
