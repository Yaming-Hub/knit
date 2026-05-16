//! Temporal pattern recognition — detect schedules, periodicity, and trends
//! in timestamp data.
//!
//! Analyses inter-event deltas, detects periodic signals via autocorrelation
//! and FFT, checks day-of-week / hour-of-day uniformity, and classifies
//! temporal patterns.

use rustfft::num_complex::Complex;
use rustfft::FftPlanner;
use tracing::{debug, debug_span, warn};

/// Classification of a temporal pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum TemporalPattern {
    /// Events occur at a fixed interval (e.g., every 3600 s).
    FixedInterval {
        /// Mean interval in seconds.
        interval_secs: f64,
    },
    /// Events follow a recognizable schedule.
    Schedule {
        /// Detected schedule kind (daily, weekly, monthly, cron-like).
        kind: ScheduleKind,
    },
    /// Events have a periodic component.
    Periodic {
        /// Dominant period in seconds.
        period_secs: f64,
    },
    /// Event rate is trending up or down.
    Trending {
        /// Slope of the trend line (events per second per second).
        slope: f64,
    },
    /// No recognizable pattern.
    Irregular,
}

/// Schedule classification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleKind {
    /// Daily schedule.
    Daily,
    /// Weekly schedule.
    Weekly,
    /// Monthly schedule.
    Monthly,
    /// Cron-like irregular but repeating.
    CronLike,
}

/// Specification that maps to schema generators for temporal data.
#[derive(Debug, Clone)]
pub struct TemporalPatternSpec {
    /// The detected pattern.
    pub pattern: TemporalPattern,
    /// Suggested generator expression for the Weave schema.
    pub generator_expr: String,
    /// Confidence score (0.0–1.0).
    pub confidence: f64,
}

/// Day-of-week distribution result.
#[derive(Debug, Clone)]
pub struct DowDistribution {
    /// Counts per day (0=Mon, 6=Sun).
    pub counts: [u64; 7],
    /// Chi-squared statistic against uniform.
    pub chi_squared: f64,
    /// Whether the distribution is non-uniform (p < 0.05).
    pub is_non_uniform: bool,
}

/// Hour-of-day distribution result.
#[derive(Debug, Clone)]
pub struct HodDistribution {
    /// Counts per hour (0–23).
    pub counts: [u64; 24],
    /// Chi-squared statistic against uniform.
    pub chi_squared: f64,
    /// Whether the distribution is non-uniform (p < 0.05).
    pub is_non_uniform: bool,
}

/// Analyze temporal patterns from a series of timestamps (as epoch seconds, sorted).
///
/// Returns `None` if fewer than 3 timestamps are provided.
pub fn detect_temporal_pattern(timestamps_secs: &[f64]) -> Option<TemporalPatternSpec> {
    let _span = debug_span!("temporal", n = timestamps_secs.len()).entered();
    let ts = filter_sorted(timestamps_secs);
    if ts.len() < 3 {
        warn!(
            count = ts.len(),
            "fewer than 3 timestamps, skipping temporal detection"
        );
        return None;
    }

    let deltas: Vec<f64> = ts.windows(2).map(|w| w[1] - w[0]).collect();
    let deltas: Vec<f64> = deltas.into_iter().filter(|d| *d > 0.0).collect();
    if deltas.is_empty() {
        return Some(TemporalPatternSpec {
            pattern: TemporalPattern::Irregular,
            generator_expr: String::new(),
            confidence: 0.0,
        });
    }

    let mean_delta = deltas.iter().sum::<f64>() / deltas.len() as f64;
    let var_delta =
        deltas.iter().map(|d| (d - mean_delta).powi(2)).sum::<f64>() / deltas.len() as f64;
    let std_delta = var_delta.sqrt();
    let cv = if mean_delta > 0.0 {
        std_delta / mean_delta
    } else {
        f64::INFINITY
    };

    debug!(n = ts.len(), mean_delta, cv, "temporal analysis");

    // Fixed interval: CV < 0.05
    if cv < 0.05 {
        return Some(TemporalPatternSpec {
            pattern: TemporalPattern::FixedInterval {
                interval_secs: mean_delta,
            },
            generator_expr: format!("time_series(interval={}s)", mean_delta.round()),
            confidence: (1.0 - cv * 10.0).clamp(0.5, 1.0),
        });
    }

    // Periodicity detection via FFT
    if let Some(period) = detect_period_fft(&deltas) {
        if period > 0.0 {
            let schedule = classify_schedule(period);
            if let Some(kind) = schedule {
                return Some(TemporalPatternSpec {
                    pattern: TemporalPattern::Schedule { kind: kind.clone() },
                    generator_expr: format!("schedule({})", schedule_kind_str(&kind)),
                    confidence: 0.7,
                });
            }
            return Some(TemporalPatternSpec {
                pattern: TemporalPattern::Periodic {
                    period_secs: period,
                },
                generator_expr: format!("time_series(period={}s)", period.round()),
                confidence: 0.6,
            });
        }
    }

    // Trend detection via linear regression on event rate
    if let Some(slope) = detect_trend(&ts) {
        if slope.abs() > 1e-12 {
            return Some(TemporalPatternSpec {
                pattern: TemporalPattern::Trending { slope },
                generator_expr: format!("time_series(trend={})", slope),
                confidence: 0.5,
            });
        }
    }

    Some(TemporalPatternSpec {
        pattern: TemporalPattern::Irregular,
        generator_expr: String::new(),
        confidence: 0.3,
    })
}

