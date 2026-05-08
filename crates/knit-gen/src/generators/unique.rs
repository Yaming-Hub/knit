//! Uniqueness-enforcing wrapper generator.
//!
//! Wraps any inner [`FieldGenerator`] and deduplicates output values via retry.
//! Uses a [`Mutex`]-protected set to track seen values across calls.
//!
//! **Cross-partition scope:** when the engine detects `Unique` fields in a
//! multi-partition entity, it switches to sequential partition generation and
//! shares the seen-set across partitions via [`UniqueGenerator::with_shared_seen`].
//! This ensures global uniqueness while preserving deterministic output.

use std::collections::HashSet;
use std::sync::Arc;

use parking_lot::Mutex;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, StringArray,
    TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray, UInt64Array,
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
/// # Cross-partition uniqueness
///
/// When constructed with [`with_shared_seen`](Self::with_shared_seen), the
/// seen-set is shared across multiple generator instances (one per partition).
/// The engine uses sequential partition generation in this case to preserve
/// deterministic output.
pub struct UniqueGenerator {
    /// The wrapped inner generator.
    inner: Box<dyn FieldGenerator>,
    /// Maximum retry rounds before accepting duplicates for remaining rows.
    max_retries: u32,
    /// Set of previously emitted values (string representation).
    seen: Arc<Mutex<HashSet<String>>>,
}

