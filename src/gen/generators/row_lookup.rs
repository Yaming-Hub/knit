//! Row-based lookup generator — samples full rows from a pre-loaded TSV.
//!
//! All fields on the same entity sharing the same row-lookup file receive
//! the same randomly chosen row index per output record, preserving
//! cross-column coherence without requiring a unique primary key column.
//!
//! Coherence is achieved via a shared [`RowIndexCache`]: the first
//! `RowLookupGenerator` in a batch computes the random row indices and
//! caches them; subsequent generators for the same file reuse those
//! indices, guaranteeing all columns read from the same source row.

use std::sync::{Arc, Mutex};

use arrow::array::{ArrayRef, StringArray};
use arrow::datatypes::DataType;
use rand::Rng;

use crate::r#gen::context::GenContext;
use crate::r#gen::traits::FieldGenerator;

/// Generate a usize in [0, n) from a trait-object RNG.
fn gen_row_index(rng: &mut dyn Rng, n: usize) -> usize {
    debug_assert!(n > 0);
    let n = n as u64;
    let threshold = u64::MAX - (u64::MAX % n);
    loop {
        let r = rng.next_u64();
        if r < threshold {
            return (r % n) as usize;
        }
    }
}

/// Shared cache of pre-computed row indices for a single batch.
///
/// All `RowLookupGenerator` instances that share the same source file hold
/// a reference to the same `RowIndexCache`. The first generator to run in a
/// batch populates the cache; subsequent generators read from it.
///
/// The cache is invalidated between batches by comparing `batch_key`
/// (the `row_offset` value, which is unique per batch).
pub struct RowIndexCache {
    batch_key: u64,
    indices: Vec<usize>,
}

impl RowIndexCache {
    /// Create a new empty cache (no valid batch yet).
    pub fn new() -> Self {
        Self {
            batch_key: u64::MAX,
            indices: Vec::new(),
        }
    }
}

/// Generate string values by sampling a random row from a full-row dictionary.
///
/// The generator holds a shared reference to all rows loaded from the TSV.
/// For each output record it reads the value at its assigned column from
/// the shared row-index vector, ensuring all columns on the same entity
/// produce values from the same source row.
///
/// Cross-column coherence is maintained via [`RowIndexCache`]: the first
/// generator in the batch loop populates it; the rest reuse it.
pub struct RowLookupGenerator {
    /// All rows from the TSV file (shared across all columns).
    rows: Arc<Vec<Vec<String>>>,
    /// Column index to read from each row.
    column: usize,
    /// Shared cache of row indices — ensures all columns use the same rows.
    index_cache: Arc<Mutex<RowIndexCache>>,
}

impl RowLookupGenerator {
    /// Create a new row lookup generator with a shared index cache.
    ///
    /// All generators for columns of the same TSV file should share the
    /// same `index_cache` so they produce coherent rows.
    pub fn new(
        rows: Arc<Vec<Vec<String>>>,
        column: usize,
        index_cache: Arc<Mutex<RowIndexCache>>,
    ) -> Self {
        Self {
            rows,
            column,
            index_cache,
        }
    }
}

impl FieldGenerator for RowLookupGenerator {
    fn generate(&self, rng: &mut dyn Rng, count: usize, ctx: &GenContext) -> ArrayRef {
        if self.rows.is_empty() {
            return Arc::new(StringArray::from(vec![None::<&str>; count])) as ArrayRef;
        }

        let row_count = self.rows.len();
        let batch_key = ctx.row_offset;

        // Lock the shared cache: populate if stale, reuse if fresh.
        let indices = {
            let mut cache = self.index_cache.lock().unwrap();
            if cache.batch_key != batch_key || cache.indices.len() != count {
                // First generator in this batch — compute row indices.
                let new_indices: Vec<usize> =
                    (0..count).map(|_| gen_row_index(rng, row_count)).collect();
                cache.batch_key = batch_key;
                cache.indices = new_indices;
            }
            cache.indices.clone()
        };

        let values: Vec<Option<&str>> = indices
            .iter()
            .map(|&row_idx| {
                self.rows
                    .get(row_idx)
                    .and_then(|row| row.get(self.column))
                    .map(|v| v.as_str())
            })
            .collect();

        Arc::new(StringArray::from(values)) as ArrayRef
    }

