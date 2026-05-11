//! Derived field generator — expression evaluator referencing sibling columns.
//!
//! Supports a full expression language with operators, functions, and field references.
//! Falls back to legacy string template mode for backward compatibility.
//!
//! # Expression mode
//!
//! Expressions are parsed at construction time into an AST, then evaluated
//! per-batch using the vectorized evaluator. Supports:
//! - Arithmetic: `${a} + ${b}`, `${price} * ${qty} * 1.1`
//! - Functions: `round(${x}, 2)`, `upper(${name})`, `if(${age} >= 18, "adult", "minor")`
//! - Comparisons: `${x} > 0 && ${y} <= 100`
//!
//! # Legacy template mode
//!
//! Expressions that cannot be parsed (e.g., `"${first} ${last}@example.com"`)
//! fall back to string template interpolation for backward compatibility.

use std::sync::Arc;

use arrow::array::{Array, ArrayRef, Float64Array, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::gen::context::GenContext;
use crate::gen::expr::ast::{self, Expr};
use crate::gen::expr::eval::{self, EvalContext};
use crate::gen::expr::parser;
use crate::gen::traits::FieldGenerator;

/// Evaluate a derived expression referencing other fields in the batch.
///
/// At construction, the expression string is parsed into an AST. If parsing
/// fails, the generator falls back to legacy string template mode.
pub struct DerivedGenerator {
    /// Original expression string (used for legacy template mode and diagnostics).
    expr: String,
    /// Parsed AST (None if expression is a legacy template).
    ast: Option<Expr>,
    /// Fields this generator depends on (extracted from AST or string heuristics).
    depends_on: Vec<String>,
    /// Stable hash of the expression, used as base seed for random functions.
    /// Combined with partition_index at generation time for per-partition isolation.
    expr_hash: u64,
}

impl DerivedGenerator {
    /// Create a new derived generator, parsing the expression at construction time.
    ///
    /// If the expression string parses as a valid expression, the AST is stored
    /// and used for vectorized evaluation. Otherwise, the generator falls back
    /// to legacy string template interpolation.
    ///
    /// Note: simple pass-through expressions like `${id}` now return the source
    /// column's native type instead of stringifying. The output layer handles
    /// any necessary type conversion.
    pub fn new(expr: String, depends_on: Vec<String>) -> Self {
        let ast = parser::parse(&expr).ok();
        let depends_on = if let Some(ref ast) = ast {
            ast::extract_field_refs(ast)
        } else {
            depends_on
        };
        // Stable hash of the expression string — used as base seed for random
        // functions so that the seed is batch-size independent.
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        expr.hash(&mut hasher);
        let expr_hash = hasher.finish();
        Self {
            expr,
            ast,
            depends_on,
            expr_hash,
        }
    }

    /// Get the list of field dependencies.
    pub fn dependencies(&self) -> &[String] {
        &self.depends_on
    }
}

/// A parsed binary numeric operation (legacy path).
struct NumericBinOp {
    left: String,
    op: char,
    right: String,
}

/// Try to parse `${a} op ${b}` where op is +, -, *, / (legacy path).
fn parse_numeric_binop(expr: &str) -> Option<NumericBinOp> {
    let trimmed = expr.trim();
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

    let trailing = rest2[close2 + 1..].trim();
    if !trailing.is_empty() {
        return None;
    }

    if left.contains('.') || right.contains('.') {
        return None;
    }

    Some(NumericBinOp { left, op, right })
}

/// Extract f64 values from an ArrayRef (supports Int64 and Float64).
fn extract_f64(arr: &ArrayRef, count: usize) -> Vec<f64> {
    if let Some(f) = arr.as_any().downcast_ref::<Float64Array>() {
        f.values().to_vec()
    } else if let Some(i) = arr.as_any().downcast_ref::<arrow::array::Int64Array>() {
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
    } else if let Some(i) = arr.as_any().downcast_ref::<arrow::array::Int64Array>() {
        i.values().iter().map(|v| v.to_string()).collect()
    } else if let Some(b) = arr.as_any().downcast_ref::<arrow::array::BooleanArray>() {
        (0..count)
            .map(|i| if b.value(i) { "true" } else { "false" }.to_string())
            .collect()
    } else {
        vec![String::new(); count]
    }
}

/// Resolve `${param.key}` placeholders in an expression using the params map.
fn resolve_params(expr: &str, params: &std::collections::HashMap<String, String>) -> String {
    if params.is_empty() || !expr.contains("${param.") {
        return expr.to_string();
    }
    let mut result = String::with_capacity(expr.len());
    let mut rest = expr;
    while let Some(start) = rest.find("${param.") {
        result.push_str(&rest[..start]);
        let after = &rest[start + 8..]; // skip "${param."
        if let Some(end) = after.find('}') {
            let key = &after[..end];
            match params.get(key) {
                Some(value) => result.push_str(value),
                None => {
                    result.push_str(&rest[start..start + 8 + end + 1]);
                }
            }
            rest = &after[end + 1..];
        } else {
            result.push_str(&rest[start..]);
            rest = "";
            break;
        }
    }
    result.push_str(rest);
    result
}

impl FieldGenerator for DerivedGenerator {
    fn generate(&self, _rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        // If we have a parsed AST, use the expression engine
        if let Some(ref ast) = self.ast {
            // Derive a stable per-partition seed for random_* functions.
            // Uses expr_hash ⊕ partition_index so that the seed is independent
            // of batch count/size, preserving batch-size determinism.
            let seed = self.expr_hash ^ (ctx.partition_index as u64);
            let eval_ctx = EvalContext {
                columns: ctx.batch_columns,
                params: ctx.params,
                row_count: count,
                row_offset: ctx.row_offset,
                seed,
                call_counter: std::cell::Cell::new(0),
            };
            match eval::evaluate(ast, &eval_ctx) {
                Ok(result) => return result,
                Err(e) => {
                    // Log at debug level — eval errors on parsed ASTs indicate
                    // unsupported features or type mismatches that the legacy
                    // path may handle differently
                    tracing::debug!(
                        expr = %self.expr,
                        error = %e,
                        "expression evaluation failed, falling back to legacy mode"
                    );
                }
            }
        }

        // Legacy path: resolve params, then try numeric binop or string template
        let expr = resolve_params(&self.expr, ctx.params);

        if let Some(binop) = parse_numeric_binop(&expr) {
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

        // String template with ${field} interpolation
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
                let mut result = expr.clone();
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
        // If we have a parsed AST, infer type from it
        if self.ast.is_some() {
            // For now, default to Float64 for expressions (most common use case).
            // The evaluator handles type-polymorphic output, and the actual
            // output type will match what the expression produces.
            DataType::Float64
        } else if parse_numeric_binop(&self.expr).is_some() {
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
        GenContext::new(map, 0, 0, 1, "test")
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

        let gen = DerivedGenerator::new("${a} + ${b}".into(), vec!["a".into(), "b".into()]);
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

        let gen = DerivedGenerator::new("${x} / ${y}".into(), vec!["x".into(), "y".into()]);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = gen.generate(&mut rng, 2, &ctx);
        let f64_arr = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        assert!(f64_arr.is_null(0)); // div by zero → null
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

        let gen = DerivedGenerator::new("${a} + ${b}".into(), vec!["a".into(), "b".into()]);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = gen.generate(&mut rng, 3, &ctx);
        let f64_arr = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        assert_eq!(f64_arr.values(), &[0.0, 0.0, 0.0]);
    }

    #[test]
    fn param_substitution_in_string_template() {
        let mut cols = HashMap::new();
        cols.insert(
            "name".to_string(),
            Arc::new(StringArray::from(vec!["Alice", "Bob"])) as ArrayRef,
        );
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(cols));
        let params: &'static HashMap<String, String> = Box::leak(Box::new(HashMap::from([(
            "prefix".to_string(),
            "Dr.".to_string(),
        )])));
        let ctx = GenContext::new(map, 0, 0, 1, "test").with_params(params);

        let gen = DerivedGenerator::new("${param.prefix} ${name}".into(), vec!["name".into()]);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = gen.generate(&mut rng, 2, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(str_arr.value(0), "Dr. Alice");
        assert_eq!(str_arr.value(1), "Dr. Bob");
    }

    #[test]
    fn param_substitution_no_params_is_noop() {
        let cols = HashMap::new();
        let ctx = make_ctx_with_columns(cols);

        let gen = DerivedGenerator::new("prefix: ${param.missing}".into(), vec![]);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = gen.generate(&mut rng, 2, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        // Unresolved param placeholder stays as-is
        assert_eq!(str_arr.value(0), "prefix: ${param.missing}");
    }

    #[test]
    fn resolve_params_unit() {
        let params = HashMap::from([
            ("env".to_string(), "prod".to_string()),
            ("version".to_string(), "2".to_string()),
        ]);
        assert_eq!(
            resolve_params("${param.env}-v${param.version}", &params),
            "prod-v2"
        );
        // No params → unchanged
        let empty = HashMap::new();
        assert_eq!(resolve_params("${param.x}", &empty), "${param.x}");
    }
}