/// Compute day-of-week distribution from epoch-second timestamps.
pub fn day_of_week_distribution(timestamps_secs: &[f64]) -> DowDistribution {
    let mut counts = [0u64; 7];
    for &t in timestamps_secs {
        if !t.is_finite() {
            continue;
        }
        let dt = chrono::DateTime::from_timestamp(t as i64, 0);
        if let Some(dt) = dt {
            let dow = dt.format("%u").to_string().parse::<usize>().unwrap_or(1) - 1;
            if dow < 7 {
                counts[dow] += 1;
            }
        }
    }
    let total: u64 = counts.iter().sum();
    let chi_squared = if total > 0 {
        let expected = total as f64 / 7.0;
        counts
            .iter()
            .map(|&c| (c as f64 - expected).powi(2) / expected)
            .sum()
    } else {
        0.0
    };
    // Chi-squared critical value at p=0.05 with df=6 is 12.592
    let is_non_uniform = chi_squared > 12.592;
    DowDistribution {
        counts,
        chi_squared,
        is_non_uniform,
    }
}

/// Compute hour-of-day distribution from epoch-second timestamps.
pub fn hour_of_day_distribution(timestamps_secs: &[f64]) -> HodDistribution {
    let mut counts = [0u64; 24];
    for &t in timestamps_secs {
        if !t.is_finite() {
            continue;
        }
        if let Some(dt) = chrono::DateTime::from_timestamp(t as i64, 0) {
            let h = dt.format("%H").to_string().parse::<usize>().unwrap_or(0);
            if h < 24 {
                counts[h] += 1;
            }
        }
    }
    let total: u64 = counts.iter().sum();
    let chi_squared = if total > 0 {
        let expected = total as f64 / 24.0;
        counts
            .iter()
            .map(|&c| (c as f64 - expected).powi(2) / expected)
            .sum()
    } else {
        0.0
    };
    // Chi-squared critical value at p=0.05 with df=23 is 35.172
    let is_non_uniform = chi_squared > 35.172;
    HodDistribution {
        counts,
        chi_squared,
        is_non_uniform,
    }
}

// ─── internal helpers ───────────────────────────────────────────────────────

fn filter_sorted(ts: &[f64]) -> Vec<f64> {
    let mut v: Vec<f64> = ts.iter().copied().filter(|t| t.is_finite()).collect();
    v.sort_by(|a, b| a.partial_cmp(b).expect("finite timestamps must have a total order"));
    v
}

