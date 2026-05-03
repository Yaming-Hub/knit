//! Uniqueness-enforcing wrapper generator.
//!
//! Wraps any inner [`FieldGenerator`] and deduplicates output values via retry.
//! Uses a [`Mutex`]-protected set to track seen values across calls.
//!
//! **Partition scope:** uniqueness is enforced within a single partition. For
//! multi-partition entities (>1M rows), duplicates may still occur across
//! partitions. This is a known limitation.

use std::collections::HashSet;
use std::sync::Mutex;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int64Array, StringArray,
    TimestampMillisecondArray, TimestampMicrosecondArray,
    TimestampNanosecondArray, TimestampSecondArray, UInt64Array,
};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Wrapper generator that enforces uniqueness on an inner generator's output.
///
/// Each call to [`generate`](FieldGenerator::generate) produces a batch from the
/// inner generator, filters out duplicates, and tops up with retries until the
/// requested count is met or `max_retries` per-row attempts are exhausted.
///
/// # Thread safety
///
/// The seen-value set is protected by a [`Mutex`], satisfying the
/// `Send + Sync` requirement of [`FieldGenerator`].
///
/// # Limitations
///
/// Uniqueness is tracked per generator instance (i.e. per partition). For
/// multi-partition entities, duplicates can appear across partitions.
pub struct UniqueGenerator {
    /// The wrapped inner generator.
    inner: Box<dyn FieldGenerator>,
    /// Maximum retry rounds before accepting duplicates for remaining rows.
    max_retries: u32,
    /// Set of previously emitted values (string representation).
    seen: Mutex<HashSet<String>>,
}

impl UniqueGenerator {
    /// Create a new uniqueness-enforcing wrapper.
    ///
    /// * `inner` – the generator to wrap.
    /// * `max_retries` – how many retry rounds when duplicates are produced.
    pub fn new(inner: Box<dyn FieldGenerator>, max_retries: u32) -> Self {
        Self {
            inner,
            max_retries,
            seen: Mutex::new(HashSet::new()),
        }
    }
}

/// Extract a string key from an Arrow array element for deduplication.
fn array_value_to_string(array: &dyn Array, index: usize) -> String {
    if array.is_null(index) {
        return "__null__".to_string();
    }
    if let Some(a) = array.as_any().downcast_ref::<StringArray>() {
        return a.value(index).to_string();
    }
    if let Some(a) = array.as_any().downcast_ref::<Int64Array>() {
        return a.value(index).to_string();
    }
    if let Some(a) = array.as_any().downcast_ref::<UInt64Array>() {
        return a.value(index).to_string();
    }
    if let Some(a) = array.as_any().downcast_ref::<Float64Array>() {
        return format!("{:.17e}", a.value(index));
    }
    if let Some(a) = array.as_any().downcast_ref::<BooleanArray>() {
        return a.value(index).to_string();
    }
    // Timestamp types
    if let Some(a) = array.as_any().downcast_ref::<TimestampSecondArray>() {
        return a.value(index).to_string();
    }
    if let Some(a) = array.as_any().downcast_ref::<TimestampMillisecondArray>() {
        return a.value(index).to_string();
    }
    if let Some(a) = array.as_any().downcast_ref::<TimestampMicrosecondArray>() {
        return a.value(index).to_string();
    }
    if let Some(a) = array.as_any().downcast_ref::<TimestampNanosecondArray>() {
        return a.value(index).to_string();
    }
    // Fallback: use debug format of the scalar
    format!("{:?}@{index}", array.data_type())
}

impl FieldGenerator for UniqueGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        if count == 0 {
            return self.inner.generate(rng, 0, ctx);
        }

        let mut seen = self.seen.lock().unwrap();
        let mut unique_indices: Vec<usize> = Vec::with_capacity(count);
        let mut collected_arrays: Vec<(ArrayRef, Vec<usize>)> = Vec::new();
        let mut remaining = count;
        let mut retry_round = 0u32;

        while remaining > 0 && retry_round <= self.max_retries {
            // Generate a batch from the inner generator
            let batch = self.inner.generate(rng, remaining, ctx);
            let mut batch_unique_indices = Vec::new();

            for i in 0..batch.len() {
                if remaining == 0 {
                    break;
                }
                let key = array_value_to_string(batch.as_ref(), i);
                if seen.insert(key) {
                    batch_unique_indices.push(i);
                    remaining -= 1;
                }
            }

            if batch_unique_indices.is_empty() {
                retry_round += 1;
            } else {
                retry_round = 0; // reset on progress
            }

            if !batch_unique_indices.is_empty() {
                collected_arrays.push((batch, batch_unique_indices));
            }
        }

        if remaining > 0 {
            tracing::warn!(
                remaining,
                max_retries = self.max_retries,
                "unique generator exhausted retries, filling remaining with duplicates"
            );
            // Fill remaining with whatever the inner generator produces
            let fill = self.inner.generate(rng, remaining, ctx);
            let all: Vec<usize> = (0..fill.len()).collect();
            collected_arrays.push((fill, all));
        }

        // Build the final array by taking selected indices from each batch
        build_result_array(&collected_arrays)
    }

    fn output_type(&self) -> DataType {
        self.inner.output_type()
    }
}

