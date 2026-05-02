//! Temporal generators — timestamps with relative offsets, time-series patterns,
//! and business-hour constraints.
//!
//! Three generators are provided:
//!
//! - [`RelativeGenerator`] — adds a random offset to an existing timestamp column.
//! - [`TimeSeriesGenerator`] — synthesises timestamps with trend + seasonality + noise.
//! - [`BusinessHoursGenerator`] — constrains timestamps to specified working hours.

use std::collections::BTreeMap;
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, TimestampMillisecondArray};
use arrow::datatypes::DataType;
use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use rand::RngCore;
use rand_distr::{Distribution, Normal, Uniform};

use crate::context::GenContext;
use crate::traits::FieldGenerator;

// ── TemporalUnit ────────────────────────────────────────────────────

/// Unit for temporal offsets.
#[derive(Debug, Clone, Copy)]
pub enum TemporalUnit {
    /// 1 second = 1 000 ms.
    Seconds,
    /// 1 minute = 60 000 ms.
    Minutes,
    /// 1 hour = 3 600 000 ms.
    Hours,
    /// 1 day = 86 400 000 ms.
    Days,
}

impl TemporalUnit {
    /// Conversion factor to milliseconds.
    pub fn to_millis(self) -> i64 {
        match self {
            Self::Seconds => 1_000,
            Self::Minutes => 60_000,
            Self::Hours => 3_600_000,
            Self::Days => 86_400_000,
        }
    }

    /// Parse from a string parameter value (case-insensitive).
    pub fn from_param(v: f64) -> Self {
        match v as i64 {
            1 => Self::Minutes,
            2 => Self::Hours,
            3 => Self::Days,
            _ => Self::Seconds,
        }
    }
}

// ── SeasonalityComponent ────────────────────────────────────────────

/// A single sinusoidal seasonality term used by [`TimeSeriesGenerator`].
#[derive(Debug, Clone)]
pub struct SeasonalityComponent {
    /// Period in milliseconds.
    pub period_ms: i64,
    /// Relative amplitude (0–1).
    pub amplitude: f64,
    /// Phase offset (0–1, fraction of period).
    pub phase: f64,
}

// ── RelativeGenerator ───────────────────────────────────────────────

/// Generates timestamps relative to an existing column by adding a random offset.
///
/// The base field must already exist in [`GenContext::batch_columns`] and be
/// castable to `TimestampMillisecond`. The offset is drawn from a normal
/// distribution parameterised by `offset_mean` and `offset_std` in the chosen
/// [`TemporalUnit`].
///
/// # Output
///
/// `DataType::Timestamp(Millisecond, None)` — the resulting timestamps are
/// always ≥ the base value (offsets are clamped to zero on the low end).
pub struct RelativeGenerator {
    /// Field name to read base timestamps from.
    base_field: String,
    /// Mean offset in the specified unit.
    offset_mean: f64,
    /// Std-dev of the offset in the specified unit.
    offset_std: f64,
    /// Unit for interpreting offset values.
    unit: TemporalUnit,
}

impl RelativeGenerator {
    /// Create a new `RelativeGenerator` from plan parameters.
    ///
    /// Expected keys in `params`: `offset_mean`, `offset_std`, `unit` (0=s,1=m,2=h,3=d).
    pub fn new(base_field: String, params: &BTreeMap<String, f64>) -> Self {
        let offset_mean = params.get("offset_mean").copied().unwrap_or(60.0);
        let offset_std = params.get("offset_std").copied().unwrap_or(10.0).abs();
        let unit = TemporalUnit::from_param(params.get("unit").copied().unwrap_or(0.0));
        Self {
            base_field,
            offset_mean,
            offset_std,
            unit,
        }
    }
}

impl FieldGenerator for RelativeGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let base = ctx.batch_columns.get(&self.base_field);
        let base_values: Vec<i64> = match base {
            Some(arr) => {
                if let Some(ts) = arr.as_any().downcast_ref::<TimestampMillisecondArray>() {
                    (0..ts.len()).map(|i| ts.value(i)).collect()
                } else if let Some(f) = arr.as_any().downcast_ref::<Float64Array>() {
                    (0..f.len()).map(|i| f.value(i) as i64).collect()
                } else {
                    tracing::warn!(
                        field = %self.base_field,
                        "base field type unsupported, using epoch 0"
                    );
                    vec![0i64; count]
                }
            }
            None => {
                tracing::warn!(
                    field = %self.base_field,
                    entity = %ctx.entity_name,
                    "base field not found in batch_columns, using epoch 0"
                );
                vec![0i64; count]
            }
        };

        let dist = Normal::new(self.offset_mean, self.offset_std.max(1e-9))
            .unwrap_or_else(|_| Normal::new(self.offset_mean, 1.0).unwrap());
        let factor = self.unit.to_millis();

        let values: Vec<i64> = base_values
            .iter()
            .map(|&b| {
                let offset = dist.sample(rng).max(0.0);
                b + (offset * factor as f64) as i64
            })
            .collect();

        Arc::new(TimestampMillisecondArray::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None)
    }
}

