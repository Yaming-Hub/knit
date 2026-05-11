//! String truncation perturbator.
//!
//! [`TruncateInjector`] shortens string values at random character boundaries,
//! simulating data truncation from field-length limits, ETL pipeline bugs, or
//! storage corruption. It breaks [`FORMAT`](crate::noise::InvariantSet::FORMAT)
//! and [`UNIQUE`](crate::noise::InvariantSet::UNIQUE) invariants (truncation
//! can collapse distinct strings into duplicates).

use arrow::array::*;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use rand::Rng;
use rand::RngCore;
use std::sync::Arc;
use tracing::trace;

use crate::noise::error::NoiseError;
use crate::noise::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

/// Truncate string values at random character positions.
///
/// For each eligible string cell, the value is shortened to a random length
/// in `[1, char_count - 1]`. Empty strings and single-character strings are
/// left unchanged. Truncation operates on UTF-8 character boundaries.
#[derive(Debug, Clone, Default)]
pub struct TruncateInjector;

impl TruncateInjector {
    /// Create a new `TruncateInjector`.
    pub fn new() -> Self {
        Self
    }
}

impl Perturbator for TruncateInjector {
    fn name(&self) -> &str {
        "TruncateInjector"
    }

    fn breaks(&self) -> InvariantSet {
        InvariantSet::FORMAT | InvariantSet::UNIQUE
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
            let eligible = matches!(col.data_type(), DataType::Utf8)
                && match &config.columns {
                    ColumnFilter::All => true,
                    ColumnFilter::ByName(names) => names.iter().any(|c| c == field.name()),
                };

            if !eligible {
                columns.push(Arc::clone(col));
                continue;
            }

            let arr = col.as_any().downcast_ref::<StringArray>().unwrap();
            let mut count = 0usize;
            let result: StringArray = (0..arr.len())
                .map(|i| {
                    if arr.is_null(i) {
                        return None;
                    }
                    let s = arr.value(i);
                    let char_count = s.chars().count();
                    // Need at least 2 chars to truncate
                    if char_count < 2
                        || !config.in_scope(i)
                        || !rng.gen_bool(config.probability.clamp(0.0, 1.0))
                    {
                        return Some(s.to_string());
                    }
                    // Truncate to [1, char_count - 1] characters
                    let new_len = rng.gen_range(1..char_count);
                    let truncated: String = s.chars().take(new_len).collect();
                    count += 1;
                    Some(truncated)
                })
                .collect();

            trace!(column = field.name(), truncated = count, "truncated strings");
            columns.push(Arc::new(result));
        }

        RecordBatch::try_new(schema, columns).map_err(NoiseError::Arrow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn make_string_batch(values: Vec<&str>) -> RecordBatch {
        let arr = StringArray::from(values);
        RecordBatch::try_new(
            arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new(
                "s",
                DataType::Utf8,
                true,
            )])
            .into(),
            vec![Arc::new(arr) as Arc<dyn Array>],
        )
        .unwrap()
    }

    #[test]
    fn truncated_strings_shorter() {
        let batch = make_string_batch(vec!["hello", "world", "testing", "abcdef"]);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(1.0);
        let result = TruncateInjector::new()
            .perturb(batch, &mut rng, &config)
            .unwrap();

        let col = result
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for i in 0..col.len() {
            let original_len = ["hello", "world", "testing", "abcdef"][i].chars().count();
            let new_len = col.value(i).chars().count();
            assert!(new_len >= 1 && new_len < original_len);
        }
    }

    #[test]
    fn empty_and_single_char_unchanged() {
        let batch = make_string_batch(vec!["", "x", "ab"]);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(1.0);
        let result = TruncateInjector::new()
            .perturb(batch, &mut rng, &config)
            .unwrap();

        let col = result
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(col.value(0), "");
        assert_eq!(col.value(1), "x");
        // "ab" could be truncated to "a"
        assert!(col.value(2).len() >= 1);
    }

    #[test]
    fn utf8_safe_truncation() {
        // Multi-byte UTF-8 characters
        let batch = make_string_batch(vec!["héllo", "日本語テスト", "café"]);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(1.0);
        let result = TruncateInjector::new()
            .perturb(batch, &mut rng, &config)
            .unwrap();

        let col = result
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        // All results should be valid UTF-8 (StringArray enforces this)
        for i in 0..col.len() {
            assert!(col.value(i).chars().count() >= 1);
        }
    }

    #[test]
    fn null_preserved() {
        let arr = StringArray::from(vec![Some("hello"), None, Some("world")]);
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

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(1.0);
        let result = TruncateInjector::new()
            .perturb(batch, &mut rng, &config)
            .unwrap();

        assert!(result.column(0).is_null(1));
    }

    #[test]
    fn skips_non_string_columns() {
        let schema = arrow::datatypes::Schema::new(vec![
            arrow::datatypes::Field::new("n", DataType::Int64, true),
            arrow::datatypes::Field::new("s", DataType::Utf8, true),
        ]);
        let batch = RecordBatch::try_new(
            schema.into(),
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3])) as Arc<dyn Array>,
                Arc::new(StringArray::from(vec!["hello", "world", "test"])),
            ],
        )
        .unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(1.0);
        let result = TruncateInjector::new()
            .perturb(batch, &mut rng, &config)
            .unwrap();

        // Int column should be unchanged
        let ints = result
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(ints.values(), &[1, 2, 3]);
    }
}