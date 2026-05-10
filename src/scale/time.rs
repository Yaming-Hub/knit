//! Time dimension scaling — cadence detection and date extension.

use crate::core::types::PartitionValue;

use super::TimeDimension;

/// Compute new partition values from a time spec.
///
/// Supported specs:
/// - Duration: `52w`, `6m`, `365d`, `2y`
/// - Relative: `+26w` (extend beyond current end)
/// - Explicit range: `2024-01-01..2025-12-31`
pub fn compute_new_partitions(
    time_dim: &TimeDimension,
    spec: &str,
) -> anyhow::Result<Vec<PartitionValue>> {
    let cadence_days = time_dim.cadence_days.unwrap_or(7) as i64;

    // Parse existing dates
    let mut dates: Vec<chrono::NaiveDate> = time_dim
        .partition_values
        .iter()
        .filter_map(|v| parse_date(v))
        .collect();
    dates.sort();

    if dates.is_empty() {
        anyhow::bail!("no parseable dates in partition values");
    }

    let first = *dates.first().unwrap();
    let last = *dates.last().unwrap();

    // Parse the spec into a target date range
    let (target_start, target_end) = if let Some(range_spec) = spec.strip_prefix('+') {
        // Relative extension: +26w means extend 26 weeks beyond current end
        let days = parse_duration_days(range_spec)?;
        (first, last + chrono::Duration::days(days))
    } else if spec.contains("..") {
        // Explicit range: 2024-01-01..2025-12-31
        let parts: Vec<&str> = spec.splitn(2, "..").collect();
        let start = parse_date(parts[0]).ok_or_else(|| {
            anyhow::anyhow!("invalid start date in range: '{}'", parts[0])
        })?;
        let end = parse_date(parts[1]).ok_or_else(|| {
            anyhow::anyhow!("invalid end date in range: '{}'", parts[1])
        })?;
        (start, end)
    } else {
        // Duration from the original start: 52w, 6m, 365d, 2y
        let days = parse_duration_days(spec)?;
        (first, first + chrono::Duration::days(days))
    };

    if target_end < target_start {
        anyhow::bail!(
            "target end date ({}) is before start date ({})",
            target_end,
            target_start
        );
    }

    // Generate dates at cadence intervals
    let mut new_dates = Vec::new();
    let mut current = target_start;
    while current <= target_end {
        new_dates.push(current);
        current += chrono::Duration::days(cadence_days);
    }

    if new_dates.is_empty() {
        new_dates.push(target_start);
    }

    // Convert to partition values with uniform weights
    let weight = 1.0 / new_dates.len() as f64;
    let values: Vec<PartitionValue> = new_dates
        .iter()
        .map(|d| PartitionValue {
            value: d.format("%Y-%m-%d").to_string(),
            weight,
        })
        .collect();

    Ok(values)
}

/// Parse a date string (YYYY-MM-DD or YYYY/MM/DD).
fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .or_else(|_| chrono::NaiveDate::parse_from_str(s, "%Y/%m/%d"))
        .ok()
}

/// Parse a duration spec like "52w", "6m", "365d", "2y" into days.
fn parse_duration_days(spec: &str) -> anyhow::Result<i64> {
    let spec = spec.trim();
    if spec.is_empty() {
        anyhow::bail!("empty duration spec");
    }

    let (num_str, unit) = spec.split_at(spec.len() - 1);
    let num: f64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid duration number in '{}'", spec))?;

    let days = match unit {
        "d" => num,
        "w" => num * 7.0,
        "m" => num * 30.0,
        "y" => num * 365.0,
        _ => anyhow::bail!(
            "unknown duration unit '{}' in '{}'; use d/w/m/y",
            unit,
            spec
        ),
    };

    Ok(days.round() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_days() {
        assert_eq!(parse_duration_days("52w").unwrap(), 364);
        assert_eq!(parse_duration_days("7d").unwrap(), 7);
        assert_eq!(parse_duration_days("6m").unwrap(), 180);
        assert_eq!(parse_duration_days("2y").unwrap(), 730);
    }

    #[test]
    fn test_compute_partitions_duration() {
        let dim = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-01-01".into(), "2024-01-08".into()],
            cadence_days: Some(7),
            cadence_confidence: 1.0,
        };
        let result = compute_new_partitions(&dim, "4w").unwrap();
        assert_eq!(result.len(), 5); // 28 days / 7 cadence = 4 intervals + start = 5
        assert_eq!(result[0].value, "2024-01-01");
    }

    #[test]
    fn test_compute_partitions_relative() {
        let dim = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-01-01".into(), "2024-01-08".into()],
            cadence_days: Some(7),
            cadence_confidence: 1.0,
        };
        let result = compute_new_partitions(&dim, "+2w").unwrap();
        // From 2024-01-01 to 2024-01-22 (last + 14 days), cadence 7
        // 2024-01-01, 01-08, 01-15, 01-22 = 4
        assert!(result.len() >= 3);
        assert_eq!(result[0].value, "2024-01-01");
    }

    #[test]
    fn test_compute_partitions_explicit_range() {
        let dim = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-01-01".into()],
            cadence_days: Some(7),
            cadence_confidence: 1.0,
        };
        let result = compute_new_partitions(&dim, "2024-01-01..2024-01-29").unwrap();
        assert_eq!(result.len(), 5); // Jan 1, 8, 15, 22, 29
    }

    #[test]
    fn test_uniform_weights() {
        let dim = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-01-01".into()],
            cadence_days: Some(7),
            cadence_confidence: 1.0,
        };
        let result = compute_new_partitions(&dim, "3w").unwrap();
        let total: f64 = result.iter().map(|v| v.weight).sum();
        assert!((total - 1.0).abs() < 1e-9);
    }
}
