//! Random null injection perturbator.
//!
//! [`NullInjector`] replaces randomly selected cells with null values.
//! It breaks the [`NOT_NULL`](crate::InvariantSet::NOT_NULL) invariant.

use arrow::array::*;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use rand::Rng;
use rand::RngCore;
use std::sync::Arc;
use tracing::trace;

use crate::error::NoiseError;
use crate::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

/// Inject random null values into eligible columns.
///
/// For each nullable column, every cell has a `config.probability` chance of
/// being replaced with null. Non-nullable columns are skipped.
#[derive(Debug, Clone, Default)]
pub struct NullInjector;

impl NullInjector {
    /// Create a new `NullInjector`.
    pub fn new() -> Self {
        Self
    }
}

impl Perturbator for NullInjector {
    fn name(&self) -> &str {
        "NullInjector"
    }

    fn breaks(&self) -> InvariantSet {
        InvariantSet::NOT_NULL
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

            if !should_apply(field.name(), &config.columns) || !field.is_nullable() {
                columns.push(Arc::clone(col));
                continue;
            }

            let nulls = inject_nulls(col.as_ref(), rng, config.probability)?;
            trace!(column = field.name(), "injected nulls");
            columns.push(nulls);
        }

        Ok(RecordBatch::try_new(schema, columns)?)
    }
}

/// Build a null bitmap and reconstruct the array with extra nulls.
fn inject_nulls(
    array: &dyn Array,
    rng: &mut dyn RngCore,
    probability: f64,
) -> Result<Arc<dyn Array>, NoiseError> {
    let len = array.len();
    let mut null_buf = vec![true; len];
    for item in null_buf.iter_mut() {
        if rng.gen::<f64>() < probability {
            *item = false;
        }
    }

    // Combine existing null bitmap with our injected nulls
    if let Some(existing) = array.nulls() {
        for (i, item) in null_buf.iter_mut().enumerate() {
            if !existing.is_valid(i) {
                *item = false;
            }
        }
    }

    // Rebuild by data type
    match array.data_type() {
        DataType::Int32 => {
            let a = array.as_any().downcast_ref::<Int32Array>().unwrap();
            let vals: Vec<Option<i32>> = (0..len)
                .map(|i| if null_buf[i] { a.is_valid(i).then(|| a.value(i)) } else { None })
                .collect();
            Ok(Arc::new(Int32Array::from(vals)))
        }
        DataType::Int64 => {
            let a = array.as_any().downcast_ref::<Int64Array>().unwrap();
            let vals: Vec<Option<i64>> = (0..len)
                .map(|i| if null_buf[i] { a.is_valid(i).then(|| a.value(i)) } else { None })
                .collect();
            Ok(Arc::new(Int64Array::from(vals)))
        }
        DataType::Float64 => {
            let a = array.as_any().downcast_ref::<Float64Array>().unwrap();
            let vals: Vec<Option<f64>> = (0..len)
                .map(|i| if null_buf[i] { a.is_valid(i).then(|| a.value(i)) } else { None })
                .collect();
            Ok(Arc::new(Float64Array::from(vals)))
        }
        DataType::Utf8 => {
            let a = array.as_any().downcast_ref::<StringArray>().unwrap();
            let vals: Vec<Option<&str>> = (0..len)
                .map(|i| if null_buf[i] { a.is_valid(i).then(|| a.value(i)) } else { None })
                .collect();
            Ok(Arc::new(StringArray::from(vals)))
        }
        DataType::Boolean => {
            let a = array.as_any().downcast_ref::<BooleanArray>().unwrap();
            let vals: Vec<Option<bool>> = (0..len)
                .map(|i| if null_buf[i] { a.is_valid(i).then(|| a.value(i)) } else { None })
                .collect();
            Ok(Arc::new(BooleanArray::from(vals)))
        }
        _ => {
            // Unsupported type — pass through unchanged
            Ok(arrow::array::make_array(array.to_data()))
        }
    }
}

fn should_apply(name: &str, filter: &ColumnFilter) -> bool {
    match filter {
        ColumnFilter::All => true,
        ColumnFilter::ByName(names) => names.iter().any(|n| n == name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::{Field, Schema};
    use rand::SeedableRng;

    fn sample_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int32, true),
            Field::new("name", DataType::Utf8, true),
        ]));
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(vec![1, 2, 3, 4, 5])),
                Arc::new(StringArray::from(vec!["a", "b", "c", "d", "e"])),
            ],
        )
        .unwrap()
    }

    #[test]
    fn null_injection_with_high_probability() {
        let injector = NullInjector::new();
        let config = PerturbConfig::default().with_probability(1.0).with_seed(42);
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(42);
        let result = injector.perturb(sample_batch(), &mut rng, &config).unwrap();
        // All cells should be null
        assert_eq!(result.column(0).null_count(), 5);
        assert_eq!(result.column(1).null_count(), 5);
    }

    #[test]
    fn null_injection_with_zero_probability() {
        let injector = NullInjector::new();
        let config = PerturbConfig::default().with_probability(0.0).with_seed(0);
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(0);
        let result = injector.perturb(sample_batch(), &mut rng, &config).unwrap();
        assert_eq!(result.column(0).null_count(), 0);
        assert_eq!(result.column(1).null_count(), 0);
    }
}