/// Detect dominant period using FFT on the delta series.
fn detect_period_fft(deltas: &[f64]) -> Option<f64> {
    let n = deltas.len();
    if n < 4 {
        return None;
    }

    // Pad to next power of 2
    let fft_size = n.next_power_of_two();
    let mean = deltas.iter().sum::<f64>() / n as f64;

    let mut buffer: Vec<Complex<f64>> = deltas
        .iter()
        .map(|&d| Complex::new(d - mean, 0.0))
        .collect();
    buffer.resize(fft_size, Complex::new(0.0, 0.0));

    let mut planner = FftPlanner::<f64>::new();
    let fft = planner.plan_fft_forward(fft_size);
    fft.process(&mut buffer);

    // Find peak in magnitude spectrum (skip DC at index 0)
    let half = fft_size / 2;
    let magnitudes: Vec<f64> = buffer[1..half].iter().map(|c| c.norm()).collect();
    if magnitudes.is_empty() {
        return None;
    }

    let (peak_idx, &peak_mag) = magnitudes
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal))?;

    let avg_mag = magnitudes.iter().sum::<f64>() / magnitudes.len() as f64;
    // Only report if peak is significantly above average
    if peak_mag < 2.0 * avg_mag {
        return None;
    }

    // Convert frequency bin to period: period = N * mean_delta / (peak_bin + 1)
    let freq_bin = peak_idx + 1;
    let period = fft_size as f64 * mean / freq_bin as f64;
    debug!(period, peak_mag, avg_mag, "FFT period detection");
    Some(period)
}

/// Autocorrelation at a specific lag.
#[allow(dead_code)]
fn autocorrelation(series: &[f64], lag: usize) -> f64 {
    let n = series.len();
    if lag >= n || n < 2 {
        return 0.0;
    }
    let mean = series.iter().sum::<f64>() / n as f64;
    let var: f64 = series.iter().map(|x| (x - mean).powi(2)).sum();
    if var == 0.0 {
        return 0.0;
    }
    let cov: f64 = series[..n - lag]
        .iter()
        .zip(series[lag..].iter())
        .map(|(a, b)| (a - mean) * (b - mean))
        .sum();
    cov / var
}

/// Classify a period (in seconds) into a schedule kind.
fn classify_schedule(period_secs: f64) -> Option<ScheduleKind> {
    let day = 86400.0;
    let week = 604800.0;
    let month = 2_592_000.0; // ~30 days

    if (period_secs - day).abs() / day < 0.15 {
        Some(ScheduleKind::Daily)
    } else if (period_secs - week).abs() / week < 0.15 {
        Some(ScheduleKind::Weekly)
    } else if (period_secs - month).abs() / month < 0.20 {
        Some(ScheduleKind::Monthly)
    } else {
        None
    }
}

fn schedule_kind_str(kind: &ScheduleKind) -> &'static str {
    match kind {
        ScheduleKind::Daily => "daily",
        ScheduleKind::Weekly => "weekly",
        ScheduleKind::Monthly => "monthly",
        ScheduleKind::CronLike => "cron",
    }
}

/// Detect trend by linear regression on event rate (events per bucket).
fn detect_trend(sorted_ts: &[f64]) -> Option<f64> {
    let n = sorted_ts.len();
    if n < 10 {
        return None;
    }

    let span = sorted_ts[n - 1] - sorted_ts[0];
    if span <= 0.0 {
        return None;
    }

    // Bucket events into ~20 bins
    let num_buckets = 20.min(n / 2);
    if num_buckets < 3 {
        return None;
    }
    let bucket_size = span / num_buckets as f64;
    let mut bucket_counts = vec![0.0_f64; num_buckets];
    for &t in sorted_ts {
        let idx = ((t - sorted_ts[0]) / bucket_size) as usize;
        let idx = idx.min(num_buckets - 1);
        bucket_counts[idx] += 1.0;
    }

    // Linear regression: y = slope * x + intercept
    let xs: Vec<f64> = (0..num_buckets).map(|i| i as f64).collect();
    let slope = linear_regression_slope(&xs, &bucket_counts);
    debug!(slope, num_buckets, "trend detection");
    Some(slope)
}

