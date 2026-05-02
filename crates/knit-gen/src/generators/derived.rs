//! Derived field generator — expression evaluator referencing sibling columns.
//!
//! Supports simple expressions that combine columns from the current batch:
//! - Numeric: `${a} + ${b}`, `${a} - ${b}`, `${a} * ${b}`, `${a} / ${b}`
//! - String concatenation: `${first_name} ${last_name}`
//!
//! Expressions are parsed at generation time, not at construction, so missing
//! columns produce a warning and a fallback null/zero array.

use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Evaluate a simple expression referencing other fields in the batch.
///
/// # Supported expressions
///
/// **Numeric binary ops** — `${field_a} op ${field_b}` where op ∈ {`+`, `-`, `*`, `/`}.
/// Both referenced columns must be numeric (`Int64` or `Float64`).
///
/// **String templates** — any expression containing `${field}` references mixed
/// with literal text is treated as a string concatenation template. Each
/// `${field}` is replaced per-row with the string representation of that column's
/// value.
///
/// # Fallback
///
/// If a referenced column is missing from [`GenContext::batch_columns`], a
/// `tracing::warn` is emitted and a zero/empty fallback is used.
pub struct DerivedGenerator {
    expr: String,
    depends_on: Vec<String>,
}

impl DerivedGenerator {
    /// Create a new derived generator.
    pub fn new(expr: String, depends_on: Vec<String>) -> Self {
        Self { expr, depends_on }
    }
}

/// A parsed binary numeric operation.
struct NumericBinOp {
    left: String,
    op: char,
    right: String,
}

/// Try to parse `${a} op ${b}` where op is +, -, *, /
fn parse_numeric_binop(expr: &str) -> Option<NumericBinOp> {
    let trimmed = expr.trim();
    // Pattern: ${name} op ${name}
    let rest = trimmed.strip_prefix("${")?;
    let close = rest.find('}')?;
    let left = rest[..close].to_string();
    let after_left = rest[close + 1..].trim();

    let op = after_left.chars().next()?;
    if !matches!(op, '+' | '-' | '*' | '/') {
        return None;
    }

    let after_op = after_left[op.len_utf8()..].trim();
    let rest2 = after_op.strip_prefix("${")?;
    let close2 = rest2.find('}')?;
    let right = rest2[..close2].to_string();

    // Nothing meaningful after the second field reference
    let trailing = rest2[close2 + 1..].trim();
    if !trailing.is_empty() {
        return None;
    }

    Some(NumericBinOp { left, op, right })
}

/// Extract f64 values from an ArrayRef (supports Int64 and Float64).
fn extract_f64(arr: &ArrayRef, count: usize) -> Vec<f64> {
    if let Some(f) = arr.as_any().downcast_ref::<Float64Array>() {
        f.values().to_vec()
    } else if let Some(i) = arr
        .as_any()
        .downcast_ref::<arrow::array::Int64Array>()
    {
        i.values().iter().map(|v| *v as f64).collect()
    } else {
        vec![0.0; count]
    }
}

/// Extract string values from an ArrayRef for template interpolation.
fn extract_strings(arr: &ArrayRef, count: usize) -> Vec<String> {
    if let Some(s) = arr.as_any().downcast_ref::<StringArray>() {
        (0..count).map(|i| s.value(i).to_string()).collect()
    } else if let Some(f) = arr.as_any().downcast_ref::<Float64Array>() {
        f.values().iter().map(|v| v.to_string()).collect()
    } else if let Some(i) = arr
        .as_any()
        .downcast_ref::<arrow::array::Int64Array>()
    {
        i.values().iter().map(|v| v.to_string()).collect()
    } else if let Some(b) = arr
        .as_any()
        .downcast_ref::<arrow::array::BooleanArray>()
    {
        (0..count)
            .map(|i| if b.value(i) { "true" } else { "false" }.to_string())
            .collect()
    } else {
        vec![String::new(); count]
    }
}

