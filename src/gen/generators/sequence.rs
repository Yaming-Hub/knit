//! Auto-increment / cyclic sequence generator.
//!
//! Produces monotonically increasing (or decreasing) integer sequences suitable
//! for surrogate primary keys. Partition-awareness is achieved through the
//! `row_offset` field in [`GenContext`], so parallel
//! partitions produce non-overlapping key ranges.

use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array};
use arrow::datatypes::DataType;
use rand::Rng;
use rand::RngExt;

use crate::r#gen::context::GenContext;
use crate::r#gen::traits::FieldGenerator;

/// Generate a monotonic integer sequence with optional jitter.
///
/// Values follow the formula `start + (row_offset + i) * step + jitter_offset`,
/// where `row_offset` comes from [`GenContext`] to produce globally-unique,
/// partition-correct sequences in parallel generation, and `jitter_offset` is
/// a random integer drawn uniformly from `[-jitter_ms, +jitter_ms]` (inclusive,
/// when jitter is configured).
///
/// # Jitter and uniqueness
///
/// When jitter is enabled, values are no longer guaranteed unique or
/// partition-disjoint. This is intentional: jitter is designed for temporal
/// sequences (epoch-ms timestamps) where slight overlap is acceptable,
/// not for primary keys or unique identifiers.
///
/// # Usage
///
/// Typically used for entity primary keys or temporal sequences. The
/// [plan](crate::plan) module assigns each partition a non-overlapping
/// row-offset range so that keys never collide across partitions.
/// Jitter is useful for temporal fields to create realistic irregular timestamps.
pub struct SequenceGenerator {
    start: i64,
    step: i64,
    jitter_ms: Option<i64>,
}

impl SequenceGenerator {
    /// Create a new sequence generator with the given start, step, and optional jitter.
    pub fn new(start: i64, step: i64, jitter_ms: Option<i64>) -> Self {
        Self {
            start,
            step,
            jitter_ms,
        }
    }
}

