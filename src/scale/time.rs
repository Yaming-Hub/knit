//! Time dimension scaling — cadence detection and date extension.

use crate::core::types::PartitionValue;

use super::{Cadence, TimeDimension};

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
    let cadence = time_dim.cadence.unwrap_or(Cadence::Days(7));

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
        let end = apply_duration(last, range_spec)?;
        (first, end)
    } else if spec.contains("..") {
        // Explicit range: 2024-01-01..2025-12-31
        let parts: Vec<&str> = spec.splitn(2, "..").collect();
        let start = parse_date(parts[0])
            .ok_or_else(|| anyhow::anyhow!("invalid start date in range: '{}'", parts[0]))?;
        let end = parse_date(parts[1])
            .ok_or_else(|| anyhow::anyhow!("invalid end date in range: '{}'", parts[1]))?;
        (start, end)
    } else {
        // Duration from the original start: 52w, 6m, 365d, 2y
        let end = apply_duration(first, spec)?;
        (first, end)
    };

    if target_end < target_start {
        anyhow::bail!(
            "target end date ({}) is before start date ({})",
            target_end,
            target_start
        );
    }

    // Generate dates at cadence intervals
    let new_dates = step_dates(target_start, target_end, cadence);

    // Convert to partition values with uniform weights
    let weight = 1.0 / new_dates.len().max(1) as f64;
    let values: Vec<PartitionValue> = new_dates
        .iter()
        .map(|d| PartitionValue {
            value: d.format("%Y-%m-%d").to_string(),
            weight,
        })
        .collect();

    Ok(values)
}

/// Step through dates from start to end using the given cadence.
fn step_dates(
    start: chrono::NaiveDate,
    end: chrono::NaiveDate,
    cadence: Cadence,
) -> Vec<chrono::NaiveDate> {
    let mut dates = Vec::new();

    match cadence {
        Cadence::Days(n) => {
            let mut current = start;
            while current <= end {
                dates.push(current);
                current += chrono::Duration::days(n as i64);
            }
        }
        Cadence::Months(n) => {
            // Detect end-of-month: if start is the last day of its month,
            // always step to the last day of each target month.
            let is_eom = start.day() == days_in_month(start.year(), start.month());
            let anchor_day = if is_eom { 31 } else { start.day() };
            let mut months_offset = 0u32;
            loop {
                let d = if is_eom {
                    // End-of-month: always use the last day
                    let total =
                        start.year() * 12 + (start.month() as i32 - 1) + months_offset as i32;
                    let y = total / 12;
                    let m = (total % 12) as u32 + 1;
                    chrono::NaiveDate::from_ymd_opt(y, m, days_in_month(y, m)).unwrap()
                } else {
                    add_months_anchored(start, months_offset, anchor_day)
                };
                if d > end {
                    break;
                }
                dates.push(d);
                months_offset += n;
            }
        }
    }

    if dates.is_empty() {
        dates.push(start);
    }
    dates
}

/// Add N calendar months from a base date, using an anchored day-of-month.
///
/// The `anchor_day` is the original intended day (e.g., 31 for end-of-month).
/// This prevents drift: Jan 31 + 1m = Feb 29, + 2m = Mar 31 (not Mar 29).
fn add_months_anchored(base: chrono::NaiveDate, months: u32, anchor_day: u32) -> chrono::NaiveDate {
    let total_months = base.year() * 12 + (base.month() as i32 - 1) + months as i32;
    let new_year = total_months / 12;
    let new_month = (total_months % 12) as u32 + 1;
    let max_day = days_in_month(new_year, new_month);
    let new_day = anchor_day.min(max_day);
    chrono::NaiveDate::from_ymd_opt(new_year, new_month, new_day)
        .expect("valid date after month addition")
}

/// Return the number of days in a given month.
pub(crate) fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

use chrono::Datelike;

/// Apply a duration spec to a base date, using calendar-aware addition for months/years.
fn apply_duration(base: chrono::NaiveDate, spec: &str) -> anyhow::Result<chrono::NaiveDate> {
    let spec = spec.trim();
    if spec.is_empty() {
        anyhow::bail!("empty duration spec");
    }

    let (num_str, unit) = spec.split_at(spec.len() - 1);
    let num: f64 = num_str
        .parse()
        .map_err(|_| anyhow::anyhow!("invalid duration number in '{}'", spec))?;

    match unit {
        "d" => Ok(base + chrono::Duration::days(num.round() as i64)),
        "w" => Ok(base + chrono::Duration::days((num * 7.0).round() as i64)),
        "m" => {
            // Calendar month addition
            let months = num.round() as u32;
            let is_eom = base.day() == days_in_month(base.year(), base.month());
            if is_eom {
                let total = base.year() * 12 + (base.month() as i32 - 1) + months as i32;
                let y = total / 12;
                let m = (total % 12) as u32 + 1;
                Ok(chrono::NaiveDate::from_ymd_opt(y, m, days_in_month(y, m)).unwrap())
            } else {
                Ok(add_months_anchored(base, months, base.day()))
            }
        }
        "y" => {
            // Calendar year addition (= 12 months)
            let months = (num * 12.0).round() as u32;
            let is_eom = base.day() == days_in_month(base.year(), base.month());
            if is_eom {
                let total = base.year() * 12 + (base.month() as i32 - 1) + months as i32;
                let y = total / 12;
                let m = (total % 12) as u32 + 1;
                Ok(chrono::NaiveDate::from_ymd_opt(y, m, days_in_month(y, m)).unwrap())
            } else {
                Ok(add_months_anchored(base, months, base.day()))
            }
        }
        _ => anyhow::bail!(
            "unknown duration unit '{}' in '{}'; use d/w/m/y",
            unit,
            spec
        ),
    }
}

