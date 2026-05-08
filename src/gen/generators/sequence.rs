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

use crate::gen::context::GenContext;
use crate::gen::traits::FieldGenerator;

/// Generate a monotonic integer sequence.
///
/// Values follow the formula `start + (row_offset + i) * step`, where
/// `row_offset` comes from [`GenContext`] to produce globally-unique,
/// partition-correct sequences in parallel generation.
///
/// # Usage
///
/// Typically used for entity primary keys. The [plan](crate::plan) module
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

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn ctx_with_offset(offset: u64) -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, offset, 0, 1, "test")
    }

    fn gen_seq(start: i64, step: i64, count: usize, offset: u64) -> Vec<i64> {
        let g = SequenceGenerator::new(start, step);
        let ctx = ctx_with_offset(offset);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = g.generate(&mut rng, count, &ctx);
        let ia = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        (0..ia.len()).map(|i| ia.value(i)).collect()
    }

    #[test]
    fn basic_sequence() {
        let vals = gen_seq(1, 1, 5, 0);
        assert_eq!(vals, vec![1, 2, 3, 4, 5]);
    }

    #[test]
    fn custom_start_and_step() {
        let vals = gen_seq(100, 10, 4, 0);
        assert_eq!(vals, vec![100, 110, 120, 130]);
    }

    #[test]
    fn negative_step() {
        let vals = gen_seq(0, -1, 4, 0);
        assert_eq!(vals, vec![0, -1, -2, -3]);
    }

    #[test]
    fn partition_offset() {
        // Partition 0: rows 0..3, Partition 1: rows 3..6
        let p0 = gen_seq(1, 1, 3, 0);
        let p1 = gen_seq(1, 1, 3, 3);
        assert_eq!(p0, vec![1, 2, 3]);
        assert_eq!(p1, vec![4, 5, 6]);
        // No overlap
        let all: std::collections::HashSet<i64> = p0.into_iter().chain(p1.into_iter()).collect();
        assert_eq!(all.len(), 6);
    }

    #[test]
    fn combined_offset_and_step() {
        // Formula: start + (row_offset + i) * step
        // start=100, step=10, row_offset=3 → 130, 140, 150
        let vals = gen_seq(100, 10, 3, 3);
        assert_eq!(vals, vec![130, 140, 150]);
    }

    #[test]
    fn zero_count() {
        let vals = gen_seq(1, 1, 0, 0);
        assert!(vals.is_empty());
    }

    #[test]
    fn output_type_is_int64() {
        let g = SequenceGenerator::new(0, 1);
        assert_eq!(g.output_type(), DataType::Int64);
    }

    #[test]
    fn deterministic_regardless_of_rng() {
        // Sequence ignores RNG — same params should always produce same output
        let g = SequenceGenerator::new(1, 1);
        let ctx = ctx_with_offset(0);
        let mut rng1 = ChaCha8Rng::seed_from_u64(42);
        let mut rng2 = ChaCha8Rng::seed_from_u64(999);
        let a = g.generate(&mut rng1, 5, &ctx);
        let b = g.generate(&mut rng2, 5, &ctx);
        let va: Vec<i64> = (0..5)
            .map(|i| a.as_any().downcast_ref::<Int64Array>().unwrap().value(i))
            .collect();
        let vb: Vec<i64> = (0..5)
            .map(|i| b.as_any().downcast_ref::<Int64Array>().unwrap().value(i))
            .collect();
        assert_eq!(
            va, vb,
            "sequence should be deterministic regardless of RNG seed"
        );
    }
}
