//! Numeric time series generator with composable additive components.
//!
//! Produces Float64 values by summing a baseline with trend, seasonality, noise,
//! autoregressive, spike, level shift, and mean reversion components.
//!
//! Stateful components (AR, level_shift, spike, mean_reversion) maintain state
//! across batches via interior mutability and require sequential partition execution.

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, TimestampMicrosecondArray, TimestampNanosecondArray};
use arrow::datatypes::DataType;
use chrono::{Datelike, Timelike};
use parking_lot::Mutex;
use rand::RngCore;
use rand_distr::{Distribution, Normal};

use crate::core::TimeSeriesComponent;
use crate::gen::context::GenContext;
use crate::gen::traits::FieldGenerator;

/// Parse a duration string like "24h", "7d", "15m", "1s" into number of steps.
/// When used with row indices (no timestamp), the value is the raw number.
/// When used with timestamps, the value is in microseconds.
fn parse_duration_to_micros(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_str, suffix) = if let Some(stripped) = s.strip_suffix('h') {
        (stripped, "h")
    } else if let Some(stripped) = s.strip_suffix('d') {
        (stripped, "d")
    } else if let Some(stripped) = s.strip_suffix('m') {
        (stripped, "m")
    } else if let Some(stripped) = s.strip_suffix('s') {
        (stripped, "s")
    } else if let Some(stripped) = s.strip_suffix('w') {
        (stripped, "w")
    } else {
        // Try as raw number (steps)
        return s.parse::<f64>().ok();
    };
    let num: f64 = num_str.parse().ok()?;
    let micros = match suffix {
        "s" => num * 1_000_000.0,
        "m" => num * 60_000_000.0,
        "h" => num * 3_600_000_000.0,
        "d" => num * 86_400_000_000.0,
        "w" => num * 604_800_000_000.0,
        _ => return None,
    };
    Some(micros)
}

/// Internal state for stateful components across batches.
#[derive(Debug)]
struct TimeSeriesState {
    /// Previous values for AR computation (most recent first).
    ar_buffer: Vec<f64>,
    /// Accumulated level shift offset.
    level_shift_offset: f64,
    /// Remaining spike rows (countdown).
    spike_remaining: u32,
    /// Current spike magnitude (while active).
    spike_value: f64,
    /// Current mean-reversion contribution.
    mean_reversion_value: f64,
}

impl TimeSeriesState {
    fn new(ar_order: usize) -> Self {
        Self {
            ar_buffer: vec![0.0; ar_order],
            level_shift_offset: 0.0,
            spike_remaining: 0,
            spike_value: 0.0,
            mean_reversion_value: 0.0,
        }
    }
}

/// Generator that produces numeric time series with composable components.
pub struct NumericTimeSeriesGenerator {
    baseline: f64,
    components: Vec<TimeSeriesComponent>,
    min: Option<f64>,
    max: Option<f64>,
    timestamp_field: Option<String>,
    /// Mutable state for AR, level_shift, spike across batches.
    state: Mutex<TimeSeriesState>,
    /// Pre-compiled holiday dates for each HolidayEffect component (index → dates).
    holiday_dates: Vec<(usize, std::collections::HashSet<chrono::NaiveDate>, f64)>,
}

impl NumericTimeSeriesGenerator {
    /// Create a new numeric time series generator.
    pub fn new(
        baseline: f64,
        components: Vec<TimeSeriesComponent>,
        min: Option<f64>,
        max: Option<f64>,
        timestamp_field: Option<String>,
    ) -> Self {
        let ar_order = components
            .iter()
            .filter_map(|c| match c {
                TimeSeriesComponent::Autoregressive { coefficients } => Some(coefficients.len()),
                _ => None,
            })
            .max()
            .unwrap_or(0);

        // Pre-compile holiday dates for O(1) lookup per row
        let holiday_dates: Vec<(usize, std::collections::HashSet<chrono::NaiveDate>, f64)> =
            components
                .iter()
                .enumerate()
                .filter_map(|(i, c)| match c {
                    TimeSeriesComponent::HolidayEffect { dates, multiplier } => {
                        let set: std::collections::HashSet<chrono::NaiveDate> = dates
                            .iter()
                            .filter_map(|d| {
                                chrono::NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d").ok()
                            })
                            .collect();
                        Some((i, set, *multiplier))
                    }
                    _ => None,
                })
                .collect();

        Self {
            baseline,
            components,
            min,
            max,
            timestamp_field,
            state: Mutex::new(TimeSeriesState::new(ar_order)),
            holiday_dates,
        }
    }

