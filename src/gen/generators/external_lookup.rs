//! External lookup generator — samples from a column loaded from CSV/JSON/Parquet.
//!
//! Supports three sampling modes:
//! - **Uniform**: random selection with replacement
//! - **Weighted**: weighted random selection using a weight column
//! - **Sequential**: deterministic round-robin based on row offset

use std::sync::Arc;

use arrow::array::{ArrayRef, StringArray};
use rand::RngCore;
use rand::distr::Distribution;
use rand_distr::weighted::WeightedAliasIndex;

use crate::core::SamplingMode;
use crate::r#gen::context::GenContext;
use crate::r#gen::traits::FieldGenerator;

/// Generate string values by sampling from an externally loaded data column.
///
/// The entry list is populated by the CLI layer after plan compilation,
/// which reads the source file, extracts the named column, and stores
/// string representations of each value.
pub struct ExternalLookupGenerator {
    /// Loaded values from the source column.
    entries: Vec<String>,
    /// Sampling strategy.
    sampling: SamplingMode,
    /// Pre-built weighted index (only for `Weighted` mode).
    weighted_index: Option<WeightedAliasIndex<f64>>,
}

impl ExternalLookupGenerator {
    /// Create a new external lookup generator from loaded entries.
    pub fn new(entries: Vec<String>, weights: Option<Vec<f64>>, sampling: SamplingMode) -> Self {
        let weighted_index = if sampling == SamplingMode::Weighted {
            weights
                .as_ref()
                .and_then(|w| WeightedAliasIndex::new(w.clone()).ok())
        } else {
            None
        };

        Self {
            entries,
            sampling,
            weighted_index,
        }
    }
}

impl FieldGenerator for ExternalLookupGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        if self.entries.is_empty() {
            let empty: Vec<&str> = vec![""; count];
            return Arc::new(StringArray::from(empty));
        }

        let values: Vec<&str> = match self.sampling {
            SamplingMode::Uniform => (0..count)
                .map(|_| {
                    let idx = rng.next_u32() as usize % self.entries.len();
                    self.entries[idx].as_str()
                })
                .collect(),

            SamplingMode::Weighted => {
                if let Some(ref wi) = self.weighted_index {
                    (0..count)
                        .map(|_| {
                            let idx = wi.sample(rng);
                            self.entries[idx].as_str()
                        })
                        .collect()
                } else {
                    // Fallback to uniform if weights weren't valid
                    (0..count)
                        .map(|_| {
                            let idx = rng.next_u32() as usize % self.entries.len();
                            self.entries[idx].as_str()
                        })
                        .collect()
                }
            }

            SamplingMode::Sequential => (0..count)
                .map(|i| {
                    let idx = (ctx.row_offset as usize + i) % self.entries.len();
                    self.entries[idx].as_str()
                })
                .collect(),
        };

        Arc::new(StringArray::from(values))
    }

    fn output_type(&self) -> arrow::datatypes::DataType {
        arrow::datatypes::DataType::Utf8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn test_ctx_with_offset(offset: u64) -> GenContext<'static> {
        static COLS: std::sync::LazyLock<HashMap<String, ArrayRef>> =
            std::sync::LazyLock::new(HashMap::new);
        GenContext::new(&COLS, offset, 0, 1, "test")
    }

    fn test_ctx() -> GenContext<'static> {
        test_ctx_with_offset(0)
    }

    fn sample_entries() -> Vec<String> {
        vec![
            "Tokyo".into(),
            "Paris".into(),
            "London".into(),
            "NYC".into(),
            "Berlin".into(),
        ]
    }

    #[test]
    fn uniform_sampling_selects_from_entries() {
        let r#gen = ExternalLookupGenerator::new(sample_entries(), None, SamplingMode::Uniform);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 20, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(str_arr.len(), 20);
        let entries = sample_entries();
        for i in 0..20 {
            assert!(
                entries.contains(&str_arr.value(i).to_string()),
                "unexpected value: {}",
                str_arr.value(i)
            );
        }
    }

    #[test]
    fn weighted_sampling_respects_weights() {
        // Give 99% weight to "Tokyo"
        let weights = vec![99.0, 0.25, 0.25, 0.25, 0.25];
        let r#gen =
            ExternalLookupGenerator::new(sample_entries(), Some(weights), SamplingMode::Weighted);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 100, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();

        let tokyo_count = (0..100).filter(|&i| str_arr.value(i) == "Tokyo").count();
        // With 99% weight, Tokyo should appear in the vast majority
        assert!(tokyo_count > 80, "expected >80 Tokyo, got {}", tokyo_count);
    }

    #[test]
    fn sequential_sampling_is_deterministic() {
        let r#gen = ExternalLookupGenerator::new(sample_entries(), None, SamplingMode::Sequential);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let entries = sample_entries();

        // First batch: offset=0
        let ctx = test_ctx_with_offset(0);
        let arr = r#gen.generate(&mut rng, 5, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for (i, entry) in entries.iter().take(5).enumerate() {
            assert_eq!(str_arr.value(i), *entry);
        }

        // Second batch: offset=5 wraps around
        let ctx = test_ctx_with_offset(5);
        let arr = r#gen.generate(&mut rng, 3, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(str_arr.value(0), entries[0]); // 5 % 5 = 0
        assert_eq!(str_arr.value(1), entries[1]); // 6 % 5 = 1
        assert_eq!(str_arr.value(2), entries[2]); // 7 % 5 = 2
    }

    #[test]
    fn empty_entries_produce_empty_strings() {
        let r#gen = ExternalLookupGenerator::new(vec![], None, SamplingMode::Uniform);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 3, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..3 {
            assert_eq!(str_arr.value(i), "");
        }
    }

    #[test]
    fn weighted_fallback_on_no_weights() {
        // Weighted mode but no weights provided — should fallback to uniform
        let r#gen = ExternalLookupGenerator::new(sample_entries(), None, SamplingMode::Weighted);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 10, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(str_arr.len(), 10);
    }

    #[test]
    fn output_type_is_utf8() {
        let r#gen = ExternalLookupGenerator::new(sample_entries(), None, SamplingMode::Uniform);
        assert_eq!(r#gen.output_type(), arrow::datatypes::DataType::Utf8);
    }
}
