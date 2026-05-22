//! Tuple lookup generator — maps a source field's value to a co-occurring value.
//!
//! Used for multi-column tuple dictionaries where the primary column is generated
//! by a [`DictionaryGenerator`](super::dictionary::DictionaryGenerator) and
//! secondary columns look up their values from a shared tuple table.
//!
//! Uses Fisher-Yates shuffle per key to ensure all tuple entries are represented
//! before any repeats, providing better coverage than random sampling.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, StringArray};
use arrow::datatypes::DataType;
use rand::Rng;

use crate::r#gen::context::GenContext;
use crate::r#gen::traits::FieldGenerator;

/// Generate string values by looking up a source field's value in a tuple table.
///
/// Reads the source field from `batch_columns`, maps each value through a
/// pre-loaded lookup table, and returns a shuffled co-occurring value.
/// Each key's entries are cycled through in shuffled order (Fisher-Yates),
/// reshuffling at each cycle boundary to avoid duplicates within a cycle.
/// Unknown source values produce a null.
pub struct TupleLookupGenerator {
    /// Name of the source field to read from batch_columns.
    source_field: String,
    /// Mapping from source field value → list of possible values for this field.
    lookup: HashMap<String, Vec<String>>,
}

impl TupleLookupGenerator {
    /// Create a new tuple lookup generator.
    pub fn new(source_field: String, lookup: HashMap<String, Vec<String>>) -> Self {
        Self {
            source_field,
            lookup,
        }
    }
}

impl FieldGenerator for TupleLookupGenerator {
    fn generate(&self, rng: &mut dyn Rng, count: usize, ctx: &GenContext) -> ArrayRef {
        let source_col = ctx.batch_columns.get(&self.source_field);
        let values: Vec<Option<&str>> =
            match source_col.and_then(|c| c.as_any().downcast_ref::<StringArray>()) {
                Some(source_arr) => {
                    // Track per-key shuffle state: (shuffled indices, current position)
                    let mut key_state: HashMap<&str, (Vec<usize>, usize)> = HashMap::new();

                    (0..count)
                        .map(|i| {
                            if i < source_arr.len() && !source_arr.is_null(i) {
                                let key = source_arr.value(i);
                                self.lookup.get(key).and_then(|entries| {
                                    let n = entries.len();
                                    if n == 0 {
                                        return None;
                                    }
                                    if n == 1 {
                                        return Some(entries[0].as_str());
                                    }

                                    let (indices, pos) =
                                        key_state.entry(key).or_insert_with(|| {
                                            // Initial Fisher-Yates shuffle
                                            let mut idx: Vec<usize> = (0..n).collect();
                                            for k in (1..n).rev() {
                                                let j = rng.next_u32() as usize % (k + 1);
                                                idx.swap(k, j);
                                            }
                                            (idx, 0)
                                        });

                                    // Reshuffle at cycle boundary
                                    if *pos > 0 && *pos % n == 0 {
                                        for k in (1..n).rev() {
                                            let j = rng.next_u32() as usize % (k + 1);
                                            indices.swap(k, j);
                                        }
                                    }

                                    let idx = indices[*pos % n];
                                    *pos += 1;
                                    Some(entries[idx].as_str())
                                })
                            } else {
                                None
                            }
                        })
                        .collect()
                }
                None => vec![None; count],
            };
        Arc::new(StringArray::from(values)) as ArrayRef
    }