impl FieldGenerator for DerivedGenerator {
    fn generate(&self, _rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        // Try numeric binary op first.
        if let Some(binop) = parse_numeric_binop(&self.expr) {
            let left_arr = ctx.batch_columns.get(&binop.left);
            let right_arr = ctx.batch_columns.get(&binop.right);

            if left_arr.is_none() {
                tracing::warn!(
                    field = %binop.left,
                    "derived: referenced column not found, producing zeros"
                );
            }
            if right_arr.is_none() {
                tracing::warn!(
                    field = %binop.right,
                    "derived: referenced column not found, producing zeros"
                );
            }

            let left_vals = left_arr
                .map(|a| extract_f64(a, count))
                .unwrap_or_else(|| vec![0.0; count]);
            let right_vals = right_arr
                .map(|a| extract_f64(a, count))
                .unwrap_or_else(|| vec![0.0; count]);

            let values: Vec<f64> = left_vals
                .iter()
                .zip(right_vals.iter())
                .map(|(l, r)| match binop.op {
                    '+' => l + r,
                    '-' => l - r,
                    '*' => l * r,
                    '/' => {
                        if *r == 0.0 {
                            0.0
                        } else {
                            l / r
                        }
                    }
                    _ => 0.0,
                })
                .collect();
            return Arc::new(Float64Array::from(values));
        }

        // Otherwise, treat as string template with ${field} interpolation.
        // Build per-field string vectors up front.
        let mut field_strings: Vec<(&str, Vec<String>)> = Vec::new();
        for dep in &self.depends_on {
            match ctx.batch_columns.get(dep) {
                Some(arr) => field_strings.push((dep.as_str(), extract_strings(arr, count))),
                None => {
                    tracing::warn!(
                        field = %dep,
                        "derived: referenced column not found, using empty strings"
                    );
                    field_strings.push((dep.as_str(), vec![String::new(); count]));
                }
            }
        }

        let values: Vec<String> = (0..count)
            .map(|row| {
                let mut result = self.expr.clone();
                for (name, strings) in &field_strings {
                    let placeholder = format!("${{{name}}}");
                    result = result.replace(&placeholder, &strings[row]);
                }
                result
            })
            .collect();

        Arc::new(StringArray::from(
            values.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        ))
    }

    fn output_type(&self) -> DataType {
        // If it looks like a numeric binop, output Float64, otherwise Utf8.
        if parse_numeric_binop(&self.expr).is_some() {
            DataType::Float64
        } else {
            DataType::Utf8
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{ArrayRef, Int64Array};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn make_ctx_with_columns(cols: HashMap<String, ArrayRef>) -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        GenContext {
            batch_columns: map,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test",
        }
    }

    #[test]
    fn numeric_addition() {
        let mut cols = HashMap::new();
        cols.insert(
            "a".to_string(),
            Arc::new(Int64Array::from(vec![1, 2, 3])) as ArrayRef,
        );
        cols.insert(
            "b".to_string(),
            Arc::new(Int64Array::from(vec![10, 20, 30])) as ArrayRef,
        );
        let ctx = make_ctx_with_columns(cols);

        let gen = DerivedGenerator::new(
            "${a} + ${b}".into(),
            vec!["a".into(), "b".into()],
        );
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = gen.generate(&mut rng, 3, &ctx);
        let f64_arr = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        assert_eq!(f64_arr.values(), &[11.0, 22.0, 33.0]);
    }

    #[test]
    fn numeric_division_by_zero() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".to_string(),
            Arc::new(Float64Array::from(vec![10.0, 20.0])) as ArrayRef,
        );
        cols.insert(
            "y".to_string(),
            Arc::new(Float64Array::from(vec![0.0, 5.0])) as ArrayRef,
        );
        let ctx = make_ctx_with_columns(cols);

        let gen = DerivedGenerator::new(
            "${x} / ${y}".into(),
            vec!["x".into(), "y".into()],
        );
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = gen.generate(&mut rng, 2, &ctx);
        let f64_arr = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        assert_eq!(f64_arr.value(0), 0.0); // div by zero → 0
        assert_eq!(f64_arr.value(1), 4.0);
    }

    #[test]
    fn string_template() {
        let mut cols = HashMap::new();
        cols.insert(
            "first".to_string(),
            Arc::new(StringArray::from(vec!["Alice", "Bob"])) as ArrayRef,
        );
        cols.insert(
            "last".to_string(),
            Arc::new(StringArray::from(vec!["Smith", "Jones"])) as ArrayRef,
        );
        let ctx = make_ctx_with_columns(cols);

        let gen = DerivedGenerator::new(
            "${first} ${last}".into(),
            vec!["first".into(), "last".into()],
        );
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = gen.generate(&mut rng, 2, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(str_arr.value(0), "Alice Smith");
        assert_eq!(str_arr.value(1), "Bob Jones");
    }

    #[test]
    fn missing_column_does_not_panic() {
        let cols = HashMap::new();
        let ctx = make_ctx_with_columns(cols);

        let gen = DerivedGenerator::new(
            "${a} + ${b}".into(),
            vec!["a".into(), "b".into()],
        );
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = gen.generate(&mut rng, 3, &ctx);
        let f64_arr = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        assert_eq!(f64_arr.values(), &[0.0, 0.0, 0.0]);
    }
}
