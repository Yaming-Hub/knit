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

use arrow::array::{
    ArrayRef, Float64Array, TimestampMicrosecondArray, TimestampMillisecondArray,
    TimestampNanosecondArray, TimestampSecondArray,
};
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
                } else if let Some(ts) = arr.as_any().downcast_ref::<TimestampMicrosecondArray>() {
                    (0..ts.len()).map(|i| ts.value(i) / 1_000).collect()
                } else if let Some(ts) = arr.as_any().downcast_ref::<TimestampNanosecondArray>() {
                    (0..ts.len()).map(|i| ts.value(i) / 1_000_000).collect()
                } else if let Some(ts) = arr.as_any().downcast_ref::<TimestampSecondArray>() {
                    (0..ts.len()).map(|i| ts.value(i) * 1_000).collect()
                } else if let Some(f) = arr.as_any().downcast_ref::<Float64Array>() {
                    (0..f.len()).map(|i| f.value(i) as i64).collect()
                } else {
                    tracing::warn!(
                        field = %self.base_field,
                        actual_type = ?arr.data_type(),
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
            .unwrap_or_else(|_| Normal::new(self.offset_mean, 1.0)
                .expect("stddev=1.0 is always valid"));
        let factor = self.unit.to_millis();

        let values: Vec<i64> = base_values
            .iter()
            .map(|&b| {
                let offset = dist.sample(rng).max(0.0);
                b.saturating_add((offset * factor as f64) as i64)
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
            .unwrap_or_else(|_| Normal::new(0.0, 1.0)
                .expect("stddev=1.0 is always valid"));
        let base_offset = ctx.row_offset as i64;

        let values: Vec<i64> = (0..count)
            .map(|i| {
                let idx = base_offset.saturating_add(i as i64);
                let base_t = self.start.saturating_add(idx.saturating_mul(self.interval_ms));
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

                base_t.saturating_add(trend).saturating_add(seasonal as i64).saturating_add(noise)
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
    /// Hours are clamped to 0–23 to prevent `and_hms_opt` failures.
    pub fn new(params: &BTreeMap<String, f64>) -> Self {
        let start_date = params.get("start_date").copied().unwrap_or(0.0) as i64;
        let start_hour = (params.get("start_hour").copied().unwrap_or(9.0) as u8).min(23);
        let end_hour = (params.get("end_hour").copied().unwrap_or(17.0) as u8).min(24);
        let weekdays_only = params.get("weekdays_only").copied().unwrap_or(1.0) != 0.0;
        Self {
            start_date,
            start_hour,
            end_hour: end_hour.max(start_hour + 1).min(24),
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
            .unwrap_or_else(|| Utc.timestamp_millis_opt(0).single()
                .expect("epoch 0 is always valid"));

        let mut day_cursor: NaiveDate = start_dt.date_naive();

        // Advance cursor by row_offset worth of business days to keep partitions distinct.
        let mut skip = base_offset;
        while skip > 0 {
            let wd = day_cursor.weekday().num_days_from_monday(); // 0=Mon
            if !self.weekdays_only || wd < 5 {
                skip -= 1;
            }
            day_cursor += chrono::Duration::days(1);
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
                .expect("start_hour is clamped to 0–23; and_hms_opt cannot fail");
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
        GenContext::new(map, 0, 0, 1, "test")
    }

    #[test]
    fn relative_timestamps_are_gte_base() {
        let base_values: Vec<i64> = vec![1_000_000, 2_000_000, 3_000_000, 4_000_000, 5_000_000];
        let base_arr: ArrayRef = Arc::new(TimestampMillisecondArray::from(base_values.clone()));
        let mut cols = HashMap::new();
        cols.insert("created_at".to_string(), base_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut params = BTreeMap::new();
        params.insert("offset_mean".into(), 10.0);
        params.insert("offset_std".into(), 2.0);
        params.insert("unit".into(), 0.0); // seconds
        let gen = RelativeGenerator::new("created_at".into(), &params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 5, &ctx);
        let ts = arr.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();
        for (i, &bv) in base_values.iter().enumerate() {
            assert!(
                ts.value(i) >= bv,
                "row {i}: {} < {}",
                ts.value(i),
                bv
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
                (9..17).contains(&hour),
                "row {i}: hour {hour} outside 9–17"
            );
            let wd = dt.weekday().num_days_from_monday();
            assert!(wd < 5, "row {i}: weekday {wd} is weekend");
        }
    }

    #[test]
    fn business_hours_clamps_invalid_hours() {
        // start_hour=30 should be clamped to 23, end_hour=50 → clamped to 24
        let mut params = BTreeMap::new();
        params.insert("start_date".into(), 1_704_067_200_000.0);
        params.insert("start_hour".into(), 30.0);
        params.insert("end_hour".into(), 50.0);
        params.insert("weekdays_only".into(), 0.0);
        let gen = BusinessHoursGenerator::new(&params);

        // Should not panic — hours are clamped
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 10, &ctx);
        assert_eq!(arr.len(), 10);
    }

    #[test]
    fn business_hours_end_24_preserves_full_range() {
        // end_hour=24 is the exclusive upper bound (midnight), should stay 24
        let mut params = BTreeMap::new();
        params.insert("start_date".into(), 1_704_067_200_000.0);
        params.insert("start_hour".into(), 20.0);
        params.insert("end_hour".into(), 24.0);
        params.insert("weekdays_only".into(), 0.0);
        let gen = BusinessHoursGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 50, &ctx);
        let ts = arr.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();

        // All timestamps should be in the 20–23 hour range (end_hour=24 is exclusive)
        for i in 0..50 {
            let dt = Utc.timestamp_millis_opt(ts.value(i)).unwrap();
            let hour = dt.hour();
            assert!(
                (20..24).contains(&hour),
                "row {i}: hour {hour} outside 20–24"
            );
        }
    }

    // ── New tests below ──────────────────────────────────────────────────

    #[test]
    fn relative_missing_base_field_uses_epoch_zero() {
        // base field doesn't exist in context → all offsets relative to 0
        let mut params = BTreeMap::new();
        params.insert("offset_mean".into(), 5.0);
        params.insert("offset_std".into(), 0.0); // deterministic
        params.insert("unit".into(), 0.0); // seconds
        let gen = RelativeGenerator::new("nonexistent".into(), &params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 5, &ctx);
        let ts = arr.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();

        // offset_mean=5s, offset_std≈0 → each value ≈ 5000ms from epoch 0
        for i in 0..5 {
            assert!(ts.value(i) >= 0, "row {i}: should be >= 0");
            // With near-zero std, should be close to 5000ms
            assert!(
                (ts.value(i) - 5000).abs() < 100,
                "row {i}: value {} not near 5000",
                ts.value(i)
            );
        }
    }

    #[test]
    fn relative_unit_days() {
        // offset in days should produce much larger values
        let base_values: Vec<i64> = vec![0; 5];
        let base_arr: ArrayRef = Arc::new(TimestampMillisecondArray::from(base_values));
        let mut cols = HashMap::new();
        cols.insert("ts".to_string(), base_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut params = BTreeMap::new();
        params.insert("offset_mean".into(), 1.0);
        params.insert("offset_std".into(), 0.0);
        params.insert("unit".into(), 3.0); // days
        let gen = RelativeGenerator::new("ts".into(), &params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 5, &ctx);
        let ts = arr.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();

        // 1 day = 86_400_000 ms; with 0 std should be close to that
        for i in 0..5 {
            let diff = (ts.value(i) - 86_400_000).abs();
            assert!(
                diff < 1_000_000,
                "row {i}: value {} not near 1 day (86.4M ms)",
                ts.value(i)
            );
        }
    }

    #[test]
    fn relative_output_type() {
        let params = BTreeMap::new();
        let gen = RelativeGenerator::new("x".into(), &params);
        assert_eq!(
            gen.output_type(),
            DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None)
        );
    }

    #[test]
    fn time_series_with_positive_trend() {
        let mut params = BTreeMap::new();
        params.insert("start".into(), 1_000_000.0);
        params.insert("interval_ms".into(), 1000.0);
        params.insert("trend_slope".into(), 500.0); // 500ms added per row
        params.insert("noise_std".into(), 0.0);
        let gen = TimeSeriesGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 10, &ctx);
        let ts = arr.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();

        // With no noise/seasonality: gap = interval_ms + trend_slope = 1000 + 500 = 1500ms exactly
        for i in 1..10 {
            let gap = ts.value(i) - ts.value(i - 1);
            assert_eq!(
                gap, 1500,
                "row {i}: gap {gap} should be exactly interval_ms + trend_slope = 1500"
            );
        }
    }

    #[test]
    fn time_series_with_seasonality() {
        let mut params = BTreeMap::new();
        params.insert("start".into(), 0.0);
        params.insert("interval_ms".into(), 1000.0);
        params.insert("trend_slope".into(), 0.0);
        params.insert("noise_std".into(), 0.0);
        // Add seasonality: period=10000ms, amplitude=2.0, phase=0
        // Max seasonal swing = amplitude * interval_ms = 2000ms > interval_ms
        params.insert("s0_period".into(), 10_000.0);
        params.insert("s0_amplitude".into(), 2.0);
        params.insert("s0_phase".into(), 0.0);
        let gen = TimeSeriesGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 20, &ctx);
        let ts = arr.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();

        // Seasonality should cause some non-monotonic behavior
        let mut has_decrease = false;
        for i in 1..20 {
            if ts.value(i) < ts.value(i - 1) {
                has_decrease = true;
                break;
            }
        }
        assert!(
            has_decrease,
            "seasonality should cause at least one decrease in timestamps"
        );
    }

    #[test]
    fn time_series_noise_adds_variance() {
        let mut params = BTreeMap::new();
        params.insert("start".into(), 0.0);
        params.insert("interval_ms".into(), 10_000.0);
        params.insert("trend_slope".into(), 0.0);
        params.insert("noise_std".into(), 1000.0); // 1 second noise std
        let gen = TimeSeriesGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 100, &ctx);
        let ts = arr.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();

        // Compute gaps and verify the empirical std is near the configured noise_std=1000ms
        let gaps: Vec<i64> = (1..100).map(|i| ts.value(i) - ts.value(i - 1)).collect();
        let mean_gap = gaps.iter().sum::<i64>() as f64 / gaps.len() as f64;
        let variance = gaps.iter().map(|&g| (g as f64 - mean_gap).powi(2)).sum::<f64>()
            / gaps.len() as f64;
        let empirical_std = variance.sqrt();
        // Each gap has noise contribution of diff of two N(0,1000) draws → std of diff = sqrt(2)*1000 ≈ 1414
        // Allow 800-2200 range for empirical std with 99 samples
        assert!(
            empirical_std > 800.0 && empirical_std < 2200.0,
            "empirical std {empirical_std:.0} should be near sqrt(2)*1000 ≈ 1414"
        );
    }

    #[test]
    fn time_series_row_offset_continues_sequence() {
        let mut params = BTreeMap::new();
        params.insert("start".into(), 0.0);
        params.insert("interval_ms".into(), 1000.0);
        params.insert("trend_slope".into(), 0.0);
        params.insert("noise_std".into(), 0.0);
        let gen = TimeSeriesGenerator::new(&params);

        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));

        // Partition 0: rows 0-4
        let ctx0 = GenContext::new(map, 0, 0, 1, "test");
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr0 = gen.generate(&mut rng, 5, &ctx0);
        let ts0 = arr0.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();

        // Partition 1: rows 5-9 (row_offset=5)
        let ctx1 = GenContext::new(map, 5, 0, 1, "test");
        let mut rng2 = ChaCha8Rng::seed_from_u64(99);
        let arr1 = gen.generate(&mut rng2, 5, &ctx1);
        let ts1 = arr1.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();

        // First value of partition 1 should continue from where partition 0 left off
        assert_eq!(ts0.value(4) + 1000, ts1.value(0));
    }

    #[test]
    fn time_series_output_type() {
        let params = BTreeMap::new();
        let gen = TimeSeriesGenerator::new(&params);
        assert_eq!(
            gen.output_type(),
            DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None)
        );
    }

    #[test]
    fn business_hours_weekends_allowed() {
        // weekdays_only=0 should allow Saturday/Sunday
        let mut params = BTreeMap::new();
        // 2024-01-06 is a Saturday
        params.insert("start_date".into(), 1_704_499_200_000.0);
        params.insert("start_hour".into(), 0.0);
        params.insert("end_hour".into(), 24.0);
        params.insert("weekdays_only".into(), 0.0);
        let gen = BusinessHoursGenerator::new(&params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 7, &ctx);
        let ts = arr.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();

        // Should include weekend days
        let mut has_weekend = false;
        for i in 0..7 {
            let dt = Utc.timestamp_millis_opt(ts.value(i)).unwrap();
            let wd = dt.weekday().num_days_from_monday();
            if wd >= 5 {
                has_weekend = true;
                break;
            }
        }
        assert!(has_weekend, "weekdays_only=false should include weekend days");
    }

    #[test]
    fn business_hours_deterministic() {
        let mut params = BTreeMap::new();
        params.insert("start_date".into(), 1_704_067_200_000.0);
        params.insert("start_hour".into(), 9.0);
        params.insert("end_hour".into(), 17.0);
        params.insert("weekdays_only".into(), 1.0);
        let gen = BusinessHoursGenerator::new(&params);

        let ctx = empty_ctx();
        let mut rng1 = ChaCha8Rng::seed_from_u64(42);
        let arr1 = gen.generate(&mut rng1, 20, &ctx);
        let mut rng2 = ChaCha8Rng::seed_from_u64(42);
        let arr2 = gen.generate(&mut rng2, 20, &ctx);

        let ts1 = arr1.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();
        let ts2 = arr2.as_any().downcast_ref::<TimestampMillisecondArray>().unwrap();
        for i in 0..20 {
            assert_eq!(ts1.value(i), ts2.value(i), "row {i} mismatch");
        }
    }
}
