//! Composite (JSON array) generator.
//!
//! Produces JSON-encoded arrays by combining an element generator (which
//! produces individual values) with a length distribution (which determines
//! how many elements per row). Output is a `Utf8` column of JSON array strings.

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::plan::GeneratorPlan;

use crate::r#gen::context::GenContext;
use crate::r#gen::generators::{SharedSeen, create_generator, create_generator_with_seen};
use crate::r#gen::traits::FieldGenerator;

/// Generate JSON array strings by composing an element generator and a length generator.
///
/// For each row, the length generator produces a count, then the element
/// generator produces that many values, which are serialized into a JSON array
/// string like `[1, 2, 3]` or `["a", "b"]`.
///
/// # Output type
///
/// Always `Utf8` — the JSON-serialized array string.
pub struct CompositeGenerator {
    element_gen: Box<dyn FieldGenerator>,
    length_gen: Box<dyn FieldGenerator>,
}

impl CompositeGenerator {
    /// Create a new composite generator from plan components.
    pub fn new(element_plan: &GeneratorPlan, length_plan: &GeneratorPlan) -> Self {
        Self {
            element_gen: create_generator(element_plan),
            length_gen: create_generator(length_plan),
        }
    }

    /// Like [`new`](Self::new), but threads a shared seen-set through to any
    /// nested `Unique` sub-generators.
    pub fn new_with_seen(
        element_plan: &GeneratorPlan,
        length_plan: &GeneratorPlan,
        shared_seen: Option<&SharedSeen>,
    ) -> Self {
        Self {
            element_gen: create_generator_with_seen(element_plan, shared_seen),
            length_gen: create_generator_with_seen(length_plan, shared_seen),
        }
    }
}

/// Serialize a single element from an Arrow array to a JSON value string.
fn element_to_json(arr: &ArrayRef, idx: usize) -> String {
    if let Some(s) = arr.as_any().downcast_ref::<StringArray>() {
        // JSON-escape the string
        let val = s.value(idx);
        format!("\"{}\"", val.replace('\\', "\\\\").replace('"', "\\\""))
    } else if let Some(f) = arr.as_any().downcast_ref::<Float64Array>() {
        let v = f.value(idx);
        if v.is_finite() {
            format!("{v}")
        } else {
            "null".to_string()
        }
    } else if let Some(i) = arr.as_any().downcast_ref::<Int64Array>() {
        format!("{}", i.value(idx))
    } else {
        "null".to_string()
    }
}

