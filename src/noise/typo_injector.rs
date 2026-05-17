//! Character-level typo injection for string columns.
//!
//! [`TypoInjector`] introduces realistic typos: character swaps, insertions,
//! deletions, and substitutions. It breaks the [`FORMAT`](crate::noise::InvariantSet::FORMAT)
//! invariant.

use arrow::array::*;
use arrow::datatypes::DataType;
use arrow::record_batch::RecordBatch;
use rand::Rng;
use rand::RngCore;
use std::sync::Arc;
use tracing::trace;

use crate::noise::error::NoiseError;
use crate::noise::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

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
    let kind = match rng.random_range(0u8..4) {
        0 => TypoKind::Swap,
        1 => TypoKind::Insert,
        2 => TypoKind::Delete,
        _ => TypoKind::Substitute,
    };

    let mut result = chars.clone();
    match kind {
        TypoKind::Swap => {
            if result.len() >= 2 {
                let pos = rng.random_range(0..result.len() - 1);
                result.swap(pos, pos + 1);
            }
        }
        TypoKind::Insert => {
            let pos = rng.random_range(0..=result.len());
            let c = (b'a' + rng.random_range(0..26u8)) as char;
            result.insert(pos, c);
        }
        TypoKind::Delete => {
            let pos = rng.random_range(0..result.len());
            result.remove(pos);
        }
        TypoKind::Substitute => {
            let pos = rng.random_range(0..result.len());
            result[pos] = (b'a' + rng.random_range(0..26u8)) as char;
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

            let a = col
                .as_any()
                .downcast_ref::<StringArray>()
                .expect("Utf8 column must downcast to StringArray");
            let vals: Vec<Option<String>> = (0..a.len())
                .map(|i| {
                    if !a.is_valid(i) {
                        return None;
                    }
                    let v = a.value(i);
                    if config.in_scope(i) && rng.random::<f64>() < config.probability {
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
        let schema = Arc::new(Schema::new(vec![Field::new("word", DataType::Utf8, true)]));
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
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
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
        // Deterministically exercise the swap path by testing that adjacent chars get swapped
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let mut saw_swap = false;
        for _ in 0..200 {
            let result = apply_typo("abcdef", &mut rng);
            // A swap produces same length but different content with adjacent pair swapped
            if result.len() == 6 && result != "abcdef" {
                let chars: Vec<char> = result.chars().collect();
                let orig: Vec<char> = "abcdef".chars().collect();
                // Check if exactly one adjacent pair is swapped
                let diffs: Vec<usize> = (0..6).filter(|&i| chars[i] != orig[i]).collect();
                if diffs.len() == 2 && diffs[1] == diffs[0] + 1 {
                    // Adjacent positions differ, and they swapped values
                    if chars[diffs[0]] == orig[diffs[1]] && chars[diffs[1]] == orig[diffs[0]] {
                        saw_swap = true;
                        break;
                    }
                }
            }
        }
        assert!(
            saw_swap,
            "expected to see at least one swap in 200 iterations"
        );
    }

    #[test]
    fn typo_all_kinds_exercised() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let original = "hello";
        let orig_chars: Vec<char> = original.chars().collect();
        let mut saw_delete = false;
        let mut saw_insert = false;
        let mut saw_swap = false;
        let mut saw_substitute = false;

        for _ in 0..500 {
            let r = apply_typo(original, &mut rng);
            let r_chars: Vec<char> = r.chars().collect();
            match r.len() {
                4 => saw_delete = true,
                6 => saw_insert = true,
                5 if r != original => {
                    // Distinguish swap vs substitute: swap has exactly 2 adjacent diffs
                    let diffs: Vec<usize> =
                        (0..5).filter(|&i| r_chars[i] != orig_chars[i]).collect();
                    if diffs.len() == 2
                        && diffs[1] == diffs[0] + 1
                        && r_chars[diffs[0]] == orig_chars[diffs[1]]
                        && r_chars[diffs[1]] == orig_chars[diffs[0]]
                    {
                        saw_swap = true;
                    } else {
                        saw_substitute = true;
                    }
                }
                _ => {}
            }
            if saw_delete && saw_insert && saw_swap && saw_substitute {
                break;
            }
        }
        assert!(saw_delete, "delete kind not observed");
        assert!(saw_insert, "insert kind not observed");
        assert!(saw_swap, "swap kind not observed");
        assert!(saw_substitute, "substitute kind not observed");
    }

    #[test]
    fn typo_empty_string_unchanged() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = apply_typo("", &mut rng);
        assert_eq!(result, "");
    }

    #[test]
    fn typo_single_char() {
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        // Single char: swap is impossible (len<2), so only insert/delete/substitute
        let mut results = std::collections::HashSet::new();
        for _ in 0..100 {
            results.insert(apply_typo("x", &mut rng).len());
        }
        // delete→0, substitute→1, insert→2 (swap would stay 1)
        assert!(
            results.contains(&0) || results.contains(&2),
            "single char should produce varied lengths: {:?}",
            results
        );
    }

    #[test]
    fn typo_zero_probability_unchanged() {
        let t = TypoInjector::new();
        let config = PerturbConfig::default().with_probability(0.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = t.perturb(string_batch(), &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        let orig = ["hello", "world", "testing", "typos", "here"];
        for (i, original) in orig.iter().enumerate() {
            assert_eq!(arr.value(i), *original);
        }
    }

    #[test]
    fn typo_skips_non_utf8_columns() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("num", DataType::Int32, false),
            Field::new("text", DataType::Utf8, true),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec!["abc", "def", "ghi"])),
            ],
        )
        .unwrap();
        let t = TypoInjector::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = t.perturb(batch, &mut rng, &config).unwrap();
        // Int column unchanged
        let nums = result
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();
        assert_eq!(nums.value(0), 1);
        assert_eq!(nums.value(1), 2);
        assert_eq!(nums.value(2), 3);
    }

    #[test]
    fn typo_null_values_preserved() {
        let schema = Arc::new(Schema::new(vec![Field::new("word", DataType::Utf8, true)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec![
                Some("hello"),
                None,
                Some("world"),
            ]))],
        )
        .unwrap();
        let t = TypoInjector::new();
        let config = PerturbConfig::default().with_probability(1.0);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let result = t.perturb(batch, &mut rng, &config).unwrap();
        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert!(arr.is_valid(0));
        assert!(!arr.is_valid(1), "null should remain null");
        assert!(arr.is_valid(2));
    }
}
