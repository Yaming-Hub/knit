//! Auto-increment / cyclic sequence generator.
//!
//! Produces monotonically increasing (or decreasing) integer sequences suitable
//! for surrogate primary keys. Partition-awareness is achieved through the
//! `row_offset` field in [`GenContext`], so parallel
//! partitions produce non-overlapping key ranges.

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Generate a monotonic integer sequence.
///
/// Values follow the formula `start + (row_offset + i) * step`, where
/// `row_offset` comes from [`GenContext`] to produce globally-unique,
/// partition-correct sequences in parallel generation.
///
/// # Usage
///
/// Typically used for entity primary keys. The [`knit-plan`](knit_plan) crate
/// assigns each partition a non-overlapping row-offset range so that keys never
/// collide across partitions.
pub struct SequenceGenerator {
    start: i64,
    step: i64,
}

impl SequenceGenerator {
    /// Create a new sequence generator with the given start and step.
    pub fn new(start: i64, step: i64) -> Self {
        Self { start, step }
    }
}

impl FieldGenerator for SequenceGenerator {
    fn generate(&self, _rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let values: Vec<i64> = (0..count)
            .map(|i| self.start + (ctx.row_offset + i as u64) as i64 * self.step)
            .collect();
        Arc::new(Int64Array::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}
