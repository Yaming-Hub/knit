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

use knit_plan::NullPlan;

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
pub fn apply_null_mask(
    array: ArrayRef,
    null_plan: &NullPlan,
    rng: &mut dyn RngCore,
    count: usize,
) -> ArrayRef {
    match null_plan {
        NullPlan::Never => array,
        NullPlan::Always => Arc::new(NullArray::new(count)),
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
            zip(&keep, &array, &null_arr).expect("zip failed in null_mask")
        }
        NullPlan::Pattern { every_n } => {
            let every_n = *every_n;
            if every_n == 0 {
                // every_n=0 is nonsensical; treat as "never null"
                return array;
            }
            let keep: BooleanArray = (0..count)
                .map(|i| Some(i % every_n != 0))
                .collect();
            let null_arr = arrow::array::new_null_array(array.data_type(), count);
            zip(&keep, &array, &null_arr).expect("zip failed in null_mask")
        }
    }
}
