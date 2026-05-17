//! Temporal spike perturbator for timestamp columns.
//!
//! [`TemporalSpikeInjector`] clusters randomly selected timestamps around
//! spike points, simulating traffic bursts, event storms, or temporal
//! anomalies. It breaks the [`TYPE_RANGE`](crate::noise::InvariantSet::TYPE_RANGE)
//! invariant because spike values may exceed the original timestamp range.

use arrow::array::*;
use arrow::datatypes::{DataType, TimeUnit};
use arrow::record_batch::RecordBatch;
use rand::Rng;
use rand::RngExt;
use rand_distr::{Distribution, Normal};
use std::sync::Arc;
use tracing::trace;

use crate::noise::error::NoiseError;
use crate::noise::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

/// Cluster timestamps around randomly selected spike points.
///
/// For each eligible timestamp column, `spike_count` spike centers are chosen
/// uniformly from the column's observed range. Selected timestamps are then
/// replaced with values sampled from a normal distribution centered on a
/// randomly chosen spike, with standard deviation controlled by `spread_ms`.
///
/// # Configuration
///
/// - `spike_count`: Number of spike centers (default: 3)
/// - `spread_ms`: Standard deviation of the Gaussian cluster in milliseconds
///   (default: 60,000 = 1 minute)
///
/// Only Arrow `Timestamp` types are targeted; plain `Int64` is not affected
/// even if it represents epoch values.
#[derive(Debug, Clone)]
pub struct TemporalSpikeInjector {
    /// Number of spike centers to create.
    pub spike_count: usize,
    /// Standard deviation of spike clusters in milliseconds.
    pub spread_ms: f64,
}

impl Default for TemporalSpikeInjector {
    fn default() -> Self {
        Self {
            spike_count: 3,
            spread_ms: 60_000.0,
        }
    }
}

impl TemporalSpikeInjector {
    /// Create a new `TemporalSpikeInjector` with default settings.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the number of spike centers.
    pub fn with_spike_count(mut self, count: usize) -> Self {
        self.spike_count = count.max(1);
        self
    }

    /// Set the spread (standard deviation) in milliseconds.
    pub fn with_spread_ms(mut self, spread: f64) -> Self {
        self.spread_ms = spread.max(1.0);
        self
    }
}

impl Perturbator for TemporalSpikeInjector {
    fn name(&self) -> &str {
        "TemporalSpikeInjector"
    }

    fn breaks(&self) -> InvariantSet {
        InvariantSet::TYPE_RANGE
    }

    fn perturb(
        &self,
        batch: RecordBatch,
        rng: &mut dyn Rng,
        config: &PerturbConfig,
    ) -> Result<RecordBatch, NoiseError> {
        let schema = batch.schema();
        let mut columns: Vec<Arc<dyn Array>> = Vec::with_capacity(batch.num_columns());

        for (col_idx, field) in schema.fields().iter().enumerate() {
            let col = batch.column(col_idx);
            let eligible = matches!(col.data_type(), DataType::Timestamp(_, _))
                && match &config.columns {
                    ColumnFilter::All => true,
                    ColumnFilter::ByName(names) => names.iter().any(|c| c == field.name()),
                };

            if !eligible {
                columns.push(Arc::clone(col));
                continue;
            }

            let spiked = self.spike_timestamps(col, rng, config)?;
            trace!(
                column = field.name(),
                spikes = self.spike_count,
                "applied temporal spikes"
            );
            columns.push(spiked);
        }

        RecordBatch::try_new(schema, columns).map_err(NoiseError::Arrow)
    }
}

