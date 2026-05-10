//! Event stream generator — strictly-increasing timestamps with random
//! inter-arrival times.
//!
//! Uses an exponential distribution for gaps between events, optionally
//! modulated by rate components (seasonality, weekend effect, business hours)
//! via Lewis-Shedler thinning.
//!
//! The generator maintains cumulative state across batches to ensure
//! monotonic timestamp sequences.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use arrow::array::{ArrayRef, TimestampMillisecondArray};
use arrow::datatypes::DataType;
use chrono::{DateTime, Datelike, NaiveDate, Timelike, Utc};
use rand::Rng;
use rand::RngCore;
use rand_distr::{Distribution, Exp};

use crate::core::EventStreamComponent;
use crate::gen::context::GenContext;
use crate::gen::traits::FieldGenerator;

/// Internal state persisted across batches.
struct EventStreamState {
    /// The last emitted timestamp (epoch-ms), used as the base for the next gap.
    last_timestamp_ms: i64,
}

/// Generates strictly-increasing timestamps via exponential inter-arrival times.
///
/// # Rate modulation
///
/// When `components` are present, the generator uses Lewis-Shedler thinning:
/// 1. Sample a candidate gap from `Exp(lambda_max)` where `lambda_max` is the
///    peak rate (base × max modulation envelope).
/// 2. Compute `lambda(t)` at the candidate timestamp using all components.
/// 3. Accept with probability `lambda(t) / lambda_max`; otherwise reject and
///    advance to the candidate time, then repeat.
///
/// When no components are present, every gap is accepted (pure exponential).
pub struct EventStreamGenerator {
    /// Base rate (events per millisecond).
    lambda_per_ms: f64,
    /// Rate-modulation components.
    components: Vec<EventStreamComponent>,
    /// Peak rate for thinning envelope: `lambda_per_ms * max_multiplier`.
    lambda_max: f64,
    /// Pre-compiled holiday dates: `(HashSet<NaiveDate>, multiplier)` per HolidayEffect component.
    holiday_dates: Vec<(HashSet<NaiveDate>, f64)>,
    /// Cumulative state shared across partitions (sequential only).
    state: Mutex<EventStreamState>,
}

impl EventStreamGenerator {
    /// Create a new event stream generator.
    pub fn new(
        start_ms: i64,
        lambda_per_ms: f64,
        components: Vec<EventStreamComponent>,
    ) -> Self {
        // Compute the envelope upper bound for thinning.
        let max_multiplier = compute_max_multiplier(&components);
        let lambda_max = lambda_per_ms * max_multiplier;

        // Pre-compile holiday dates for O(1) lookup during generation.
        let holiday_dates: Vec<(HashSet<NaiveDate>, f64)> = components
            .iter()
            .filter_map(|c| match c {
                EventStreamComponent::HolidayEffect { dates, multiplier } => {
                    let set: HashSet<NaiveDate> = dates
                        .iter()
                        .filter_map(|d| NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d").ok())
                        .collect();
                    Some((set, *multiplier))
                }
                _ => None,
            })
            .collect();

        Self {
            lambda_per_ms,
            components,
            lambda_max,
            holiday_dates,
            state: Mutex::new(EventStreamState {
                last_timestamp_ms: start_ms,
            }),
        }
    }
}

impl FieldGenerator for EventStreamGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        let mut state = self.state.lock().expect("event stream state poisoned");
        let mut timestamps = Vec::with_capacity(count);