// ── TimeSeriesGenerator ─────────────────────────────────────────────

/// Generates a monotonically-trending time series with optional seasonality and noise.
///
/// The timestamp for row *i* is computed as:
///
/// ```text
/// t_i = start + i × interval_ms + trend(i) + Σ seasonality(t) + noise
/// ```
///
/// # Output
///
/// `DataType::Timestamp(Millisecond, None)`
pub struct TimeSeriesGenerator {
    /// Epoch-millisecond start time.
    start: i64,
    /// Linear trend slope (ms per row).
    trend_slope: f64,
    /// Seasonality components.
    seasonality: Vec<SeasonalityComponent>,
    /// Std-dev of residual Gaussian noise (ms).
    noise_std: f64,
    /// Base interval between consecutive events (ms).
    interval_ms: i64,
}

impl TimeSeriesGenerator {
    /// Create from plan parameters.
    ///
    /// Expected keys: `start`, `trend_slope`, `noise_std`, `interval_ms`,
    /// and optionally `s{n}_period`, `s{n}_amplitude`, `s{n}_phase` for up to
    /// 4 seasonality components.
    pub fn new(params: &BTreeMap<String, f64>) -> Self {
        let start = params.get("start").copied().unwrap_or(0.0) as i64;
        let trend_slope = params.get("trend_slope").copied().unwrap_or(0.0);
        let noise_std = params.get("noise_std").copied().unwrap_or(0.0).abs();
        let interval_ms = params.get("interval_ms").copied().unwrap_or(1000.0) as i64;

        let mut seasonality = Vec::new();
        for n in 0..4 {
            let key = format!("s{n}_period");
            if let Some(&period) = params.get(&key) {
                let amp = params.get(&format!("s{n}_amplitude")).copied().unwrap_or(0.1);
                let phase = params.get(&format!("s{n}_phase")).copied().unwrap_or(0.0);
                seasonality.push(SeasonalityComponent {
                    period_ms: period as i64,
                    amplitude: amp,
                    phase,
                });
            }
        }

        Self {
            start,
            trend_slope,
            seasonality,
            noise_std,
            interval_ms,
        }
    }
}

impl FieldGenerator for TimeSeriesGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let noise_dist = Normal::new(0.0, self.noise_std.max(1e-9))
            .unwrap_or_else(|_| Normal::new(0.0, 1.0).unwrap());
        let base_offset = ctx.row_offset as i64;

        let values: Vec<i64> = (0..count)
            .map(|i| {
                let idx = base_offset + i as i64;
                let base_t = self.start + idx * self.interval_ms;
                let trend = (self.trend_slope * idx as f64) as i64;

                let seasonal: f64 = self
                    .seasonality
                    .iter()
                    .map(|s| {
                        let t_frac =
                            (base_t as f64 + trend as f64) / s.period_ms.max(1) as f64 + s.phase;
                        s.amplitude * (self.interval_ms as f64) * (2.0 * std::f64::consts::PI * t_frac).sin()
                    })
                    .sum();

                let noise = if self.noise_std > 0.0 {
                    noise_dist.sample(rng) as i64
                } else {
                    0
                };

                base_t + trend + seasonal as i64 + noise
            })
            .collect();

        Arc::new(TimestampMillisecondArray::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None)
    }
}

// ── BusinessHoursGenerator ──────────────────────────────────────────

/// Generates timestamps constrained to business hours.
///
/// Each generated timestamp falls between `start_hour` and `end_hour` (UTC).
/// If `weekdays_only` is true, Saturday and Sunday are skipped.
///
/// # Output
///
/// `DataType::Timestamp(Millisecond, None)`
pub struct BusinessHoursGenerator {
    /// Epoch-millis for the first possible date (rows are distributed forward from here).
    start_date: i64,
    /// Inclusive start hour (0–23).
    start_hour: u8,
    /// Exclusive end hour (0–23, must be > start_hour).
    end_hour: u8,
    /// If true, skip Saturday (6) and Sunday (7).
    weekdays_only: bool,
}

