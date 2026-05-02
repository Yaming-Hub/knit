//! Null-mask application based on [`NullPlan`].

use arrow::array::{ArrayRef, BooleanArray, NullArray};
use arrow::compute::kernels::zip::zip;
use rand::RngCore;
use std::sync::Arc;

use knit_plan::NullPlan;

/// Apply a null mask to a generated array according to the given [`NullPlan`].
///
/// - `Never`  — returns the array unchanged.
/// - `Always` — returns an all-null [`NullArray`].
/// - `Probability(p)` — each element is independently null with probability *p*.
/// - `Pattern { every_n }` — every *n*-th element (0-indexed) is null.
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
            let keep: BooleanArray = (0..count)
                .map(|i| Some(i % every_n != 0))
                .collect();
            let null_arr = arrow::array::new_null_array(array.data_type(), count);
            zip(&keep, &array, &null_arr).expect("zip failed in null_mask")
        }
    }
}