    fn output_type(&self) -> DataType {
        DataType::Utf8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;

    fn make_cache() -> Arc<Mutex<RowIndexCache>> {
        Arc::new(Mutex::new(RowIndexCache::new()))
    }

    #[test]
    fn row_lookup_samples_values() {
        let rows = Arc::new(vec![
            vec!["Alice".to_string(), "30".to_string(), "NYC".to_string()],
            vec!["Bob".to_string(), "25".to_string(), "LA".to_string()],
            vec!["Carol".to_string(), "35".to_string(), "SF".to_string()],
        ]);

        let cache = make_cache();
        let gen_col0 = RowLookupGenerator::new(rows.clone(), 0, cache.clone());
        let gen_col1 = RowLookupGenerator::new(rows.clone(), 1, cache.clone());
        let gen_col2 = RowLookupGenerator::new(rows.clone(), 2, cache.clone());

        let cols = std::collections::HashMap::new();
        let ctx = GenContext::new(&cols, 0, 0, 1, "test");
        let mut rng = rand::rng();

        let result0 = gen_col0.generate(&mut rng, 10, &ctx);
        let arr0 = result0.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(arr0.len(), 10);

        for i in 0..10 {
            assert!(!arr0.is_null(i));
            let val = arr0.value(i);
            assert!(val == "Alice" || val == "Bob" || val == "Carol");
        }

        // col1 and col2 use same cache (same batch_key=0), so they get same indices
        let result1 = gen_col1.generate(&mut rng, 10, &ctx);
        let arr1 = result1.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..10 {
            let val = arr1.value(i);
            assert!(val == "30" || val == "25" || val == "35");
        }

        let result2 = gen_col2.generate(&mut rng, 10, &ctx);
        let arr2 = result2.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..10 {
            let val = arr2.value(i);
            assert!(val == "NYC" || val == "LA" || val == "SF");
        }
    }

    #[test]
    fn row_lookup_cross_column_coherence() {
        // Verify that all columns produce values from the SAME source row
        let rows = Arc::new(vec![
            vec!["Alice".to_string(), "30".to_string(), "NYC".to_string()],
            vec!["Bob".to_string(), "25".to_string(), "LA".to_string()],
            vec!["Carol".to_string(), "35".to_string(), "SF".to_string()],
        ]);

        let cache = make_cache();
        let gen_col0 = RowLookupGenerator::new(rows.clone(), 0, cache.clone());
        let gen_col1 = RowLookupGenerator::new(rows.clone(), 1, cache.clone());
        let gen_col2 = RowLookupGenerator::new(rows.clone(), 2, cache.clone());

        let cols = std::collections::HashMap::new();
        let ctx = GenContext::new(&cols, 0, 0, 1, "test");
        let mut rng = rand::rng();

        let count = 20;
        let r0 = gen_col0.generate(&mut rng, count, &ctx);
        let r1 = gen_col1.generate(&mut rng, count, &ctx);
        let r2 = gen_col2.generate(&mut rng, count, &ctx);

        let a0 = r0.as_any().downcast_ref::<StringArray>().unwrap();
        let a1 = r1.as_any().downcast_ref::<StringArray>().unwrap();
        let a2 = r2.as_any().downcast_ref::<StringArray>().unwrap();

        // Every output row must be a valid combination from the source
        let valid_rows: Vec<(&str, &str, &str)> =
            vec![("Alice", "30", "NYC"), ("Bob", "25", "LA"), ("Carol", "35", "SF")];

        for i in 0..count {
            let combo = (a0.value(i), a1.value(i), a2.value(i));
            assert!(
                valid_rows.contains(&combo),
                "Row {i} has incoherent combination: {combo:?}"
            );
        }
    }

    #[test]
    fn row_lookup_empty_returns_nulls() {
        let rows = Arc::new(Vec::new());
        let generator = RowLookupGenerator::new(rows, 0, make_cache());
        let cols = std::collections::HashMap::new();
        let ctx = GenContext::new(&cols, 0, 0, 1, "test");
        let mut rng = rand::rng();

        let result = generator.generate(&mut rng, 3, &ctx);
        let arr = result.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(arr.len(), 3);
        assert!(arr.is_null(0));
    }

    #[test]
    fn row_lookup_cache_invalidates_between_batches() {
        let rows = Arc::new(vec![
            vec!["A".to_string(), "1".to_string()],
            vec!["B".to_string(), "2".to_string()],
        ]);

        let cache = make_cache();
        let gen_col0 = RowLookupGenerator::new(rows.clone(), 0, cache.clone());
        let gen_col1 = RowLookupGenerator::new(rows.clone(), 1, cache.clone());

        let cols = std::collections::HashMap::new();
        let mut rng = rand::rng();

        // Batch 1 (row_offset=0)
        let ctx1 = GenContext::new(&cols, 0, 0, 1, "test");
        let r0_b1 = gen_col0.generate(&mut rng, 5, &ctx1);
        let r1_b1 = gen_col1.generate(&mut rng, 5, &ctx1);
        let a0 = r0_b1.as_any().downcast_ref::<StringArray>().unwrap();
        let a1 = r1_b1.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..5 {
            let matches =
                (a0.value(i) == "A" && a1.value(i) == "1") ||
                (a0.value(i) == "B" && a1.value(i) == "2");
            assert!(matches, "Batch 1 row {i} incoherent");
        }

        // Batch 2 (row_offset=5) — cache must invalidate
        let ctx2 = GenContext::new(&cols, 5, 0, 1, "test");
        let r0_b2 = gen_col0.generate(&mut rng, 5, &ctx2);
        let r1_b2 = gen_col1.generate(&mut rng, 5, &ctx2);
        let a0 = r0_b2.as_any().downcast_ref::<StringArray>().unwrap();
        let a1 = r1_b2.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..5 {
            let matches =
                (a0.value(i) == "A" && a1.value(i) == "1") ||
                (a0.value(i) == "B" && a1.value(i) == "2");
            assert!(matches, "Batch 2 row {i} incoherent");
        }
    }
}