    /// Extract timestamp micros from the batch context, if a timestamp field is referenced.
    fn get_timestamps(&self, ctx: &GenContext, count: usize) -> Option<Vec<i64>> {
        let ts_field = self.timestamp_field.as_ref()?;
        let col = ctx.batch_columns.get(ts_field.as_str())?;

        // Try microsecond first, then nanosecond
        if let Some(ts) = col.as_any().downcast_ref::<TimestampMicrosecondArray>() {
            return Some((0..count).map(|i| ts.value(i)).collect());
        }
        if let Some(ts) = col.as_any().downcast_ref::<TimestampNanosecondArray>() {
            return Some((0..count).map(|i| ts.value(i) / 1_000).collect());
        }
        // Millisecond timestamps
        if let Some(ts) = col
            .as_any()
            .downcast_ref::<arrow::array::TimestampMillisecondArray>()
        {
            return Some((0..count).map(|i| ts.value(i) * 1_000).collect());
        }
        None
    }
}

impl FieldGenerator for NumericTimeSeriesGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let mut state = self.state.lock();
        let timestamps = self.get_timestamps(ctx, count);

        let mut values = Vec::with_capacity(count);

        for i in 0..count {
            let global_idx = ctx.row_offset as f64 + i as f64;
            let mut value = self.baseline;

            // Get timestamp info for calendar-aware components
            let ts_micros = timestamps.as_ref().map(|ts| ts[i]);
            let datetime = ts_micros.and_then(|us| {
                chrono::DateTime::from_timestamp_micros(us).map(|dt| dt.naive_utc())
            });

            for component in &self.components {
                match component {
                    TimeSeriesComponent::Trend { slope, degree } => {
                        let t = global_idx;
                        value += slope * t.powi(*degree as i32);
                    }
                    TimeSeriesComponent::Seasonality {
                        period,
                        amplitude,
                        phase,
                    } => {
                        let period_val = if let Some(ts) = ts_micros {
                            // Calendar-aware: period in micros
                            let p = parse_duration_to_micros(period).unwrap_or(1_000_000.0);
                            ts as f64 / p
                        } else {
                            // Row-index based: period as number of rows
                            let p = period.parse::<f64>().unwrap_or(100.0);
                            global_idx / p
                        };
                        value +=
                            amplitude * (2.0 * std::f64::consts::PI * period_val + phase).sin();
                    }
                    TimeSeriesComponent::Noise { std_dev } => {
                        if *std_dev > 0.0 {
                            let dist = Normal::new(0.0, *std_dev).unwrap_or_else(|_| {
                                Normal::new(0.0, 1.0)
                                    .expect("fallback normal noise uses valid parameters")
                            });
                            value += dist.sample(rng);
                        }
                    }
                    TimeSeriesComponent::Autoregressive { coefficients } => {
                        let mut ar_contribution = 0.0;
                        for (j, coeff) in coefficients.iter().enumerate() {
                            if j < state.ar_buffer.len() {
                                ar_contribution += coeff * state.ar_buffer[j];
                            }
                        }
                        value += ar_contribution;
                        // Update AR buffer: shift right, insert current deviation
                        let final_deviation = value - self.baseline;
                        if !state.ar_buffer.is_empty() {
                            state.ar_buffer.rotate_right(1);
                            state.ar_buffer[0] = final_deviation;
                        }
                    }
                    TimeSeriesComponent::Spike {
                        probability,
                        magnitude,
                        duration_steps,
                    } => {
                        if state.spike_remaining > 0 {
                            value += state.spike_value;
                            state.spike_remaining -= 1;
                        } else {
                            let r = (rng.next_u64() as f64) / (u64::MAX as f64);
                            if r < *probability {
                                state.spike_value = *magnitude;
                                state.spike_remaining = duration_steps.saturating_sub(1);
                                value += *magnitude;
                            }
                        }
                    }
                    TimeSeriesComponent::LevelShift {
                        probability,
                        magnitude,
                    } => {
                        let r = (rng.next_u64() as f64) / (u64::MAX as f64);
                        if r < *probability {
                            state.level_shift_offset += magnitude;
                        }
                        value += state.level_shift_offset;
                    }
                    TimeSeriesComponent::MeanReversion { target, speed } => {
                        let current = value + state.mean_reversion_value;
                        let pull = speed * (target - current);
                        state.mean_reversion_value += pull;
                        value += state.mean_reversion_value;
                    }
                    TimeSeriesComponent::WeekendEffect { multiplier } => {
                        if let Some(dt) = &datetime {
                            let weekday = dt.weekday().num_days_from_monday();
                            if weekday >= 5 {
                                // Saturday (5) or Sunday (6)
                                value *= multiplier;
                            }
                        }
                    }
                    TimeSeriesComponent::BusinessHoursEffect {
                        start_hour,
                        end_hour,
                        active_multiplier,
                    } => {
                        if let Some(dt) = &datetime {
                            let hour = dt.hour() as u8;
                            if hour >= *start_hour && hour < *end_hour {
                                value *= active_multiplier;
                            }
                        }
                    }
                    TimeSeriesComponent::HolidayEffect { .. } => {
                        // Handled via pre-compiled holiday_dates below
                    }
                }
            }

            // Apply pre-compiled holiday effects (multiplicative)
            if let Some(dt) = &datetime {
                let date = dt.date();
                for (_, dates_set, mult) in &self.holiday_dates {
                    if dates_set.contains(&date) {
                        value *= mult;
                    }
                }
            }

            // Clamp to [min, max] if specified
            if let Some(min_val) = self.min {
                value = value.max(min_val);
            }
            if let Some(max_val) = self.max {
                value = value.min(max_val);
            }

            values.push(value);
        }

        Arc::new(Float64Array::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Float64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx() -> GenContext<'static> {
        static EMPTY: std::sync::LazyLock<HashMap<String, ArrayRef>> =
            std::sync::LazyLock::new(HashMap::new);
        GenContext::new(&EMPTY, 0, 0, 1, "test")
    }

    #[test]
    fn test_baseline_only() {
        let gen = NumericTimeSeriesGenerator::new(42.0, vec![], None, None, None);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let ctx = make_ctx();
        let result = gen.generate(&mut rng, 10, &ctx);
        let arr = result.as_any().downcast_ref::<Float64Array>().unwrap();
        for i in 0..10 {
            assert!((arr.value(i) - 42.0).abs() < 0.001);
        }
    }

    #[test]
    fn test_trend() {
        let gen = NumericTimeSeriesGenerator::new(
            0.0,
            vec![TimeSeriesComponent::Trend {
                slope: 2.0,
                degree: 1,
            }],
            None,
            None,
            None,
        );
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let ctx = make_ctx();
        let result = gen.generate(&mut rng, 5, &ctx);
        let arr = result.as_any().downcast_ref::<Float64Array>().unwrap();
        // Values should be 0, 2, 4, 6, 8
        assert!((arr.value(0) - 0.0).abs() < 0.001);
        assert!((arr.value(1) - 2.0).abs() < 0.001);
        assert!((arr.value(4) - 8.0).abs() < 0.001);
    }

    #[test]
    fn test_noise() {
        let gen = NumericTimeSeriesGenerator::new(
            50.0,
            vec![TimeSeriesComponent::Noise { std_dev: 5.0 }],
            None,
            None,
            None,
        );
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = make_ctx();
        let result = gen.generate(&mut rng, 1000, &ctx);
        let arr = result.as_any().downcast_ref::<Float64Array>().unwrap();
        let mean: f64 = (0..1000).map(|i| arr.value(i)).sum::<f64>() / 1000.0;
        // Mean should be close to baseline 50
        assert!((mean - 50.0).abs() < 2.0, "mean={}", mean);
    }

    #[test]
    fn test_clamping() {
        let gen = NumericTimeSeriesGenerator::new(
            50.0,
            vec![TimeSeriesComponent::Trend {
                slope: 100.0,
                degree: 1,
            }],
            Some(0.0),
            Some(100.0),
            None,
        );
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let ctx = make_ctx();
        let result = gen.generate(&mut rng, 5, &ctx);
        let arr = result.as_any().downcast_ref::<Float64Array>().unwrap();
        for i in 0..5 {
            assert!(arr.value(i) >= 0.0);
            assert!(arr.value(i) <= 100.0);
        }
    }

    #[test]
    fn test_seasonality_oscillates() {
        let gen = NumericTimeSeriesGenerator::new(
            50.0,
            vec![TimeSeriesComponent::Seasonality {
                period: "100".to_string(),
                amplitude: 20.0,
                phase: 0.0,
            }],
            None,
            None,
            None,
        );
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let ctx = make_ctx();
        let result = gen.generate(&mut rng, 100, &ctx);
        let arr = result.as_any().downcast_ref::<Float64Array>().unwrap();
        let min_val = (0..100).map(|i| arr.value(i)).fold(f64::MAX, f64::min);
        let max_val = (0..100).map(|i| arr.value(i)).fold(f64::MIN, f64::max);
        // Should oscillate around 50 ± 20
        assert!(max_val > 60.0, "max={}", max_val);
        assert!(min_val < 40.0, "min={}", min_val);
    }

    #[test]
    fn test_ar_autocorrelation() {
        let gen = NumericTimeSeriesGenerator::new(
            50.0,
            vec![
                TimeSeriesComponent::Noise { std_dev: 1.0 },
                TimeSeriesComponent::Autoregressive {
                    coefficients: vec![0.9],
                },
            ],
            None,
            None,
            None,
        );
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = make_ctx();
        let result = gen.generate(&mut rng, 200, &ctx);
        let arr = result.as_any().downcast_ref::<Float64Array>().unwrap();
        // AR(1) with 0.9 coefficient should show strong autocorrelation
        // Adjacent values should be much closer than random
        let mut diffs = Vec::new();
        for i in 1..200 {
            diffs.push((arr.value(i) - arr.value(i - 1)).abs());
        }
        let avg_diff: f64 = diffs.iter().sum::<f64>() / diffs.len() as f64;
        // Average adjacent difference should be much smaller than the std_dev
        // because AR(1) with 0.9 creates smooth transitions
        assert!(avg_diff < 5.0, "avg_diff={}", avg_diff);
    }

    #[test]
    fn test_level_shift() {
        let gen = NumericTimeSeriesGenerator::new(
            50.0,
            vec![TimeSeriesComponent::LevelShift {
                probability: 1.0, // every row shifts
                magnitude: 1.0,
            }],
            None,
            None,
            None,
        );
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let ctx = make_ctx();
        let result = gen.generate(&mut rng, 10, &ctx);
        let arr = result.as_any().downcast_ref::<Float64Array>().unwrap();
        // Each row adds +1 to the level shift
        // Row 0: 50 + 1 = 51, Row 1: 50 + 2 = 52, ...
        assert!((arr.value(0) - 51.0).abs() < 0.001);
        assert!((arr.value(9) - 60.0).abs() < 0.001);
    }

    #[test]
    fn test_holiday_effect_multiplies_on_matching_date() {
        use arrow::array::TimestampMicrosecondArray;

        // Create timestamps: Jan 1 (holiday) and Jan 2 (normal), 2024
        let ts_jan1 = chrono::NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_micros();
        let ts_jan2 = chrono::NaiveDate::from_ymd_opt(2024, 1, 2)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_micros();

        let ts_arr: ArrayRef = Arc::new(TimestampMicrosecondArray::from(vec![ts_jan1, ts_jan2]));
        let mut cols = HashMap::new();
        cols.insert("ts".to_string(), ts_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let gen = NumericTimeSeriesGenerator::new(
            100.0,
            vec![TimeSeriesComponent::HolidayEffect {
                dates: vec!["2024-01-01".to_string()],
                multiplier: 2.0,
            }],
            None,
            None,
            Some("ts".to_string()),
        );

        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let result = gen.generate(&mut rng, 2, &ctx);
        let arr = result.as_any().downcast_ref::<Float64Array>().unwrap();

        // Row 0 (Jan 1 = holiday): 100 * 2.0 = 200
        assert!(
            (arr.value(0) - 200.0).abs() < 0.01,
            "holiday row should be 200, got {}",
            arr.value(0)
        );
        // Row 1 (Jan 2 = normal): 100 * 1.0 = 100
        assert!(
            (arr.value(1) - 100.0).abs() < 0.01,
            "normal row should be 100, got {}",
            arr.value(1)
        );
    }

    #[test]
    fn test_holiday_effect_dip() {
        use arrow::array::TimestampMicrosecondArray;

        let ts_dec25 = chrono::NaiveDate::from_ymd_opt(2024, 12, 25)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_micros();
        let ts_dec26 = chrono::NaiveDate::from_ymd_opt(2024, 12, 26)
            .unwrap()
            .and_hms_opt(10, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_micros();

        let ts_arr: ArrayRef = Arc::new(TimestampMicrosecondArray::from(vec![ts_dec25, ts_dec26]));
        let mut cols = HashMap::new();
        cols.insert("ts".to_string(), ts_arr);
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let gen = NumericTimeSeriesGenerator::new(
            100.0,
            vec![TimeSeriesComponent::HolidayEffect {
                dates: vec!["2024-12-25".to_string()],
                multiplier: 0.1, // dip to 10%
            }],
            None,
            None,
            Some("ts".to_string()),
        );

        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let result = gen.generate(&mut rng, 2, &ctx);
        let arr = result.as_any().downcast_ref::<Float64Array>().unwrap();

        // Row 0 (Dec 25 = holiday): 100 * 0.1 = 10
        assert!(
            (arr.value(0) - 10.0).abs() < 0.01,
            "holiday dip should be 10, got {}",
            arr.value(0)
        );
        // Row 1 (Dec 26 = normal): 100
        assert!(
            (arr.value(1) - 100.0).abs() < 0.01,
            "normal should be 100, got {}",
            arr.value(1)
        );
    }
}