    fn output_type(&self) -> DataType {
        DataType::Utf8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::StringArray;
    use std::sync::Arc;

    #[test]
    fn lookup_maps_values() {
        let mut lookup: HashMap<String, Vec<String>> = HashMap::new();
        lookup.insert("Seattle".to_string(), vec!["WA".to_string()]);
        lookup.insert("Portland".to_string(), vec!["OR".to_string()]);
        lookup.insert("Denver".to_string(), vec!["CO".to_string()]);

        let generator = TupleLookupGenerator::new("city".to_string(), lookup);

        let cities: ArrayRef = Arc::new(StringArray::from(vec!["Seattle", "Portland", "Denver"]));
        let mut cols = HashMap::new();
        cols.insert("city".to_string(), cities);

        let ctx = GenContext::new(&cols, 0, 0, 1, "test");
        let mut rng = rand::rng();
        let result = generator.generate(&mut rng, 3, &ctx);
        let arr = result.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(arr.value(0), "WA");
        assert_eq!(arr.value(1), "OR");
        assert_eq!(arr.value(2), "CO");
    }

    #[test]
    fn lookup_unknown_returns_null() {
        let mut lookup: HashMap<String, Vec<String>> = HashMap::new();
        lookup.insert("Seattle".to_string(), vec!["WA".to_string()]);

        let generator = TupleLookupGenerator::new("city".to_string(), lookup);

        let cities: ArrayRef = Arc::new(StringArray::from(vec!["Seattle", "Unknown"]));
        let mut cols = HashMap::new();
        cols.insert("city".to_string(), cities);

        let ctx = GenContext::new(&cols, 0, 0, 1, "test");
        let mut rng = rand::rng();
        let result = generator.generate(&mut rng, 2, &ctx);
        let arr = result.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(arr.value(0), "WA");
        assert!(arr.is_null(1));
    }

    #[test]
    fn lookup_missing_source_returns_nulls() {
        let generator = TupleLookupGenerator::new("city".to_string(), HashMap::new());
        let cols = HashMap::new();
        let ctx = GenContext::new(&cols, 0, 0, 1, "test");
        let mut rng = rand::rng();
        let result = generator.generate(&mut rng, 3, &ctx);
        let arr = result.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(arr.len(), 3);
        assert!(arr.is_null(0));
    }

    #[test]
    fn shuffle_covers_all_entries_per_cycle() {
        // With 3 entries per key and 3 rows per key, each entry should appear exactly once
        let mut lookup: HashMap<String, Vec<String>> = HashMap::new();
        lookup.insert(
            "A".to_string(),
            vec!["x".to_string(), "y".to_string(), "z".to_string()],
        );

        let generator = TupleLookupGenerator::new("cat".to_string(), lookup);

        let source: ArrayRef = Arc::new(StringArray::from(vec!["A", "A", "A"]));
        let mut cols = HashMap::new();
        cols.insert("cat".to_string(), source);

        let ctx = GenContext::new(&cols, 0, 0, 1, "test");
        let mut rng = rand::rng();
        let result = generator.generate(&mut rng, 3, &ctx);
        let arr = result.as_any().downcast_ref::<StringArray>().unwrap();

        // All three values must appear (no duplicates in a single cycle)
        let mut values: Vec<&str> = (0..3).map(|i| arr.value(i)).collect();
        values.sort();
        assert_eq!(values, vec!["x", "y", "z"]);
    }

    #[test]
    fn shuffle_reshuffles_at_cycle_boundary() {
        // With 2 entries and 4 rows, each entry appears exactly twice
        let mut lookup: HashMap<String, Vec<String>> = HashMap::new();
        lookup.insert("K".to_string(), vec!["a".to_string(), "b".to_string()]);

        let generator = TupleLookupGenerator::new("key".to_string(), lookup);

        let source: ArrayRef = Arc::new(StringArray::from(vec!["K", "K", "K", "K"]));
        let mut cols = HashMap::new();
        cols.insert("key".to_string(), source);

        let ctx = GenContext::new(&cols, 0, 0, 1, "test");
        let mut rng = rand::rng();
        let result = generator.generate(&mut rng, 4, &ctx);
        let arr = result.as_any().downcast_ref::<StringArray>().unwrap();

        // Count occurrences: each value should appear exactly 2 times
        let values: Vec<&str> = (0..4).map(|i| arr.value(i)).collect();
        let a_count = values.iter().filter(|&&v| v == "a").count();
        let b_count = values.iter().filter(|&&v| v == "b").count();
        assert_eq!(a_count, 2);
        assert_eq!(b_count, 2);

        // First cycle: no duplicates
        let first_cycle: Vec<&str> = values[0..2].to_vec();
        assert_ne!(first_cycle[0], first_cycle[1]);
    }
}
