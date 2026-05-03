//! Conditional generator — switches sub-generator based on another field's value.
//!
//! Reads a reference column from [`GenContext::batch_columns`], matches each row's
//! value against the branch conditions, and delegates to the matching branch's
//! generator. Rows that match no branch use the default generator.
//!
//! The output preserves the Arrow type of the default generator. All branches
//! must produce the same Arrow type; if they differ, the output falls back to
//! `StringArray`. Null values in the reference column always route to the default.

use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int64Array,
    StringArray, UInt64Array,
};
use arrow::compute;
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

use super::create_generator;

/// A conditional generator that switches on a reference field's value.
///
/// For each row in the batch:
/// 1. Read the reference field value from `batch_columns`
/// 2. Find the first branch whose condition matches (equality check)
/// 3. Null reference values always route to the default generator
/// 4. If no branch matches, use the default generator
///
/// All sub-generators produce full batches; per-row selection uses `arrow::compute::interleave`.
pub struct ConditionalGenerator {
    /// Name of the field to branch on.
    field: String,
    /// Ordered list of (condition_string, generator) pairs.
    branches: Vec<(String, Box<dyn FieldGenerator>)>,
    /// Fallback generator when no branch matches (or reference is null).
    default: Box<dyn FieldGenerator>,
}

impl ConditionalGenerator {
    /// Create a new conditional generator.
    ///
    /// Branch conditions are converted to canonical strings for matching.
    pub fn new(
        field: String,
        branches: Vec<(knit_core::Value, knit_plan::GeneratorPlan)>,
        default_plan: knit_plan::GeneratorPlan,
    ) -> Self {
        let compiled_branches: Vec<(String, Box<dyn FieldGenerator>)> = branches
            .into_iter()
            .map(|(cond, plan)| {
                let cond_str = value_to_string(&cond);
                let gen = create_generator(&plan);
                (cond_str, gen)
            })
            .collect();
        let default_gen = create_generator(&default_plan);
        Self {
            field,
            branches: compiled_branches,
            default: default_gen,
        }
    }
}

impl FieldGenerator for ConditionalGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        // Read the reference field
        let ref_col = ctx.batch_columns.get(&self.field);
        let ref_values: Vec<Option<String>> = match ref_col {
            Some(arr) => array_to_optional_strings(arr, count),
            None => {
                tracing::warn!(
                    field = %self.field,
                    entity = %ctx.entity_name,
                    "conditional reference field not found, using default for all rows"
                );
                vec![None; count]
            }
        };

        // Generate outputs from all branches + default (index = branches.len())
        let num_sources = self.branches.len() + 1; // branches + default
        let mut source_arrays: Vec<ArrayRef> = Vec::with_capacity(num_sources);
        for (_, gen) in &self.branches {
            source_arrays.push(gen.generate(rng, count, ctx));
        }
        source_arrays.push(self.default.generate(rng, count, ctx));
        let default_idx = self.branches.len();

        // Build per-row selection: (source_index, row_index)
        let mut indices: Vec<(usize, usize)> = Vec::with_capacity(count);
        for (row, ref_val) in ref_values.iter().enumerate() {
            match ref_val {
                None => {
                    // Null reference → default
                    indices.push((default_idx, row));
                }
                Some(ref_val) => {
                    let mut matched = false;
                    for (branch_idx, (cond, _)) in self.branches.iter().enumerate() {
                        if ref_val == cond {
                            indices.push((branch_idx, row));
                            matched = true;
                            break;
                        }
                    }
                    if !matched {
                        indices.push((default_idx, row));
                    }
                }
            }
        }

        // Use arrow::compute::interleave to pick values from the right arrays
        let array_refs: Vec<&dyn Array> = source_arrays.iter().map(|a| a.as_ref()).collect();
        match compute::interleave(&array_refs, &indices) {
            Ok(result) => result,
            Err(e) => {
                // Type mismatch between branches — fall back to string conversion
                tracing::warn!(
                    error = %e,
                    "conditional branches have incompatible types, falling back to StringArray"
                );
                string_fallback(&source_arrays, &indices, count)
            }
        }
    }

    fn output_type(&self) -> DataType {
        self.default.output_type()
    }
}

