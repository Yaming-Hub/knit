//! Foreign-key violation perturbator.
//!
//! [`FkViolateInjector`] replaces values in targeted columns with values that
//! are guaranteed to be absent from the referenced domain, breaking referential
//! integrity. Because the noise pipeline operates on Arrow [`RecordBatch`]es
//! without FK metadata, this injector should be configured with
//! [`ColumnFilter::ByName`](crate::noise::ColumnFilter::ByName) to target
//! specific FK columns.

use arrow::array::*;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use rand::Rng;
use rand::RngCore;
use std::sync::Arc;
use tracing::trace;

use crate::noise::error::NoiseError;
use crate::noise::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

/// Replace FK values with references that are unlikely to exist in the parent table.
///
/// For integer columns, selected cells are replaced with `observed_max + offset`
/// where offset is a random value in `[1, 1_000_000]`. For string columns,
/// values are replaced with `"INVALID_FK_{random_hex}"`.
///
/// # Usage
///
/// This injector works best when targeted at specific FK columns via
/// `PerturbConfig::with_columns()`, since the noise pipeline does not carry
/// FK relationship metadata.
///
/// ```ignore
/// let config = PerturbConfig::default()
///     .with_probability(0.1)
///     .with_columns(vec!["user_id".into(), "order_id".into()]);
/// ```
#[derive(Debug, Clone, Default)]
pub struct FkViolateInjector;

impl FkViolateInjector {
    /// Create a new `FkViolateInjector`.
    pub fn new() -> Self {
        Self
    }
}

impl Perturbator for FkViolateInjector {
    fn name(&self) -> &str {
        "FkViolateInjector"
    }

    fn breaks(&self) -> InvariantSet {
        InvariantSet::FK_INTEGRITY
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
            let eligible = matches!(col.data_type(), DataType::Int64 | DataType::Utf8)
                && match &config.columns {
                    ColumnFilter::All => true,
                    ColumnFilter::ByName(names) => names.iter().any(|c| c == field.name()),
                };

            if !eligible {
                columns.push(Arc::clone(col));
                continue;
            }

            match col.data_type() {
                DataType::Int64 => {
                    let arr = col.as_any().downcast_ref::<Int64Array>().unwrap();
                    let observed_max = (0..arr.len())
                        .filter(|&i| !arr.is_null(i))
                        .map(|i| arr.value(i))
                        .max()
                        .unwrap_or(0);

                    let mut count = 0usize;
                    let result: Int64Array = (0..arr.len())
                        .map(|i| {
                            if arr.is_null(i) {
                                return None;
                            }
                            if !config.in_scope(i)
                                || !rng.gen_bool(config.probability.clamp(0.0, 1.0))
                            {
                                return Some(arr.value(i));
                            }
                            let offset = rng.gen_range(1i64..=1_000_000);
                            count += 1;
                            Some(observed_max.saturating_add(offset))
                        })
                        .collect();

                    trace!(
                        column = field.name(),
                        violated = count,
                        "FK violation (int)"
                    );
                    columns.push(Arc::new(result));
                }
                DataType::Utf8 => {
                    let arr = col.as_any().downcast_ref::<StringArray>().unwrap();
                    let mut count = 0usize;
                    let result: StringArray = (0..arr.len())
                        .map(|i| {
                            if arr.is_null(i) {
                                return None;
                            }
                            if !config.in_scope(i)
                                || !rng.gen_bool(config.probability.clamp(0.0, 1.0))
                            {
                                return Some(arr.value(i).to_string());
                            }
                            count += 1;
                            Some(format!("INVALID_FK_{:08x}", rng.next_u32()))
                        })
                        .collect();

                    trace!(
                        column = field.name(),
                        violated = count,
                        "FK violation (string)"
                    );
                    columns.push(Arc::new(result));
                }
                _ => {
                    columns.push(Arc::clone(col));
                }
            }
        }

        RecordBatch::try_new(schema, columns).map_err(NoiseError::Arrow)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn int_fk_violation_exceeds_max() {
        let arr = Int64Array::from(vec![1, 2, 3, 4, 5]);
        let batch = RecordBatch::try_new(
            arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new(
                "user_id",
                DataType::Int64,
                true,
            )])
            .into(),
            vec![Arc::new(arr) as Arc<dyn Array>],
        )
        .unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default()
            .with_probability(1.0)
            .with_columns(vec!["user_id".into()]);
        let result = FkViolateInjector::new()
            .perturb(batch, &mut rng, &config)
            .unwrap();

        let col = result
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        for i in 0..col.len() {
            assert!(col.value(i) > 5, "violated FK should exceed max");
        }
    }

    #[test]
    fn string_fk_violation_prefix() {
        let arr = StringArray::from(vec!["user_1", "user_2", "user_3"]);
        let batch = RecordBatch::try_new(
            arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new(
                "ref",
                DataType::Utf8,
                true,
            )])
            .into(),
            vec![Arc::new(arr) as Arc<dyn Array>],
        )
        .unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default()
            .with_probability(1.0)
            .with_columns(vec!["ref".into()]);
        let result = FkViolateInjector::new()
            .perturb(batch, &mut rng, &config)
            .unwrap();

        let col = result
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        for i in 0..col.len() {
            assert!(
                col.value(i).starts_with("INVALID_FK_"),
                "violated FK should have INVALID_FK_ prefix"
            );
        }
    }

    #[test]
    fn null_preserved() {
        let arr = Int64Array::from(vec![Some(1), None, Some(3)]);
        let batch = RecordBatch::try_new(
            arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new(
                "id",
                DataType::Int64,
                true,
            )])
            .into(),
            vec![Arc::new(arr) as Arc<dyn Array>],
        )
        .unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default()
            .with_probability(1.0)
            .with_columns(vec!["id".into()]);
        let result = FkViolateInjector::new()
            .perturb(batch, &mut rng, &config)
            .unwrap();

        assert!(result.column(0).is_null(1));
    }

    #[test]
    fn unfiltered_columns_unchanged() {
        let schema = arrow::datatypes::Schema::new(vec![
            arrow::datatypes::Field::new("fk_col", DataType::Int64, true),
            arrow::datatypes::Field::new("data_col", DataType::Int64, true),
        ]);
        let batch = RecordBatch::try_new(
            schema.into(),
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3])) as Arc<dyn Array>,
                Arc::new(Int64Array::from(vec![100, 200, 300])),
            ],
        )
        .unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default()
            .with_probability(1.0)
            .with_columns(vec!["fk_col".into()]);
        let result = FkViolateInjector::new()
            .perturb(batch, &mut rng, &config)
            .unwrap();

        // data_col should be unchanged
        let data = result
            .column(1)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        assert_eq!(data.values(), &[100, 200, 300]);
    }
}