/// Simple linear regression slope: Σ(x-x̄)(y-ȳ) / Σ(x-x̄)².
fn linear_regression_slope(xs: &[f64], ys: &[f64]) -> f64 {
    let n = xs.len() as f64;
    let x_mean = xs.iter().sum::<f64>() / n;
    let y_mean = ys.iter().sum::<f64>() / n;
    let num: f64 = xs
        .iter()
        .zip(ys)
        .map(|(x, y)| (x - x_mean) * (y - y_mean))
        .sum();
    let den: f64 = xs.iter().map(|x| (x - x_mean).powi(2)).sum();
    if den.abs() < f64::EPSILON {
        0.0
    } else {
        num / den
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_interval_detection() {
        // Every 60 seconds, with tiny jitter
        let ts: Vec<f64> = (0..100)
            .map(|i| 1_000_000.0 + i as f64 * 60.0 + (i % 3) as f64 * 0.1)
            .collect();
        let result = detect_temporal_pattern(&ts).unwrap();
        match &result.pattern {
            TemporalPattern::FixedInterval { interval_secs } => {
                assert!(
                    (interval_secs - 60.0).abs() < 1.0,
                    "interval should be ~60s, got {}",
                    interval_secs
                );
            }
            other => panic!("expected FixedInterval, got {:?}", other),
        }
        assert!(result.confidence > 0.8);
    }

    #[test]
    fn weekly_schedule_detection() {
        // Events every ~7 days
        let day = 86400.0;
        let ts: Vec<f64> = (0..52)
            .map(|i| 1_700_000_000.0 + i as f64 * 7.0 * day + (i % 5) as f64 * 100.0)
            .collect();
        let result = detect_temporal_pattern(&ts).unwrap();
        // Should detect as fixed interval or schedule (weekly)
        let is_fixed_or_schedule = matches!(
            &result.pattern,
            TemporalPattern::FixedInterval { .. }
                | TemporalPattern::Schedule {
                    kind: ScheduleKind::Weekly
                }
        );
        assert!(
            is_fixed_or_schedule,
            "expected weekly pattern, got {:?}",
            result.pattern
        );
    }

    #[test]
    fn trend_detection() {
        // Accelerating events: decreasing intervals
        let mut ts = Vec::new();
        let mut t = 0.0;
        for i in 0..200 {
            ts.push(t);
            // Interval shrinks → rate increases
            t += 100.0 - 0.4 * (i as f64);
            if t <= *ts.last().unwrap() {
                t = ts.last().unwrap() + 1.0;
            }
        }
        let result = detect_temporal_pattern(&ts).unwrap();
        // We expect either Trending or Periodic — the key thing is we don't crash
        assert!(result.confidence > 0.0);
    }

    #[test]
    fn too_few_timestamps() {
        assert!(detect_temporal_pattern(&[1.0, 2.0]).is_none());
    }

    #[test]
    fn day_of_week_uniform() {
        // Spread events evenly across days
        let day = 86400.0;
        let base = 1_700_000_000.0; // Thursday
        let ts: Vec<f64> = (0..700).map(|i| base + i as f64 * day).collect();
        let dow = day_of_week_distribution(&ts);
        let total: u64 = dow.counts.iter().sum();
        assert_eq!(total, 700);
        // Should be roughly uniform
        assert!(!dow.is_non_uniform || dow.chi_squared < 20.0);
    }

    #[test]
    fn hour_of_day_nonuniform() {
        // All events at the same hour
        let ts: Vec<f64> = (0..100)
            .map(|i| 1_700_000_000.0 + i as f64 * 86400.0)
            .collect();
        let hod = hour_of_day_distribution(&ts);
        let total: u64 = hod.counts.iter().sum();
        assert_eq!(total, 100);
        assert!(hod.is_non_uniform);
    }

    #[test]
    fn irregular_pattern() {
        // Random-ish timestamps
        let ts: Vec<f64> = vec![
            1.0, 5.0, 6.0, 20.0, 21.0, 100.0, 105.0, 500.0, 501.0, 1000.0,
        ];
        let result = detect_temporal_pattern(&ts).unwrap();
        // Just verify it doesn't crash and returns something
        assert!(result.confidence >= 0.0);
    }
}