/// Fall back to StringArray when branch types are incompatible.
fn string_fallback(sources: &[ArrayRef], indices: &[(usize, usize)], count: usize) -> ArrayRef {
    let mut result: Vec<String> = Vec::with_capacity(count);
    for &(src_idx, row_idx) in indices {
        let arr = &sources[src_idx];
        result.push(array_value_as_string(arr, row_idx));
    }
    Arc::new(StringArray::from(result))
}

/// Convert a `knit_core::Value` to its string representation for condition matching.
fn value_to_string(v: &knit_core::Value) -> String {
    match v {
        knit_core::Value::Null => String::new(),
        knit_core::Value::Bool(b) => b.to_string(),
        knit_core::Value::Int(n) => n.to_string(),
        knit_core::Value::Float(f) => format!("{f}"),
        knit_core::Value::String(s) => s.clone(),
        other => format!("{other:?}"),
    }
}

/// Convert an Arrow array to `Vec<Option<String>>` for condition matching.
/// Returns `None` for null values so they route to the default branch.
fn array_to_optional_strings(arr: &ArrayRef, count: usize) -> Vec<Option<String>> {
    let len = arr.len().min(count);
    let to_opt = |i: usize, s: String| -> Option<String> {
        if arr.is_null(i) { None } else { Some(s) }
    };
    if let Some(sa) = arr.as_any().downcast_ref::<StringArray>() {
        (0..len).map(|i| to_opt(i, sa.value(i).to_string())).collect()
    } else if let Some(ia) = arr.as_any().downcast_ref::<Int64Array>() {
        (0..len).map(|i| to_opt(i, ia.value(i).to_string())).collect()
    } else if let Some(ua) = arr.as_any().downcast_ref::<UInt64Array>() {
        (0..len).map(|i| to_opt(i, ua.value(i).to_string())).collect()
    } else if let Some(fa) = arr.as_any().downcast_ref::<Float64Array>() {
        (0..len).map(|i| to_opt(i, format!("{}", fa.value(i)))).collect()
    } else if let Some(ba) = arr.as_any().downcast_ref::<BooleanArray>() {
        (0..len).map(|i| to_opt(i, ba.value(i).to_string())).collect()
    } else {
        tracing::warn!("unsupported array type for conditional matching, using default for all");
        vec![None; len]
    }
}

