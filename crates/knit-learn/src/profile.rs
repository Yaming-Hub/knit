//! Column profiling — compute statistical profiles for Arrow record batches.
//!
//! Produces a [`ColumnProfile`] for each column, including basic statistics
//! (count, nulls, distinct), plus optional numeric, string, and temporal
//! sub-profiles.

use std::collections::HashSet;

use arrow::array::{
    Array, AsArray, TimestampMicrosecondArray,
    TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray,
};
use arrow::compute::concat_batches;
use arrow::datatypes::{DataType, TimeUnit};
use arrow::record_batch::RecordBatch;
use tracing::debug;

use crate::error::LearnResult;

/// Full profile for a single column.
#[derive(Debug, Clone)]
pub struct ColumnProfile {
    /// Column name.
    pub name: String,
    /// Arrow data type.
    pub data_type: DataType,
    /// Total number of rows.
    pub count: u64,
    /// Number of null values.
    pub null_count: u64,
    /// Fraction of null values (0.0–1.0).
    pub null_rate: f64,
    /// Number of distinct non-null values (if computed).
    pub distinct_count: Option<u64>,
    /// Ratio of distinct values to total non-null values.
    pub cardinality_ratio: Option<f64>,
    /// Numeric statistics (for integer/float columns).
    pub numeric: Option<NumericProfile>,
    /// String statistics (for UTF-8 columns).
    pub string: Option<StringProfile>,
    /// Temporal statistics (for date/timestamp columns).
    pub temporal: Option<TemporalProfile>,
}

/// Numeric column statistics.
#[derive(Debug, Clone)]
pub struct NumericProfile {
    /// Minimum value.
    pub min: f64,
    /// Maximum value.
    pub max: f64,
    /// Arithmetic mean.
    pub mean: f64,
    /// Median (p50).
    pub median: f64,
    /// Standard deviation (sample).
    pub std_dev: f64,
    /// Skewness.
    pub skewness: f64,
    /// Excess kurtosis.
    pub kurtosis: f64,
    /// Percentiles: p1, p5, p10, p25, p50, p75, p90, p95, p99.
    pub percentiles: Percentiles,
}

/// Named percentiles.
#[derive(Debug, Clone)]
pub struct Percentiles {
    /// 1st percentile.
    pub p1: f64,
    /// 5th percentile.
    pub p5: f64,
    /// 10th percentile.
    pub p10: f64,
    /// 25th percentile.
    pub p25: f64,
    /// 50th percentile (median).
    pub p50: f64,
    /// 75th percentile.
    pub p75: f64,
    /// 90th percentile.
    pub p90: f64,
    /// 95th percentile.
    pub p95: f64,
    /// 99th percentile.
    pub p99: f64,
}

/// String column statistics.
#[derive(Debug, Clone)]
pub struct StringProfile {
    /// Minimum string length.
    pub min_length: usize,
    /// Maximum string length.
    pub max_length: usize,
    /// Average string length.
    pub avg_length: f64,
    /// Detected patterns and their match rates.
    pub patterns: Vec<(String, f64)>,
}

/// Temporal column statistics.
#[derive(Debug, Clone)]
pub struct TemporalProfile {
    /// Earliest timestamp (as ISO 8601 string).
    pub min: String,
    /// Latest timestamp (as ISO 8601 string).
    pub max: String,
    /// Detected granularity (e.g., "second", "day").
    pub granularity: String,
}

/// Compute profiles for all columns in the given record batches.
///
/// Concatenates all batches and profiles each column independently.
///
/// # Errors
///
/// Returns `LearnError` if Arrow concatenation fails.
pub fn compute_profiles(batches: &[RecordBatch]) -> LearnResult<Vec<ColumnProfile>> {
    if batches.is_empty() {
        return Ok(vec![]);
    }

    let schema = batches[0].schema();
    let combined = concat_batches(&schema, batches)?;
    let mut profiles = Vec::with_capacity(schema.fields().len());

    for (i, field) in schema.fields().iter().enumerate() {
        let col = combined.column(i);
        let profile = profile_column(field.name(), field.data_type(), col);
        profiles.push(profile);
    }

    debug!(columns = profiles.len(), "Profiling complete");
    Ok(profiles)
}

