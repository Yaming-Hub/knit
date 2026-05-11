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
    Array, ArrayRef, Float64Array, Int64Array, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray,
};
use arrow::datatypes::DataType;
use chrono::{Datelike, NaiveDate, TimeZone, Utc};
use rand::RngCore;
use rand_distr::{Distribution, Normal, Uniform};

use crate::gen::context::GenContext;
use crate::gen::traits::FieldGenerator;

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

/// Offset mode for relative timestamp generation.
#[derive(Debug, Clone)]
pub enum RelativeOffsetMode {
    /// Legacy mode: Normal distribution with mean and std-dev (backward compatible).
    Normal {
        /// Mean offset in the specified unit.
        mean: f64,
        /// Standard deviation of the offset.
        std_dev: f64,
    },
    /// Distribution mode: configurable distribution with optional min/max clamping.
    Distribution {
        /// The distribution to sample from.
        kind: RelativeDistKind,
        /// Minimum offset in milliseconds (clamp lower bound).
        min_ms: Option<f64>,
        /// Maximum offset in milliseconds (clamp upper bound).
        max_ms: Option<f64>,
    },
    /// Constant mode: fixed offset with no randomness.
    Constant {
        /// Fixed offset in milliseconds.
        offset_ms: f64,
    },
}

/// Supported distribution kinds for relative offsets.
///
/// Only a subset of distributions make sense for temporal offsets (continuous,
/// non-negative). Discrete and probability distributions are excluded.
#[derive(Debug, Clone)]
pub enum RelativeDistKind {
    /// Normal(mean, std_dev) in the specified unit.
    Normal(f64, f64),
    /// LogNormal(mu, sigma) in the specified unit.
    LogNormal(f64, f64),
    /// Uniform(min, max) in the specified unit.
    Uniform(f64, f64),
    /// Exponential(lambda) in the specified unit.
    Exponential(f64),
}

/// Generates timestamps relative to an existing column by adding a random offset.
///
/// The base field must already exist in [`GenContext::batch_columns`] and be
/// castable to `TimestampMillisecond`. The offset is drawn from a configurable
/// distribution with optional min/max clamping.
///
/// Three offset modes are supported:
/// - **Normal** (legacy): draws from `Normal(mean, std_dev)`, backward compatible
/// - **Distribution**: draws from a named distribution (normal, log_normal,
///   uniform, exponential) with optional duration-based min/max bounds
/// - **Constant**: fixed offset with no randomness
///
/// # Output
///
/// `DataType::Timestamp(Millisecond, None)` — the resulting timestamps
pub struct RelativeGenerator {
    /// Field name to read base timestamps from.
    base_field: String,
    /// Offset mode determining how offsets are computed.
    mode: RelativeOffsetMode,
    /// Unit for interpreting distribution output values.
    unit: TemporalUnit,
}

