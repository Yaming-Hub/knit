//! Type inference for string columns.
//!
//! Detects hidden semantic types within string data: integers, floats,
//! booleans, dates, UUIDs, and categorical values. Also detects common
//! string patterns (email, phone, URL, UUID).

use std::collections::HashMap;

use regex::Regex;
use tracing::debug;

/// Semantic type inferred from column data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InferredType {
    /// All values parse as integers.
    Integer,
    /// All values parse as floating-point numbers.
    Float,
    /// All values parse as booleans (true/false, yes/no, 0/1).
    Boolean,
    /// All values match a date pattern.
    Date(DateFormat),
    /// All values look like UUIDs.
    Uuid,
    /// Low cardinality — likely a categorical/enum column.
    Categorical,
    /// No specific type detected; remains a free-form string.
    Text,
}

/// Detected date format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DateFormat {
    /// ISO 8601 (`2024-01-15`, `2024-01-15T10:30:00`).
    Iso8601,
    /// US format (`01/15/2024`, `1/15/2024`).
    Us,
    /// European format (`15/01/2024`, `15.01.2024`).
    Eu,
    /// Other/custom pattern.
    Custom(String),
}

/// Common string pattern detected via regex.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum StringPattern {
    /// Email address.
    Email,
    /// Phone number.
    Phone,
    /// UUID v4 (or similar).
    Uuid,
    /// URL / URI.
    Url,
    /// Date string.
    Date,
    /// Person name (first + last).
    Name,
}

/// Result of type inference on a single column.
#[derive(Debug, Clone)]
pub struct TypeInference {
    /// The inferred semantic type.
    pub inferred_type: InferredType,
    /// Fraction of non-null values that matched the type (0.0–1.0).
    pub confidence: f64,
    /// Detected string patterns and their match rates.
    pub patterns: HashMap<StringPattern, f64>,
    /// If a date type was detected, the format.
    pub date_format: Option<DateFormat>,
}

/// Infer the semantic type of a string column.
///
/// Examines all values and returns the best-fit type with a confidence score.
/// Null and empty values are excluded from analysis.
///
/// The `categorical_threshold` controls when a column is classified as
/// categorical — if `distinct / total <= threshold`, the column is considered
/// categorical.
pub fn infer_type(values: &[Option<&str>], categorical_threshold: f64) -> TypeInference {
    let non_null: Vec<&str> = values.iter().filter_map(|v| *v).filter(|v| !v.is_empty()).collect();

    if non_null.is_empty() {
        return TypeInference {
            inferred_type: InferredType::Text,
            confidence: 0.0,
            patterns: HashMap::new(),
            date_format: None,
        };
    }

    let total = non_null.len() as f64;

    // Check patterns
    let patterns = detect_patterns(&non_null);

    // Try integer
    let int_count = non_null.iter().filter(|v| v.parse::<i64>().is_ok()).count();
    if int_count as f64 / total >= 0.95 {
        debug!(rate = int_count as f64 / total, "Inferred Integer");
        return TypeInference {
            inferred_type: InferredType::Integer,
            confidence: int_count as f64 / total,
            patterns,
            date_format: None,
        };
    }

    // Try float
    let float_count = non_null.iter().filter(|v| v.parse::<f64>().is_ok()).count();
    if float_count as f64 / total >= 0.95 {
        debug!(rate = float_count as f64 / total, "Inferred Float");
        return TypeInference {
            inferred_type: InferredType::Float,
            confidence: float_count as f64 / total,
            patterns,
            date_format: None,
        };
    }

    // Try boolean
    let bool_count = non_null
        .iter()
        .filter(|v| {
            matches!(
                v.to_lowercase().as_str(),
                "true" | "false" | "yes" | "no" | "0" | "1" | "t" | "f" | "y" | "n"
            )
        })
        .count();
    if bool_count as f64 / total >= 0.95 {
        return TypeInference {
            inferred_type: InferredType::Boolean,
            confidence: bool_count as f64 / total,
            patterns,
            date_format: None,
        };
    }

    // Try UUID
    let uuid_re = Regex::new(
        r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
    )
    .unwrap();
    let uuid_count = non_null.iter().filter(|v| uuid_re.is_match(v)).count();
    if uuid_count as f64 / total >= 0.95 {
        return TypeInference {
            inferred_type: InferredType::Uuid,
            confidence: uuid_count as f64 / total,
            patterns,
            date_format: None,
        };
    }

    // Try date
    if let Some((fmt, rate)) = detect_date_format(&non_null) {
        if rate >= 0.90 {
            return TypeInference {
                inferred_type: InferredType::Date(fmt.clone()),
                confidence: rate,
                patterns,
                date_format: Some(fmt),
            };
        }
    }

    // Check categorical (low cardinality), but only if no strong semantic pattern was detected
    let has_strong_pattern = patterns.values().any(|&rate| rate > 0.8);
    let mut distinct: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for v in &non_null {
        distinct.insert(v);
    }
    let cardinality_ratio = distinct.len() as f64 / total;
    if !has_strong_pattern && cardinality_ratio <= categorical_threshold && distinct.len() <= 200 {
        return TypeInference {
            inferred_type: InferredType::Categorical,
            confidence: 1.0 - cardinality_ratio,
            patterns,
            date_format: None,
        };
    }

    TypeInference {
        inferred_type: InferredType::Text,
        confidence: 1.0,
        patterns,
        date_format: None,
    }
}

