//! Uniqueness-enforcing wrapper generator.
//!
//! Wraps any inner [`FieldGenerator`] and deduplicates output values via retry.
//! Uses interior mutability (`RefCell`) to track seen values across calls,
//! which is safe because generators are single-threaded per partition.

use std::cell::RefCell;
use std::collections::HashSet;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int64Array, StringArray,
};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Wrapper generator that enforces uniqueness on an inner generator's output.
///
/// Each call to [`generate`](FieldGenerator::generate) produces values using the
/// inner generator and retries duplicates up to `max_retries` times per row.
/// If retries are exhausted the duplicate is included and a warning is logged.
///
/// # Interior mutability
///
/// The seen-value set is stored in a [`RefCell`] because the
/// [`FieldGenerator`] trait requires `&self`. This is safe because generators
/// run on a single thread per partition.
pub struct UniqueGenerator {
    /// The wrapped inner generator.
    inner: Box<dyn FieldGenerator>,
    /// Maximum retry attempts per row before accepting a duplicate.
    max_retries: u32,
    /// Set of previously emitted values (string representation), using
    /// interior mutability for `&self` compatibility.
    seen: RefCell<HashSet<String>>,
}

// SAFETY: `RefCell` is not `Sync`, but generators are used single-threaded
// per partition. We implement `Sync` to satisfy the trait bound.
unsafe impl Sync for UniqueGenerator {}

impl UniqueGenerator {
    /// Create a new uniqueness-enforcing wrapper.
    ///
    /// * `inner` – the generator to wrap.
    /// * `max_retries` – how many times to retry when a duplicate is produced.
    pub fn new(inner: Box<dyn FieldGenerator>, max_retries: u32) -> Self {
        Self {
            inner,
            max_retries,
            seen: RefCell::new(HashSet::new()),
        }
    }
}

/// Extract a string key from an Arrow array element for deduplication.
fn array_value_to_string(array: &dyn Array, index: usize) -> String {
    if array.is_null(index) {
        return "__null__".to_string();
    }
    if let Some(arr) = array.as_any().downcast_ref::<StringArray>() {
        return arr.value(index).to_string();
    }
    if let Some(arr) = array.as_any().downcast_ref::<Int64Array>() {
        return arr.value(index).to_string();
    }
    if let Some(arr) = array.as_any().downcast_ref::<Float64Array>() {
        return arr.value(index).to_string();
    }
    if let Some(arr) = array.as_any().downcast_ref::<BooleanArray>() {
        return arr.value(index).to_string();
    }
    format!("__unknown_{index}__")
}

impl FieldGenerator for UniqueGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        if count == 0 {
            return self.inner.generate(rng, 0, ctx);
        }

        let mut result_arrays: Vec<ArrayRef> = Vec::with_capacity(count);
        let mut seen = self.seen.borrow_mut();

        for _ in 0..count {
            let mut attempts = 0u32;
            loop {
                let single = self.inner.generate(rng, 1, ctx);
                let key = array_value_to_string(single.as_ref(), 0);

                if !seen.contains(&key) {
                    seen.insert(key);
                    result_arrays.push(single);
                    break;
                }

                attempts += 1;
                if attempts >= self.max_retries {
                    tracing::warn!(
                        attempts = self.max_retries,
                        "unique generator exceeded max retries, accepting duplicate"
                    );
                    seen.insert(key);
                    result_arrays.push(single);
                    break;
                }
            }
        }

        // Concatenate single-element arrays into one array of length `count`.
        let refs: Vec<&dyn Array> = result_arrays.iter().map(|a| a.as_ref()).collect();
        arrow::compute::concat(&refs).expect("concat of same-type arrays should not fail")
    }

    fn output_type(&self) -> DataType {
        self.inner.output_type()
    }
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