impl RelativeGenerator {
    /// Create a new `RelativeGenerator` from plan parameters.
    ///
    /// The `offset_mode` parameter selects the variant:
    /// - `0` (or absent): Legacy Normal mode — uses `offset_mean`, `offset_std`
    /// - `1`: Distribution mode — uses `distribution` string param, distribution params, `min_ms`, `max_ms`
    /// - `2`: Constant mode — uses `constant_ms`
    pub fn new(
        base_field: String,
        params: &BTreeMap<String, f64>,
        string_params: &BTreeMap<String, String>,
    ) -> Self {
        let unit = TemporalUnit::from_param(params.get("unit").copied().unwrap_or(0.0));
        let mode_val = params.get("offset_mode").copied().unwrap_or(0.0) as i64;

        let mode = match mode_val {
            1 => {
                // Distribution mode
                let dist_str = string_params
                    .get("distribution")
                    .map(|s| s.trim_matches('"').to_lowercase())
                    .unwrap_or_else(|| "normal".into());

                let kind = match dist_str.as_str() {
                    "log_normal" => {
                        let mu = params.get("mu").copied().unwrap_or(1.0);
                        let sigma = params.get("sigma").copied().unwrap_or(0.5);
                        RelativeDistKind::LogNormal(mu, sigma)
                    }
                    "uniform" => {
                        let lo = params.get("min").or(params.get("low")).copied().unwrap_or(0.0);
                        let hi = params
                            .get("max")
                            .or(params.get("high"))
                            .copied()
                            .unwrap_or(1.0);
                        RelativeDistKind::Uniform(lo, hi)
                    }
                    "exponential" => {
                        let lambda = params.get("lambda").copied().unwrap_or(1.0);
                        RelativeDistKind::Exponential(lambda)
                    }
                    _ => {
                        // Default to normal
                        let mean = params
                            .get("mean")
                            .or(params.get("offset_mean"))
                            .copied()
                            .unwrap_or(0.0);
                        let std_dev = params
                            .get("std_dev")
                            .or(params.get("offset_std"))
                            .copied()
                            .unwrap_or(1.0)
                            .abs();
                        RelativeDistKind::Normal(mean, std_dev)
                    }
                };

                let min_ms = params.get("min_ms").copied();
                let max_ms = params.get("max_ms").copied();

                RelativeOffsetMode::Distribution {
                    kind,
                    min_ms,
                    max_ms,
                }
            }
            2 => {
                // Constant mode
                let offset_ms = params.get("constant_ms").copied().unwrap_or(0.0);
                RelativeOffsetMode::Constant { offset_ms }
            }
            _ => {
                // Legacy Normal mode (mode 0)
                let mean = params.get("offset_mean").copied().unwrap_or(60.0);
                let std_dev = params.get("offset_std").copied().unwrap_or(10.0).abs();
                RelativeOffsetMode::Normal { mean, std_dev }
            }
        };

        Self {
            base_field,
            mode,
            unit,
        }
    }