/// Construct the final `ArrayRef` by taking selected indices from collected batches.
fn build_result_array(collected: &[(ArrayRef, Vec<usize>)]) -> ArrayRef {
    use arrow::compute::concat;

    // Fast path: single batch with all indices sequential
    if collected.len() == 1 {
        let (arr, indices) = &collected[0];
        if indices.len() == arr.len()
            && indices.iter().enumerate().all(|(i, &idx)| i == idx)
        {
            return arr.clone();
        }
    }

    // Take selected rows from each batch
    let mut slices: Vec<ArrayRef> = Vec::new();
    for (arr, indices) in collected {
        for &idx in indices {
            slices.push(arr.slice(idx, 1));
        }
    }

    if slices.is_empty() {
        // Shouldn't happen, but handle gracefully
        return collected
            .first()
            .map(|(a, _)| a.slice(0, 0))
            .unwrap_or_else(|| {
                std::sync::Arc::new(arrow::array::NullArray::new(0)) as ArrayRef
            });
    }

    let refs: Vec<&dyn Array> = slices.iter().map(|a| a.as_ref()).collect();
    concat(&refs).expect("concat of same-type arrays should not fail")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generators::constant::ConstantGenerator;
    use crate::generators::one_of::OneOfGenerator;
    use knit_core::{Value, WeightedChoice};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    /// Build a minimal [`GenContext`] for tests.
    fn test_ctx() -> GenContext<'static> {
        // Leak a HashMap so we can return a 'static reference.
        let map: &'static HashMap<String, ArrayRef> =
            Box::leak(Box::new(HashMap::new()));
        GenContext {
            batch_columns: map,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test",
        }
    }

    #[test]
    fn unique_output_has_no_duplicates() {
        // Use UUID generator which produces rng-based unique-ish values.
        use crate::generators::uuid_gen::UuidGenerator;
        let inner = Box::new(UuidGenerator);
        let gen = UniqueGenerator::new(inner, 100);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();

        let arr = gen.generate(&mut rng, 50, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let values: HashSet<&str> = (0..str_arr.len()).map(|i| str_arr.value(i)).collect();
        assert_eq!(values.len(), 50, "all values must be unique");
    }

    #[test]
    fn unique_with_small_one_of_set_collisions() {
        // OneOf with only 3 choices — generating 3 values should still work.
        let choices = vec![
            WeightedChoice { value: Value::String("a".into()), weight: 1.0 },
            WeightedChoice { value: Value::String("b".into()), weight: 1.0 },
            WeightedChoice { value: Value::String("c".into()), weight: 1.0 },
        ];
        let inner = Box::new(OneOfGenerator::new(choices));
        let gen = UniqueGenerator::new(inner, 1000);
        let mut rng = ChaCha8Rng::seed_from_u64(99);
        let ctx = test_ctx();

        let arr = gen.generate(&mut rng, 3, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let values: HashSet<&str> = (0..str_arr.len()).map(|i| str_arr.value(i)).collect();
        assert_eq!(values.len(), 3, "should produce 3 unique values from 3 choices");
    }

    #[test]
    fn unique_max_retries_graceful_degradation() {
        // Constant generator always produces the same value; after first row
        // every subsequent row will exhaust retries.
        let inner = Box::new(ConstantGenerator::new(Value::String("same".into())));
        let gen = UniqueGenerator::new(inner, 5);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let ctx = test_ctx();

        // Should not panic — gracefully degrades.
        let arr = gen.generate(&mut rng, 3, &ctx);
        assert_eq!(arr.len(), 3);
    }

    #[test]
    fn unique_count_zero() {
        use crate::generators::uuid_gen::UuidGenerator;
        let inner = Box::new(UuidGenerator);
        let gen = UniqueGenerator::new(inner, 100);
        let mut rng = ChaCha8Rng::seed_from_u64(0);
        let ctx = test_ctx();

        let arr = gen.generate(&mut rng, 0, &ctx);
        assert_eq!(arr.len(), 0);
    }

    #[test]
    fn unique_output_type_matches_inner() {
        use crate::generators::uuid_gen::UuidGenerator;
        let inner = Box::new(UuidGenerator);
        let expected = inner.output_type();
        let gen = UniqueGenerator::new(inner, 10);
        assert_eq!(gen.output_type(), expected);
    }

    #[test]
    fn unique_wrapping_faker() {
        // Faker generators produce strings; just make sure it doesn't panic.
        use crate::generators::faker::FakerGenerator;
        let inner = Box::new(FakerGenerator::new("first_name".into(), "en_US".into()));
        let gen = UniqueGenerator::new(inner, 100);
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let ctx = test_ctx();

        let arr = gen.generate(&mut rng, 10, &ctx);
        assert_eq!(arr.len(), 10);
        assert_eq!(gen.output_type(), DataType::Utf8);
    }
}
