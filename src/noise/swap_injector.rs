//! Value swap perturbator for any column type.
//!
//! [`SwapInjector`] randomly swaps values between rows within the same column,
//! preserving the multiset of values but disrupting cross-column relationships.
//! Since all values remain valid (just reordered), it breaks no single-column
//! invariants — but may break **cross-column semantics** (e.g., name–age
//! correspondence) that are not modeled by [`InvariantSet`].

use arrow::array::*;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use rand::seq::SliceRandom;
use rand::Rng;
use rand::RngCore;
use std::sync::Arc;
use tracing::trace;

use crate::noise::error::NoiseError;
use crate::noise::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

/// Swap values between randomly selected row pairs within each column.
///
/// Each row has `config.probability` chance of participating in a swap.
/// Participating rows are paired sequentially (first with second, third with
/// fourth, etc.). If an odd number is selected, the last row is left unchanged.
///
/// # Invariants
///
/// No single-column invariants are broken — the multiset of values (including
/// nulls) is preserved. However, cross-column relationships may be disrupted.
#[derive(Debug, Clone, Default)]
pub struct SwapInjector;

impl SwapInjector {
    /// Create a new `SwapInjector`.
    pub fn new() -> Self {
        Self
    }
}

impl Perturbator for SwapInjector {
    fn name(&self) -> &str {
        "SwapInjector"
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
        let n = batch.num_rows();
        if n < 2 {
            return Ok(batch);
        }

        let mut columns: Vec<Arc<dyn Array>> = Vec::with_capacity(batch.num_columns());

        for (col_idx, field) in schema.fields().iter().enumerate() {
            let col = batch.column(col_idx);
            let eligible = match &config.columns {
                ColumnFilter::All => true,
                ColumnFilter::ByName(names) => names.iter().any(|c| c == field.name()),
            };

            if !eligible {
                columns.push(Arc::clone(col));
                continue;
            }

            // Select rows to swap (only from in-scope rows)
            let mut swap_indices: Vec<usize> = (0..n)
                .filter(|&i| config.in_scope(i) && rng.gen_bool(config.probability.clamp(0.0, 1.0)))
                .collect();
            swap_indices.shuffle(rng);
            // Pair up — drop last if odd
            let pair_count = swap_indices.len() / 2;

            if pair_count == 0 {
                columns.push(Arc::clone(col));
                continue;
            }

            let swapped = swap_array(col, &swap_indices, pair_count)?;
            trace!(
                column = field.name(),
                pairs = pair_count,
                "swapped value pairs"
            );
            columns.push(swapped);
        }

        RecordBatch::try_new(schema, columns).map_err(NoiseError::Arrow)
    }
}

/// Swap values at paired indices in an array.
fn swap_array(
    col: &Arc<dyn Array>,
    indices: &[usize],
    pair_count: usize,
) -> Result<Arc<dyn Array>, NoiseError> {
    // Build a permutation: identity except for swapped pairs
    let n = col.len();
    let mut perm: Vec<usize> = (0..n).collect();
    for p in 0..pair_count {
        let a = indices[p * 2];
        let b = indices[p * 2 + 1];
        perm[a] = b;
        perm[b] = a;
    }

    match col.data_type() {
        DataType::Boolean => swap_bool(col, &perm),
        _ => swap_via_take(col, &perm),
    }
}

fn swap_bool(col: &Arc<dyn Array>, perm: &[usize]) -> Result<Arc<dyn Array>, NoiseError> {
    let arr = col.as_any().downcast_ref::<BooleanArray>().unwrap();
    let result: BooleanArray = perm
        .iter()
        .map(|&i| {
            if arr.is_null(i) {
                None
            } else {
                Some(arr.value(i))
            }
        })
        .collect();
    Ok(Arc::new(result))
}

fn swap_via_take(col: &Arc<dyn Array>, perm: &[usize]) -> Result<Arc<dyn Array>, NoiseError> {
    let indices = UInt32Array::from(perm.iter().map(|&i| i as u32).collect::<Vec<_>>());
    arrow::compute::take(col.as_ref(), &indices, None)
        .map_err(NoiseError::Arrow)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    #[test]
    fn multiset_preserved() {
        let arr = Int64Array::from(vec![10, 20, 30, 40, 50]);
        let batch = RecordBatch::try_new(
            arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new(
                "x",
                DataType::Int64,
                true,
            )])
            .into(),
            vec![Arc::new(arr) as Arc<dyn Array>],
        )
        .unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(1.0);
        let result = SwapInjector::new().perturb(batch, &mut rng, &config).unwrap();

        let col = result
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        let mut vals: Vec<i64> = (0..col.len()).map(|i| col.value(i)).collect();
        vals.sort();
        assert_eq!(vals, vec![10, 20, 30, 40, 50]);
    }

    #[test]
    fn null_count_preserved() {
        let arr = Int64Array::from(vec![Some(1), None, Some(3), None, Some(5)]);
        let batch = RecordBatch::try_new(
            arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new(
                "x",
                DataType::Int64,
                true,
            )])
            .into(),
            vec![Arc::new(arr) as Arc<dyn Array>],
        )
        .unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(1.0);
        let result = SwapInjector::new().perturb(batch, &mut rng, &config).unwrap();

        assert_eq!(result.column(0).null_count(), 2);
    }

    #[test]
    fn string_swap() {
        let arr = StringArray::from(vec!["a", "b", "c", "d"]);
        let batch = RecordBatch::try_new(
            arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new(
                "s",
                DataType::Utf8,
                true,
            )])
            .into(),
            vec![Arc::new(arr) as Arc<dyn Array>],
        )
        .unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(99);
        let config = PerturbConfig::default().with_probability(1.0);
        let result = SwapInjector::new().perturb(batch, &mut rng, &config).unwrap();

        let col = result
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let mut vals: Vec<&str> = (0..col.len()).map(|i| col.value(i)).collect();
        vals.sort();
        assert_eq!(vals, vec!["a", "b", "c", "d"]);
    }

    #[test]
    fn zero_probability_no_change() {
        let arr = Int64Array::from(vec![1, 2, 3]);
        let batch = RecordBatch::try_new(
            arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new(
                "x",
                DataType::Int64,
                true,
            )])
            .into(),
            vec![Arc::new(arr) as Arc<dyn Array>],
        )
        .unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(0.0);
        let result = SwapInjector::new().perturb(batch, &mut rng, &config).unwrap();

        let col = result
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(col.values(), &[1, 2, 3]);
    }
}