impl BusinessHoursGenerator {
    /// Create from plan parameters.
    ///
    /// Expected keys: `start_date` (epoch ms), `start_hour`, `end_hour`, `weekdays_only` (0/1).
    pub fn new(params: &BTreeMap<String, f64>) -> Self {
        let start_date = params.get("start_date").copied().unwrap_or(0.0) as i64;
        let start_hour = params.get("start_hour").copied().unwrap_or(9.0) as u8;
        let end_hour = params.get("end_hour").copied().unwrap_or(17.0) as u8;
        let weekdays_only = params.get("weekdays_only").copied().unwrap_or(1.0) != 0.0;
        Self {
            start_date,
            start_hour,
            end_hour: end_hour.max(start_hour + 1),
            weekdays_only,
        }
    }
}

impl FieldGenerator for BusinessHoursGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let hour_range_ms =
            (self.end_hour as i64 - self.start_hour as i64) * 3_600_000;
        let intra_day = Uniform::new(0i64, hour_range_ms.max(1));
        let base_offset = ctx.row_offset as i64;

        let mut values = Vec::with_capacity(count);
        // Start from start_date and advance day by day, placing rows in valid slots.
        let start_dt = Utc
            .timestamp_millis_opt(self.start_date)
            .single()
            .unwrap_or_else(|| Utc.timestamp_millis_opt(0).unwrap());

        let mut day_cursor: NaiveDate = start_dt.date_naive();

        // Advance cursor by row_offset worth of business days to keep partitions distinct.
        let mut skip = base_offset;
        while skip > 0 {
            let wd = day_cursor.weekday().num_days_from_monday(); // 0=Mon
            if !self.weekdays_only || wd < 5 {
                skip -= 1;
            }
            if skip >= 0 {
                day_cursor += chrono::Duration::days(1);
            }
        }

        let mut generated = 0;
        while generated < count {
            let wd = day_cursor.weekday().num_days_from_monday();
            if self.weekdays_only && wd >= 5 {
                day_cursor += chrono::Duration::days(1);
                continue;
            }
            let day_start = day_cursor
                .and_hms_opt(self.start_hour as u32, 0, 0)
                .unwrap();
            let day_start_ms = day_start.and_utc().timestamp_millis();
            let offset = intra_day.sample(rng);
            values.push(day_start_ms + offset);
            generated += 1;
            day_cursor += chrono::Duration::days(1);
        }

        Arc::new(TimestampMillisecondArray::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::TimestampMillisecondArray;
    use chrono::Timelike;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn empty_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext {
            batch_columns: map,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test",
        }
    }

    #[test]
    fn relative_timestamps_are_gte_base() {
        let base_values: Vec<i64> = vec![1_000_000, 2_000_000, 3_000_000, 4_000_000, 5_000_000];
        let base_arr: ArrayRef = Arc::new(TimestampMillisecondArray::from(base_values.clone()));
        let mut cols = HashMap::new();
        cols.insert("created_at".to_string(), base_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext {
            batch_columns: cols,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test",
        };

        let mut params = BTreeMap::new();
        params.insert("offset_mean".into(), 10.0);
        params.insert("offset_std".into(), 2.0);
        params.insert("unit".into(), 0.0); // seconds
        let gen = RelativeGenerator::new("created_at".into(), &params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 5, &ctx);
        let ts = arr.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();
        for i in 0..5 {
            assert!(
                ts.value(i) >= base_values[i],
                "row {i}: {} < {}",
                ts.value(i),
                base_values[i]
            );
        }
    }

    #[test]
    fn time_series_is_roughly_increasing() {
        let mut params = BTreeMap::new();
        params.insert("start".into(), 0.0);
        params.insert("interval_ms".into(), 1000.0);
        params.insert("trend_slope".into(), 0.0);
        params.insert("noise_std".into(), 0.0);
        let gen = TimeSeriesGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 100, &ctx);
        let ts = arr.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();
        // With no noise and no seasonality, should be strictly increasing.
        for i in 1..100 {
            assert!(ts.value(i) > ts.value(i - 1));
        }
    }

    #[test]
    fn business_hours_within_range() {
        let mut params = BTreeMap::new();
        // 2024-01-01 00:00 UTC (a Monday)
        params.insert("start_date".into(), 1_704_067_200_000.0);
        params.insert("start_hour".into(), 9.0);
        params.insert("end_hour".into(), 17.0);
        params.insert("weekdays_only".into(), 1.0);
        let gen = BusinessHoursGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 50, &ctx);
        let ts = arr.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();

        for i in 0..50 {
            let dt = Utc.timestamp_millis_opt(ts.value(i)).unwrap();
            let hour = dt.hour();
            assert!(
                hour >= 9 && hour < 17,
                "row {i}: hour {hour} outside 9–17"
            );
            let wd = dt.weekday().num_days_from_monday();
            assert!(wd < 5, "row {i}: weekday {wd} is weekend");
        }
    }
}