/// Profile a single column array.
fn profile_column(name: &str, data_type: &DataType, array: &dyn Array) -> ColumnProfile {
    let count = array.len() as u64;
    let null_count = array.null_count() as u64;
    let null_rate = if count > 0 {
        null_count as f64 / count as f64
    } else {
        0.0
    };

    let (distinct_count, cardinality_ratio) = compute_distinct(data_type, array);
    let numeric = compute_numeric(data_type, array);
    let string = compute_string(data_type, array);
    let temporal = compute_temporal(data_type, array);

    ColumnProfile {
        name: name.to_string(),
        data_type: data_type.clone(),
        count,
        null_count,
        null_rate,
        distinct_count,
        cardinality_ratio,
        numeric,
        string,
        temporal,
    }
}

/// Maximum number of distinct values to track before switching to approximate.
const MAX_DISTINCT_TRACK: usize = 100_000;

/// Compute distinct count for supported types.
///
/// Caps tracking at [`MAX_DISTINCT_TRACK`] entries to bound memory usage.
/// When the cap is reached, returns the cap value as an approximation.
fn compute_distinct(data_type: &DataType, array: &dyn Array) -> (Option<u64>, Option<f64>) {
    let non_null = array.len() - array.null_count();
    if non_null == 0 {
        return (Some(0), Some(0.0));
    }

    let distinct = match data_type {
        DataType::Utf8 => {
            let arr = array.as_string::<i32>();
            let mut set = HashSet::new();
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    set.insert(arr.value(i).to_string());
                    if set.len() >= MAX_DISTINCT_TRACK {
                        break;
                    }
                }
            }
            set.len() as u64
        }
        DataType::Int64 => {
            let arr = array.as_primitive::<arrow::datatypes::Int64Type>();
            let mut set = HashSet::new();
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    set.insert(arr.value(i));
                    if set.len() >= MAX_DISTINCT_TRACK {
                        break;
                    }
                }
            }
            set.len() as u64
        }
        DataType::Int32 => {
            let arr = array.as_primitive::<arrow::datatypes::Int32Type>();
            let mut set = HashSet::new();
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    set.insert(arr.value(i));
                }
            }
            set.len() as u64
        }
        DataType::Float64 => {
            let arr = array.as_primitive::<arrow::datatypes::Float64Type>();
            let mut set = HashSet::new();
            for i in 0..arr.len() {
                if !arr.is_null(i) {
                    set.insert(arr.value(i).to_bits());
                }
            }
            set.len() as u64
        }
        _ => return (None, None),
    };

    let ratio = distinct as f64 / non_null as f64;
    (Some(distinct), Some(ratio))
}

/// Extract f64 values from numeric columns.
fn extract_f64_values(data_type: &DataType, array: &dyn Array) -> Option<Vec<f64>> {
    match data_type {
        DataType::Int8 => {
            let arr = array.as_primitive::<arrow::datatypes::Int8Type>();
            Some(
                (0..arr.len())
                    .filter(|&i| !arr.is_null(i))
                    .map(|i| arr.value(i) as f64)
                    .collect(),
            )
        }
        DataType::Int16 => {
            let arr = array.as_primitive::<arrow::datatypes::Int16Type>();
            Some(
                (0..arr.len())
                    .filter(|&i| !arr.is_null(i))
                    .map(|i| arr.value(i) as f64)
                    .collect(),
            )
        }
        DataType::Int32 => {
            let arr = array.as_primitive::<arrow::datatypes::Int32Type>();
            Some(
                (0..arr.len())
                    .filter(|&i| !arr.is_null(i))
                    .map(|i| arr.value(i) as f64)
                    .collect(),
            )
        }
        DataType::Int64 => {
            let arr = array.as_primitive::<arrow::datatypes::Int64Type>();
            Some(
                (0..arr.len())
                    .filter(|&i| !arr.is_null(i))
                    .map(|i| arr.value(i) as f64)
                    .collect(),
            )
        }
        DataType::Float32 => {
            let arr = array.as_primitive::<arrow::datatypes::Float32Type>();
            Some(
                (0..arr.len())
                    .filter(|&i| !arr.is_null(i))
                    .map(|i| arr.value(i) as f64)
                    .filter(|v| v.is_finite())
                    .collect(),
            )
        }
        DataType::Float64 => {
            let arr = array.as_primitive::<arrow::datatypes::Float64Type>();
            Some(
                (0..arr.len())
                    .filter(|&i| !arr.is_null(i))
                    .map(|i| arr.value(i))
                    .filter(|v| v.is_finite())
                    .collect(),
            )
        }
        _ => None,
    }
}