impl UniqueGenerator {
    /// Create a new uniqueness-enforcing wrapper with a fresh (empty) seen-set.
    ///
    /// * `inner` – the generator to wrap.
    /// * `max_retries` – how many retry rounds when duplicates are produced.
    pub fn new(inner: Box<dyn FieldGenerator>, max_retries: u32) -> Self {
        Self {
            inner,
            max_retries,
            seen: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Create a uniqueness-enforcing wrapper that shares a seen-set with other
    /// instances (e.g. across partitions).
    ///
    /// The caller is responsible for ensuring deterministic access order
    /// (sequential partition generation) when sharing.
    pub fn with_shared_seen(
        inner: Box<dyn FieldGenerator>,
        max_retries: u32,
        seen: Arc<Mutex<HashSet<String>>>,
    ) -> Self {
        Self {
            inner,
            max_retries,
            seen,
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
    // Date types
    if let Some(a) = array.as_any().downcast_ref::<Date32Array>() {
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

        let mut seen = self.seen.lock();
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
        if indices.len() == arr.len() && indices.iter().enumerate().all(|(i, &idx)| i == idx) {
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
            .unwrap_or_else(|| std::sync::Arc::new(arrow::array::NullArray::new(0)) as ArrayRef);
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
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, 0, 0, 1, "test")
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
            WeightedChoice {
                value: Value::String("a".into()),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::String("b".into()),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::String("c".into()),
                weight: 1.0,
            },
        ];
        let inner = Box::new(OneOfGenerator::new(choices));
        let gen = UniqueGenerator::new(inner, 1000);
        let mut rng = ChaCha8Rng::seed_from_u64(99);
        let ctx = test_ctx();

        let arr = gen.generate(&mut rng, 3, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let values: HashSet<&str> = (0..str_arr.len()).map(|i| str_arr.value(i)).collect();
        assert_eq!(
            values.len(),
            3,
            "should produce 3 unique values from 3 choices"
        );
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
        let inner = Box::new(FakerGenerator::new(
            "first_name".into(),
            "en_US".into(),
            vec![],
        ));
        let gen = UniqueGenerator::new(inner, 100);
        let mut rng = ChaCha8Rng::seed_from_u64(7);
        let ctx = test_ctx();

        let arr = gen.generate(&mut rng, 10, &ctx);
        assert_eq!(arr.len(), 10);
        assert_eq!(gen.output_type(), DataType::Utf8);
    }

    #[test]
    fn unique_cross_batch_dedup() {
        // Use low-cardinality inner so second call MUST rely on persisted seen set
        let choices = vec![
            WeightedChoice {
                value: Value::String("a".into()),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::String("b".into()),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::String("c".into()),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::String("d".into()),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::String("e".into()),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::String("f".into()),
                weight: 1.0,
            },
        ];
        let inner = Box::new(OneOfGenerator::new(choices));
        let gen = UniqueGenerator::new(inner, 1000);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();

        let arr1 = gen.generate(&mut rng, 3, &ctx);
        let arr2 = gen.generate(&mut rng, 3, &ctx);
        let s1 = arr1.as_any().downcast_ref::<StringArray>().unwrap();
        let s2 = arr2.as_any().downcast_ref::<StringArray>().unwrap();

        let set1: HashSet<&str> = (0..s1.len()).map(|i| s1.value(i)).collect();
        let set2: HashSet<&str> = (0..s2.len()).map(|i| s2.value(i)).collect();
        assert_eq!(set1.len(), 3);
        assert_eq!(set2.len(), 3);
        assert!(
            set1.is_disjoint(&set2),
            "cross-batch values must not overlap: batch1={set1:?}, batch2={set2:?}"
        );
    }

    #[test]
    fn unique_with_int_inner() {
        // Use OneOf with integer choices to test Int64Array dedup path
        let choices = vec![
            WeightedChoice {
                value: Value::Int(10),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::Int(20),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::Int(30),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::Int(40),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::Int(50),
                weight: 1.0,
            },
        ];
        let inner = Box::new(OneOfGenerator::new(choices));
        let gen = UniqueGenerator::new(inner, 1000);
        let mut rng = ChaCha8Rng::seed_from_u64(99);
        let ctx = test_ctx();

        let arr = gen.generate(&mut rng, 5, &ctx);
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        let values: HashSet<i64> = (0..int_arr.len()).map(|i| int_arr.value(i)).collect();
        assert_eq!(values.len(), 5, "all 5 unique ints should be produced");
    }

    #[test]
    fn unique_max_retries_returns_correct_length() {
        // Constant inner with max_retries=2: should still produce requested count
        let inner = Box::new(ConstantGenerator::new(Value::Int(7)));
        let gen = UniqueGenerator::new(inner, 2);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let ctx = test_ctx();

        let arr = gen.generate(&mut rng, 5, &ctx);
        assert_eq!(arr.len(), 5, "should always return requested count");
        // First value is unique, rest are duplicates filled in
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..5 {
            assert_eq!(int_arr.value(i), 7, "constant fills all slots");
        }
    }

    #[test]
    fn unique_deterministic_with_same_seed() {
        use crate::generators::uuid_gen::UuidGenerator;

        let make_gen = || UniqueGenerator::new(Box::new(UuidGenerator), 100);
        let gen1 = make_gen();
        let gen2 = make_gen();
        let ctx = test_ctx();

        let mut rng1 = ChaCha8Rng::seed_from_u64(42);
        let arr1 = gen1.generate(&mut rng1, 20, &ctx);
        let mut rng2 = ChaCha8Rng::seed_from_u64(42);
        let arr2 = gen2.generate(&mut rng2, 20, &ctx);

        let s1 = arr1.as_any().downcast_ref::<StringArray>().unwrap();
        let s2 = arr2.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..20 {
            assert_eq!(s1.value(i), s2.value(i), "row {i} must match");
        }
    }

    #[test]
    fn unique_with_float_inner() {
        // Float dedup uses scientific notation key
        let choices = vec![
            WeightedChoice {
                value: Value::Float(1.1),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::Float(2.2),
                weight: 1.0,
            },
            WeightedChoice {
                value: Value::Float(3.3),
                weight: 1.0,
            },
        ];
        let inner = Box::new(OneOfGenerator::new(choices));
        let gen = UniqueGenerator::new(inner, 1000);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();

        let arr = gen.generate(&mut rng, 3, &ctx);
        let float_arr = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        let values: HashSet<u64> = (0..float_arr.len())
            .map(|i| float_arr.value(i).to_bits())
            .collect();
        assert_eq!(values.len(), 3, "all 3 unique floats should be produced");
    }

    #[test]
    fn shared_seen_set_cross_partition_uniqueness() {
        // Two generators sharing the same seen-set should not produce overlapping values.
        use crate::generators::uuid_gen::UuidGenerator;

        let shared = Arc::new(Mutex::new(HashSet::new()));
        let gen1 =
            UniqueGenerator::with_shared_seen(Box::new(UuidGenerator), 100, Arc::clone(&shared));
        let gen2 =
            UniqueGenerator::with_shared_seen(Box::new(UuidGenerator), 100, Arc::clone(&shared));
        let ctx = test_ctx();

        let mut rng1 = ChaCha8Rng::seed_from_u64(1);
        let arr1 = gen1.generate(&mut rng1, 50, &ctx);
        let mut rng2 = ChaCha8Rng::seed_from_u64(2);
        let arr2 = gen2.generate(&mut rng2, 50, &ctx);

        let s1 = arr1.as_any().downcast_ref::<StringArray>().unwrap();
        let s2 = arr2.as_any().downcast_ref::<StringArray>().unwrap();
        let set1: HashSet<&str> = (0..s1.len()).map(|i| s1.value(i)).collect();
        let set2: HashSet<&str> = (0..s2.len()).map(|i| s2.value(i)).collect();
        assert_eq!(set1.len(), 50);
        assert_eq!(set2.len(), 50);
        assert!(
            set1.is_disjoint(&set2),
            "shared seen-set should prevent cross-partition duplicates"
        );
    }

    #[test]
    fn shared_seen_deterministic() {
        // Shared-seen generators must produce identical output across runs.
        use crate::generators::uuid_gen::UuidGenerator;

        let run = || {
            let shared = Arc::new(Mutex::new(HashSet::new()));
            let gen1 = UniqueGenerator::with_shared_seen(
                Box::new(UuidGenerator),
                100,
                Arc::clone(&shared),
            );
            let gen2 = UniqueGenerator::with_shared_seen(
                Box::new(UuidGenerator),
                100,
                Arc::clone(&shared),
            );
            let ctx = test_ctx();
            let mut rng1 = ChaCha8Rng::seed_from_u64(10);
            let a1 = gen1.generate(&mut rng1, 20, &ctx);
            let mut rng2 = ChaCha8Rng::seed_from_u64(20);
            let a2 = gen2.generate(&mut rng2, 20, &ctx);
            (a1, a2)
        };

        let (a1_r1, a2_r1) = run();
        let (a1_r2, a2_r2) = run();
        let s = |a: &ArrayRef| {
            let s = a.as_any().downcast_ref::<StringArray>().unwrap();
            (0..s.len()).map(|i| s.value(i).to_string()).collect::<Vec<_>>()
        };
        assert_eq!(s(&a1_r1), s(&a1_r2), "partition 1 must be deterministic");
        assert_eq!(s(&a2_r1), s(&a2_r2), "partition 2 must be deterministic");
    }
}
