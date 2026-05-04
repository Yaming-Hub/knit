//! String format corruption perturbator.
//!
//! [`FormatCorruptor`] corrupts common string formats (emails, dates, URLs)
//! by injecting structural errors. It breaks the
//! [`FORMAT`](crate::InvariantSet::FORMAT) invariant.

use arrow::array::*;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use rand::Rng;
use rand::RngCore;
use std::sync::Arc;
use tracing::trace;

use crate::error::NoiseError;
use crate::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

/// Corrupt string formats by removing structural characters.
///
/// Targets strings that look like emails, dates, or URLs and removes or
/// replaces key structural characters (e.g., `@` in emails, `-` in dates).
#[derive(Debug, Clone, Default)]
pub struct FormatCorruptor;

impl FormatCorruptor {
    /// Create a new `FormatCorruptor`.
    pub fn new() -> Self {
        Self
    }
}

/// Corrupt a string value by damaging its structural format.
fn corrupt_format(s: &str, rng: &mut dyn RngCore) -> String {
    // Detect and corrupt common patterns
    if s.contains('@') && s.contains('.') {
        // Looks like an email — remove @ or domain dot
        return match rng.gen_range(0u8..3) {
            0 => s.replacen('@', "", 1),
            1 => s.replacen('.', "", 1),
            _ => format!("{}@", s),
        };
    }

    // Looks like a date (YYYY-MM-DD pattern)
    if s.len() == 10 && s.chars().nth(4) == Some('-') && s.chars().nth(7) == Some('-') {
        return match rng.gen_range(0u8..3) {
            0 => s.replace('-', ""),
            1 => s.replacen('-', "/", 2),
            _ => format!("{}-13-32", &s[..4]),
        };
    }

    // Looks like a URL
    if s.starts_with("http://") || s.starts_with("https://") {
        return match rng.gen_range(0u8..2) {
            0 => s.replacen("://", ":/", 1),
            _ => s.replacen("http", "htp", 1),
        };
    }

    // Generic: scramble a random segment
    let chars: Vec<char> = s.chars().collect();
    if chars.len() > 2 {
        let pos = rng.gen_range(0..chars.len());
        let mut result = chars;
        result[pos] = '#';
        result.into_iter().collect()
    } else {
        format!("#{}", s)
    }
}

impl Perturbator for FormatCorruptor {
    fn name(&self) -> &str {
        "FormatCorruptor"
    }

    fn breaks(&self) -> InvariantSet {
        InvariantSet::FORMAT
    }

    fn perturb(
        &self,
        batch: RecordBatch,
        rng: &mut dyn RngCore,
        config: &PerturbConfig,
    ) -> Result<RecordBatch, NoiseError> {
        let schema = batch.schema();
        let mut columns: Vec<Arc<dyn Array>> = Vec::with_capacity(batch.num_columns());

        for (col_idx, field) in schema.fields().iter().enumerate() {
            let col = batch.column(col_idx);

            if !matches!(field.data_type(), DataType::Utf8)
                || !should_apply(field.name(), &config.columns)
            {
                columns.push(Arc::clone(col));
                continue;
            }

            let a = col.as_any().downcast_ref::<StringArray>().unwrap();
            let vals: Vec<Option<String>> = (0..a.len())
                .map(|i| {
                    if !a.is_valid(i) {
                        return None;
                    }
                    let v = a.value(i);
                    if rng.gen::<f64>() < config.probability {
                        Some(corrupt_format(v, rng))
                    } else {
                        Some(v.to_string())
                    }
                })
                .collect();
            let new_arr: Vec<Option<&str>> = vals.iter().map(|o| o.as_deref()).collect();
            trace!(column = field.name(), "corrupted formats");
            columns.push(Arc::new(StringArray::from(new_arr)));
        }

        Ok(RecordBatch::try_new(schema, columns)?)
    }
}