/// Compute numeric profile for numeric columns.
fn compute_numeric(data_type: &DataType, array: &dyn Array) -> Option<NumericProfile> {
    let values = extract_f64_values(data_type, array)?;
    if values.is_empty() {
        return None;
    }

    let mut sorted = values.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let n = sorted.len() as f64;
    let sum: f64 = sorted.iter().sum();
    let mean = sum / n;

    let variance = sorted.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (n - 1.0).max(1.0);
    let std_dev = variance.sqrt();

    let skewness = if std_dev > 0.0 && n > 2.0 {
        let m3 = sorted.iter().map(|v| ((v - mean) / std_dev).powi(3)).sum::<f64>();
        m3 * n / ((n - 1.0) * (n - 2.0))
    } else {
        0.0
    };

    let kurtosis = if std_dev > 0.0 && n > 3.0 {
        let m4 = sorted.iter().map(|v| ((v - mean) / std_dev).powi(4)).sum::<f64>();
        
        (n * (n + 1.0) * m4) / ((n - 1.0) * (n - 2.0) * (n - 3.0))
            - 3.0 * (n - 1.0).powi(2) / ((n - 2.0) * (n - 3.0))
    } else {
        0.0
    };

    let percentile = |p: f64| -> f64 {
        let idx = (p / 100.0 * (sorted.len() - 1) as f64).round() as usize;
        sorted[idx.min(sorted.len() - 1)]
    };

    Some(NumericProfile {
        min: sorted[0],
        max: sorted[sorted.len() - 1],
        mean,
        median: percentile(50.0),
        std_dev,
        skewness,
        kurtosis,
        percentiles: Percentiles {
            p1: percentile(1.0),
            p5: percentile(5.0),
            p10: percentile(10.0),
            p25: percentile(25.0),
            p50: percentile(50.0),
            p75: percentile(75.0),
            p90: percentile(90.0),
            p95: percentile(95.0),
            p99: percentile(99.0),
        },
    })
}

/// Compute string profile for UTF-8 columns.
fn compute_string(data_type: &DataType, array: &dyn Array) -> Option<StringProfile> {
    if !matches!(data_type, DataType::Utf8 | DataType::LargeUtf8) {
        return None;
    }

    let arr = array.as_string::<i32>();
    let mut lengths = Vec::new();
    let mut values = Vec::new();

    for i in 0..arr.len() {
        if !arr.is_null(i) {
            let v = arr.value(i);
            lengths.push(v.len());
            values.push(v);
        }
    }

    if lengths.is_empty() {
        return None;
    }

    let min_length = *lengths.iter().min().unwrap();
    let max_length = *lengths.iter().max().unwrap();
    let avg_length = lengths.iter().sum::<usize>() as f64 / lengths.len() as f64;

    // Pattern detection
    let patterns = detect_string_patterns(&values);

    Some(StringProfile {
        min_length,
        max_length,
        avg_length,
        patterns,
    })
}