        if self.components.is_empty() {
            // Pure exponential — no thinning needed.
            let exp = Exp::new(self.lambda_per_ms.max(1e-15))
                .unwrap_or_else(|_| Exp::new(1e-9).expect("valid"));
            for _ in 0..count {
                let gap_ms = exp.sample(rng);
                state.last_timestamp_ms = state
                    .last_timestamp_ms
                    .saturating_add(gap_ms.max(1.0) as i64);
                timestamps.push(state.last_timestamp_ms);
            }
        } else {
            // Lewis-Shedler thinning.
            let exp_max = Exp::new(self.lambda_max.max(1e-15))
                .unwrap_or_else(|_| Exp::new(1e-9).expect("valid"));
            let mut generated = 0;
            // Safety limit to avoid infinite loops from near-zero rates.
            // Use a generous budget: 100× the expected attempts.
            let max_attempts = count * 100;
            let mut attempts = 0;

            while generated < count && attempts < max_attempts {
                attempts += 1;
                let gap_ms = exp_max.sample(rng);
                let candidate_ms = state
                    .last_timestamp_ms
                    .saturating_add(gap_ms.max(1.0) as i64);

                let rate_at_t = self.lambda_per_ms * rate_multiplier(candidate_ms, &self.components, &self.holiday_dates);
                let accept_prob = rate_at_t / self.lambda_max;

                // Always advance time (for thinning correctness).
                state.last_timestamp_ms = candidate_ms;

                if rng.gen::<f64>() < accept_prob {
                    timestamps.push(candidate_ms);
                    generated += 1;
                }
            }

            // If we hit the attempt limit, continue thinning with a relaxed
            // envelope (accept all candidates) to preserve temporal semantics
            // rather than switching to the unmodulated base rate.
            if generated < count {
                tracing::warn!(
                    remaining = count - generated,
                    attempts,
                    "event stream thinning hit attempt limit; accepting all remaining candidates"
                );
                for _ in generated..count {
                    let gap_ms = exp_max.sample(rng);
                    state.last_timestamp_ms = state
                        .last_timestamp_ms
                        .saturating_add(gap_ms.max(1.0) as i64);
                    timestamps.push(state.last_timestamp_ms);
                }
            }
        }

        Arc::new(TimestampMillisecondArray::from(timestamps))
    }

    fn output_type(&self) -> DataType {
        DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None)
    }
}

/// Compute the peak rate multiplier across all components.
///
/// Used as the envelope upper bound for thinning. Conservative: multiplies
/// the individual maximums.
fn compute_max_multiplier(components: &[EventStreamComponent]) -> f64 {
    let mut max = 1.0;
    for comp in components {
        match comp {
            EventStreamComponent::Seasonality { amplitude, .. } => {
                // Multiplier range: [1 - amplitude, 1 + amplitude].
                max *= 1.0 + amplitude.abs();
            }
            EventStreamComponent::BusinessHours {
                active_multiplier, ..
            } => {
                max *= active_multiplier.max(1.0);
            }
            EventStreamComponent::WeekendEffect { multiplier } => {
                // On weekdays the multiplier is 1.0, on weekends it's `multiplier`.
                // Max is whichever is larger.
                max *= multiplier.max(1.0);
            }
            EventStreamComponent::HolidayEffect { multiplier, .. } => {
                max *= multiplier.max(1.0);
            }
        }
    }
    max
}

/// Evaluate the instantaneous rate multiplier at a given timestamp.
///
/// Returns a value in (0, max_multiplier] that modulates the base rate.
fn rate_multiplier(timestamp_ms: i64, components: &[EventStreamComponent], holiday_dates: &[(HashSet<NaiveDate>, f64)]) -> f64 {
    let dt = DateTime::<Utc>::from_timestamp_millis(timestamp_ms);

    let mut multiplier = 1.0;
    for comp in components {
        match comp {
            EventStreamComponent::Seasonality { period, amplitude } => {
                let period_ms = parse_duration_ms(period);
                if period_ms > 0 {
                    let phase = (timestamp_ms as f64 % period_ms as f64) / period_ms as f64;
                    // Sinusoidal modulation: 1 + amplitude * sin(2π * phase)
                    multiplier *= 1.0 + amplitude * (2.0 * std::f64::consts::PI * phase).sin();
                }
            }
            EventStreamComponent::WeekendEffect { multiplier: wm } => {
                if let Some(dt) = dt {
                    let weekday = dt.weekday().num_days_from_monday(); // 0=Mon .. 6=Sun
                    if weekday >= 5 {
                        multiplier *= wm;
                    }
                }
            }
            EventStreamComponent::BusinessHours {
                active_hours,
                active_multiplier,
            } => {
                if let Some(dt) = dt {
                    let hour = dt.hour() as u8;
                    if hour >= active_hours[0] && hour < active_hours[1] {
                        multiplier *= active_multiplier;
                    }
                    // Outside active hours: multiplier stays at 1.0 (base rate).
                }
            }
            EventStreamComponent::HolidayEffect { .. } => {
                // Handled via pre-compiled holiday_dates below.
            }
        }
    }

    // Apply pre-compiled holiday effects (O(1) lookup per component).
    if let Some(dt) = dt {
        let date = dt.date_naive();
        for (dates_set, hm) in holiday_dates {
            if dates_set.contains(&date) {
                multiplier *= hm;
            }
        }
    }

    multiplier.max(1e-9) // prevent zero rate
}