impl FieldGenerator for SequenceGenerator {
    fn generate(&self, rng: &mut dyn Rng, count: usize, ctx: &GenContext) -> ArrayRef {
        let values: Vec<i64> = match self.jitter_ms {
            Some(j) if j > 0 => (0..count)
                .map(|i| {
                    let base = self.start + (ctx.row_offset + i as u64) as i64 * self.step;
                    let offset = rng.random_range(-j..=j);
                    base + offset
                })
                .collect(),
            _ => (0..count)
                .map(|i| self.start + (ctx.row_offset + i as u64) as i64 * self.step)
                .collect(),
        };
        Arc::new(Int64Array::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

/// Cycle through a fixed list of string values round-robin.
///
/// Deterministic: `values[(row_offset + i) % values.len()]`.
/// Partition-safe via `row_offset` from [`GenContext`].
pub struct CyclicValuesGenerator {
    values: Vec<String>,
}

impl CyclicValuesGenerator {
    /// Creates a new `CyclicValuesGenerator` from the given string values.
    pub fn new(values: Vec<String>) -> Self {
        Self { values }
    }
}

impl FieldGenerator for CyclicValuesGenerator {
    fn generate(&self, _rng: &mut dyn Rng, count: usize, ctx: &GenContext) -> ArrayRef {
        let n = self.values.len();
        let result: Vec<&str> = (0..count)
            .map(|i| {
                let idx = ((ctx.row_offset + i as u64) % n as u64) as usize;
                self.values[idx].as_str()
            })
            .collect();
        Arc::new(arrow::array::StringArray::from(result))
    }

    fn output_type(&self) -> DataType {
        DataType::Utf8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand::rngs::ChaCha8Rng;
    use std::collections::HashMap;

    fn ctx_with_offset(offset: u64) -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, offset, 0, 1, "test")
    }

    fn gen_seq(start: i64, step: i64, count: usize, offset: u64) -> Vec<i64> {
        let g = SequenceGenerator::new(start, step, None);
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
        let all: std::collections::HashSet<i64> = p0.into_iter().chain(p1).collect();
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
        let g = SequenceGenerator::new(0, 1, None);
        assert_eq!(g.output_type(), DataType::Int64);
    }

    #[test]
    fn deterministic_regardless_of_rng() {
        // Sequence ignores RNG — same params should always produce same output
        let g = SequenceGenerator::new(1, 1, None);
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

    // ── CyclicValuesGenerator tests ─────────────────────────────────

    fn gen_cyclic(values: &[&str], count: usize, offset: u64) -> Vec<String> {
        let g = CyclicValuesGenerator::new(values.iter().map(|s| s.to_string()).collect());
        let ctx = ctx_with_offset(offset);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = g.generate(&mut rng, count, &ctx);
        let sa = arr
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .unwrap();
        (0..sa.len()).map(|i| sa.value(i).to_string()).collect()
    }

    #[test]
    fn cyclic_basic() {
        let vals = gen_cyclic(&["Mon", "Tue", "Wed"], 6, 0);
        assert_eq!(vals, vec!["Mon", "Tue", "Wed", "Mon", "Tue", "Wed"]);
    }

    #[test]
    fn cyclic_with_offset() {
        let vals = gen_cyclic(&["A", "B", "C", "D"], 4, 2);
        assert_eq!(vals, vec!["C", "D", "A", "B"]);
    }

    #[test]
    fn cyclic_partition_continuity() {
        let p0 = gen_cyclic(&["X", "Y", "Z"], 3, 0);
        let p1 = gen_cyclic(&["X", "Y", "Z"], 3, 3);
        assert_eq!(p0, vec!["X", "Y", "Z"]);
        assert_eq!(p1, vec!["X", "Y", "Z"]);
    }

    #[test]
    fn cyclic_single_value() {
        let vals = gen_cyclic(&["only"], 4, 0);
        assert_eq!(vals, vec!["only", "only", "only", "only"]);
    }

    #[test]
    fn cyclic_output_type() {
        let g = CyclicValuesGenerator::new(vec!["a".into()]);
        assert_eq!(g.output_type(), DataType::Utf8);
    }

    #[test]
    fn cyclic_deterministic_regardless_of_rng() {
        let g = CyclicValuesGenerator::new(vec!["a".into(), "b".into(), "c".into()]);
        let ctx = ctx_with_offset(0);
        let mut rng1 = ChaCha8Rng::seed_from_u64(42);
        let mut rng2 = ChaCha8Rng::seed_from_u64(999);
        let a = g.generate(&mut rng1, 5, &ctx);
        let b = g.generate(&mut rng2, 5, &ctx);
        let va: Vec<String> = (0..5)
            .map(|i| {
                a.as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .unwrap()
                    .value(i)
                    .to_string()
            })
            .collect();
        let vb: Vec<String> = (0..5)
            .map(|i| {
                b.as_any()
                    .downcast_ref::<arrow::array::StringArray>()
                    .unwrap()
                    .value(i)
                    .to_string()
            })
            .collect();
        assert_eq!(va, vb, "cyclic values should be deterministic");
    }

    // ── Jitter tests ────────────────────────────────────────────────

    fn gen_seq_jitter(
        start: i64,
        step: i64,
        jitter_ms: i64,
        count: usize,
        offset: u64,
        seed: u64,
    ) -> Vec<i64> {
        let g = SequenceGenerator::new(start, step, Some(jitter_ms));
        let ctx = ctx_with_offset(offset);
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let arr = g.generate(&mut rng, count, &ctx);
        let ia = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        (0..ia.len()).map(|i| ia.value(i)).collect()
    }

    #[test]
    fn jitter_values_within_bounds() {
        let jitter = 1000; // ±1000
        let start = 0;
        let step = 10_000;
        let vals = gen_seq_jitter(start, step, jitter, 100, 0, 42);
        for (i, &v) in vals.iter().enumerate() {
            let base = start + (i as i64) * step;
            assert!(
                v >= base - jitter && v <= base + jitter,
                "value {} at index {} outside [{}, {}]",
                v,
                i,
                base - jitter,
                base + jitter,
            );
        }
    }

    #[test]
    fn jitter_produces_different_values_than_no_jitter() {
        let no_jitter = gen_seq(1000, 5000, 10, 0);
        let with_jitter = gen_seq_jitter(1000, 5000, 500, 10, 0, 42);
        // At least one value should differ due to jitter
        assert_ne!(
            no_jitter, with_jitter,
            "jitter should modify at least one value"
        );
    }

    #[test]
    fn jitter_deterministic_with_same_seed() {
        let a = gen_seq_jitter(0, 1000, 100, 20, 0, 42);
        let b = gen_seq_jitter(0, 1000, 100, 20, 0, 42);
        assert_eq!(a, b, "same seed should produce same jitter");
    }

    #[test]
    fn jitter_different_seeds_differ() {
        let a = gen_seq_jitter(0, 1000, 100, 20, 0, 42);
        let b = gen_seq_jitter(0, 1000, 100, 20, 0, 99);
        assert_ne!(a, b, "different seeds should produce different jitter");
    }

    #[test]
    fn jitter_zero_behaves_like_no_jitter() {
        let no_jitter = gen_seq(0, 100, 10, 0);
        let zero_jitter = gen_seq_jitter(0, 100, 0, 10, 0, 42);
        assert_eq!(
            no_jitter, zero_jitter,
            "jitter=0 should produce same values as no jitter"
        );
    }

    #[test]
    fn jitter_with_partition_offset() {
        let jitter = 500;
        let step = 10_000;
        let vals = gen_seq_jitter(0, step, jitter, 5, 10, 42);
        for (i, &v) in vals.iter().enumerate() {
            let base = (10 + i as i64) * step;
            assert!(
                v >= base - jitter && v <= base + jitter,
                "partition-offset value {} at index {} outside bounds",
                v,
                i,
            );
        }
    }
}
