//! Sampling strategies for record batches.
//!
//! Supports three strategies: **full** (identity), **head** (first N rows),
//! and **reservoir** (uniform random single-pass).

use arrow::compute::concat_batches;
use arrow::record_batch::RecordBatch;
use rand::Rng;
use std::sync::Arc;
use tracing::debug;

use crate::error::LearnResult;

/// Sampling strategy to apply to ingested data.
#[derive(Debug, Clone)]
pub enum SamplingStrategy {
    /// Keep all rows.
    Full,
    /// Keep first `n` rows.
    Head(usize),
    /// Reservoir sampling: uniformly sample `n` rows in a single pass.
    Reservoir(usize),
}

/// Apply a sampling strategy to a set of record batches.
///
/// Returns a new `Vec<RecordBatch>` (typically a single batch) containing
/// the sampled rows.
///
/// # Errors
///
/// Returns `LearnError` if Arrow operations fail.
pub fn apply_sampling(
    batches: &[RecordBatch],
    strategy: &SamplingStrategy,
) -> LearnResult<Vec<RecordBatch>> {
    if batches.is_empty() {
        return Ok(vec![]);
    }

    match strategy {
        SamplingStrategy::Full => Ok(batches.to_vec()),
        SamplingStrategy::Head(n) => sample_head(batches, *n),
        SamplingStrategy::Reservoir(n) => sample_reservoir(batches, *n),
    }
}

/// Take the first `n` rows across batches.
fn sample_head(batches: &[RecordBatch], n: usize) -> LearnResult<Vec<RecordBatch>> {
    let schema = batches[0].schema();
    let combined = concat_batches(&schema, batches)?;
    let take = n.min(combined.num_rows());
    debug!(rows = take, "Head sampling");
    Ok(vec![combined.slice(0, take)])
}

/// Reservoir sampling (Algorithm R) over record batches.
fn sample_reservoir(batches: &[RecordBatch], n: usize) -> LearnResult<Vec<RecordBatch>> {
    let schema = batches[0].schema();
    let combined = concat_batches(&schema, batches)?;
    let total = combined.num_rows();

    if total <= n {
        debug!(rows = total, "Reservoir: dataset smaller than sample size");
        return Ok(vec![combined]);
    }

    // Build reservoir indices
    let mut reservoir: Vec<usize> = (0..n).collect();
    let mut rng = rand::thread_rng();

    for i in n..total {
        let j = rng.gen_range(0..=i);
        if j < n {
            reservoir[j] = i;
        }
    }

    reservoir.sort_unstable();
    debug!(sample_size = n, total, "Reservoir sampling");

    // Build index array for take
    let indices = arrow::array::UInt64Array::from(
        reservoir.iter().map(|&i| i as u64).collect::<Vec<_>>(),
    );
    let columns: Vec<_> = (0..combined.num_columns())
        .map(|c| arrow::compute::take(combined.column(c), &indices, None))
        .collect::<Result<Vec<_>, _>>()?;

    let batch = RecordBatch::try_new(Arc::new(combined.schema().as_ref().clone()), columns)?;
    Ok(vec![batch])
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int32Array, StringArray};
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    fn make_batch(n: usize) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int32, false),
            Field::new("name", DataType::Utf8, false),
        ]));
        let ids: Vec<i32> = (0..n as i32).collect();
        let names: Vec<String> = (0..n).map(|i| format!("row_{i}")).collect();
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(ids)),
                Arc::new(StringArray::from(
                    names.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
                )),
            ],
        )
        .unwrap()
    }

    #[test]
    fn full_returns_all() {
        let batch = make_batch(10);
        let result = apply_sampling(&[batch], &SamplingStrategy::Full).unwrap();
        let total: usize = result.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 10);
    }

    #[test]
    fn head_truncates() {
        let batch = make_batch(100);
        let result = apply_sampling(&[batch], &SamplingStrategy::Head(10)).unwrap();
        let total: usize = result.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 10);
    }

    #[test]
    fn head_on_small_data() {
        let batch = make_batch(5);
        let result = apply_sampling(&[batch], &SamplingStrategy::Head(100)).unwrap();
        let total: usize = result.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 5);
    }

    #[test]
    fn reservoir_correct_size() {
        let batch = make_batch(1000);
        let result = apply_sampling(&[batch], &SamplingStrategy::Reservoir(50)).unwrap();
        let total: usize = result.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 50);
    }

    #[test]
    fn reservoir_small_data() {
        let batch = make_batch(5);
        let result = apply_sampling(&[batch], &SamplingStrategy::Reservoir(100)).unwrap();
        let total: usize = result.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 5);
    }

    #[test]
    fn empty_batches() {
        let result = apply_sampling(&[], &SamplingStrategy::Full).unwrap();
        assert!(result.is_empty());
    }
}
