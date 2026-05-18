//! Tuple lookup generator — maps a source field's value to a co-occurring value.
//!
//! Used for multi-column tuple dictionaries where the primary column is generated
//! by a [`DictionaryGenerator`](super::dictionary::DictionaryGenerator) and
//! secondary columns look up their values from a shared tuple table.

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
/// pre-loaded lookup table, and returns the corresponding co-occurring value.
/// Unknown source values produce a null.
pub struct TupleLookupGenerator {
    /// Name of the source field to read from batch_columns.
    source_field: String,
    /// Mapping from source field value → this field's value.
    lookup: HashMap<String, String>,
}

impl TupleLookupGenerator {
    /// Create a new tuple lookup generator.
    pub fn new(source_field: String, lookup: HashMap<String, String>) -> Self {
        Self {
            source_field,
            lookup,
        }
    }
}

impl FieldGenerator for TupleLookupGenerator {
    fn generate(&self, _rng: &mut dyn Rng, count: usize, ctx: &GenContext) -> ArrayRef {
        let source_col = ctx.batch_columns.get(&self.source_field);
        let values: Vec<Option<&str>> =
            match source_col.and_then(|c| c.as_any().downcast_ref::<StringArray>()) {
                Some(source_arr) => (0..count)
                    .map(|i| {
                        if i < source_arr.len() && !source_arr.is_null(i) {
                            let key = source_arr.value(i);
                            self.lookup.get(key).map(|v| v.as_str())
                        } else {
                            None
                        }
                    })
                    .collect(),
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
        let mut lookup = HashMap::new();
        lookup.insert("Seattle".to_string(), "WA".to_string());
        lookup.insert("Portland".to_string(), "OR".to_string());
        lookup.insert("Denver".to_string(), "CO".to_string());

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
        let mut lookup = HashMap::new();
        lookup.insert("Seattle".to_string(), "WA".to_string());

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
}