    /// Sample a single offset in milliseconds for the given RNG.
    fn sample_offset(&self, rng: &mut dyn RngCore) -> i64 {
        let factor = self.unit.to_millis() as f64;

        match &self.mode {
            RelativeOffsetMode::Normal { mean, std_dev } => {
                let dist = Normal::new(*mean, std_dev.max(1e-9)).unwrap_or_else(|_| {
                    Normal::new(*mean, 1.0).expect("stddev=1.0 is always valid")
                });
                let offset = dist.sample(rng).max(0.0);
                (offset * factor) as i64
            }
            RelativeOffsetMode::Distribution {
                kind,
                min_ms,
                max_ms,
            } => {
                let raw = match kind {
                    RelativeDistKind::Normal(mean, std_dev) => {
                        let d = Normal::new(*mean, std_dev.max(1e-9)).unwrap_or_else(|_| {
                            Normal::new(*mean, 1.0).expect("stddev=1.0 is always valid")
                        });
                        d.sample(rng)
                    }
                    RelativeDistKind::LogNormal(mu, sigma) => {
                        let d =
                            rand_distr::LogNormal::new(*mu, sigma.max(1e-9)).unwrap_or_else(|_| {
                                rand_distr::LogNormal::new(*mu, 0.5)
                                    .expect("sigma=0.5 is always valid")
                            });
                        d.sample(rng)
                    }
                    RelativeDistKind::Uniform(lo, hi) => {
                        let d = Uniform::new(*lo, hi.max(lo + 1e-9));
                        d.sample(rng)
                    }
                    RelativeDistKind::Exponential(lambda) => {
                        let d = rand_distr::Exp::new(lambda.max(1e-9)).unwrap_or_else(|_| {
                            rand_distr::Exp::new(1.0).expect("lambda=1.0 is always valid")
                        });
                        d.sample(rng)
                    }
                };
                let offset_ms = (raw * factor) as i64;
                // Apply min/max clamping
                let clamped = match (min_ms, max_ms) {
                    (Some(lo), Some(hi)) => offset_ms.max(*lo as i64).min(*hi as i64),
                    (Some(lo), None) => offset_ms.max(*lo as i64),
                    (None, Some(hi)) => offset_ms.min(*hi as i64),
                    (None, None) => offset_ms,
                };
                clamped
            }
            RelativeOffsetMode::Constant { offset_ms } => *offset_ms as i64,
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
                } else if let Some(int_arr) = arr.as_any().downcast_ref::<Int64Array>() {
                    // Temporal sequences produce Int64 (epoch ms) before coercion
                    (0..int_arr.len()).map(|i| int_arr.value(i)).collect()
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

        let values: Vec<i64> = base_values
            .iter()
            .map(|&b| {
                let offset = self.sample_offset(rng);
                b.saturating_add(offset)
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
                let amp = params
                    .get(&format!("s{n}_amplitude"))
                    .copied()
                    .unwrap_or(0.1);
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
            .unwrap_or_else(|_| Normal::new(0.0, 1.0).expect("stddev=1.0 is always valid"));
        let base_offset = ctx.row_offset as i64;

        let values: Vec<i64> = (0..count)
            .map(|i| {
                let idx = base_offset.saturating_add(i as i64);
                let base_t = self
                    .start
                    .saturating_add(idx.saturating_mul(self.interval_ms));
                let trend = (self.trend_slope * idx as f64) as i64;

                let seasonal: f64 = self
                    .seasonality
                    .iter()
                    .map(|s| {
                        let t_frac =
                            (base_t as f64 + trend as f64) / s.period_ms.max(1) as f64 + s.phase;
                        s.amplitude
                            * (self.interval_ms as f64)
                            * (2.0 * std::f64::consts::PI * t_frac).sin()
                    })
                    .sum();

                let noise = if self.noise_std > 0.0 {
                    noise_dist.sample(rng) as i64
                } else {
                    0
                };

                base_t
                    .saturating_add(trend)
                    .saturating_add(seasonal as i64)
                    .saturating_add(noise)
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
/// Each generated timestamp falls between `start_hour` and `end_hour`.
/// Active days are controlled by a bitmask (bit 0=Mon, bit 6=Sun).
/// When a `timezone_field` is present, business hours are interpreted in
/// each row's local timezone and converted to UTC for storage.
///
/// # Output
///
/// `DataType::Timestamp(Millisecond, None)` — always stored as UTC epoch millis.
pub struct BusinessHoursGenerator {
    /// Epoch-millis for the first possible date.
    start_date_ms: i64,
    /// Epoch-millis for the last possible date (end of day).
    end_date_ms: Option<i64>,
    /// Inclusive start hour (0–23).
    start_hour: u8,
    /// Exclusive end hour (1–24, must be > start_hour).
    end_hour: u8,
    /// Bitmask of active days: bit 0=Mon, bit 1=Tue, ..., bit 6=Sun.
    days_mask: u8,
    /// Fixed IANA timezone (e.g. "America/New_York").
    timezone: Option<chrono_tz::Tz>,
    /// Name of the field containing per-row timezone strings.
    timezone_field: Option<String>,
    /// Set of NaiveDate values to exclude (holidays).
    exclude_dates: std::collections::HashSet<NaiveDate>,
}

impl BusinessHoursGenerator {
    /// Create from plan parameters.
    ///
    /// Numeric keys: `start_hour`, `end_hour`, `days_mask`,
    /// `date_range_min_ms`, `date_range_max_ms`, `start_date`.
    /// String keys: `timezone`, `timezone_field`, `exclude_dates`.
    pub fn new(
        params: &BTreeMap<String, f64>,
        string_params: &BTreeMap<String, String>,
    ) -> Self {
        let start_hour = (params.get("start_hour").copied().unwrap_or(9.0) as u8).min(23);
        let end_hour = (params.get("end_hour").copied().unwrap_or(17.0) as u8).min(24);
        let end_hour = end_hour.max(start_hour + 1).min(24);
        let days_mask = params.get("days_mask").copied().unwrap_or(31.0) as u8;

        let start_date_ms = params
            .get("date_range_min_ms")
            .or_else(|| params.get("start_date"))
            .copied()
            .unwrap_or(0.0) as i64;
        let end_date_ms = params
            .get("date_range_max_ms")
            .map(|v| *v as i64 + 86_400_000 - 1); // end of max day (inclusive)

        let timezone = string_params
            .get("timezone")
            .and_then(|s| s.parse::<chrono_tz::Tz>().ok());
        let timezone_field = string_params.get("timezone_field").cloned();

        let exclude_dates: std::collections::HashSet<NaiveDate> = string_params
            .get("exclude_dates")
            .map(|s| {
                s.split(',')
                    .filter_map(|d| NaiveDate::parse_from_str(d.trim(), "%Y-%m-%d").ok())
                    .collect()
            })
            .unwrap_or_default();

        Self {
            start_date_ms,
            end_date_ms,
            start_hour,
            end_hour,
            days_mask,
            timezone,
            timezone_field,
            exclude_dates,
        }
    }

    /// Check if a weekday (0=Mon, 6=Sun) is active per the days_mask.
    fn is_active_day(&self, weekday_from_monday: u32) -> bool {
        // If days_mask is 0 (no active days), treat all days as active to prevent infinite loop
        if self.days_mask == 0 {
            return true;
        }
        self.days_mask & (1 << weekday_from_monday) != 0
    }

    /// Check if a date is excluded.
    fn is_excluded(&self, date: &NaiveDate) -> bool {
        self.exclude_dates.contains(date)
    }

    /// Resolve timezone for a given row from batch_columns, falling back to
    /// the fixed timezone or UTC.
    fn resolve_tz(
        &self,
        row_idx: usize,
        ctx: &crate::gen::context::GenContext,
    ) -> chrono_tz::Tz {
        if let Some(ref tz_field) = self.timezone_field {
            if let Some(col) = ctx.batch_columns.get(tz_field) {
                if let Some(arr) = col.as_any().downcast_ref::<arrow::array::StringArray>() {
                    if row_idx < arr.len() && !arr.is_null(row_idx) {
                        if let Ok(tz) = arr.value(row_idx).parse::<chrono_tz::Tz>() {
                            return tz;
                        }
                    }
                }
            }
        }
        self.timezone.unwrap_or(chrono_tz::UTC)
    }
}

impl FieldGenerator for BusinessHoursGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let nominal_range_ms = (self.end_hour as i64 - self.start_hour as i64) * 3_600_000;
        let base_offset = ctx.row_offset as i64;

        let has_per_row_tz = self.timezone_field.is_some();
        let fixed_tz = self.timezone;

        let mut values = Vec::with_capacity(count);
        let start_dt = Utc
            .timestamp_millis_opt(self.start_date_ms)
            .single()
            .unwrap_or_else(|| {
                Utc.timestamp_millis_opt(0)
                    .single()
                    .expect("epoch 0 is always valid")
            });

        let mut day_cursor: NaiveDate = start_dt.date_naive();

        // Helper: compute actual UTC start/end millis for a given day + timezone.
        // On DST transition days the real interval may differ from the nominal span.
        let compute_utc_interval =
            |date: &NaiveDate, tz: chrono_tz::Tz, sh: u8, eh: u8| -> (i64, i64) {
                let local_start = date.and_hms_opt(sh as u32, 0, 0).unwrap();
                let local_end = if eh == 24 {
                    (*date + chrono::Duration::days(1))
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                } else {
                    date.and_hms_opt(eh as u32, 0, 0).unwrap()
                };
                let utc_start = tz
                    .from_local_datetime(&local_start)
                    .earliest()
                    .unwrap_or_else(|| {
                        // Nonexistent (DST spring-forward): try +1h
                        tz.from_local_datetime(
                            &date
                                .and_hms_opt((sh as u32 + 1).min(23), 0, 0)
                                .unwrap(),
                        )
                        .earliest()
                        .unwrap_or_else(|| {
                            Utc.timestamp_millis_opt(0)
                                .single()
                                .unwrap()
                                .with_timezone(&tz)
                        })
                    })
                    .with_timezone(&Utc)
                    .timestamp_millis();
                let utc_end = tz
                    .from_local_datetime(&local_end)
                    .earliest()
                    .unwrap_or_else(|| {
                        tz.from_local_datetime(
                            &date
                                .and_hms_opt((eh as u32).min(23), 0, 0)
                                .unwrap(),
                        )
                        .earliest()
                        .unwrap_or_else(|| {
                            Utc.timestamp_millis_opt(0)
                                .single()
                                .unwrap()
                                .with_timezone(&tz)
                        })
                    })
                    .with_timezone(&Utc)
                    .timestamp_millis();
                (utc_start, utc_end.max(utc_start + 1))
            };

        // Max days to search before giving up (prevents infinite loop on impossible configs)
        let max_search_days: i64 = if self.end_date_ms.is_some() {
            // If date range is set, one full cycle + 7 should suffice
            let range_days = (self.end_date_ms.unwrap() - self.start_date_ms) / 86_400_000 + 7;
            range_days.max(14)
        } else {
            // Without date range, 365 days should find at least one valid day
            365
        };

        // Advance cursor by row_offset worth of valid days for partition safety.
        let mut skip = base_offset;
        let mut search_count: i64 = 0;
        while skip > 0 {
            if search_count > max_search_days + skip {
                tracing::warn!("BusinessHoursGenerator: no valid days found after {} searches during offset skip", search_count);
                break;
            }
            let wd = day_cursor.weekday().num_days_from_monday();
            if self.is_active_day(wd) && !self.is_excluded(&day_cursor) {
                if let Some(end_ms) = self.end_date_ms {
                    let day_ms = day_cursor
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_utc()
                        .timestamp_millis();
                    if day_ms > end_ms {
                        day_cursor = start_dt.date_naive();
                        search_count += 1;
                        continue;
                    }
                }
                skip -= 1;
            }
            day_cursor += chrono::Duration::days(1);
            search_count += 1;
        }

        let mut generated = 0;
        let mut consecutive_invalid = 0i64;
        while generated < count {
            if consecutive_invalid > max_search_days {
                tracing::warn!("BusinessHoursGenerator: no valid days found after {} consecutive searches, stopping generation", consecutive_invalid);
                break;
            }

            let wd = day_cursor.weekday().num_days_from_monday();
            if !self.is_active_day(wd) || self.is_excluded(&day_cursor) {
                day_cursor += chrono::Duration::days(1);
                if let Some(end_ms) = self.end_date_ms {
                    let day_ms = day_cursor
                        .and_hms_opt(0, 0, 0)
                        .unwrap()
                        .and_utc()
                        .timestamp_millis();
                    if day_ms > end_ms {
                        day_cursor = start_dt.date_naive();
                    }
                }
                consecutive_invalid += 1;
                continue;
            }
            consecutive_invalid = 0;

            if has_per_row_tz {
                // Per-row timezone: compute real UTC interval for this day
                let tz = self.resolve_tz(generated, ctx);
                let (utc_start, utc_end) =
                    compute_utc_interval(&day_cursor, tz, self.start_hour, self.end_hour);
                let range = utc_end - utc_start;
                let offset_ms = Uniform::new(0i64, range.max(1)).sample(rng);
                values.push(utc_start + offset_ms);
            } else if let Some(tz) = fixed_tz {
                // Fixed timezone: compute real UTC interval for this day
                let (utc_start, utc_end) =
                    compute_utc_interval(&day_cursor, tz, self.start_hour, self.end_hour);
                let range = utc_end - utc_start;
                let offset_ms = Uniform::new(0i64, range.max(1)).sample(rng);
                values.push(utc_start + offset_ms);
            } else {
                // No timezone — interpret as UTC (backward compatible)
                let day_start = day_cursor
                    .and_hms_opt(self.start_hour as u32, 0, 0)
                    .expect("start_hour is clamped to 0–23");
                let day_start_ms = day_start.and_utc().timestamp_millis();
                let offset_ms = Uniform::new(0i64, nominal_range_ms.max(1)).sample(rng);
                values.push(day_start_ms + offset_ms);
            }

            generated += 1;
            day_cursor += chrono::Duration::days(1);
            if let Some(end_ms) = self.end_date_ms {
                let day_ms = day_cursor
                    .and_hms_opt(0, 0, 0)
                    .unwrap()
                    .and_utc()
                    .timestamp_millis();
                if day_ms > end_ms {
                    day_cursor = start_dt.date_naive();
                }
            }
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
        let gen = RelativeGenerator::new("created_at".into(), &params, &BTreeMap::new());

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 5, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        for (i, &bv) in base_values.iter().enumerate() {
            assert!(ts.value(i) >= bv, "row {i}: {} < {}", ts.value(i), bv);
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
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
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
        params.insert("days_mask".into(), 0x1F as f64);
        let gen = BusinessHoursGenerator::new(&params, &BTreeMap::new());

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 50, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        for i in 0..50 {
            let dt = Utc.timestamp_millis_opt(ts.value(i)).unwrap();
            let hour = dt.hour();
            assert!((9..17).contains(&hour), "row {i}: hour {hour} outside 9–17");
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
        params.insert("days_mask".into(), 0x7F as f64);
        let gen = BusinessHoursGenerator::new(&params, &BTreeMap::new());

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
        params.insert("days_mask".into(), 0x7F as f64);
        let gen = BusinessHoursGenerator::new(&params, &BTreeMap::new());

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 50, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

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
        let gen = RelativeGenerator::new("nonexistent".into(), &params, &BTreeMap::new());

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 5, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

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
        let gen = RelativeGenerator::new("ts".into(), &params, &BTreeMap::new());

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 5, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

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
        let gen = RelativeGenerator::new("x".into(), &params, &BTreeMap::new());
        assert_eq!(
            gen.output_type(),
            DataType::Timestamp(arrow::datatypes::TimeUnit::Millisecond, None)
        );
    }

    #[test]
    fn relative_constant_offset() {
        // Constant mode: offset_mode=2, constant_ms=86400000 (1 day)
        let mut params = BTreeMap::new();
        params.insert("offset_mode".into(), 2.0);
        params.insert("constant_ms".into(), 86_400_000.0); // 1 day in ms
        let gen = RelativeGenerator::new("ts".into(), &params, &BTreeMap::new());

        let base_ms = 1_700_000_000_000i64;
        let mut cols = HashMap::new();
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![base_ms; 5])) as ArrayRef,
        );
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 5, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        for i in 0..5 {
            assert_eq!(
                ts.value(i),
                base_ms + 86_400_000,
                "row {i}: expected constant 1-day offset"
            );
        }
    }

    #[test]
    fn relative_distribution_log_normal() {
        let mut params = BTreeMap::new();
        params.insert("offset_mode".into(), 1.0);
        params.insert("mu".into(), 1.0);
        params.insert("sigma".into(), 0.5);
        params.insert("unit".into(), 0.0); // seconds
        let mut string_params = BTreeMap::new();
        string_params.insert("distribution".into(), "log_normal".into());

        let gen = RelativeGenerator::new("ts".into(), &params, &string_params);

        let base_ms = 1_000_000i64;
        let mut cols = HashMap::new();
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![base_ms; 100])) as ArrayRef,
        );
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 100, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        for i in 0..100 {
            assert!(
                ts.value(i) > base_ms,
                "row {i}: log_normal offset should be positive, got {} vs base {}",
                ts.value(i),
                base_ms
            );
        }
    }

    #[test]
    fn relative_distribution_with_min_max_clamping() {
        let mut params = BTreeMap::new();
        params.insert("offset_mode".into(), 1.0);
        params.insert("mean".into(), 5.0);
        params.insert("std_dev".into(), 100.0);
        params.insert("unit".into(), 0.0);
        params.insert("min_ms".into(), 2_000.0);
        params.insert("max_ms".into(), 10_000.0);
        let mut string_params = BTreeMap::new();
        string_params.insert("distribution".into(), "normal".into());

        let gen = RelativeGenerator::new("ts".into(), &params, &string_params);

        let base_ms = 0i64;
        let mut cols = HashMap::new();
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![base_ms; 200])) as ArrayRef,
        );
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 200, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        for i in 0..200 {
            let v = ts.value(i);
            assert!(
                v >= 2_000 && v <= 10_000,
                "row {i}: value {} not in [2000, 10000]",
                v
            );
        }
    }

    #[test]
    fn relative_distribution_uniform() {
        let mut params = BTreeMap::new();
        params.insert("offset_mode".into(), 1.0);
        params.insert("low".into(), 1.0);
        params.insert("high".into(), 5.0);
        params.insert("unit".into(), 3.0); // days
        let mut string_params = BTreeMap::new();
        string_params.insert("distribution".into(), "uniform".into());

        let gen = RelativeGenerator::new("ts".into(), &params, &string_params);

        let base_ms = 0i64;
        let mut cols = HashMap::new();
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![base_ms; 50])) as ArrayRef,
        );
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 50, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        let day_ms = 86_400_000i64;
        for i in 0..50 {
            let v = ts.value(i);
            assert!(
                v >= day_ms && v <= 5 * day_ms,
                "row {i}: value {} not in [1 day, 5 days]",
                v
            );
        }
    }

    #[test]
    fn relative_distribution_exponential() {
        let mut params = BTreeMap::new();
        params.insert("offset_mode".into(), 1.0);
        params.insert("lambda".into(), 0.5);
        params.insert("unit".into(), 2.0); // hours
        let mut string_params = BTreeMap::new();
        string_params.insert("distribution".into(), "exponential".into());

        let gen = RelativeGenerator::new("ts".into(), &params, &string_params);

        let base_ms = 1_000_000i64;
        let mut cols = HashMap::new();
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![base_ms; 50])) as ArrayRef,
        );
        let cols: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let ctx = GenContext::new(cols, 0, 0, 1, "test");

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 50, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        for i in 0..50 {
            assert!(
                ts.value(i) > base_ms,
                "row {i}: exponential offset should be positive"
            );
        }
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
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

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
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

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
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        // Compute gaps and verify the empirical std is near the configured noise_std=1000ms
        let gaps: Vec<i64> = (1..100).map(|i| ts.value(i) - ts.value(i - 1)).collect();
        let mean_gap = gaps.iter().sum::<i64>() as f64 / gaps.len() as f64;
        let variance = gaps
            .iter()
            .map(|&g| (g as f64 - mean_gap).powi(2))
            .sum::<f64>()
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
        let ts0 = arr0
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        // Partition 1: rows 5-9 (row_offset=5)
        let ctx1 = GenContext::new(map, 5, 0, 1, "test");
        let mut rng2 = ChaCha8Rng::seed_from_u64(99);
        let arr1 = gen.generate(&mut rng2, 5, &ctx1);
        let ts1 = arr1
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

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
        params.insert("days_mask".into(), 0x7F as f64);
        let gen = BusinessHoursGenerator::new(&params, &BTreeMap::new());

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 7, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

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
        assert!(
            has_weekend,
            "weekdays_only=false should include weekend days"
        );
    }

    #[test]
    fn business_hours_deterministic() {
        let mut params = BTreeMap::new();
        params.insert("start_date".into(), 1_704_067_200_000.0);
        params.insert("start_hour".into(), 9.0);
        params.insert("end_hour".into(), 17.0);
        params.insert("days_mask".into(), 0x1F as f64);
        let gen = BusinessHoursGenerator::new(&params, &BTreeMap::new());

        let ctx = empty_ctx();
        let mut rng1 = ChaCha8Rng::seed_from_u64(42);
        let arr1 = gen.generate(&mut rng1, 20, &ctx);
        let mut rng2 = ChaCha8Rng::seed_from_u64(42);
        let arr2 = gen.generate(&mut rng2, 20, &ctx);

        let ts1 = arr1
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        let ts2 = arr2
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        for i in 0..20 {
            assert_eq!(ts1.value(i), ts2.value(i), "row {i} mismatch");
        }
    }

    #[test]
    fn business_hours_days_mask() {
        // Only Tuesday and Thursday (bits 1 and 3)
        let mut params = BTreeMap::new();
        // 2024-01-01 is Monday
        params.insert("start_date".into(), 1_704_067_200_000.0);
        params.insert("start_hour".into(), 9.0);
        params.insert("end_hour".into(), 17.0);
        params.insert("days_mask".into(), 10.0); // bits 1 and 3 = Tue + Thu
        let gen = BusinessHoursGenerator::new(&params, &BTreeMap::new());

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 20, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        for i in 0..20 {
            let dt = Utc.timestamp_millis_opt(ts.value(i)).unwrap();
            let wd = dt.weekday().num_days_from_monday();
            assert!(
                wd == 1 || wd == 3,
                "row {i}: expected Tue(1) or Thu(3), got weekday {wd}"
            );
        }
    }

    #[test]
    fn business_hours_exclude_dates() {
        let mut params = BTreeMap::new();
        // 2024-01-01 is Monday
        params.insert("start_date".into(), 1_704_067_200_000.0);
        params.insert("start_hour".into(), 9.0);
        params.insert("end_hour".into(), 17.0);
        params.insert("days_mask".into(), 127.0); // all days
        let mut string_params = BTreeMap::new();
        // Exclude Jan 2 and Jan 3
        string_params.insert(
            "exclude_dates".into(),
            "2024-01-02,2024-01-03".into(),
        );
        let gen = BusinessHoursGenerator::new(&params, &string_params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 10, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        for i in 0..10 {
            let dt = Utc.timestamp_millis_opt(ts.value(i)).unwrap();
            let date = dt.date_naive();
            assert_ne!(
                date,
                NaiveDate::from_ymd_opt(2024, 1, 2).unwrap(),
                "row {i}: should skip Jan 2"
            );
            assert_ne!(
                date,
                NaiveDate::from_ymd_opt(2024, 1, 3).unwrap(),
                "row {i}: should skip Jan 3"
            );
        }
    }

    #[test]
    fn business_hours_date_range_wraps() {
        let mut params = BTreeMap::new();
        params.insert("start_hour".into(), 0.0);
        params.insert("end_hour".into(), 24.0);
        params.insert("days_mask".into(), 127.0); // all days
        // 2024-01-01 to 2024-01-05 (5 days)
        let min_ms = NaiveDate::from_ymd_opt(2024, 1, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis() as f64;
        let max_ms = NaiveDate::from_ymd_opt(2024, 1, 5)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap()
            .and_utc()
            .timestamp_millis() as f64;
        params.insert("date_range_min_ms".into(), min_ms);
        params.insert("date_range_max_ms".into(), max_ms);
        let gen = BusinessHoursGenerator::new(&params, &BTreeMap::new());

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        // Generate more rows than days to force wrapping
        let arr = gen.generate(&mut rng, 15, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        let min_date = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        let max_date = NaiveDate::from_ymd_opt(2024, 1, 5).unwrap();
        for i in 0..15 {
            let dt = Utc.timestamp_millis_opt(ts.value(i)).unwrap();
            let date = dt.date_naive();
            assert!(
                date >= min_date && date <= max_date,
                "row {i}: date {date} outside range {min_date}..{max_date}"
            );
        }
    }

    #[test]
    fn business_hours_fixed_timezone() {
        let mut params = BTreeMap::new();
        // 2024-01-15 (Monday) — no DST in January
        params.insert("start_date".into(), 1_705_276_800_000.0);
        params.insert("start_hour".into(), 9.0);
        params.insert("end_hour".into(), 17.0);
        params.insert("days_mask".into(), 31.0); // weekdays
        let mut string_params = BTreeMap::new();
        string_params.insert("timezone".into(), "America/New_York".into());
        let gen = BusinessHoursGenerator::new(&params, &string_params);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = empty_ctx();
        let arr = gen.generate(&mut rng, 10, &ctx);
        let ts = arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();

        // In January, EST is UTC-5. 9 AM EST = 14 UTC, 5 PM EST = 22 UTC
        for i in 0..10 {
            let dt = Utc.timestamp_millis_opt(ts.value(i)).unwrap();
            let hour = dt.hour();
            assert!(
                (14..22).contains(&hour),
                "row {i}: UTC hour {hour} should be in 14–22 (9-17 EST)"
            );
        }
    }
}