/// Parse a date string (YYYY-MM-DD, YYYY/MM/DD, or YYYYMMDD).
fn parse_date(s: &str) -> Option<chrono::NaiveDate> {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .or_else(|_| chrono::NaiveDate::parse_from_str(s, "%Y/%m/%d"))
        .or_else(|_| chrono::NaiveDate::parse_from_str(s, "%Y%m%d"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_apply_duration() {
        let base = chrono::NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();
        assert_eq!(
            apply_duration(base, "52w").unwrap(),
            base + chrono::Duration::days(364)
        );
        assert_eq!(
            apply_duration(base, "7d").unwrap(),
            base + chrono::Duration::days(7)
        );
        // 6m from Jan 1 = Jul 1 (calendar month addition)
        assert_eq!(
            apply_duration(base, "6m").unwrap(),
            chrono::NaiveDate::from_ymd_opt(2024, 7, 1).unwrap()
        );
        // 2y from Jan 1 = Jan 1 two years later
        assert_eq!(
            apply_duration(base, "2y").unwrap(),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()
        );
    }

    #[test]
    fn test_apply_duration_eom() {
        // 6m from Jan 31 = Jul 31 (end-of-month aware)
        let jan31 = chrono::NaiveDate::from_ymd_opt(2024, 1, 31).unwrap();
        assert_eq!(
            apply_duration(jan31, "6m").unwrap(),
            chrono::NaiveDate::from_ymd_opt(2024, 7, 31).unwrap()
        );
        // 1m from Feb 29 (leap year, EOM) = Mar 31 (last day of March)
        let feb29 = chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap();
        assert_eq!(
            apply_duration(feb29, "1m").unwrap(),
            chrono::NaiveDate::from_ymd_opt(2024, 3, 31).unwrap()
        );
    }

    #[test]
    fn test_compute_partitions_duration() {
        let dim = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-01-01".into(), "2024-01-08".into()],
            cadence: Some(Cadence::Days(7)),
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
            cadence: Some(Cadence::Days(7)),
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
            cadence: Some(Cadence::Days(7)),
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
            cadence: Some(Cadence::Days(7)),
            cadence_confidence: 1.0,
        };
        let result = compute_new_partitions(&dim, "3w").unwrap();
        let total: f64 = result.iter().map(|v| v.weight).sum();
        assert!((total - 1.0).abs() < 1e-9);
    }

    #[test]
    fn test_cadence_override_changes_output() {
        // With weekly cadence: 28 days / 7 = 5 partitions
        let dim_weekly = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-01-01".into()],
            cadence: Some(Cadence::Days(7)),
            cadence_confidence: 1.0,
        };
        let weekly = compute_new_partitions(&dim_weekly, "4w").unwrap();
        assert_eq!(weekly.len(), 5);

        // With daily cadence: 28 days / 1 = 29 partitions
        let dim_daily = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-01-01".into()],
            cadence: Some(Cadence::Days(1)),
            cadence_confidence: 1.0,
        };
        let daily = compute_new_partitions(&dim_daily, "4w").unwrap();
        assert_eq!(daily.len(), 29);
    }

    #[test]
    fn test_parse_date_yyyymmdd() {
        assert!(parse_date("20240101").is_some());
        assert_eq!(
            parse_date("20240101").unwrap(),
            parse_date("2024-01-01").unwrap()
        );
    }

    #[test]
    fn test_yyyymmdd_partitions() {
        let dim = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["20240101".into(), "20240108".into()],
            cadence: Some(Cadence::Days(7)),
            cadence_confidence: 1.0,
        };
        let result = compute_new_partitions(&dim, "3w").unwrap();
        assert!(result.len() >= 3);
    }

    #[test]
    fn test_monthly_cadence_basic() {
        let dim = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-01-01".into()],
            cadence: Some(Cadence::Months(1)),
            cadence_confidence: 1.0,
        };
        let result = compute_new_partitions(&dim, "2024-01-01..2024-06-30").unwrap();
        assert_eq!(result.len(), 6); // Jan, Feb, Mar, Apr, May, Jun
        assert_eq!(result[0].value, "2024-01-01");
        assert_eq!(result[1].value, "2024-02-01");
        assert_eq!(result[5].value, "2024-06-01");
    }

    #[test]
    fn test_monthly_cadence_end_of_month_clamp() {
        // Starting Jan 31, monthly stepping should clamp to end-of-month
        let dim = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-01-31".into()],
            cadence: Some(Cadence::Months(1)),
            cadence_confidence: 1.0,
        };
        let result = compute_new_partitions(&dim, "2024-01-31..2024-05-31").unwrap();
        assert_eq!(result[0].value, "2024-01-31");
        assert_eq!(result[1].value, "2024-02-29"); // Leap year
        assert_eq!(result[2].value, "2024-03-31");
        assert_eq!(result[3].value, "2024-04-30"); // 30-day month
        assert_eq!(result[4].value, "2024-05-31");
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn test_quarterly_cadence() {
        let dim = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-01-01".into()],
            cadence: Some(Cadence::Months(3)),
            cadence_confidence: 1.0,
        };
        let result = compute_new_partitions(&dim, "1y").unwrap();
        // 2024-01-01 + 1y = 2025-01-01 (calendar year)
        // Q1=Jan1, Q2=Apr1, Q3=Jul1, Q4=Oct1, Q5=Jan1(2025) = 5 partitions
        assert_eq!(result.len(), 5);
        assert_eq!(result[0].value, "2024-01-01");
        assert_eq!(result[1].value, "2024-04-01");
        assert_eq!(result[2].value, "2024-07-01");
        assert_eq!(result[3].value, "2024-10-01");
        assert_eq!(result[4].value, "2025-01-01");
    }

    #[test]
    fn test_add_months_anchored_basic() {
        let d = chrono::NaiveDate::from_ymd_opt(2024, 1, 15).unwrap();
        assert_eq!(
            add_months_anchored(d, 1, 15),
            chrono::NaiveDate::from_ymd_opt(2024, 2, 15).unwrap()
        );
        assert_eq!(
            add_months_anchored(d, 12, 15),
            chrono::NaiveDate::from_ymd_opt(2025, 1, 15).unwrap()
        );
    }

    #[test]
    fn test_add_months_anchored_clamp() {
        let jan31 = chrono::NaiveDate::from_ymd_opt(2024, 1, 31).unwrap();
        // Feb has 29 days in 2024 (leap year), but anchor 31 clamps to 29
        assert_eq!(
            add_months_anchored(jan31, 1, 31),
            chrono::NaiveDate::from_ymd_opt(2024, 2, 29).unwrap()
        );
        // Mar has 31 days — anchor 31 fits
        assert_eq!(
            add_months_anchored(jan31, 2, 31),
            chrono::NaiveDate::from_ymd_opt(2024, 3, 31).unwrap()
        );
        // Non-leap year
        let jan31_2023 = chrono::NaiveDate::from_ymd_opt(2023, 1, 31).unwrap();
        assert_eq!(
            add_months_anchored(jan31_2023, 1, 31),
            chrono::NaiveDate::from_ymd_opt(2023, 2, 28).unwrap()
        );
    }

    #[test]
    fn test_add_months_anchored_year_boundary() {
        let dec = chrono::NaiveDate::from_ymd_opt(2024, 12, 15).unwrap();
        assert_eq!(
            add_months_anchored(dec, 1, 15),
            chrono::NaiveDate::from_ymd_opt(2025, 1, 15).unwrap()
        );
        assert_eq!(
            add_months_anchored(dec, 13, 15),
            chrono::NaiveDate::from_ymd_opt(2026, 1, 15).unwrap()
        );
    }

    #[test]
    fn test_monthly_cadence_from_feb29() {
        // Starting Feb 29 (EOM), should step to end-of-month for each month
        let dim = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-02-29".into()],
            cadence: Some(Cadence::Months(1)),
            cadence_confidence: 1.0,
        };
        let result = compute_new_partitions(&dim, "2024-02-29..2024-06-30").unwrap();
        assert_eq!(result[0].value, "2024-02-29");
        assert_eq!(result[1].value, "2024-03-31");
        assert_eq!(result[2].value, "2024-04-30");
        assert_eq!(result[3].value, "2024-05-31");
        assert_eq!(result[4].value, "2024-06-30");
        assert_eq!(result.len(), 5);
    }

    #[test]
    fn test_time_6m_from_jan31_with_monthly_cadence() {
        // --time 6m from Jan 31 with monthly cadence should produce 7 partitions
        let dim = TimeDimension {
            entity_name: "Events".into(),
            partition_field: "date".into(),
            partition_values: vec!["2024-01-31".into()],
            cadence: Some(Cadence::Months(1)),
            cadence_confidence: 1.0,
        };
        let result = compute_new_partitions(&dim, "6m").unwrap();
        // target_end = Jan 31 + 6 months = Jul 31
        assert_eq!(result[0].value, "2024-01-31");
        assert_eq!(result[1].value, "2024-02-29");
        assert_eq!(result[2].value, "2024-03-31");
        assert_eq!(result[3].value, "2024-04-30");
        assert_eq!(result[4].value, "2024-05-31");
        assert_eq!(result[5].value, "2024-06-30");
        assert_eq!(result[6].value, "2024-07-31");
        assert_eq!(result.len(), 7);
    }
}