impl TemporalSpikeInjector {
    fn spike_timestamps(
        &self,
        col: &Arc<dyn Array>,
        rng: &mut dyn Rng,
        config: &PerturbConfig,
    ) -> Result<Arc<dyn Array>, NoiseError> {
        let (unit, tz) = match col.data_type() {
            DataType::Timestamp(u, t) => (*u, t.clone()),
            _ => return Ok(Arc::clone(col)),
        };

        // Convert to milliseconds for spike computation
        let millis: Vec<Option<i64>> = extract_millis(col, &unit);
        let n = millis.len();

        // Find range for spike center placement
        let (min_val, max_val) = millis.iter().fold((i64::MAX, i64::MIN), |(lo, hi), v| {
            if let Some(v) = v {
                (lo.min(*v), hi.max(*v))
            } else {
                (lo, hi)
            }
        });

        if min_val > max_val || n == 0 {
            return Ok(Arc::clone(col));
        }

        // For uniform columns (all same value), use that value as the sole spike center.
        let spike_centers: Vec<i64> = if min_val == max_val {
            vec![min_val; self.spike_count]
        } else {
            (0..self.spike_count)
                .map(|_| rng.random_range(min_val..=max_val))
                .collect()
        };

        let normal = Normal::new(0.0, self.spread_ms)
            .map_err(|e| NoiseError::InvalidConfig(format!("invalid spread: {e}")))?;

        // Scale spread for non-millisecond units
        let unit_factor = match unit {
            TimeUnit::Second => 1_000.0,
            TimeUnit::Millisecond => 1.0,
            TimeUnit::Microsecond => 0.001,
            TimeUnit::Nanosecond => 0.000_001,
        };

        // Apply spikes
        let result_millis: Vec<Option<i64>> = millis
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let ms = (*v)?;
                if !config.in_scope(i) || !rng.random_bool(config.probability.clamp(0.0, 1.0)) {
                    return Some(ms);
                }
                // Pick a random spike center
                let center = spike_centers[rng.random_range(0..spike_centers.len())];
                // Sample offset from normal distribution, scaled to unit
                let offset_ms = normal.sample(rng);
                let offset_native = (offset_ms / unit_factor) as i64;
                let spiked = center.saturating_add(offset_native);
                Some(spiked)
            })
            .collect();

        // Convert back to original unit
        build_timestamp_array(&result_millis, &unit, tz)
    }
}

fn extract_millis(col: &Arc<dyn Array>, unit: &TimeUnit) -> Vec<Option<i64>> {
    let n = col.len();
    (0..n)
        .map(|i| {
            if col.is_null(i) {
                return None;
            }
            match unit {
                TimeUnit::Second => {
                    let arr = col
                        .as_any()
                        .downcast_ref::<TimestampSecondArray>()
                        .expect("second timestamp column must downcast to TimestampSecondArray");
                    Some(arr.value(i))
                }
                TimeUnit::Millisecond => {
                    let arr = col
                        .as_any()
                        .downcast_ref::<TimestampMillisecondArray>()
                        .expect("millisecond timestamp column must downcast to TimestampMillisecondArray");
                    Some(arr.value(i))
                }
                TimeUnit::Microsecond => {
                    let arr = col
                        .as_any()
                        .downcast_ref::<TimestampMicrosecondArray>()
                        .expect("microsecond timestamp column must downcast to TimestampMicrosecondArray");
                    Some(arr.value(i))
                }
                TimeUnit::Nanosecond => {
                    let arr = col
                        .as_any()
                        .downcast_ref::<TimestampNanosecondArray>()
                        .expect("nanosecond timestamp column must downcast to TimestampNanosecondArray");
                    Some(arr.value(i))
                }
            }
        })
        .collect()
}