impl FieldGenerator for CompositeGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        // First, determine lengths for each row.
        let length_arr = self.length_gen.generate(rng, count, ctx);
        let lengths: Vec<usize> = if let Some(i) = length_arr.as_any().downcast_ref::<Int64Array>()
        {
            i.values().iter().map(|v| (*v).max(0) as usize).collect()
        } else if let Some(f) = length_arr.as_any().downcast_ref::<Float64Array>() {
            f.values()
                .iter()
                .map(|v| v.round().max(0.0) as usize)
                .collect()
        } else {
            vec![0; count]
        };

        // Generate all elements at once (sum of lengths) for efficiency.
        let total_elements: usize = lengths.iter().sum();
        let all_elements = if total_elements > 0 {
            self.element_gen.generate(rng, total_elements, ctx)
        } else {
            // Generate a dummy array of length 0 - just use 0 count.
            self.element_gen.generate(rng, 0, ctx)
        };

        // Build JSON array strings by slicing into the element array.
        let mut offset = 0usize;
        let values: Vec<String> = lengths
            .iter()
            .map(|&len| {
                let mut parts = Vec::with_capacity(len);
                for j in 0..len {
                    parts.push(element_to_json(&all_elements, offset + j));
                }
                offset += len;
                format!("[{}]", parts.join(","))
            })
            .collect();

        Arc::new(StringArray::from(
            values.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        ))
    }

    fn output_type(&self) -> DataType {
        DataType::Utf8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::Value;
    use crate::plan::GeneratorPlan;
    use arrow::array::ArrayRef;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, 0, 0, 1, "test")
    }

    #[test]
    fn composite_produces_json_arrays() {
        let r#gen = CompositeGenerator::new(
            &GeneratorPlan::Constant(Value::Int(42)),
            &GeneratorPlan::Constant(Value::Int(3)),
        );
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = r#gen.generate(&mut rng, 2, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();

        assert_eq!(str_arr.value(0), "[42,42,42]");
        assert_eq!(str_arr.value(1), "[42,42,42]");
    }

    #[test]
    fn composite_with_strings() {
        let r#gen = CompositeGenerator::new(
            &GeneratorPlan::Constant(Value::String("hi".into())),
            &GeneratorPlan::Constant(Value::Int(2)),
        );
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = r#gen.generate(&mut rng, 1, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();

        assert_eq!(str_arr.value(0), "[\"hi\",\"hi\"]");
    }

    #[test]
    fn composite_zero_length() {
        let r#gen = CompositeGenerator::new(
            &GeneratorPlan::Constant(Value::Int(1)),
            &GeneratorPlan::Constant(Value::Int(0)),
        );
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = r#gen.generate(&mut rng, 3, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();

        for i in 0..3 {
            assert_eq!(str_arr.value(i), "[]");
        }
    }

    #[test]
    fn composite_with_floats() {
        let r#gen = CompositeGenerator::new(
            &GeneratorPlan::Constant(Value::Float(std::f64::consts::PI)),
            &GeneratorPlan::Constant(Value::Int(2)),
        );
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = r#gen.generate(&mut rng, 1, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let val = str_arr.value(0);
        // Should be a JSON array of two float values
        assert!(
            val.starts_with('[') && val.ends_with(']'),
            "should be JSON array: {val}"
        );
        assert!(val.contains("3.14"), "should contain 3.14: {val}");
    }

    #[test]
    fn composite_single_element() {
        let r#gen = CompositeGenerator::new(
            &GeneratorPlan::Constant(Value::String("only".into())),
            &GeneratorPlan::Constant(Value::Int(1)),
        );
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = r#gen.generate(&mut rng, 1, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(str_arr.value(0), "[\"only\"]");
    }

    #[test]
    fn composite_string_with_special_chars() {
        // Test JSON escaping of quotes and backslashes
        let r#gen = CompositeGenerator::new(
            &GeneratorPlan::Constant(Value::String("path\\name \"quoted\"".into())),
            &GeneratorPlan::Constant(Value::Int(1)),
        );
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = r#gen.generate(&mut rng, 1, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let val = str_arr.value(0);
        // Quotes should be escaped
        assert!(
            val.contains("\\\""),
            "quotes should be escaped in JSON: {val}"
        );
        // Backslashes should be escaped (original \ becomes \\)
        assert!(
            val.contains("\\\\"),
            "backslashes should be escaped in JSON: {val}"
        );
    }

    #[test]
    fn composite_output_type_is_utf8() {
        let r#gen = CompositeGenerator::new(
            &GeneratorPlan::Constant(Value::Int(1)),
            &GeneratorPlan::Constant(Value::Int(1)),
        );
        assert_eq!(r#gen.output_type(), DataType::Utf8);
    }

    #[test]
    fn composite_multiple_rows_different_lengths() {
        use crate::core::WeightedChoice;
        // Use OneOf to produce varying lengths [1, 3]
        let r#gen = CompositeGenerator::new(
            &GeneratorPlan::Constant(Value::Int(7)),
            &GeneratorPlan::OneOf {
                choices: vec![
                    WeightedChoice {
                        value: Value::Int(1),
                        weight: 1.0,
                    },
                    WeightedChoice {
                        value: Value::Int(3),
                        weight: 1.0,
                    },
                ],
                cumulative_weights: vec![0.5, 1.0],
            },
        );
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = r#gen.generate(&mut rng, 10, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        let mut seen_lengths = std::collections::HashSet::new();
        for i in 0..10 {
            let val = str_arr.value(i);
            assert!(
                val.starts_with('[') && val.ends_with(']'),
                "row {i}: should be JSON array: {val}"
            );
            let inner = &val[1..val.len() - 1];
            let elem_count = if inner.is_empty() {
                0
            } else {
                inner.split(',').count()
            };
            seen_lengths.insert(elem_count);
            if !inner.is_empty() {
                for part in inner.split(',') {
                    assert_eq!(part.trim(), "7", "row {i}: element should be 7: {val}");
                }
            }
        }
        assert!(seen_lengths.contains(&1), "should produce length-1 arrays");
        assert!(seen_lengths.contains(&3), "should produce length-3 arrays");
    }

    #[test]
    fn composite_deterministic() {
        let r#gen = CompositeGenerator::new(
            &GeneratorPlan::Constant(Value::Int(42)),
            &GeneratorPlan::Constant(Value::Int(2)),
        );
        let ctx = make_ctx();
        let a = r#gen.generate(&mut ChaCha8Rng::seed_from_u64(99), 5, &ctx);
        let b = r#gen.generate(&mut ChaCha8Rng::seed_from_u64(99), 5, &ctx);
        let a_s = a.as_any().downcast_ref::<StringArray>().unwrap();
        let b_s = b.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..5 {
            assert_eq!(
                a_s.value(i),
                b_s.value(i),
                "row {i} should be deterministic"
            );
        }
    }

    #[test]
    fn composite_negative_length_clamped_to_zero() {
        // Negative length should be clamped to 0, producing empty arrays
        let r#gen = CompositeGenerator::new(
            &GeneratorPlan::Constant(Value::Int(42)),
            &GeneratorPlan::Constant(Value::Int(-5)),
        );
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = r#gen.generate(&mut rng, 3, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..3 {
            assert_eq!(
                str_arr.value(i),
                "[]",
                "negative length should produce empty array"
            );
        }
    }
}