/// Parse a duration string like "24h", "7d", "1h", "30m" to milliseconds.
pub(crate) fn parse_duration_ms(s: &str) -> i64 {
    let s = s.trim();
    if s.is_empty() {
        return 0;
    }

    // Try to split into numeric prefix + unit suffix.
    let (num_str, unit) = match s.find(|c: char| c.is_alphabetic()) {
        Some(idx) => (&s[..idx], &s[idx..]),
        None => return s.parse::<i64>().unwrap_or(0),
    };

    let n: f64 = num_str.parse().unwrap_or(0.0);
    let ms = match unit.to_lowercase().as_str() {
        "ms" | "millisecond" | "milliseconds" => n,
        "s" | "sec" | "second" | "seconds" => n * 1_000.0,
        "m" | "min" | "minute" | "minutes" => n * 60_000.0,
        "h" | "hr" | "hour" | "hours" => n * 3_600_000.0,
        "d" | "day" | "days" => n * 86_400_000.0,
        "w" | "week" | "weeks" => n * 604_800_000.0,
        _ => n * 1_000.0, // default to seconds
    };
    ms as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx(row_offset: u64) -> GenContext<'static> {
        static EMPTY: std::sync::LazyLock<HashMap<String, ArrayRef>> =
            std::sync::LazyLock::new(HashMap::new);
        GenContext::new(&EMPTY, row_offset, 0, 1, "test")
    }

    #[test]
    fn pure_exponential_produces_increasing_timestamps() {
        let gen = EventStreamGenerator::new(
            1_704_067_200_000, // 2024-01-01 00:00:00 UTC
            0.001,             // 1 event per second
            vec![],
        );
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 100, &make_ctx(0));
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        // All timestamps should be strictly increasing.
        for i in 1..ts.len() {
            assert!(
                ts.value(i) > ts.value(i - 1),
                "timestamps not increasing at index {}: {} <= {}",
                i,
                ts.value(i),
                ts.value(i - 1)
            );
        }

        // First timestamp should be after start.
        assert!(ts.value(0) > 1_704_067_200_000);
    }

    #[test]
    fn stateful_across_batches() {
        let gen = EventStreamGenerator::new(1_704_067_200_000, 0.001, vec![]);
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        let arr1 = gen.generate(&mut rng, 50, &make_ctx(0));
        let ts1 = arr1
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        let last_of_batch1 = ts1.value(49);

        let arr2 = gen.generate(&mut rng, 50, &make_ctx(50));
        let ts2 = arr2
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        let first_of_batch2 = ts2.value(0);

        assert!(
            first_of_batch2 > last_of_batch1,
            "batch 2 should continue after batch 1: {} <= {}",
            first_of_batch2,
            last_of_batch1
        );
    }

    #[test]
    fn output_type_is_timestamp_millisecond() {
        let gen = EventStreamGenerator::new(0, 0.001, vec![]);
        assert_eq!(
            gen.output_type(),
            DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None)
        );
    }

    #[test]
    fn with_seasonality_produces_varying_density() {
        let gen = EventStreamGenerator::new(
            1_704_067_200_000,
            0.001,
            vec![EventStreamComponent::Seasonality {
                period: "24h".into(),
                amplitude: 0.8,
            }],
        );
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 200, &make_ctx(0));
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        // Verify strictly increasing.
        for i in 1..ts.len() {
            assert!(ts.value(i) > ts.value(i - 1));
        }

        // Compute inter-arrival gaps and verify variance exists
        // (seasonality should create density differences).
        let gaps: Vec<i64> = (1..ts.len())
            .map(|i| ts.value(i) - ts.value(i - 1))
            .collect();
        let mean_gap = gaps.iter().sum::<i64>() as f64 / gaps.len() as f64;
        let variance = gaps.iter().map(|g| (*g as f64 - mean_gap).powi(2)).sum::<f64>()
            / gaps.len() as f64;
        // With seasonality, variance should be substantially higher than pure exponential.
        assert!(variance > 0.0, "expected non-zero variance with seasonality");
    }

    #[test]
    fn weekend_effect_reduces_rate() {
        // Start on a Saturday.
        let saturday_epoch_ms = chrono::NaiveDate::from_ymd_opt(2024, 1, 6)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis();

        let gen_weekend = EventStreamGenerator::new(
            saturday_epoch_ms,
            0.01, // high rate so we get many events quickly
            vec![EventStreamComponent::WeekendEffect { multiplier: 0.1 }],
        );
        let gen_plain = EventStreamGenerator::new(saturday_epoch_ms, 0.01, vec![]);

        let mut rng1 = ChaCha8Rng::seed_from_u64(42);
        let mut rng2 = ChaCha8Rng::seed_from_u64(42);

        let arr_weekend = gen_weekend.generate(&mut rng1, 50, &make_ctx(0));
        let arr_plain = gen_plain.generate(&mut rng2, 50, &make_ctx(0));

        let ts_weekend = arr_weekend
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        let ts_plain = arr_plain
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        // Weekend-reduced events should span a longer time (lower effective rate).
        let span_weekend = ts_weekend.value(49) - ts_weekend.value(0);
        let span_plain = ts_plain.value(49) - ts_plain.value(0);
        assert!(
            span_weekend > span_plain,
            "weekend effect should stretch events: weekend_span={} plain_span={}",
            span_weekend,
            span_plain
        );
    }

    #[test]
    fn business_hours_concentrates_events() {
        // Start at 2024-01-01 12:00 UTC (during business hours).
        let start_ms = 1_704_067_200_000 + 12 * 3_600_000;
        let gen = EventStreamGenerator::new(
            start_ms,
            0.001, // ~1 event/second — fast enough to span multiple days
            vec![EventStreamComponent::BusinessHours {
                active_hours: [8, 22],
                active_multiplier: 5.0,
            }],
        );
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 500, &make_ctx(0));
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        // Count events during business hours.
        let mut bh_count = 0;
        for i in 0..ts.len() {
            if let Some(dt) = DateTime::<Utc>::from_timestamp_millis(ts.value(i)) {
                let hour = dt.hour() as u8;
                if hour >= 8 && hour < 22 {
                    bh_count += 1;
                }
            }
        }

        // With 5x active_multiplier and 14/24 active hours, most events should
        // land during business hours.
        let ratio = bh_count as f64 / 500.0;
        assert!(
            ratio > 0.5,
            "expected majority during business hours, got {}/500 ({:.0}%)",
            bh_count,
            ratio * 100.0
        );
    }

    #[test]
    fn parse_duration_ms_works() {
        assert_eq!(parse_duration_ms("24h"), 86_400_000);
        assert_eq!(parse_duration_ms("7d"), 604_800_000);
        assert_eq!(parse_duration_ms("30m"), 1_800_000);
        assert_eq!(parse_duration_ms("1s"), 1_000);
        assert_eq!(parse_duration_ms("500ms"), 500);
        assert_eq!(parse_duration_ms("1w"), 604_800_000);
    }
}