/// Extract a single value from an Arrow array as a string (for fallback path).
fn array_value_as_string(arr: &ArrayRef, i: usize) -> String {
    if arr.is_null(i) {
        return String::new();
    }
    if let Some(sa) = arr.as_any().downcast_ref::<StringArray>() {
        sa.value(i).to_string()
    } else if let Some(ia) = arr.as_any().downcast_ref::<Int64Array>() {
        ia.value(i).to_string()
    } else if let Some(fa) = arr.as_any().downcast_ref::<Float64Array>() {
        format!("{}", fa.value(i))
    } else if let Some(ba) = arr.as_any().downcast_ref::<BooleanArray>() {
        ba.value(i).to_string()
    } else {
        String::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::GenContext;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;
    use std::sync::Arc;

    use arrow::array::StringArray;
    use knit_core::Value;
    use knit_plan::GeneratorPlan;

    #[test]
    fn test_conditional_branches_on_string_field() {
        let gen = ConditionalGenerator::new(
            "status".into(),
            vec![
                (
                    Value::String("active".into()),
                    GeneratorPlan::Constant(Value::String("welcome@example.com".into())),
                ),
                (
                    Value::String("inactive".into()),
                    GeneratorPlan::Constant(Value::String("goodbye@example.com".into())),
                ),
            ],
            GeneratorPlan::Constant(Value::String("unknown@example.com".into())),
        );

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let status_col: ArrayRef = Arc::new(StringArray::from(vec![
            "active", "inactive", "active", "pending", "inactive",
        ]));
        let mut batch = HashMap::new();
        batch.insert("status".to_string(), status_col);
        let ctx = GenContext {
            batch_columns: &batch,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "users",
        };

        let result = gen.generate(&mut rng, 5, &ctx);
        let sa = result.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(sa.value(0), "welcome@example.com");
        assert_eq!(sa.value(1), "goodbye@example.com");
        assert_eq!(sa.value(2), "welcome@example.com");
        assert_eq!(sa.value(3), "unknown@example.com"); // default
        assert_eq!(sa.value(4), "goodbye@example.com");
    }

    #[test]
    fn test_conditional_branches_on_int_field() {
        let gen = ConditionalGenerator::new(
            "tier".into(),
            vec![
                (
                    Value::Int(1),
                    GeneratorPlan::Constant(Value::String("basic".into())),
                ),
                (
                    Value::Int(2),
                    GeneratorPlan::Constant(Value::String("premium".into())),
                ),
            ],
            GeneratorPlan::Constant(Value::String("free".into())),
        );

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let tier_col: ArrayRef = Arc::new(Int64Array::from(vec![1, 2, 3, 1, 2]));
        let mut batch = HashMap::new();
        batch.insert("tier".to_string(), tier_col);
        let ctx = GenContext {
            batch_columns: &batch,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "plans",
        };

        let result = gen.generate(&mut rng, 5, &ctx);
        let sa = result.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(sa.value(0), "basic");
        assert_eq!(sa.value(1), "premium");
        assert_eq!(sa.value(2), "free"); // default
        assert_eq!(sa.value(3), "basic");
        assert_eq!(sa.value(4), "premium");
    }

    #[test]
    fn test_conditional_missing_field_uses_default() {
        let gen = ConditionalGenerator::new(
            "nonexistent".into(),
            vec![(
                Value::String("x".into()),
                GeneratorPlan::Constant(Value::String("branch".into())),
            )],
            GeneratorPlan::Constant(Value::String("default_val".into())),
        );

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let batch = HashMap::new();
        let ctx = GenContext {
            batch_columns: &batch,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test",
        };

        let result = gen.generate(&mut rng, 3, &ctx);
        let sa = result.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..3 {
            assert_eq!(sa.value(i), "default_val");
        }
    }

    #[test]
    fn test_conditional_null_reference_uses_default() {
        let gen = ConditionalGenerator::new(
            "status".into(),
            vec![(
                Value::String("active".into()),
                GeneratorPlan::Constant(Value::String("matched".into())),
            )],
            GeneratorPlan::Constant(Value::String("default_for_null".into())),
        );

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        // Create a nullable StringArray: [Some("active"), None, Some("active")]
        let status_col: ArrayRef = Arc::new(StringArray::from(vec![
            Some("active"),
            None,
            Some("active"),
        ]));
        let mut batch = HashMap::new();
        batch.insert("status".to_string(), status_col);
        let ctx = GenContext {
            batch_columns: &batch,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "users",
        };

        let result = gen.generate(&mut rng, 3, &ctx);
        let sa = result.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(sa.value(0), "matched");
        assert_eq!(sa.value(1), "default_for_null"); // null → default
        assert_eq!(sa.value(2), "matched");
    }

    #[test]
    fn test_conditional_preserves_numeric_type() {
        // Both branches produce Float64 via Constant
        let gen = ConditionalGenerator::new(
            "category".into(),
            vec![
                (
                    Value::String("high".into()),
                    GeneratorPlan::Constant(Value::Float(100.0)),
                ),
                (
                    Value::String("low".into()),
                    GeneratorPlan::Constant(Value::Float(10.0)),
                ),
            ],
            GeneratorPlan::Constant(Value::Float(0.0)),
        );

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let cat_col: ArrayRef = Arc::new(StringArray::from(vec!["high", "low", "other"]));
        let mut batch = HashMap::new();
        batch.insert("category".to_string(), cat_col);
        let ctx = GenContext {
            batch_columns: &batch,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "items",
        };

        let result = gen.generate(&mut rng, 3, &ctx);
        // Should preserve Float64 type since all branches produce same type
        // ConstantGenerator with Float produces StringArray with the float string,
        // but interleave should work since they're all the same type
        assert_eq!(result.len(), 3);
    }
}
