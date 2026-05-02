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
}
