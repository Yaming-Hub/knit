//! Null-mask application based on [`NullPlan`].
//!
//! After a field generator produces an array of non-null values,
//! [`apply_null_mask`] introduces nulls according to the field's [`NullPlan`].
//! This separation keeps generators simple (they never worry about nulls) while
//! giving users fine-grained control over null distribution.

use arrow::array::{ArrayRef, BooleanArray, NullArray};
use arrow::compute::kernels::zip::zip;
use rand::RngCore;
use std::sync::Arc;

use crate::gen::error::GenError;
use crate::plan::NullPlan;

/// Apply a null mask to a generated array according to the given [`NullPlan`].
///
/// Called by the batch-generation loop *after* each field generator runs.
/// The original array is combined with an all-null array using Arrow's `zip`
/// kernel, guided by a boolean keep-mask.
///
/// # Variants
///
/// - `Never`  — returns the array unchanged (no nulls introduced).
/// - `Always` — returns an all-null [`NullArray`] of the same length.
/// - `Probability(p)` — each element is independently null with probability *p*.
/// - `Pattern { every_n }` — every *n*-th element (0-indexed) is null.
///   If `every_n` is 0, behaves as `Never` (no division by zero).
///
/// # Errors
///
/// Returns [`GenError::Arrow`] if the Arrow `zip` kernel fails (e.g. due to
/// unexpected type incompatibility between the source and null arrays).
pub fn apply_null_mask(
    array: ArrayRef,
    null_plan: &NullPlan,
    rng: &mut dyn RngCore,
    count: usize,
) -> Result<ArrayRef, GenError> {
    match null_plan {
        NullPlan::Never => Ok(array),
        NullPlan::Always => Ok(Arc::new(NullArray::new(count))),
        NullPlan::Probability(p) => {
            let p = *p;
            // Build a boolean mask: true = keep, false = null.
            let keep: BooleanArray = (0..count)
                .map(|_| {
                    let r = (rng.next_u64() as f64) / (u64::MAX as f64);
                    Some(r >= p)
                })
                .collect();
            // Create an all-null array of the same type for the "false" branch.
            let null_arr = arrow::array::new_null_array(array.data_type(), count);
            Ok(zip(&keep, &array, &null_arr)?)
        }
        NullPlan::Pattern { every_n } => {
            let every_n = *every_n;
            if every_n == 0 {
                // every_n=0 is nonsensical; treat as "never null"
                return Ok(array);
            }
            let keep: BooleanArray = (0..count).map(|i| Some(i % every_n != 0)).collect();
            let null_arr = arrow::array::new_null_array(array.data_type(), count);
            Ok(zip(&keep, &array, &null_arr)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Array, Int64Array, StringArray};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn make_rng() -> ChaCha8Rng {
        ChaCha8Rng::seed_from_u64(42)
    }

    fn make_int_array(n: usize) -> ArrayRef {
        Arc::new(Int64Array::from((0..n as i64).collect::<Vec<_>>()))
    }

    #[test]
    fn never_preserves_all_values() {
        let arr = make_int_array(100);
        let result = apply_null_mask(arr.clone(), &NullPlan::Never, &mut make_rng(), 100).unwrap();
        assert_eq!(result.null_count(), 0);
        assert_eq!(result.len(), 100);
        // Verify values are unchanged
        let orig = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        let res = result.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..100 {
            assert_eq!(orig.value(i), res.value(i));
        }
    }

    #[test]
    fn always_nulls_all_values() {
        let arr = make_int_array(50);
        let result = apply_null_mask(arr, &NullPlan::Always, &mut make_rng(), 50).unwrap();
        assert_eq!(result.len(), 50);
        // NullArray: data_type is Null, and all elements are logically null
        assert_eq!(*result.data_type(), arrow::datatypes::DataType::Null);
    }

    #[test]
    fn probability_zero_no_nulls() {
        let arr = make_int_array(100);
        let result = apply_null_mask(
            arr.clone(),
            &NullPlan::Probability(0.0),
            &mut make_rng(),
            100,
        )
        .unwrap();
        assert_eq!(result.null_count(), 0);
        // Verify values preserved
        let orig = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        let res = result.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..100 {
            assert_eq!(orig.value(i), res.value(i));
        }
    }

    #[test]
    fn probability_one_all_nulls() {
        let arr = make_int_array(100);
        let result =
            apply_null_mask(arr, &NullPlan::Probability(1.0), &mut make_rng(), 100).unwrap();
        assert_eq!(result.null_count(), 100);
    }

    #[test]
    fn probability_half_approximate() {
        let arr = make_int_array(10_000);
        let result =
            apply_null_mask(arr, &NullPlan::Probability(0.5), &mut make_rng(), 10_000).unwrap();
        let ratio = result.null_count() as f64 / 10_000.0;
        assert!(
            ratio > 0.47 && ratio < 0.53,
            "expected ~50% nulls, got {:.1}%",
            ratio * 100.0
        );
    }

    #[test]
    fn pattern_every_2() {
        let arr = make_int_array(10);
        let result =
            apply_null_mask(arr, &NullPlan::Pattern { every_n: 2 }, &mut make_rng(), 10).unwrap();
        // Indices 0, 2, 4, 6, 8 should be null (every 2nd, 0-indexed)
        assert_eq!(result.null_count(), 5);
        for i in 0..10 {
            if i % 2 == 0 {
                assert!(result.is_null(i), "index {i} should be null");
            } else {
                assert!(result.is_valid(i), "index {i} should be valid");
            }
        }
    }

    #[test]
    fn pattern_every_1_all_null() {
        let arr = make_int_array(10);
        let result =
            apply_null_mask(arr, &NullPlan::Pattern { every_n: 1 }, &mut make_rng(), 10).unwrap();
        // every_n=1: every element (i%1==0) is null
        assert_eq!(result.null_count(), 10);
    }

    #[test]
    fn pattern_every_n_zero_no_nulls() {
        let arr = make_int_array(10);
        let result = apply_null_mask(
            arr.clone(),
            &NullPlan::Pattern { every_n: 0 },
            &mut make_rng(),
            10,
        )
        .unwrap();
        // every_n=0 treated as "never null"
        assert_eq!(result.null_count(), 0);
        // Verify values preserved
        let orig = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        let res = result.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..10 {
            assert_eq!(orig.value(i), res.value(i));
        }
    }

    #[test]
    fn pattern_larger_than_count() {
        let arr = make_int_array(5);
        let result =
            apply_null_mask(arr, &NullPlan::Pattern { every_n: 100 }, &mut make_rng(), 5).unwrap();
        // Only index 0 matches (0 % 100 == 0)
        assert_eq!(result.null_count(), 1);
        assert!(result.is_null(0));
    }

    #[test]
    fn works_with_string_arrays() {
        let arr: ArrayRef = Arc::new(StringArray::from(vec!["a", "b", "c", "d"]));
        let result =
            apply_null_mask(arr, &NullPlan::Pattern { every_n: 2 }, &mut make_rng(), 4).unwrap();
        assert_eq!(result.null_count(), 2);
        let str_arr = result.as_any().downcast_ref::<StringArray>().unwrap();
        assert!(str_arr.is_null(0));
        assert_eq!(str_arr.value(1), "b");
        assert!(str_arr.is_null(2));
        assert_eq!(str_arr.value(3), "d");
    }
}