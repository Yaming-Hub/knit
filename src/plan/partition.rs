//! Partition planning — divides entity row spaces into parallel work units.
//!
//! Each partition is a contiguous, non-overlapping range of rows that can be
//! generated independently by a single thread with its own deterministic RNG.
//! Partition boundaries depend only on entity row count and target size,
//! ensuring reproducibility regardless of available thread count.

use std::collections::{BTreeMap, HashMap};

use crate::core::{CountSpec, Value};

use crate::plan::types::PartitionRange;

/// Default target partition size: 2^20 = 1,048,576 rows.
/// Entities with fewer rows use a single partition.
const TARGET_PARTITION_SIZE: u64 = 1_048_576;

/// Resolve a [`CountSpec`] to a concrete row count for planning purposes.
///
/// - `Fixed(n)` → use `n` directly
/// - `Range { min, max }` → use `max` (plan for worst case)
/// - `Expression { expr }` → evaluate expression against model params
/// - `Distribution(spec)` → use the expected value of the distribution
pub fn resolve_count(count: &CountSpec, params: &BTreeMap<String, Value>) -> Result<u64, String> {
    match count {
        CountSpec::Fixed(n) => Ok(*n),
        CountSpec::Range { min: _, max } => Ok(*max),
        CountSpec::Expression { expr } => evaluate_count_expr(expr, params),
        CountSpec::Distribution(spec) => {
            // Use expected value based on distribution kind.
            let params = &spec.params;
            let v = match spec.kind {
                crate::core::DistributionKind::Normal => {
                    params.get("mean").copied().unwrap_or(0.0).max(0.0) as u64
                }
                crate::core::DistributionKind::Uniform => {
                    let min = params.get("min").copied().unwrap_or(0.0);
                    let max = params.get("max").copied().unwrap_or(0.0);
                    ((min + max) / 2.0).max(0.0) as u64
                }
                crate::core::DistributionKind::Poisson => {
                    params.get("lambda").copied().unwrap_or(0.0).max(0.0) as u64
                }
                crate::core::DistributionKind::Exponential => {
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
            };
            Ok(v)
        }
    }
}

/// Evaluate a count expression against model parameters.
///
/// Only param refs, literals, and pure arithmetic/math functions are allowed.
/// Field refs and random/row functions are rejected.
fn evaluate_count_expr(expr: &str, params: &BTreeMap<String, Value>) -> Result<u64, String> {
    use crate::gen::expr::{eval, parser};

    let ast = parser::parse(expr).map_err(|e| format!("count expression parse error: {}", e.message))?;

    // Validate: reject field refs, random functions, row_number
    validate_count_ast(&ast)?;

    // Convert model params to string map for the evaluator.
    let str_params: HashMap<String, String> = params
        .iter()
        .map(|(k, v)| (k.clone(), value_to_string(v)))
        .collect();

    let ctx = eval::EvalContext {
        columns: &HashMap::new(),
        params: &str_params,
        row_count: 1,
        row_offset: 0,
        seed: 0,
        call_counter: std::cell::Cell::new(0),
    };

    let result = eval::evaluate(&ast, &ctx)
        .map_err(|e| format!("count expression eval error: {}", e.message))?;

    // Extract scalar from 1-element array.
    use arrow::array::{Float64Array, Int64Array};
    if let Some(arr) = result.as_any().downcast_ref::<Int64Array>() {
        let v = arr.value(0);
        if v <= 0 {
            return Err(format!("count expression evaluated to {v}, must be > 0"));
        }
        Ok(v as u64)
    } else if let Some(arr) = result.as_any().downcast_ref::<Float64Array>() {
        let v = arr.value(0);
        if !v.is_finite() || v <= 0.0 {
            return Err(format!("count expression evaluated to {v}, must be a finite value > 0"));
        }
        Ok(v.round() as u64)
    } else {
        Err("count expression must evaluate to a numeric value".to_string())
    }
}

/// Validate that a count expression AST only contains allowed constructs.
pub fn validate_count_ast(expr: &crate::gen::expr::ast::Expr) -> Result<(), String> {
    use crate::gen::expr::ast::Expr;
    match expr {
        Expr::Literal(_) | Expr::ParamRef(_) => Ok(()),
        Expr::FieldRef(name) => {
            Err(format!("field reference '${{{name}}}' is not allowed in count expressions"))
        }
        Expr::BinaryOp { left, right, .. } => {
            validate_count_ast(left)?;
            validate_count_ast(right)
        }
        Expr::UnaryOp { operand, .. } => validate_count_ast(operand),
        Expr::FuncCall { name, args } => {
            // Reject non-deterministic / row-dependent functions.
            let forbidden = [
                "row_number", "random_int", "random_float", "random_normal",
                "random_choice", "random_string",
            ];
            if forbidden.contains(&name.as_str()) {
                return Err(format!(
                    "function '{name}' is not allowed in count expressions (must be deterministic)"
                ));
            }
            for arg in args {
                validate_count_ast(arg)?;
            }
            Ok(())
        }
    }
}

/// Convert a Value to its string representation for expression evaluation.
fn value_to_string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Int(n) => n.to_string(),
        Value::Float(f) => f.to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Null => String::new(),
        Value::Array(arr) => serde_json::to_string(arr).unwrap_or_default(),
        Value::Map(map) => serde_json::to_string(map).unwrap_or_default(),
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

    tracing::debug!(
        total_rows,
        num_partitions,
        rows_per_partition,
        partitions_with_extra_row = remainder,
        "partition plan computed"
    );

    let mut partitions = Vec::with_capacity(num_partitions as usize);
    let mut start = 0u64;

    for i in 0..num_partitions {
        let extra = if (i as u64) < remainder { 1 } else { 0 };
        let end = start + rows_per_partition + extra;
        let seed = crate::plan::rng_tree::derive_seed(entity_seed, &i.to_le_bytes());
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
        let empty = BTreeMap::new();
        assert_eq!(resolve_count(&CountSpec::Fixed(5000), &empty).unwrap(), 5000);
    }

    #[test]
    fn test_resolve_range() {
        let empty = BTreeMap::new();
        assert_eq!(resolve_count(&CountSpec::Range { min: 100, max: 500 }, &empty).unwrap(), 500);
    }

    #[test]
    fn test_resolve_expression_simple() {
        let params = BTreeMap::from([
            ("user_count".to_string(), Value::Int(500)),
        ]);
        let count = CountSpec::Expression {
            expr: "${param.user_count}".to_string(),
        };
        assert_eq!(resolve_count(&count, &params).unwrap(), 500);
    }

    #[test]
    fn test_resolve_expression_arithmetic() {
        let params = BTreeMap::from([
            ("base".to_string(), Value::Int(100)),
            ("scale".to_string(), Value::Int(5)),
        ]);
        let count = CountSpec::Expression {
            expr: "${param.base} * ${param.scale}".to_string(),
        };
        assert_eq!(resolve_count(&count, &params).unwrap(), 500);
    }

    #[test]
    fn test_resolve_expression_float_rounds() {
        let params = BTreeMap::from([
            ("x".to_string(), Value::Float(3.7)),
            ("y".to_string(), Value::Float(10.0)),
        ]);
        let count = CountSpec::Expression {
            expr: "${param.x} * ${param.y}".to_string(),
        };
        // 3.7 * 10.0 = 37.0 → 37
        assert_eq!(resolve_count(&count, &params).unwrap(), 37);
    }

    #[test]
    fn test_resolve_expression_rejects_negative() {
        let params = BTreeMap::from([
            ("x".to_string(), Value::Int(-5)),
        ]);
        let count = CountSpec::Expression {
            expr: "${param.x}".to_string(),
        };
        assert!(resolve_count(&count, &params).is_err());
    }

    #[test]
    fn test_resolve_expression_rejects_field_ref() {
        let params = BTreeMap::new();
        let count = CountSpec::Expression {
            expr: "${some_field} + 1".to_string(),
        };
        assert!(resolve_count(&count, &params).is_err());
    }

    #[test]
    fn test_resolve_expression_rejects_random() {
        let params = BTreeMap::new();
        let count = CountSpec::Expression {
            expr: "random_int(1, 100)".to_string(),
        };
        assert!(resolve_count(&count, &params).is_err());
    }

    #[test]
    fn test_resolve_expression_missing_param() {
        let params = BTreeMap::new();
        let count = CountSpec::Expression {
            expr: "${param.missing}".to_string(),
        };
        assert!(resolve_count(&count, &params).is_err());
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