fn build_timestamp_array(
    values: &[Option<i64>],
    unit: &TimeUnit,
    tz: Option<Arc<str>>,
) -> Result<Arc<dyn Array>, NoiseError> {
    match unit {
        TimeUnit::Second => {
            let arr: TimestampSecondArray = values.iter().copied().collect();
            Ok(Arc::new(arr.with_timezone_opt(tz)))
        }
        TimeUnit::Millisecond => {
            let arr: TimestampMillisecondArray = values.iter().copied().collect();
            Ok(Arc::new(arr.with_timezone_opt(tz)))
        }
        TimeUnit::Microsecond => {
            let arr: TimestampMicrosecondArray = values.iter().copied().collect();
            Ok(Arc::new(arr.with_timezone_opt(tz)))
        }
        TimeUnit::Nanosecond => {
            let arr: TimestampNanosecondArray = values.iter().copied().collect();
            Ok(Arc::new(arr.with_timezone_opt(tz)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand::rngs::ChaCha8Rng;

    fn make_ts_batch(values: Vec<i64>) -> RecordBatch {
        let arr = TimestampMillisecondArray::from(values);
        RecordBatch::try_new(
            arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new(
                "ts",
                DataType::Timestamp(TimeUnit::Millisecond, None),
                true,
            )])
            .into(),
            vec![Arc::new(arr) as Arc<dyn Array>],
        )
        .unwrap()
    }

    #[test]
    fn spikes_cluster_around_centers() {
        // 100 evenly spaced timestamps over 24 hours
        let base = 1_700_000_000_000i64; // some epoch ms
        let values: Vec<i64> = (0..100).map(|i| base + i * 864_000).collect();
        let batch = make_ts_batch(values);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(0.5);
        let injector = TemporalSpikeInjector::new()
            .with_spike_count(2)
            .with_spread_ms(10_000.0);
        let result = injector.perturb(batch, &mut rng, &config).unwrap();

        let col = result
            .column(0)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        // At least some values should have changed
        let original_base = 1_700_000_000_000i64;
        let changed = (0..100)
            .filter(|&i| col.value(i) != original_base + i as i64 * 864_000)
            .count();
        assert!(changed > 0, "some timestamps should be spiked");
    }

    #[test]
    fn null_preserved() {
        let arr = TimestampMillisecondArray::from(vec![Some(1000), None, Some(3000)]);
        let batch = RecordBatch::try_new(
            arrow::datatypes::Schema::new(vec![arrow::datatypes::Field::new(
                "ts",
                DataType::Timestamp(TimeUnit::Millisecond, None),
                true,
            )])
            .into(),
            vec![Arc::new(arr) as Arc<dyn Array>],
        )
        .unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(1.0);
        let result = TemporalSpikeInjector::new()
            .perturb(batch, &mut rng, &config)
            .unwrap();

        assert!(result.column(0).is_null(1));
    }

    #[test]
    fn skips_non_timestamp_columns() {
        let schema = arrow::datatypes::Schema::new(vec![
            arrow::datatypes::Field::new("n", DataType::Int64, true),
            arrow::datatypes::Field::new(
                "ts",
                DataType::Timestamp(TimeUnit::Millisecond, None),
                true,
            ),
        ]);
        let batch = RecordBatch::try_new(
            schema.into(),
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3])) as Arc<dyn Array>,
                Arc::new(TimestampMillisecondArray::from(vec![1000, 2000, 3000])),
            ],
        )
        .unwrap();

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(1.0);
        let result = TemporalSpikeInjector::new()
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

    #[test]
    fn zero_probability_no_change() {
        let batch = make_ts_batch(vec![1000, 2000, 3000]);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let config = PerturbConfig::default().with_probability(0.0);
        let result = TemporalSpikeInjector::new()
            .perturb(batch, &mut rng, &config)
            .unwrap();

        let col = result
            .column(0)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        assert_eq!(col.value(0), 1000);
        assert_eq!(col.value(1), 2000);
        assert_eq!(col.value(2), 3000);
    }

    #[test]
    fn non_timestamp_columns_unchanged() {
        use rand::SeedableRng;
        use rand::rngs::StdRng;

        let schema = arrow::datatypes::Schema::new(vec![
            arrow::datatypes::Field::new("count", DataType::Int64, true),
            arrow::datatypes::Field::new("label", DataType::Utf8, true),
        ]);
        let batch = RecordBatch::try_new(
            schema.into(),
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3])) as Arc<dyn Array>,
                Arc::new(StringArray::from(vec!["a", "b", "c"])),
            ],
        )
        .unwrap();

        let mut rng = StdRng::seed_from_u64(42);
        let result = TemporalSpikeInjector::new()
            .perturb(batch, &mut rng, &PerturbConfig::default().with_probability(1.0))
            .unwrap();

        let counts = result
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap();
        let labels = result
            .column(1)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();

        assert_eq!(counts.values(), &[1, 2, 3]);
        assert_eq!((0..labels.len()).map(|i| labels.value(i)).collect::<Vec<_>>(), vec!["a", "b", "c"]);
    }

    #[test]
    fn with_spike_count_builder() {
        let injector = TemporalSpikeInjector::new().with_spike_count(7);

        assert_eq!(injector.spike_count, 7);
    }

    #[test]
    fn with_spread_ms_builder() {
        let injector = TemporalSpikeInjector::new().with_spread_ms(2_500.0);

        assert_eq!(injector.spread_ms, 2_500.0);
    }
}