/// Detect common patterns in string values.
fn detect_string_patterns(values: &[&str]) -> Vec<(String, f64)> {
    use regex::Regex;

    if values.is_empty() {
        return vec![];
    }

    let total = values.len() as f64;
    let checks: Vec<(&str, Regex)> = vec![
        (
            "email",
            Regex::new(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$").unwrap(),
        ),
        ("phone", Regex::new(r"^\+?[\d\s\-\(\)]{7,15}$").unwrap()),
        (
            "uuid",
            Regex::new(
                r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
            )
            .unwrap(),
        ),
        ("url", Regex::new(r"^https?://[^\s]+$").unwrap()),
        ("date", Regex::new(r"^\d{4}-\d{2}-\d{2}").unwrap()),
    ];

    let mut results = Vec::new();
    for (name, re) in &checks {
        let count = values.iter().filter(|v| re.is_match(v)).count();
        let rate = count as f64 / total;
        if rate > 0.1 {
            results.push((name.to_string(), rate));
        }
    }

    results
}

/// Compute temporal profile for timestamp/date columns.
fn compute_temporal(data_type: &DataType, array: &dyn Array) -> Option<TemporalProfile> {
    let (min_ts, max_ts) = match data_type {
        DataType::Timestamp(TimeUnit::Second, _) => {
            let arr = array
                .as_any()
                .downcast_ref::<TimestampSecondArray>()?;
            extract_ts_range(arr.len(), |i| {
                if arr.is_null(i) { None } else { Some(arr.value(i) * 1_000_000) }
            })?
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let arr = array
                .as_any()
                .downcast_ref::<TimestampMillisecondArray>()?;
            extract_ts_range(arr.len(), |i| {
                if arr.is_null(i) { None } else { Some(arr.value(i) * 1_000) }
            })?
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            let arr = array
                .as_any()
                .downcast_ref::<TimestampMicrosecondArray>()?;
            extract_ts_range(arr.len(), |i| {
                if arr.is_null(i) { None } else { Some(arr.value(i)) }
            })?
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            let arr = array
                .as_any()
                .downcast_ref::<TimestampNanosecondArray>()?;
            extract_ts_range(arr.len(), |i| {
                if arr.is_null(i) { None } else { Some(arr.value(i) / 1_000) }
            })?
        }
        DataType::Date32 => {
            let arr = array
                .as_any()
                .downcast_ref::<arrow::array::Date32Array>()?;
            // Date32 stores days since epoch; convert to microseconds
            extract_ts_range(arr.len(), |i| {
                if arr.is_null(i) { None } else { Some(arr.value(i) as i64 * 86_400_000_000) }
            })?
        }
        DataType::Date64 => {
            let arr = array
                .as_any()
                .downcast_ref::<arrow::array::Date64Array>()?;
            // Date64 stores milliseconds since epoch; convert to microseconds
            extract_ts_range(arr.len(), |i| {
                if arr.is_null(i) { None } else { Some(arr.value(i) * 1_000) }
            })?
        }
        _ => return None,
    };

    let fmt_ts = |us: i64| -> String {
        let secs = us / 1_000_000;
        let nanos = ((us % 1_000_000) * 1000) as u32;
        chrono::DateTime::from_timestamp(secs, nanos)
            .map(|dt| dt.format("%Y-%m-%dT%H:%M:%S").to_string())
            .unwrap_or_else(|| format!("{us}μs"))
    };

    let diff_us = max_ts - min_ts;
    let granularity = if diff_us < 1_000_000 {
        "sub-second"
    } else if diff_us < 60 * 1_000_000 {
        "second"
    } else if diff_us < 3600 * 1_000_000 {
        "minute"
    } else if diff_us < 86400 * 1_000_000 {
        "hour"
    } else {
        "day"
    };

    Some(TemporalProfile {
        min: fmt_ts(min_ts),
        max: fmt_ts(max_ts),
        granularity: granularity.to_string(),
    })
}

/// Extract min/max from a timestamp array, values in microseconds.
fn extract_ts_range(
    len: usize,
    get_us: impl Fn(usize) -> Option<i64>,
) -> Option<(i64, i64)> {
    let mut min_v = i64::MAX;
    let mut max_v = i64::MIN;
    let mut found = false;
    for i in 0..len {
        if let Some(v) = get_us(i) {
            found = true;
            if v < min_v {
                min_v = v;
            }
            if v > max_v {
                max_v = v;
            }
        }
    }
    if found {
        Some((min_v, max_v))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Float64Array, Int32Array, StringArray, TimestampSecondArray};
    use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
    use std::sync::Arc;

    fn make_int_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("val", DataType::Int32, true),
        ]));
        RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(vec![
                Some(10),
                Some(20),
                None,
                Some(30),
                Some(40),
                Some(50),
            ]))],
        )
        .unwrap()
    }

    fn make_string_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("text", DataType::Utf8, true),
        ]));
        RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec![
                Some("hello"),
                Some("world"),
                None,
                Some("foo"),
                Some("bar baz"),
            ]))],
        )
        .unwrap()
    }

    #[test]
    fn profile_integer_column() {
        let batch = make_int_batch();
        let profiles = compute_profiles(&[batch]).unwrap();
        assert_eq!(profiles.len(), 1);
        let p = &profiles[0];
        assert_eq!(p.name, "val");
        assert_eq!(p.count, 6);
        assert_eq!(p.null_count, 1);
        assert!(p.numeric.is_some());
        let num = p.numeric.as_ref().unwrap();
        assert_eq!(num.min, 10.0);
        assert_eq!(num.max, 50.0);
    }

    #[test]
    fn profile_string_column() {
        let batch = make_string_batch();
        let profiles = compute_profiles(&[batch]).unwrap();
        assert_eq!(profiles.len(), 1);
        let p = &profiles[0];
        assert_eq!(p.count, 5);
        assert_eq!(p.null_count, 1);
        assert!(p.string.is_some());
        let s = p.string.as_ref().unwrap();
        assert_eq!(s.min_length, 3); // "foo"
        assert_eq!(s.max_length, 7); // "bar baz"
    }

    #[test]
    fn profile_empty_batches() {
        let result = compute_profiles(&[]).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn profile_float_column() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("f", DataType::Float64, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0, 4.0, 5.0]))],
        )
        .unwrap();
        let profiles = compute_profiles(&[batch]).unwrap();
        let num = profiles[0].numeric.as_ref().unwrap();
        assert!((num.mean - 3.0).abs() < 1e-10);
        assert_eq!(num.median, 3.0);
    }

    #[test]
    fn profile_timestamp_column() {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Second, None),
            false,
        )]));
        // 2024-01-01 00:00:00 to 2024-01-02 00:00:00
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(TimestampSecondArray::from(vec![
                1704067200, // 2024-01-01
                1704153600, // 2024-01-02
            ]))],
        )
        .unwrap();
        let profiles = compute_profiles(&[batch]).unwrap();
        let t = profiles[0].temporal.as_ref().unwrap();
        assert!(t.min.contains("2024-01-01"));
        assert!(t.max.contains("2024-01-02"));
        assert_eq!(t.granularity, "day");
    }

    #[test]
    fn distinct_count_strings() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("s", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec!["a", "b", "a", "c", "b"]))],
        )
        .unwrap();
        let profiles = compute_profiles(&[batch]).unwrap();
        assert_eq!(profiles[0].distinct_count, Some(3));
        let ratio = profiles[0].cardinality_ratio.unwrap();
        assert!((ratio - 0.6).abs() < 1e-10);
    }

    #[test]
    fn numeric_percentiles() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("v", DataType::Float64, false),
        ]));
        let vals: Vec<f64> = (1..=100).map(|i| i as f64).collect();
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Float64Array::from(vals))],
        )
        .unwrap();
        let profiles = compute_profiles(&[batch]).unwrap();
        let p = profiles[0].numeric.as_ref().unwrap().percentiles.clone();
        assert!(p.p25 >= 24.0 && p.p25 <= 26.0);
        assert!(p.p75 >= 74.0 && p.p75 <= 76.0);
    }

    #[test]
    fn string_pattern_detection() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("email", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec![
                "alice@example.com",
                "bob@test.org",
                "carol@domain.co.uk",
            ]))],
        )
        .unwrap();
        let profiles = compute_profiles(&[batch]).unwrap();
        let s = profiles[0].string.as_ref().unwrap();
        assert!(s.patterns.iter().any(|(name, rate)| name == "email" && *rate > 0.9));
    }
}