fn should_apply(name: &str, filter: &ColumnFilter) -> bool {
    match filter {
        ColumnFilter::All => true,
        ColumnFilter::ByName(names) => names.iter().any(|n| n == name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::{Field, Schema};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn email_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("email", DataType::Utf8, true),
        ]));
        RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec![
                "alice@example.com",
                "bob@test.org",
                "2024-01-15",
                "https://example.com",
                "plain-text",
            ]))],
        )
        .unwrap()
    }

    #[test]
    fn format_corruption_modifies_strings() {
        let f = FormatCorruptor::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = f.perturb(email_batch(), &mut rng, &config).unwrap();
        let arr = result.column(0).as_any().downcast_ref::<StringArray>().unwrap();
        let originals = [
            "alice@example.com",
            "bob@test.org",
            "2024-01-15",
            "https://example.com",
            "plain-text",
        ];
        let changed = (0..arr.len())
            .filter(|&i| arr.value(i) != originals[i])
            .count();
        assert!(changed > 0, "expected some formats to be corrupted");
    }

    #[test]
    fn email_corruption_breaks_email_structure() {
        // With prob=1.0, every email gets corrupted via one of 3 strategies:
        // remove @, remove first dot, or append @
        let f = FormatCorruptor::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut seen = std::collections::HashSet::new();
        for seed in 0..20u64 {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let schema = Arc::new(Schema::new(vec![
                Field::new("email", DataType::Utf8, true),
            ]));
            let batch = RecordBatch::try_new(
                schema,
                vec![Arc::new(StringArray::from(vec!["user@example.com"]))],
            )
            .unwrap();
            let result = f.perturb(batch, &mut rng, &config).unwrap();
            let arr = result.column(0).as_any().downcast_ref::<StringArray>().unwrap();
            let val = arr.value(0);
            assert_ne!(val, "user@example.com", "seed {seed}: expected corruption");
            // Must match one of the 3 strategies
            let strategy = if val == "userexample.com" {
                "remove_at"
            } else if val == "user@examplecom" {
                "remove_dot"
            } else if val == "user@example.com@" {
                "append_at"
            } else {
                panic!("seed {seed}: unexpected corruption: {val}");
            };
            seen.insert(strategy);
        }
        assert_eq!(seen.len(), 3, "all 3 email strategies should be exercised: {seen:?}");
    }

    #[test]
    fn date_corruption_breaks_date_format() {
        let f = FormatCorruptor::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut seen = std::collections::HashSet::new();
        for seed in 0..20u64 {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let schema = Arc::new(Schema::new(vec![
                Field::new("d", DataType::Utf8, false),
            ]));
            let batch = RecordBatch::try_new(
                schema,
                vec![Arc::new(StringArray::from(vec!["2024-01-15"]))],
            )
            .unwrap();
            let result = f.perturb(batch, &mut rng, &config).unwrap();
            let arr = result.column(0).as_any().downcast_ref::<StringArray>().unwrap();
            let val = arr.value(0);
            assert_ne!(val, "2024-01-15", "seed {seed}: expected corruption");
            let strategy = if val == "20240115" {
                "remove_dashes"
            } else if val == "2024/01/15" {
                "replace_slash"
            } else if val == "2024-13-32" {
                "invalid_monthday"
            } else {
                panic!("seed {seed}: unexpected date corruption: {val}");
            };
            seen.insert(strategy);
        }
        assert_eq!(seen.len(), 3, "all 3 date strategies should be exercised: {seen:?}");
    }

    #[test]
    fn url_corruption_breaks_url_format() {
        let f = FormatCorruptor::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut seen = std::collections::HashSet::new();
        for seed in 0..20u64 {
            let mut rng = ChaCha8Rng::seed_from_u64(seed);
            let schema = Arc::new(Schema::new(vec![
                Field::new("url", DataType::Utf8, false),
            ]));
            let batch = RecordBatch::try_new(
                schema,
                vec![Arc::new(StringArray::from(vec!["https://example.com"]))],
            )
            .unwrap();
            let result = f.perturb(batch, &mut rng, &config).unwrap();
            let arr = result.column(0).as_any().downcast_ref::<StringArray>().unwrap();
            let val = arr.value(0);
            assert_ne!(val, "https://example.com", "seed {seed}");
            let strategy = if val == "https:/example.com" {
                "remove_slash"
            } else if val == "htps://example.com" {
                "replace_http"
            } else {
                panic!("seed {seed}: unexpected URL corruption: {val}");
            };
            seen.insert(strategy);
        }
        assert_eq!(seen.len(), 2, "both URL strategies should be exercised: {seen:?}");
    }

    #[test]
    fn generic_string_gets_hash_replacement() {
        let f = FormatCorruptor::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![
            Field::new("s", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec!["hello world"]))],
        )
        .unwrap();
        let result = f.perturb(batch, &mut rng, &config).unwrap();
        let arr = result.column(0).as_any().downcast_ref::<StringArray>().unwrap();
        let val = arr.value(0);
        assert!(val.contains('#'), "generic string should get # replacement: {val}");
        assert_eq!(val.len(), "hello world".len(), "length should be preserved");
    }

    #[test]
    fn short_generic_string_gets_hash_prefix() {
        let f = FormatCorruptor::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![
            Field::new("s", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec!["ab"]))],
        )
        .unwrap();
        let result = f.perturb(batch, &mut rng, &config).unwrap();
        let arr = result.column(0).as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(arr.value(0), "#ab");
    }

    #[test]
    fn zero_probability_leaves_strings_unchanged() {
        let f = FormatCorruptor::new();
        let config = PerturbConfig::default().with_probability(0.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = f.perturb(email_batch(), &mut rng, &config).unwrap();
        let arr = result.column(0).as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(arr.value(0), "alice@example.com");
        assert_eq!(arr.value(1), "bob@test.org");
        assert_eq!(arr.value(2), "2024-01-15");
        assert_eq!(arr.value(3), "https://example.com");
        assert_eq!(arr.value(4), "plain-text");
    }

    #[test]
    fn non_utf8_columns_are_skipped() {
        let f = FormatCorruptor::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![
            Field::new("num", DataType::Int32, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(vec![1, 2, 3]))],
        )
        .unwrap();
        let result = f.perturb(batch, &mut rng, &config).unwrap();
        let arr = result.column(0).as_any().downcast_ref::<Int32Array>().unwrap();
        assert_eq!(arr.value(0), 1);
        assert_eq!(arr.value(1), 2);
        assert_eq!(arr.value(2), 3);
    }

    #[test]
    fn column_filter_by_name() {
        let f = FormatCorruptor::new();
        let config = PerturbConfig::default()
            .with_probability(1.0)
            .with_columns(vec!["targeted".to_string()]);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![
            Field::new("targeted", DataType::Utf8, false),
            Field::new("safe", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(StringArray::from(vec!["user@example.com"])),
                Arc::new(StringArray::from(vec!["user@example.com"])),
            ],
        )
        .unwrap();
        let result = f.perturb(batch, &mut rng, &config).unwrap();
        let targeted = result.column(0).as_any().downcast_ref::<StringArray>().unwrap();
        let safe = result.column(1).as_any().downcast_ref::<StringArray>().unwrap();
        assert_ne!(targeted.value(0), "user@example.com", "targeted col should be corrupted");
        assert_eq!(safe.value(0), "user@example.com", "safe col should be untouched");
    }

    #[test]
    fn null_values_preserved() {
        let f = FormatCorruptor::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let schema = Arc::new(Schema::new(vec![
            Field::new("s", DataType::Utf8, true),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec![
                Some("hello"),
                None,
                Some("world"),
            ]))],
        )
        .unwrap();
        let result = f.perturb(batch, &mut rng, &config).unwrap();
        let arr = result.column(0).as_any().downcast_ref::<StringArray>().unwrap();
        assert!(!arr.is_valid(1), "null should remain null");
    }

    #[test]
    fn breaks_format_invariant() {
        let f = FormatCorruptor::new();
        assert_eq!(f.breaks(), InvariantSet::FORMAT);
        assert_eq!(f.name(), "FormatCorruptor");
    }
}
