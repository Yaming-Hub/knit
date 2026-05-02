//! Character-level typo injection for string columns.
//!
//! [`TypoInjector`] introduces realistic typos: character swaps, insertions,
//! deletions, and substitutions. It breaks the [`FORMAT`](crate::InvariantSet::FORMAT)
//! invariant.

use arrow::array::*;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use rand::Rng;
use rand::RngCore;
use std::sync::Arc;
use tracing::trace;

use crate::error::NoiseError;
use crate::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

/// Inject character-level typos into string columns.
///
/// Supports four typo kinds: swap adjacent characters, insert a random char,
/// delete a random char, and substitute a random char.
#[derive(Debug, Clone, Default)]
pub struct TypoInjector;

impl TypoInjector {
    /// Create a new `TypoInjector`.
    pub fn new() -> Self {
        Self
    }
}

/// The kind of typo to inject.
#[derive(Debug, Clone, Copy)]
enum TypoKind {
    Swap,
    Insert,
    Delete,
    Substitute,
}

fn apply_typo(s: &str, rng: &mut dyn RngCore) -> String {
    if s.is_empty() {
        return s.to_string();
    }
    let chars: Vec<char> = s.chars().collect();
    let kind = match rng.gen_range(0u8..4) {
        0 => TypoKind::Swap,
        1 => TypoKind::Insert,
        2 => TypoKind::Delete,
        _ => TypoKind::Substitute,
    };

    let mut result = chars.clone();
    match kind {
        TypoKind::Swap => {
            if result.len() >= 2 {
                let pos = rng.gen_range(0..result.len() - 1);
                result.swap(pos, pos + 1);
            }
        }
        TypoKind::Insert => {
            let pos = rng.gen_range(0..=result.len());
            let c = (b'a' + rng.gen_range(0..26u8)) as char;
            result.insert(pos, c);
        }
        TypoKind::Delete => {
            let pos = rng.gen_range(0..result.len());
            result.remove(pos);
        }
        TypoKind::Substitute => {
            let pos = rng.gen_range(0..result.len());
            result[pos] = (b'a' + rng.gen_range(0..26u8)) as char;
        }
    }
    result.into_iter().collect()
}

impl Perturbator for TypoInjector {
    fn name(&self) -> &str {
        "TypoInjector"
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
                        Some(apply_typo(v, rng))
                    } else {
                        Some(v.to_string())
                    }
                })
                .collect();
            let new_arr: Vec<Option<&str>> = vals.iter().map(|o| o.as_deref()).collect();
            trace!(column = field.name(), "injected typos");
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

    fn string_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("word", DataType::Utf8, true),
        ]));
        RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec![
                "hello", "world", "testing", "typos", "here",
            ]))],
        )
        .unwrap()
    }

    #[test]
    fn typo_injection_modifies_strings() {
        let t = TypoInjector::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = t.perturb(string_batch(), &mut rng, &config).unwrap();
        let arr = result.column(0).as_any().downcast_ref::<StringArray>().unwrap();
        let changed = (0..arr.len())
            .filter(|&i| {
                let orig = ["hello", "world", "testing", "typos", "here"];
                arr.value(i) != orig[i]
            })
            .count();
        assert!(changed > 0, "expected some strings to change");
    }

    #[test]
    fn typo_apply_swap() {
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        // Just test that apply_typo doesn't panic
        for _ in 0..100 {
            let _ = apply_typo("abcdef", &mut rng);
        }
    }
}