/// Detect common string patterns and their match rates.
fn detect_patterns(values: &[&str]) -> HashMap<StringPattern, f64> {
    let total = values.len() as f64;
    if total == 0.0 {
        return HashMap::new();
    }

    let email_re = Regex::new(r"^[a-zA-Z0-9._%+-]+@[a-zA-Z0-9.-]+\.[a-zA-Z]{2,}$").unwrap();
    let phone_re = Regex::new(r"^\+?\d[\d\s\-\(\)]{6,14}$").unwrap();
    let uuid_re = Regex::new(
        r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$",
    )
    .unwrap();
    let url_re = Regex::new(r"^https?://[^\s]+$").unwrap();
    let date_re = Regex::new(r"^\d{4}-\d{2}-\d{2}").unwrap();
    // Name pattern: 2-4 capitalized words (e.g., "John Smith", "Mary Jane Watson")
    let name_re = Regex::new(r"^[A-Z][a-z]+(?:\s[A-Z][a-z]+){1,3}$").unwrap();

    let checks: Vec<(StringPattern, &Regex)> = vec![
        (StringPattern::Email, &email_re),
        (StringPattern::Phone, &phone_re),
        (StringPattern::Uuid, &uuid_re),
        (StringPattern::Url, &url_re),
        (StringPattern::Date, &date_re),
        (StringPattern::Name, &name_re),
    ];

    let mut result = HashMap::new();
    for (pattern, re) in checks {
        let count = values.iter().filter(|v| re.is_match(v)).count();
        let rate = count as f64 / total;
        if rate > 0.1 {
            result.insert(pattern, rate);
        }
    }

    result
}

