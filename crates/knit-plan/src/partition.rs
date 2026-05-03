//! Partition planning — divides entity row spaces into parallel work units.
//!
//! Each partition is a contiguous, non-overlapping range of rows that can be
//! generated independently by a single thread with its own deterministic RNG.
//! Partition boundaries depend only on entity row count and target size,
//! ensuring reproducibility regardless of available thread count.

use knit_core::CountSpec;

use crate::types::PartitionRange;

/// Default target partition size: 2^20 = 1,048,576 rows.
/// Entities with fewer rows use a single partition.
const TARGET_PARTITION_SIZE: u64 = 1_048_576;

/// Resolve a [`CountSpec`] to a concrete row count for planning purposes.
///
/// - `Fixed(n)` → use `n` directly
/// - `Range { min, max }` → use `max` (plan for worst case)
/// - `Distribution(spec)` → use the expected value of the distribution
pub fn resolve_count(count: &CountSpec) -> u64 {
    match count {
        CountSpec::Fixed(n) => *n,
        CountSpec::Range { min: _, max } => *max,
        CountSpec::Distribution(spec) => {
            // Use expected value based on distribution kind.
            let params = &spec.params;
            match spec.kind {
                knit_core::DistributionKind::Normal => {
                    params.get("mean").copied().unwrap_or(0.0).max(0.0) as u64
                }
                knit_core::DistributionKind::Uniform => {
                    let min = params.get("min").copied().unwrap_or(0.0);
                    let max = params.get("max").copied().unwrap_or(0.0);
                    ((min + max) / 2.0).max(0.0) as u64
                }
                knit_core::DistributionKind::Poisson => {
                    params.get("lambda").copied().unwrap_or(0.0).max(0.0) as u64
                }
                knit_core::DistributionKind::Exponential => {
                    let lambda = params.get("lambda").copied().unwrap_or(1.0);
                    if lambda > 0.0 {
                        (1.0 / lambda) as u64
                    } else {
                        0
                    }
                }
                _ => {
                    // Fallback: use mean if available, otherwise 1000.
                    params.get("mean").copied().unwrap_or(1000.0).max(0.0) as u64
                }
            }
        }
    }
}

/// Compute partition ranges for a given total row count.
///
/// Divides `total_rows` into contiguous, non-overlapping partitions of
/// approximately `TARGET_PARTITION_SIZE` rows each. Each partition gets a
/// deterministic seed derived from `entity_seed` for reproducible generation.
///
/// Returns at least one partition even if `total_rows` is 0.
pub fn compute_partitions(total_rows: u64, entity_seed: u64) -> Vec<PartitionRange> {
    if total_rows == 0 {
        return vec![PartitionRange {
            partition_id: 0,
            start_row: 0,
            end_row: 0,
            seed: entity_seed,
        }];
    }

    let num_partitions = if total_rows <= TARGET_PARTITION_SIZE {
        1
    } else {
        total_rows.div_ceil(TARGET_PARTITION_SIZE) as u32
    };

    let rows_per_partition = total_rows / num_partitions as u64;
    let remainder = total_rows % num_partitions as u64;

    let mut partitions = Vec::with_capacity(num_partitions as usize);
    let mut start = 0u64;

    for i in 0..num_partitions {
        let extra = if (i as u64) < remainder { 1 } else { 0 };
        let end = start + rows_per_partition + extra;
        let seed = crate::rng_tree::derive_seed(entity_seed, &i.to_le_bytes());
        partitions.push(PartitionRange {
            partition_id: i,
            start_row: start,
            end_row: end,
            seed,
        });
        start = end;
    }

    partitions
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_fixed() {
        assert_eq!(resolve_count(&CountSpec::Fixed(5000)), 5000);
    }

    #[test]
    fn test_resolve_range() {
        assert_eq!(resolve_count(&CountSpec::Range { min: 100, max: 500 }), 500);
    }

    #[test]
    fn test_single_partition() {
        let parts = compute_partitions(500_000, 42);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].start_row, 0);
        assert_eq!(parts[0].end_row, 500_000);
    }

    #[test]
    fn test_multiple_partitions() {
        let parts = compute_partitions(5_000_000, 42);
        assert_eq!(parts.len(), 5);
        assert_eq!(parts[0].start_row, 0);
        assert_eq!(parts.last().unwrap().end_row, 5_000_000);
        // Verify contiguous ranges
        for i in 1..parts.len() {
            assert_eq!(parts[i].start_row, parts[i - 1].end_row);
        }
    }
}
