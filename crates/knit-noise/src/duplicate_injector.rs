//! Row duplication perturbator.
//!
//! [`DuplicateInjector`] appends duplicate copies of randomly selected rows,
//! breaking the [`UNIQUE`](crate::InvariantSet::UNIQUE) invariant.

use arrow::array::*;
use arrow::record_batch::RecordBatch;
use rand::Rng;
use rand::RngCore;
use std::sync::Arc;
use tracing::trace;

use crate::error::NoiseError;
use crate::traits::{InvariantSet, PerturbConfig, Perturbator};

/// Append duplicate rows selected at random.
///
/// Each row has `config.probability` chance of being duplicated.
/// Duplicates are appended at the end of the batch.
#[derive(Debug, Clone, Default)]
pub struct DuplicateInjector;

impl DuplicateInjector {
    /// Create a new `DuplicateInjector`.
    pub fn new() -> Self {
        Self
    }
}

impl Perturbator for DuplicateInjector {
    fn name(&self) -> &str {
        "DuplicateInjector"
    }

    fn breaks(&self) -> InvariantSet {
        InvariantSet::UNIQUE
    }

    fn perturb(
        &self,
        batch: RecordBatch,
        rng: &mut dyn RngCore,
        config: &PerturbConfig,
    ) -> Result<RecordBatch, NoiseError> {
        let n = batch.num_rows();
        if n == 0 {
            return Ok(batch);
        }

        // Select rows to duplicate
        let mut dup_indices: Vec<usize> = Vec::new();
        for i in 0..n {
            if rng.gen::<f64>() < config.probability {
                dup_indices.push(i);
            }
        }

        if dup_indices.is_empty() {
            return Ok(batch);
        }

        trace!(count = dup_indices.len(), "duplicating rows");

        // Build index array for take
        let all_indices: Vec<u32> = (0..n as u32)
            .chain(dup_indices.iter().map(|&i| i as u32))
            .collect();

        let index_arr = UInt32Array::from(all_indices);
        let schema = batch.schema();

        let mut new_columns: Vec<Arc<dyn Array>> = Vec::with_capacity(batch.num_columns());
        for col_idx in 0..batch.num_columns() {
            let col = batch.column(col_idx);
            let taken = arrow::compute::take(col.as_ref(), &index_arr, None)?;
            new_columns.push(taken);
        }

        Ok(RecordBatch::try_new(schema, new_columns)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::{DataType, Field, Schema};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn sample_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int32, false)]));
        RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(vec![1, 2, 3, 4, 5]))],
        )
        .unwrap()
    }

    #[test]
    fn duplicate_injection_adds_rows() {
        let d = DuplicateInjector::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = d.perturb(sample_batch(), &mut rng, &config).unwrap();
        // Should have original 5 + 5 duplicates = 10
        assert_eq!(result.num_rows(), 10);
    }

    #[test]
    fn zero_probability_no_duplicates() {
        let d = DuplicateInjector::new();
        let config = PerturbConfig::default().with_probability(0.0);
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let result = d.perturb(sample_batch(), &mut rng, &config).unwrap();
        assert_eq!(result.num_rows(), 5);
    }

    #[test]
    fn duplicated_rows_match_originals() {
        let d = DuplicateInjector::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = d.perturb(sample_batch(), &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        // p=1.0: every row is duplicated, so result = originals ++ originals (same order)
        assert_eq!(arr.len(), 10);
        let originals: Vec<i32> = (0..5).map(|i| arr.value(i)).collect();
        let duplicates: Vec<i32> = (5..10).map(|i| arr.value(i)).collect();
        assert_eq!(originals, vec![1, 2, 3, 4, 5]);
        assert_eq!(
            duplicates,
            vec![1, 2, 3, 4, 5],
            "duplicates should match originals exactly"
        );
    }

    #[test]
    fn empty_batch_unchanged() {
        let d = DuplicateInjector::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int32, false)]));
        let batch =
            RecordBatch::try_new(schema, vec![Arc::new(Int32Array::from(Vec::<i32>::new()))])
                .unwrap();
        let result = d.perturb(batch, &mut rng, &config).unwrap();
        assert_eq!(result.num_rows(), 0);
    }

    #[test]
    fn partial_probability_adds_some_rows() {
        let d = DuplicateInjector::new();
        let config = PerturbConfig::default().with_probability(0.5);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        // Use larger batch for statistical assertions
        let schema = Arc::new(Schema::new(vec![Field::new("id", DataType::Int32, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from((0..100).collect::<Vec<i32>>()))],
        )
        .unwrap();
        let result = d.perturb(batch, &mut rng, &config).unwrap();
        // Should have 100 originals + ~50 duplicates (allow 130-170)
        assert!(
            result.num_rows() >= 130 && result.num_rows() <= 170,
            "expected ~150 rows, got {}",
            result.num_rows()
        );
    }

    #[test]
    fn duplicate_name_and_breaks() {
        let d = DuplicateInjector::new();
        assert_eq!(d.name(), "DuplicateInjector");
        assert_eq!(d.breaks(), InvariantSet::UNIQUE);
    }
}