/// Attempt to detect the dominant date format.
///
/// Returns the format and match rate for the best candidate.
fn detect_date_format(values: &[&str]) -> Option<(DateFormat, f64)> {
    let total = values.len() as f64;
    if total == 0.0 {
        return None;
    }

    let iso_re = Regex::new(r"^\d{4}-\d{2}-\d{2}").unwrap();
    let us_re = Regex::new(r"^\d{1,2}/\d{1,2}/\d{4}$").unwrap();
    let eu_re = Regex::new(r"^\d{1,2}\.\d{1,2}\.\d{4}$").unwrap();

    let iso_count = values.iter().filter(|v| iso_re.is_match(v)).count();
    let us_count = values.iter().filter(|v| us_re.is_match(v)).count();
    let eu_count = values.iter().filter(|v| eu_re.is_match(v)).count();

    let mut best: Option<(DateFormat, usize)> = None;
    for (fmt, count) in [
        (DateFormat::Iso8601, iso_count),
        (DateFormat::Us, us_count),
        (DateFormat::Eu, eu_count),
    ] {
        if count > best.as_ref().map_or(0, |b| b.1) {
            best = Some((fmt, count));
        }
    }

    best.map(|(fmt, count)| (fmt, count as f64 / total))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infer_integer_column() {
        let vals: Vec<Option<&str>> = vec![Some("1"), Some("2"), Some("3"), Some("100")];
        let result = infer_type(&vals, 0.05);
        assert_eq!(result.inferred_type, InferredType::Integer);
        assert!(result.confidence >= 0.95);
    }

    #[test]
    fn infer_float_column() {
        let vals: Vec<Option<&str>> =
            vec![Some("1.5"), Some("2.7"), Some("3.14"), Some("0.001")];
        let result = infer_type(&vals, 0.05);
        assert_eq!(result.inferred_type, InferredType::Float);
    }

    #[test]
    fn infer_boolean_column() {
        let vals: Vec<Option<&str>> =
            vec![Some("true"), Some("false"), Some("yes"), Some("no")];
        let result = infer_type(&vals, 0.05);
        assert_eq!(result.inferred_type, InferredType::Boolean);
    }

    #[test]
    fn infer_uuid_column() {
        let vals: Vec<Option<&str>> = vec![
            Some("550e8400-e29b-41d4-a716-446655440000"),
            Some("6ba7b810-9dad-11d1-80b4-00c04fd430c8"),
            Some("f47ac10b-58cc-4372-a567-0e02b2c3d479"),
        ];
        let result = infer_type(&vals, 0.05);
        assert_eq!(result.inferred_type, InferredType::Uuid);
    }

    #[test]
    fn infer_date_iso() {
        let vals: Vec<Option<&str>> = vec![
            Some("2024-01-15"),
            Some("2024-02-20"),
            Some("2023-12-01"),
        ];
        let result = infer_type(&vals, 0.05);
        assert!(matches!(
            result.inferred_type,
            InferredType::Date(DateFormat::Iso8601)
        ));
    }

    #[test]
    fn infer_categorical() {
        let vals: Vec<Option<&str>> = vec![
            Some("red"),
            Some("blue"),
            Some("red"),
            Some("green"),
            Some("blue"),
            Some("red"),
            Some("green"),
            Some("blue"),
            Some("red"),
            Some("green"),
        ];
        let result = infer_type(&vals, 0.5);
        assert_eq!(result.inferred_type, InferredType::Categorical);
    }

    #[test]
    fn infer_text_fallback() {
        let vals: Vec<Option<&str>> = vec![
            Some("The quick brown fox"),
            Some("jumps over the lazy dog"),
            Some("Hello, world!"),
            Some("Lorem ipsum dolor sit amet"),
        ];
        let result = infer_type(&vals, 0.05);
        assert_eq!(result.inferred_type, InferredType::Text);
    }

    #[test]
    fn detect_email_pattern() {
        let vals = vec![
            "alice@example.com",
            "bob@test.org",
            "carol@domain.co.uk",
        ];
        let patterns = detect_patterns(&vals);
        assert!(patterns.contains_key(&StringPattern::Email));
        assert!(*patterns.get(&StringPattern::Email).unwrap() > 0.9);
    }

    #[test]
    fn detect_url_pattern() {
        let vals = vec![
            "https://example.com",
            "http://test.org/path",
            "https://domain.co.uk/a/b",
        ];
        let patterns = detect_patterns(&vals);
        assert!(patterns.contains_key(&StringPattern::Url));
    }

    #[test]
    fn empty_values() {
        let vals: Vec<Option<&str>> = vec![None, None];
        let result = infer_type(&vals, 0.05);
        assert_eq!(result.inferred_type, InferredType::Text);
        assert_eq!(result.confidence, 0.0);
    }
}
