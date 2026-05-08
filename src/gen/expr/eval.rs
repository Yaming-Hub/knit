//! Vectorized expression evaluator over Arrow arrays.
//!
//! Walks the expression AST and produces an [`ArrayRef`] for a batch of rows.
//! Uses per-element iteration with null-aware logic.
//!
//! # Null semantics
//!
//! - **Scalar ops**: SQL-like null propagation — if any operand is null, the result is null.
//! - **`coalesce`**: Returns the first non-null argument.
//! - **`if`**: Selects branch based on condition.
//! - **`nullif`**: Returns null if both arguments are equal.

use std::cell::Cell;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int64Array, StringArray,
    TimestampMicrosecondArray, TimestampMillisecondArray, TimestampNanosecondArray,
    TimestampSecondArray,
};
use arrow::datatypes::DataType;
use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, Timelike};
use siphasher::sip::SipHasher;

use super::ast::{BinOp, Expr, LiteralValue, UnOp};

/// Error during expression evaluation.
#[derive(Debug, Clone)]
pub struct EvalError {
    /// Error message.
    pub message: String,
}

impl std::fmt::Display for EvalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "eval error: {}", self.message)
    }
}

impl std::error::Error for EvalError {}

/// Context for evaluation — provides column data and parameters.
pub struct EvalContext<'a> {
    /// Batch columns keyed by field name.
    pub columns: &'a HashMap<String, ArrayRef>,
    /// User parameters keyed by name.
    pub params: &'a HashMap<String, String>,
    /// Number of rows in the batch.
    pub row_count: usize,
    /// Absolute row offset within the entity (cumulative across batches).
    pub row_offset: u64,
    /// Base seed for random functions. Each `random_*` call derives
    /// per-row values from `splitmix64(seed ⊕ call_id ⊕ row_index)`.
    /// Set to 0 when random functions are not needed.
    pub seed: u64,
    /// Auto-incrementing counter for random call sites within one evaluation.
    /// Ensures multiple `random_*` calls in the same expression produce
    /// independent streams.
    pub call_counter: Cell<u64>,
}

/// Evaluate an expression AST against a batch context, producing an Arrow array.
pub fn evaluate(expr: &Expr, ctx: &EvalContext<'_>) -> Result<ArrayRef, EvalError> {
    match expr {
        Expr::Literal(lit) => eval_literal(lit, ctx.row_count),
        Expr::FieldRef(name) => {
            ctx.columns.get(name).cloned().ok_or_else(|| EvalError {
                message: format!("field `{name}` not found in batch"),
            })
        }
        Expr::ParamRef(key) => {
            let value = ctx.params.get(key).ok_or_else(|| EvalError {
                message: format!("parameter `{key}` not found"),
            })?;
            if let Ok(i) = value.parse::<i64>() {
                Ok(Arc::new(Int64Array::from(vec![i; ctx.row_count])))
            } else if let Ok(f) = value.parse::<f64>() {
                Ok(Arc::new(Float64Array::from(vec![f; ctx.row_count])))
            } else {
                Ok(Arc::new(StringArray::from(
                    vec![value.as_str(); ctx.row_count],
                )))
            }
        }
        Expr::BinaryOp { left, op, right } => {
            let left_arr = evaluate(left, ctx)?;
            let right_arr = evaluate(right, ctx)?;
            eval_binary_op(&left_arr, *op, &right_arr)
        }
        Expr::UnaryOp { op, operand } => {
            let arr = evaluate(operand, ctx)?;
            eval_unary_op(*op, &arr)
        }
        Expr::FuncCall { name, args } => eval_function(name, args, ctx),
    }
}

// ─── Downcasting helpers ────────────────────────────────────────────────────

fn as_i64(arr: &ArrayRef) -> Option<&Int64Array> {
    arr.as_any().downcast_ref::<Int64Array>()
}

fn as_f64(arr: &ArrayRef) -> Option<&Float64Array> {
    arr.as_any().downcast_ref::<Float64Array>()
}

fn as_str(arr: &ArrayRef) -> Option<&StringArray> {
    arr.as_any().downcast_ref::<StringArray>()
}

fn as_bool(arr: &ArrayRef) -> Option<&BooleanArray> {
    arr.as_any().downcast_ref::<BooleanArray>()
}

/// Extract millisecond timestamps from any temporal array type.
///
/// Handles `TimestampSecondArray`, `TimestampMillisecondArray`,
/// `TimestampMicrosecondArray`, `TimestampNanosecondArray`, and `Int64Array`.
fn as_millis(arr: &ArrayRef) -> Option<Vec<Option<i64>>> {
    let len = arr.len();
    if let Some(a) = arr.as_any().downcast_ref::<TimestampMillisecondArray>() {
        Some((0..len).map(|i| if a.is_null(i) { None } else { Some(a.value(i)) }).collect())
    } else if let Some(a) = arr.as_any().downcast_ref::<TimestampSecondArray>() {
        Some((0..len).map(|i| if a.is_null(i) { None } else { Some(a.value(i) * 1_000) }).collect())
    } else if let Some(a) = arr.as_any().downcast_ref::<TimestampMicrosecondArray>() {
        Some((0..len).map(|i| if a.is_null(i) { None } else { Some(a.value(i).div_euclid(1_000)) }).collect())
    } else if let Some(a) = arr.as_any().downcast_ref::<TimestampNanosecondArray>() {
        Some((0..len).map(|i| if a.is_null(i) { None } else { Some(a.value(i).div_euclid(1_000_000)) }).collect())
    } else {
        as_i64(arr).map(|a| {
            (0..len).map(|i| if a.is_null(i) { None } else { Some(a.value(i)) }).collect()
        })
    }
}

/// Extract millis or return an error.
fn require_millis(arr: &ArrayRef, fname: &str) -> Result<Vec<Option<i64>>, EvalError> {
    as_millis(arr).ok_or_else(|| EvalError {
        message: format!("{fname}: expected timestamp or integer, got {:?}", arr.data_type()),
    })
}

fn require_bool(arr: &ArrayRef) -> Result<&BooleanArray, EvalError> {
    as_bool(arr).ok_or_else(|| EvalError {
        message: format!("expected Boolean, got {:?}", arr.data_type()),
    })
}

// ─── Literal ────────────────────────────────────────────────────────────────

fn eval_literal(lit: &LiteralValue, count: usize) -> Result<ArrayRef, EvalError> {
    Ok(match lit {
        LiteralValue::Int(v) => Arc::new(Int64Array::from(vec![*v; count])),
        LiteralValue::Float(v) => Arc::new(Float64Array::from(vec![*v; count])),
        LiteralValue::Str(v) => Arc::new(StringArray::from(vec![v.as_str(); count])),
        LiteralValue::Bool(v) => Arc::new(BooleanArray::from(vec![*v; count])),
        // Null uses a NullArray that signals "untyped null" — downstream functions
        // (if, coalesce, nullif) must handle this by coercing to the peer type.
        LiteralValue::Null => Arc::new(arrow::array::NullArray::new(count)),
    })
}

// ─── Numeric coercion ───────────────────────────────────────────────────────

fn is_null_array(arr: &ArrayRef) -> bool {
    arr.data_type() == &DataType::Null
}

/// Create a typed all-null array matching the given data type.
fn typed_nulls(dt: &DataType, count: usize) -> ArrayRef {
    match dt {
        DataType::Int64 => Arc::new(Int64Array::from(vec![None::<i64>; count])),
        DataType::Float64 => Arc::new(Float64Array::from(vec![None::<f64>; count])),
        DataType::Utf8 => Arc::new(StringArray::from(vec![None::<&str>; count])),
        DataType::Boolean => Arc::new(BooleanArray::from(vec![None::<bool>; count])),
        _ => Arc::new(arrow::array::NullArray::new(count)),
    }
}

/// Extract f64 values from an array, supporting Int64, Float64, timestamps, and Null.
fn to_f64_vec(arr: &ArrayRef) -> Result<Vec<Option<f64>>, EvalError> {
    if is_null_array(arr) {
        return Ok(vec![None; arr.len()]);
    }
    if let Some(fa) = as_f64(arr) {
        Ok((0..fa.len())
            .map(|i| if fa.is_null(i) { None } else { Some(fa.value(i)) })
            .collect())
    } else if let Some(ia) = as_i64(arr) {
        Ok((0..ia.len())
            .map(|i| {
                if ia.is_null(i) {
                    None
                } else {
                    Some(ia.value(i) as f64)
                }
            })
            .collect())
    } else if let Some(millis) = as_millis(arr) {
        Ok(millis.into_iter().map(|v| v.map(|m| m as f64)).collect())
    } else {
        Err(EvalError {
            message: format!("cannot convert {:?} to Float64", arr.data_type()),
        })
    }
}

fn is_numeric(dt: &DataType) -> bool {
    matches!(dt, DataType::Int64 | DataType::Float64)
}

// ─── Binary ops ─────────────────────────────────────────────────────────────

fn eval_binary_op(left: &ArrayRef, op: BinOp, right: &ArrayRef) -> Result<ArrayRef, EvalError> {
    match op {
        BinOp::Add => eval_arith(left, right, op),
        BinOp::Sub | BinOp::Mul | BinOp::Div => eval_arith(left, right, op),
        BinOp::Mod => eval_mod(left, right),
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => {
            eval_cmp(left, right, op)
        }
        BinOp::And => {
            let l = require_bool(left)?;
            let r = require_bool(right)?;
            // SQL three-valued logic: false AND null = false
            let result: BooleanArray = (0..l.len())
                .map(|i| match (null_safe_bool(l, i), null_safe_bool(r, i)) {
                    (Some(a), Some(b)) => Some(a && b),
                    (Some(false), _) | (_, Some(false)) => Some(false),
                    _ => None,
                })
                .collect();
            Ok(Arc::new(result))
        }
        BinOp::Or => {
            let l = require_bool(left)?;
            let r = require_bool(right)?;
            // SQL three-valued logic: true OR null = true
            let result: BooleanArray = (0..l.len())
                .map(|i| match (null_safe_bool(l, i), null_safe_bool(r, i)) {
                    (Some(a), Some(b)) => Some(a || b),
                    (Some(true), _) | (_, Some(true)) => Some(true),
                    _ => None,
                })
                .collect();
            Ok(Arc::new(result))
        }
    }
}

fn null_safe_bool(arr: &BooleanArray, i: usize) -> Option<bool> {
    if arr.is_null(i) {
        None
    } else {
        Some(arr.value(i))
    }
}

fn eval_arith(left: &ArrayRef, right: &ArrayRef, op: BinOp) -> Result<ArrayRef, EvalError> {
    // NullArray + anything → all null
    if is_null_array(left) || is_null_array(right) {
        return Ok(Arc::new(Float64Array::from(vec![None::<f64>; left.len()])));
    }

    // String concatenation via +
    if op == BinOp::Add && left.data_type() == &DataType::Utf8 && right.data_type() == &DataType::Utf8
    {
        let l = as_str(left).unwrap();
        let r = as_str(right).unwrap();
        let result: StringArray = (0..l.len())
            .map(|i| {
                if l.is_null(i) || r.is_null(i) {
                    None
                } else {
                    Some(format!("{}{}", l.value(i), r.value(i)))
                }
            })
            .collect();
        return Ok(Arc::new(result));
    }

    let lv = to_f64_vec(left)?;
    let rv = to_f64_vec(right)?;
    let result: Float64Array = lv
        .iter()
        .zip(rv.iter())
        .map(|(a, b)| match (a, b) {
            (Some(a), Some(b)) => Some(match op {
                BinOp::Add => a + b,
                BinOp::Sub => a - b,
                BinOp::Mul => a * b,
                BinOp::Div => {
                    if *b == 0.0 {
                        return None; // SQL-like: div by zero → null
                    }
                    a / b
                }
                _ => unreachable!(),
            }),
            _ => None,
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_mod(left: &ArrayRef, right: &ArrayRef) -> Result<ArrayRef, EvalError> {
    if left.data_type() == &DataType::Int64 && right.data_type() == &DataType::Int64 {
        let la = as_i64(left).unwrap();
        let ra = as_i64(right).unwrap();
        let result: Int64Array = (0..la.len())
            .map(|i| {
                if la.is_null(i) || ra.is_null(i) {
                    None
                } else {
                    let b = ra.value(i);
                    if b == 0 {
                        None
                    } else {
                        Some(la.value(i) % b)
                    }
                }
            })
            .collect();
        return Ok(Arc::new(result));
    }
    let lv = to_f64_vec(left)?;
    let rv = to_f64_vec(right)?;
    let result: Float64Array = lv
        .iter()
        .zip(rv.iter())
        .map(|(a, b)| match (a, b) {
            (Some(a), Some(b)) if *b != 0.0 => Some(a % b),
            _ => None,
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_cmp(left: &ArrayRef, right: &ArrayRef, op: BinOp) -> Result<ArrayRef, EvalError> {
    // NullArray compared with anything → all null
    if is_null_array(left) || is_null_array(right) {
        return Ok(Arc::new(BooleanArray::from(vec![None::<bool>; left.len()])));
    }

    if is_numeric(left.data_type()) && is_numeric(right.data_type()) {
        let lv = to_f64_vec(left)?;
        let rv = to_f64_vec(right)?;
        let result: BooleanArray = lv
            .iter()
            .zip(rv.iter())
            .map(|(a, b)| match (a, b) {
                (Some(a), Some(b)) => Some(match op {
                    BinOp::Eq => a == b,
                    BinOp::Ne => a != b,
                    BinOp::Lt => a < b,
                    BinOp::Gt => a > b,
                    BinOp::Le => a <= b,
                    BinOp::Ge => a >= b,
                    _ => unreachable!(),
                }),
                _ => None,
            })
            .collect();
        return Ok(Arc::new(result));
    }

    if left.data_type() == &DataType::Utf8 && right.data_type() == &DataType::Utf8 {
        let l = as_str(left).unwrap();
        let r = as_str(right).unwrap();
        let result: BooleanArray = (0..l.len())
            .map(|i| {
                if l.is_null(i) || r.is_null(i) {
                    None
                } else {
                    let a = l.value(i);
                    let b = r.value(i);
                    Some(match op {
                        BinOp::Eq => a == b,
                        BinOp::Ne => a != b,
                        BinOp::Lt => a < b,
                        BinOp::Gt => a > b,
                        BinOp::Le => a <= b,
                        BinOp::Ge => a >= b,
                        _ => unreachable!(),
                    })
                }
            })
            .collect();
        return Ok(Arc::new(result));
    }

    // Boolean comparison (equality only)
    if left.data_type() == &DataType::Boolean && right.data_type() == &DataType::Boolean {
        let l = as_bool(left).unwrap();
        let r = as_bool(right).unwrap();
        let result: BooleanArray = (0..l.len())
            .map(|i| {
                if l.is_null(i) || r.is_null(i) {
                    None
                } else {
                    let a = l.value(i);
                    let b = r.value(i);
                    Some(match op {
                        BinOp::Eq => a == b,
                        BinOp::Ne => a != b,
                        _ => return None,
                    })
                }
            })
            .collect();
        return Ok(Arc::new(result));
    }

    Err(EvalError {
        message: format!(
            "cannot compare {:?} and {:?}",
            left.data_type(),
            right.data_type()
        ),
    })
}

// ─── Unary ops ──────────────────────────────────────────────────────────────

fn eval_unary_op(op: UnOp, arr: &ArrayRef) -> Result<ArrayRef, EvalError> {
    match op {
        UnOp::Neg => {
            if let Some(ia) = as_i64(arr) {
                let result: Int64Array =
                    ia.iter().map(|v| v.map(|i| -i)).collect();
                Ok(Arc::new(result))
            } else if let Some(fa) = as_f64(arr) {
                let result: Float64Array =
                    fa.iter().map(|v| v.map(|f| -f)).collect();
                Ok(Arc::new(result))
            } else {
                Err(EvalError {
                    message: format!("cannot negate {:?}", arr.data_type()),
                })
            }
        }
        UnOp::Not => {
            let b = require_bool(arr)?;
            let result: BooleanArray = (0..b.len())
                .map(|i| {
                    if b.is_null(i) {
                        None
                    } else {
                        Some(!b.value(i))
                    }
                })
                .collect();
            Ok(Arc::new(result))
        }
    }
}

// ─── Functions ──────────────────────────────────────────────────────────────

fn eval_function(name: &str, args: &[Expr], ctx: &EvalContext<'_>) -> Result<ArrayRef, EvalError> {
    match name {
        "abs" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_abs(&arr)
        }
        "ceil" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_f64_map(&arr, "ceil", f64::ceil)
        }
        "floor" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_f64_map(&arr, "floor", f64::floor)
        }
        "round" => {
            check_args(name, args, 1, 2)?;
            let arr = evaluate(&args[0], ctx)?;
            let decimals = if args.len() > 1 {
                match &args[1] {
                    Expr::Literal(LiteralValue::Int(d)) => *d as i32,
                    _ => {
                        let d = evaluate(&args[1], ctx)?;
                        as_i64(&d)
                            .ok_or_else(|| EvalError {
                                message: "round: decimals must be integer".into(),
                            })?
                            .value(0) as i32
                    }
                }
            } else {
                0
            };
            eval_round(&arr, decimals)
        }
        "min" => {
            check_args(name, args, 2, 2)?;
            let a = evaluate(&args[0], ctx)?;
            let b = evaluate(&args[1], ctx)?;
            eval_min_max(&a, &b, true)
        }
        "max" => {
            check_args(name, args, 2, 2)?;
            let a = evaluate(&args[0], ctx)?;
            let b = evaluate(&args[1], ctx)?;
            eval_min_max(&a, &b, false)
        }
        "clamp" => {
            check_args(name, args, 3, 3)?;
            let val = evaluate(&args[0], ctx)?;
            let lo = evaluate(&args[1], ctx)?;
            let hi = evaluate(&args[2], ctx)?;
            let clamped = eval_min_max(&val, &hi, true)?;
            eval_min_max(&clamped, &lo, false)
        }
        "upper" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_string_map(&arr, "upper", |s| s.to_uppercase())
        }
        "lower" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_string_map(&arr, "lower", |s| s.to_lowercase())
        }
        "trim" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_string_map(&arr, "trim", |s| s.trim().to_string())
        }
        "len" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            let sa = as_str(&arr).ok_or_else(|| EvalError {
                message: "len: expected string".into(),
            })?;
            let result: Int64Array = (0..sa.len())
                .map(|i| {
                    if sa.is_null(i) {
                        None
                    } else {
                        Some(sa.value(i).chars().count() as i64)
                    }
                })
                .collect();
            Ok(Arc::new(result))
        }
        "concat" => {
            check_args(name, args, 2, 16)?;
            let arrays: Vec<ArrayRef> = args
                .iter()
                .map(|a| evaluate(a, ctx))
                .collect::<Result<_, _>>()?;
            eval_concat(&arrays, ctx.row_count)
        }
        "substr" => {
            check_args(name, args, 2, 3)?;
            let arr = evaluate(&args[0], ctx)?;
            let start = evaluate(&args[1], ctx)?;
            let length = if args.len() > 2 {
                Some(evaluate(&args[2], ctx)?)
            } else {
                None
            };
            eval_substr(&arr, &start, length.as_ref())
        }
        "replace" => {
            check_args(name, args, 3, 3)?;
            let arr = evaluate(&args[0], ctx)?;
            let from = evaluate(&args[1], ctx)?;
            let to = evaluate(&args[2], ctx)?;
            eval_replace(&arr, &from, &to)
        }
        "cast_int" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_cast_int(&arr)
        }
        "cast_float" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            let v = to_f64_vec(&arr)?;
            let result: Float64Array = v.into_iter().collect();
            Ok(Arc::new(result))
        }
        "cast_string" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_cast_string(&arr)
        }
        "if" => {
            check_args(name, args, 3, 3)?;
            let cond = evaluate(&args[0], ctx)?;
            let then = evaluate(&args[1], ctx)?;
            let otherwise = evaluate(&args[2], ctx)?;
            eval_if(&cond, &then, &otherwise)
        }
        "coalesce" => {
            check_args(name, args, 2, 16)?;
            let arrays: Vec<ArrayRef> = args
                .iter()
                .map(|a| evaluate(a, ctx))
                .collect::<Result<_, _>>()?;
            eval_coalesce(&arrays)
        }
        "nullif" => {
            check_args(name, args, 2, 2)?;
            let a = evaluate(&args[0], ctx)?;
            let b = evaluate(&args[1], ctx)?;
            eval_nullif(&a, &b)
        }
        // Phase 2: Math functions
        "sqrt" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_f64_map_checked(&arr, "sqrt", |x| {
                if x < 0.0 { None } else { Some(x.sqrt()) }
            })
        }
        "pow" => {
            check_args(name, args, 2, 2)?;
            let base = evaluate(&args[0], ctx)?;
            let exp = evaluate(&args[1], ctx)?;
            eval_f64_map2(&base, &exp, "pow", |b, e| {
                let result = b.powf(e);
                if result.is_finite() { Some(result) } else { None }
            })
        }
        "log" => {
            check_args(name, args, 2, 2)?;
            let val = evaluate(&args[0], ctx)?;
            let base = evaluate(&args[1], ctx)?;
            eval_f64_map2(&val, &base, "log", |v, b| {
                if v <= 0.0 || b <= 0.0 || (b - 1.0).abs() < f64::EPSILON {
                    None
                } else {
                    let result = v.log(b);
                    if result.is_finite() { Some(result) } else { None }
                }
            })
        }
        "ln" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_f64_map_checked(&arr, "ln", |x| {
                if x <= 0.0 { None } else { Some(x.ln()) }
            })
        }
        "exp" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_f64_map_checked(&arr, "exp", |x| {
                let result = x.exp();
                if result.is_finite() { Some(result) } else { None }
            })
        }
        // Phase 2: String functions
        "left" => {
            check_args(name, args, 2, 2)?;
            let arr = evaluate(&args[0], ctx)?;
            let n = evaluate(&args[1], ctx)?;
            eval_left_right(&arr, &n, true)
        }
        "right" => {
            check_args(name, args, 2, 2)?;
            let arr = evaluate(&args[0], ctx)?;
            let n = evaluate(&args[1], ctx)?;
            eval_left_right(&arr, &n, false)
        }
        "pad_left" => {
            check_args(name, args, 3, 3)?;
            let arr = evaluate(&args[0], ctx)?;
            let len = evaluate(&args[1], ctx)?;
            let fill = evaluate(&args[2], ctx)?;
            eval_pad(&arr, &len, &fill, true)
        }
        "pad_right" => {
            check_args(name, args, 3, 3)?;
            let arr = evaluate(&args[0], ctx)?;
            let len = evaluate(&args[1], ctx)?;
            let fill = evaluate(&args[2], ctx)?;
            eval_pad(&arr, &len, &fill, false)
        }
        "starts_with" => {
            check_args(name, args, 2, 2)?;
            let arr = evaluate(&args[0], ctx)?;
            let prefix = evaluate(&args[1], ctx)?;
            eval_string_predicate(&arr, &prefix, "starts_with", |s, p| s.starts_with(p))
        }
        "ends_with" => {
            check_args(name, args, 2, 2)?;
            let arr = evaluate(&args[0], ctx)?;
            let suffix = evaluate(&args[1], ctx)?;
            eval_string_predicate(&arr, &suffix, "ends_with", |s, p| s.ends_with(p))
        }
        "contains" => {
            check_args(name, args, 2, 2)?;
            let arr = evaluate(&args[0], ctx)?;
            let needle = evaluate(&args[1], ctx)?;
            eval_string_predicate(&arr, &needle, "contains", |s, p| s.contains(p))
        }
        // Phase 2: Hash and row numbering
        "hash" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_hash(&arr)
        }
        "row_number" => {
            check_args(name, args, 0, 0)?;
            let result: Int64Array = (0..ctx.row_count as i64)
                .map(|i| Some(ctx.row_offset as i64 + i))
                .collect();
            Ok(Arc::new(result))
        }
        "case" => {
            check_args(name, args, 2, 32)?;
            eval_case(args, ctx)
        }
        // ─── Date/time construction ────────────────────────────────────
        "make_date" => {
            check_args(name, args, 3, 3)?;
            let y = evaluate(&args[0], ctx)?;
            let m = evaluate(&args[1], ctx)?;
            let d = evaluate(&args[2], ctx)?;
            eval_make_date(&y, &m, &d)
        }
        "make_time" => {
            check_args(name, args, 3, 3)?;
            let h = evaluate(&args[0], ctx)?;
            let m = evaluate(&args[1], ctx)?;
            let s = evaluate(&args[2], ctx)?;
            eval_make_time(&h, &m, &s)
        }
        "make_datetime" => {
            check_args(name, args, 6, 6)?;
            let y = evaluate(&args[0], ctx)?;
            let mo = evaluate(&args[1], ctx)?;
            let d = evaluate(&args[2], ctx)?;
            let h = evaluate(&args[3], ctx)?;
            let mi = evaluate(&args[4], ctx)?;
            let s = evaluate(&args[5], ctx)?;
            eval_make_datetime(&y, &mo, &d, &h, &mi, &s)
        }
        "make_duration" => {
            check_args(name, args, 2, 2)?;
            let n = evaluate(&args[0], ctx)?;
            let unit = evaluate(&args[1], ctx)?;
            eval_make_duration(&n, &unit)
        }
        "to_date" => {
            check_args(name, args, 2, 2)?;
            let s = evaluate(&args[0], ctx)?;
            let fmt = evaluate(&args[1], ctx)?;
            eval_to_date(&s, &fmt)
        }
        "to_datetime" => {
            check_args(name, args, 2, 2)?;
            let s = evaluate(&args[0], ctx)?;
            let fmt = evaluate(&args[1], ctx)?;
            eval_to_datetime(&s, &fmt)
        }
        "epoch_seconds" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_epoch_seconds(&arr)
        }
        "from_epoch" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_from_epoch(&arr)
        }
        // ─── Date/time extraction ──────────────────────────────────────
        "year" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_date_extract(&arr, "year", |dt| dt.year() as i64)
        }
        "month" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_date_extract(&arr, "month", |dt| dt.month() as i64)
        }
        "day" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_date_extract(&arr, "day", |dt| dt.day() as i64)
        }
        "hour" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_date_extract(&arr, "hour", |dt| dt.hour() as i64)
        }
        "minute" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_date_extract(&arr, "minute", |dt| dt.minute() as i64)
        }
        "second" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_date_extract(&arr, "second", |dt| dt.second() as i64)
        }
        "day_of_week" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_date_extract(&arr, "day_of_week", |dt| {
                dt.weekday().num_days_from_monday() as i64
            })
        }
        "day_of_year" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_date_extract(&arr, "day_of_year", |dt| dt.ordinal() as i64)
        }
        "week_of_year" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_date_extract(&arr, "week_of_year", |dt| dt.iso_week().week() as i64)
        }
        "quarter" => {
            check_args(name, args, 1, 1)?;
            let arr = evaluate(&args[0], ctx)?;
            eval_date_extract(&arr, "quarter", |dt| ((dt.month() - 1) / 3 + 1) as i64)
        }
        // ─── Date/time arithmetic ──────────────────────────────────────
        "date_add" => {
            check_args(name, args, 3, 3)?;
            let d = evaluate(&args[0], ctx)?;
            let n = evaluate(&args[1], ctx)?;
            let unit = evaluate(&args[2], ctx)?;
            eval_date_add_sub(&d, &n, &unit, true)
        }
        "date_sub" => {
            check_args(name, args, 3, 3)?;
            let d = evaluate(&args[0], ctx)?;
            let n = evaluate(&args[1], ctx)?;
            let unit = evaluate(&args[2], ctx)?;
            eval_date_add_sub(&d, &n, &unit, false)
        }
        "date_diff" => {
            check_args(name, args, 3, 3)?;
            let d1 = evaluate(&args[0], ctx)?;
            let d2 = evaluate(&args[1], ctx)?;
            let unit = evaluate(&args[2], ctx)?;
            eval_date_diff(&d1, &d2, &unit)
        }
        "duration_add" => {
            check_args(name, args, 2, 2)?;
            let d = evaluate(&args[0], ctx)?;
            let dur = evaluate(&args[1], ctx)?;
            eval_duration_add(&d, &dur)
        }
        "start_of" => {
            check_args(name, args, 2, 2)?;
            let d = evaluate(&args[0], ctx)?;
            let unit = evaluate(&args[1], ctx)?;
            eval_start_of(&d, &unit)
        }
        "end_of" => {
            check_args(name, args, 2, 2)?;
            let d = evaluate(&args[0], ctx)?;
            let unit = evaluate(&args[1], ctx)?;
            eval_end_of(&d, &unit)
        }
        // ─── Date/time formatting ──────────────────────────────────────
        "format_date" => {
            check_args(name, args, 2, 2)?;
            let d = evaluate(&args[0], ctx)?;
            let fmt = evaluate(&args[1], ctx)?;
            eval_format_date(&d, &fmt)
        }
        "format_duration" => {
            check_args(name, args, 2, 2)?;
            let d = evaluate(&args[0], ctx)?;
            let style = evaluate(&args[1], ctx)?;
            eval_format_duration(&d, &style)
        }
        // ─── Random functions ──────────────────────────────────────────
        "random_int" => {
            check_args(name, args, 2, 2)?;
            let min_arr = evaluate(&args[0], ctx)?;
            let max_arr = evaluate(&args[1], ctx)?;
            eval_random_int(&min_arr, &max_arr, ctx)
        }
        "random_float" => {
            check_args(name, args, 2, 2)?;
            let min_arr = evaluate(&args[0], ctx)?;
            let max_arr = evaluate(&args[1], ctx)?;
            eval_random_float(&min_arr, &max_arr, ctx)
        }
        "random_duration" => {
            check_args(name, args, 2, 2)?;
            let min_arr = evaluate(&args[0], ctx)?;
            let max_arr = evaluate(&args[1], ctx)?;
            eval_random_duration(&min_arr, &max_arr, ctx)
        }
        _ => Err(EvalError {
            message: format!("unknown function: `{name}`"),
        }),
    }
}

fn check_args(name: &str, args: &[Expr], min: usize, max: usize) -> Result<(), EvalError> {
    if args.len() < min || args.len() > max {
        Err(EvalError {
            message: format!(
                "`{name}` expects {min}..={max} arguments, got {}",
                args.len()
            ),
        })
    } else {
        Ok(())
    }
}

// ─── Math helpers ───────────────────────────────────────────────────────────

fn eval_abs(arr: &ArrayRef) -> Result<ArrayRef, EvalError> {
    if let Some(ia) = as_i64(arr) {
        let result: Int64Array = ia.iter().map(|v| v.map(|i| i.abs())).collect();
        Ok(Arc::new(result))
    } else if let Some(fa) = as_f64(arr) {
        let result: Float64Array = fa.iter().map(|v| v.map(|f| f.abs())).collect();
        Ok(Arc::new(result))
    } else {
        Err(EvalError {
            message: format!("abs: unsupported type {:?}", arr.data_type()),
        })
    }
}

fn eval_f64_map(
    arr: &ArrayRef,
    name: &str,
    f: fn(f64) -> f64,
) -> Result<ArrayRef, EvalError> {
    let v = to_f64_vec(arr).map_err(|e| EvalError {
        message: format!("{name}: {e}"),
    })?;
    let result: Float64Array = v.into_iter().map(|opt| opt.map(f)).collect();
    Ok(Arc::new(result))
}

/// Like `eval_f64_map` but the function returns `Option<f64>` for domain errors.
fn eval_f64_map_checked(
    arr: &ArrayRef,
    name: &str,
    f: impl Fn(f64) -> Option<f64>,
) -> Result<ArrayRef, EvalError> {
    let v = to_f64_vec(arr).map_err(|e| EvalError {
        message: format!("{name}: {e}"),
    })?;
    let result: Float64Array = v
        .into_iter()
        .map(|opt| opt.and_then(&f))
        .collect();
    Ok(Arc::new(result))
}

/// Two-argument Float64 operation with domain error handling.
fn eval_f64_map2(
    a: &ArrayRef,
    b: &ArrayRef,
    name: &str,
    f: impl Fn(f64, f64) -> Option<f64>,
) -> Result<ArrayRef, EvalError> {
    let av = to_f64_vec(a).map_err(|e| EvalError {
        message: format!("{name}: {e}"),
    })?;
    let bv = to_f64_vec(b).map_err(|e| EvalError {
        message: format!("{name}: {e}"),
    })?;
    let result: Float64Array = av
        .iter()
        .zip(bv.iter())
        .map(|(a, b)| match (a, b) {
            (Some(a), Some(b)) => f(*a, *b),
            _ => None,
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_round(arr: &ArrayRef, decimals: i32) -> Result<ArrayRef, EvalError> {
    let v = to_f64_vec(arr)?;
    let factor = 10f64.powi(decimals);
    let result: Float64Array = v
        .into_iter()
        .map(|opt| opt.map(|f| (f * factor).round() / factor))
        .collect();
    Ok(Arc::new(result))
}

fn eval_min_max(a: &ArrayRef, b: &ArrayRef, is_min: bool) -> Result<ArrayRef, EvalError> {
    let av = to_f64_vec(a)?;
    let bv = to_f64_vec(b)?;
    let result: Float64Array = av
        .iter()
        .zip(bv.iter())
        .map(|(a, b)| match (a, b) {
            (Some(a), Some(b)) => Some(if is_min { a.min(*b) } else { a.max(*b) }),
            _ => None,
        })
        .collect();
    Ok(Arc::new(result))
}

// ─── String helpers ─────────────────────────────────────────────────────────

fn eval_string_map(
    arr: &ArrayRef,
    name: &str,
    f: fn(&str) -> String,
) -> Result<ArrayRef, EvalError> {
    let sa = as_str(arr).ok_or_else(|| EvalError {
        message: format!("{name}: expected string"),
    })?;
    let result: StringArray = (0..sa.len())
        .map(|i| {
            if sa.is_null(i) {
                None
            } else {
                Some(f(sa.value(i)))
            }
        })
        .collect();
    Ok(Arc::new(result))
}

fn array_to_strings(arr: &ArrayRef) -> Result<Vec<Option<String>>, EvalError> {
    let len = arr.len();
    if is_null_array(arr) {
        return Ok(vec![None; len]);
    }
    if let Some(sa) = as_str(arr) {
        Ok((0..len)
            .map(|i| {
                if sa.is_null(i) {
                    None
                } else {
                    Some(sa.value(i).to_string())
                }
            })
            .collect())
    } else if let Some(ia) = as_i64(arr) {
        Ok(ia.iter().map(|v| v.map(|i| i.to_string())).collect())
    } else if let Some(fa) = as_f64(arr) {
        Ok(fa.iter().map(|v| v.map(|f| f.to_string())).collect())
    } else if let Some(ba) = as_bool(arr) {
        Ok((0..len)
            .map(|i| {
                if ba.is_null(i) {
                    None
                } else {
                    Some(ba.value(i).to_string())
                }
            })
            .collect())
    } else {
        Err(EvalError {
            message: format!("cannot convert {:?} to strings", arr.data_type()),
        })
    }
}

fn eval_concat(arrays: &[ArrayRef], count: usize) -> Result<ArrayRef, EvalError> {
    let string_arrays: Vec<Vec<Option<String>>> = arrays
        .iter()
        .map(array_to_strings)
        .collect::<Result<_, _>>()?;

    let result: StringArray = (0..count)
        .map(|i| {
            let mut s = String::new();
            for arr in &string_arrays {
                match &arr[i] {
                    Some(v) => s.push_str(v),
                    None => return None,
                }
            }
            Some(s)
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_substr(
    arr: &ArrayRef,
    start: &ArrayRef,
    length: Option<&ArrayRef>,
) -> Result<ArrayRef, EvalError> {
    let sa = as_str(arr).ok_or_else(|| EvalError {
        message: "substr: expected string".into(),
    })?;
    let starts = as_i64(start).ok_or_else(|| EvalError {
        message: "substr: start must be integer".into(),
    })?;

    let count = sa.len();
    let result: StringArray = (0..count)
        .map(|i| {
            if sa.is_null(i) || starts.is_null(i) {
                return None;
            }
            let s = sa.value(i);
            let st = starts.value(i).max(0) as usize;
            // Use character indices for UTF-8 safety
            let chars: Vec<char> = s.chars().collect();
            if st >= chars.len() {
                return Some(String::new());
            }
            if let Some(len_arr) = length {
                let la = as_i64(len_arr)?;
                if la.is_null(i) {
                    return None;
                }
                let l = la.value(i).max(0) as usize;
                let end = (st + l).min(chars.len());
                Some(chars[st..end].iter().collect())
            } else {
                Some(chars[st..].iter().collect())
            }
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_replace(arr: &ArrayRef, from: &ArrayRef, to: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let sa = as_str(arr).ok_or_else(|| EvalError {
        message: "replace: expected string".into(),
    })?;
    let fa = as_str(from).ok_or_else(|| EvalError {
        message: "replace: from must be string".into(),
    })?;
    let ta = as_str(to).ok_or_else(|| EvalError {
        message: "replace: to must be string".into(),
    })?;

    let result: StringArray = (0..sa.len())
        .map(|i| {
            if sa.is_null(i) || fa.is_null(i) || ta.is_null(i) {
                None
            } else {
                Some(sa.value(i).replace(fa.value(i), ta.value(i)))
            }
        })
        .collect();
    Ok(Arc::new(result))
}

// ─── Cast helpers ───────────────────────────────────────────────────────────

fn eval_cast_int(arr: &ArrayRef) -> Result<ArrayRef, EvalError> {
    if let Some(ia) = as_i64(arr) {
        return Ok(Arc::new(ia.clone()));
    }
    if let Some(fa) = as_f64(arr) {
        let result: Int64Array = fa.iter().map(|v| v.map(|f| f as i64)).collect();
        return Ok(Arc::new(result));
    }
    if let Some(sa) = as_str(arr) {
        let result: Int64Array = (0..sa.len())
            .map(|i| {
                if sa.is_null(i) {
                    None
                } else {
                    sa.value(i).parse::<i64>().ok()
                }
            })
            .collect();
        return Ok(Arc::new(result));
    }
    if let Some(ba) = as_bool(arr) {
        let result: Int64Array = (0..ba.len())
            .map(|i| {
                if ba.is_null(i) {
                    None
                } else {
                    Some(ba.value(i) as i64)
                }
            })
            .collect();
        return Ok(Arc::new(result));
    }
    Err(EvalError {
        message: format!("cannot cast {:?} to Int64", arr.data_type()),
    })
}

fn eval_cast_string(arr: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let strings = array_to_strings(arr)?;
    let result: StringArray = strings.iter().map(|v| v.as_deref()).collect();
    Ok(Arc::new(result))
}

// ─── Phase 2 string helpers ────────────────────────────────────────────────

fn eval_left_right(arr: &ArrayRef, n: &ArrayRef, is_left: bool) -> Result<ArrayRef, EvalError> {
    let sa = as_str(arr).ok_or_else(|| EvalError {
        message: format!("{}: expected string", if is_left { "left" } else { "right" }),
    })?;
    let na = as_i64(n).ok_or_else(|| EvalError {
        message: format!("{}: n must be integer", if is_left { "left" } else { "right" }),
    })?;
    let result: StringArray = (0..sa.len())
        .map(|i| {
            if sa.is_null(i) || na.is_null(i) {
                return None;
            }
            let s = sa.value(i);
            let count = na.value(i).max(0) as usize;
            let chars: Vec<char> = s.chars().collect();
            if is_left {
                Some(chars.iter().take(count).collect::<String>())
            } else {
                let skip = chars.len().saturating_sub(count);
                Some(chars.iter().skip(skip).collect::<String>())
            }
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_pad(
    arr: &ArrayRef,
    len: &ArrayRef,
    fill: &ArrayRef,
    is_left: bool,
) -> Result<ArrayRef, EvalError> {
    let fname = if is_left { "pad_left" } else { "pad_right" };
    let sa = as_str(arr).ok_or_else(|| EvalError {
        message: format!("{fname}: expected string"),
    })?;
    let la = as_i64(len).ok_or_else(|| EvalError {
        message: format!("{fname}: length must be integer"),
    })?;
    let fa = as_str(fill).ok_or_else(|| EvalError {
        message: format!("{fname}: fill must be string"),
    })?;
    let result: StringArray = (0..sa.len())
        .map(|i| {
            if sa.is_null(i) || la.is_null(i) || fa.is_null(i) {
                return None;
            }
            let s = sa.value(i);
            let target_len = la.value(i).max(0) as usize;
            let fill_str = fa.value(i);
            let fill_char = match fill_str.chars().next() {
                Some(c) => c,
                None => return Some(s.to_string()),
            };
            let char_count = s.chars().count();
            if char_count >= target_len {
                return Some(s.to_string());
            }
            let pad_count = target_len - char_count;
            let padding: String = std::iter::repeat_n(fill_char, pad_count).collect();
            if is_left {
                Some(format!("{padding}{s}"))
            } else {
                Some(format!("{s}{padding}"))
            }
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_string_predicate(
    arr: &ArrayRef,
    pattern: &ArrayRef,
    name: &str,
    f: fn(&str, &str) -> bool,
) -> Result<ArrayRef, EvalError> {
    let sa = as_str(arr).ok_or_else(|| EvalError {
        message: format!("{name}: expected string"),
    })?;
    let pa = as_str(pattern).ok_or_else(|| EvalError {
        message: format!("{name}: pattern must be string"),
    })?;
    let result: BooleanArray = (0..sa.len())
        .map(|i| {
            if sa.is_null(i) || pa.is_null(i) {
                None
            } else {
                Some(f(sa.value(i), pa.value(i)))
            }
        })
        .collect();
    Ok(Arc::new(result))
}

// ─── Hash helper ────────────────────────────────────────────────────────────

fn eval_hash(arr: &ArrayRef) -> Result<ArrayRef, EvalError> {
    // Fixed keys for deterministic hashing across runs
    let strings = array_to_strings(arr)?;
    let result: Int64Array = strings
        .iter()
        .map(|v| {
            v.as_ref().map(|s| {
                let mut hasher = SipHasher::new_with_keys(0x5175_6972_6B79_2D6B, 0x6E69_745F_6861_7368);
                s.hash(&mut hasher);
                hasher.finish() as i64
            })
        })
        .collect();
    Ok(Arc::new(result))
}

/// When one of a pair of arrays is NullArray, coerce it to typed nulls
/// matching the other's data type.
fn resolve_null_pair(a: &ArrayRef, b: &ArrayRef) -> (ArrayRef, ArrayRef) {
    if is_null_array(a) && !is_null_array(b) {
        (typed_nulls(b.data_type(), a.len()), b.clone())
    } else if !is_null_array(a) && is_null_array(b) {
        (a.clone(), typed_nulls(a.data_type(), b.len()))
    } else {
        (a.clone(), b.clone())
    }
}

// ─── Conditional helpers ────────────────────────────────────────────────────

/// Evaluate `case(cond1, val1, cond2, val2, ..., default)`.
///
/// Arguments come in (condition, value) pairs. An optional trailing
/// argument without a condition pair serves as the default value.
/// If no branch matches and no default is given, the result is null.
fn eval_case(args: &[Expr], ctx: &EvalContext<'_>) -> Result<ArrayRef, EvalError> {
    let count = ctx.row_count;
    let has_default = args.len() % 2 == 1;
    let pair_count = args.len() / 2;

    // Evaluate all condition/value pairs eagerly, validating conditions are boolean
    let mut branches: Vec<(ArrayRef, ArrayRef)> = Vec::with_capacity(pair_count);
    for i in 0..pair_count {
        let cond = evaluate(&args[i * 2], ctx)?;
        require_bool(&cond)?;
        let val = evaluate(&args[i * 2 + 1], ctx)?;
        branches.push((cond, val));
    }
    let default_val = if has_default {
        Some(evaluate(args.last().unwrap(), ctx)?)
    } else {
        None
    };

    // Determine output type, promoting mixed Int64/Float64 to Float64
    let all_types: Vec<DataType> = branches
        .iter()
        .map(|(_, v)| v.data_type().clone())
        .chain(default_val.iter().map(|v| v.data_type().clone()))
        .filter(|dt| dt != &DataType::Null)
        .collect();

    let mut result_type = all_types
        .first()
        .cloned()
        .unwrap_or(DataType::Int64);

    // Promote to Float64 if any branch is Float64 and result_type is Int64
    if result_type == DataType::Int64 && all_types.iter().any(|dt| dt == &DataType::Float64) {
        result_type = DataType::Float64;
    }

    // Track which rows have been assigned
    let mut assigned = vec![false; count];

    match result_type {
        DataType::Float64 => {
            let mut result: Vec<Option<f64>> = vec![None; count];
            for (cond, val) in &branches {
                let ba = as_bool(cond).unwrap();
                let fv = to_f64_vec(val)?;
                for i in 0..count {
                    if !assigned[i] && !ba.is_null(i) && ba.value(i) {
                        result[i] = fv[i];
                        assigned[i] = true;
                    }
                }
            }
            if let Some(ref dv) = default_val {
                let fv = to_f64_vec(dv)?;
                for i in 0..count {
                    if !assigned[i] {
                        result[i] = fv[i];
                    }
                }
            }
            Ok(Arc::new(Float64Array::from(result)))
        }
        DataType::Int64 => {
            let mut result: Vec<Option<i64>> = vec![None; count];
            for (cond, val) in &branches {
                let ba = as_bool(cond).unwrap();
                let ia = as_i64(val).ok_or_else(|| EvalError {
                    message: "case: type mismatch in value branch".into(),
                })?;
                for i in 0..count {
                    if !assigned[i] && !ba.is_null(i) && ba.value(i) {
                        result[i] = if ia.is_null(i) { None } else { Some(ia.value(i)) };
                        assigned[i] = true;
                    }
                }
            }
            if let Some(ref dv) = default_val {
                let ia = as_i64(dv).ok_or_else(|| EvalError {
                    message: "case: type mismatch in default".into(),
                })?;
                for i in 0..count {
                    if !assigned[i] {
                        result[i] = if ia.is_null(i) { None } else { Some(ia.value(i)) };
                    }
                }
            }
            Ok(Arc::new(Int64Array::from(result)))
        }
        DataType::Boolean => {
            let mut result: Vec<Option<bool>> = vec![None; count];
            for (cond, val) in &branches {
                let ba = as_bool(cond).unwrap();
                let va = as_bool(val).ok_or_else(|| EvalError {
                    message: "case: type mismatch in value branch".into(),
                })?;
                for i in 0..count {
                    if !assigned[i] && !ba.is_null(i) && ba.value(i) {
                        result[i] = if va.is_null(i) { None } else { Some(va.value(i)) };
                        assigned[i] = true;
                    }
                }
            }
            if let Some(ref dv) = default_val {
                let va = as_bool(dv).ok_or_else(|| EvalError {
                    message: "case: type mismatch in default".into(),
                })?;
                for i in 0..count {
                    if !assigned[i] {
                        result[i] = if va.is_null(i) { None } else { Some(va.value(i)) };
                    }
                }
            }
            Ok(Arc::new(BooleanArray::from(result)))
        }
        _ => {
            // String output path
            let mut result: Vec<Option<String>> = vec![None; count];
            for (cond, val) in &branches {
                let ba = as_bool(cond).unwrap();
                let sv = array_to_strings(val)?;
                for i in 0..count {
                    if !assigned[i] && !ba.is_null(i) && ba.value(i) {
                        result[i] = sv[i].clone();
                        assigned[i] = true;
                    }
                }
            }
            if let Some(ref dv) = default_val {
                let sv = array_to_strings(dv)?;
                for i in 0..count {
                    if !assigned[i] {
                        result[i] = sv[i].clone();
                    }
                }
            }
            let arr: StringArray = result.iter().map(|v| v.as_deref()).collect();
            Ok(Arc::new(arr))
        }
    }
}

fn eval_if(
    cond: &ArrayRef,
    then: &ArrayRef,
    otherwise: &ArrayRef,
) -> Result<ArrayRef, EvalError> {
    let ba = require_bool(cond)?;
    let count = ba.len();

    // Resolve NullArray to the peer's type
    let (then, otherwise) = resolve_null_pair(then, otherwise);

    // Promote mixed numeric types to Float64
    let (then, otherwise) = if is_numeric(then.data_type())
        && is_numeric(otherwise.data_type())
        && then.data_type() != otherwise.data_type()
    {
        let promote = |arr: &ArrayRef| -> ArrayRef {
            if arr.data_type() == &DataType::Float64 {
                arr.clone()
            } else {
                let ia = as_i64(arr).unwrap();
                let fa: Float64Array = (0..ia.len())
                    .map(|i| {
                        if ia.is_null(i) {
                            None
                        } else {
                            Some(ia.value(i) as f64)
                        }
                    })
                    .collect();
                Arc::new(fa) as ArrayRef
            }
        };
        (promote(&then), promote(&otherwise))
    } else {
        (then, otherwise)
    };

    match then.data_type() {
        DataType::Int64 => {
            let t_vals = as_i64(&then).unwrap();
            let o_vals = as_i64(&otherwise).ok_or_else(|| EvalError {
                message: format!(
                    "if: type mismatch: then is Int64, otherwise is {:?}",
                    otherwise.data_type()
                ),
            })?;
            let result: Int64Array = (0..count)
                .map(|i| {
                    if ba.is_null(i) {
                        return None;
                    }
                    if ba.value(i) {
                        if t_vals.is_null(i) { None } else { Some(t_vals.value(i)) }
                    } else if o_vals.is_null(i) {
                        None
                    } else {
                        Some(o_vals.value(i))
                    }
                })
                .collect();
            Ok(Arc::new(result))
        }
        DataType::Float64 => {
            let tv = to_f64_vec(&then)?;
            let ov = to_f64_vec(&otherwise)?;
            let result: Float64Array = (0..count)
                .map(|i| {
                    if ba.is_null(i) {
                        None
                    } else if ba.value(i) {
                        tv[i]
                    } else {
                        ov[i]
                    }
                })
                .collect();
            Ok(Arc::new(result))
        }
        DataType::Utf8 => {
            let ts = array_to_strings(&then)?;
            let os = array_to_strings(&otherwise)?;
            let result: StringArray = (0..count)
                .map(|i| {
                    if ba.is_null(i) {
                        None
                    } else if ba.value(i) {
                        ts[i].as_deref()
                    } else {
                        os[i].as_deref()
                    }
                })
                .collect();
            Ok(Arc::new(result))
        }
        DataType::Boolean => {
            let t = as_bool(&then).unwrap();
            let o = require_bool(&otherwise)?;
            let result: BooleanArray = (0..count)
                .map(|i| {
                    if ba.is_null(i) {
                        None
                    } else if ba.value(i) {
                        null_safe_bool(t, i)
                    } else {
                        null_safe_bool(o, i)
                    }
                })
                .collect();
            Ok(Arc::new(result))
        }
        other => Err(EvalError {
            message: format!("if: unsupported result type {other}"),
        }),
    }
}

fn eval_coalesce(arrays: &[ArrayRef]) -> Result<ArrayRef, EvalError> {
    if arrays.is_empty() {
        return Err(EvalError {
            message: "coalesce requires at least one argument".into(),
        });
    }
    let count = arrays[0].len();

    // Find the first non-Null type to determine output type
    let mut result_type = arrays
        .iter()
        .map(|a| a.data_type().clone())
        .find(|dt| dt != &DataType::Null)
        .unwrap_or(DataType::Int64);

    // Promote to Float64 if any non-null array is Float64 and result_type is Int64
    if result_type == DataType::Int64
        && arrays
            .iter()
            .any(|a| a.data_type() == &DataType::Float64)
    {
        result_type = DataType::Float64;
    }

    // Coerce NullArrays and promote Int64→Float64 when needed
    let typed_arrays: Vec<ArrayRef> = arrays
        .iter()
        .map(|a| {
            if is_null_array(a) {
                typed_nulls(&result_type, a.len())
            } else if result_type == DataType::Float64 && a.data_type() == &DataType::Int64 {
                let ia = as_i64(a).unwrap();
                let fa: Float64Array = (0..ia.len())
                    .map(|i| {
                        if ia.is_null(i) {
                            None
                        } else {
                            Some(ia.value(i) as f64)
                        }
                    })
                    .collect();
                Arc::new(fa) as ArrayRef
            } else {
                a.clone()
            }
        })
        .collect();

    match result_type {
        DataType::Int64 => {
            let typed: Vec<&Int64Array> = typed_arrays
                .iter()
                .map(|a| {
                    as_i64(a).ok_or_else(|| EvalError {
                        message: "coalesce: type mismatch".into(),
                    })
                })
                .collect::<Result<_, _>>()?;
            let result: Int64Array = (0..count)
                .map(|i| {
                    for arr in &typed {
                        if !arr.is_null(i) {
                            return Some(arr.value(i));
                        }
                    }
                    None
                })
                .collect();
            Ok(Arc::new(result))
        }
        DataType::Float64 => {
            let vecs: Vec<Vec<Option<f64>>> = typed_arrays
                .iter()
                .map(to_f64_vec)
                .collect::<Result<_, _>>()?;
            let result: Float64Array = (0..count)
                .map(|i| {
                    for v in &vecs {
                        if let Some(val) = v[i] {
                            return Some(val);
                        }
                    }
                    None
                })
                .collect();
            Ok(Arc::new(result))
        }
        DataType::Utf8 => {
            let typed: Vec<&StringArray> = typed_arrays
                .iter()
                .map(|a| {
                    as_str(a).ok_or_else(|| EvalError {
                        message: "coalesce: type mismatch".into(),
                    })
                })
                .collect::<Result<_, _>>()?;
            let result: StringArray = (0..count)
                .map(|i| {
                    for arr in &typed {
                        if !arr.is_null(i) {
                            return Some(arr.value(i).to_string());
                        }
                    }
                    None
                })
                .collect();
            Ok(Arc::new(result))
        }
        DataType::Boolean => {
            let typed: Vec<&BooleanArray> = typed_arrays
                .iter()
                .map(|a| {
                    as_bool(a).ok_or_else(|| EvalError {
                        message: "coalesce: type mismatch".into(),
                    })
                })
                .collect::<Result<_, _>>()?;
            let result: BooleanArray = (0..count)
                .map(|i| {
                    for arr in &typed {
                        if !arr.is_null(i) {
                            return Some(arr.value(i));
                        }
                    }
                    None
                })
                .collect();
            Ok(Arc::new(result))
        }
        other => Err(EvalError {
            message: format!("coalesce: unsupported type {other}"),
        }),
    }
}

fn eval_nullif(a: &ArrayRef, b: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let eq_arr = eval_cmp(a, b, BinOp::Eq)?;
    let eq = as_bool(&eq_arr).unwrap();
    let count = eq.len();

    if let Some(ia) = as_i64(a) {
        let result: Int64Array = (0..count)
            .map(|i| {
                if ia.is_null(i) || (!eq.is_null(i) && eq.value(i)) {
                    None
                } else {
                    Some(ia.value(i))
                }
            })
            .collect();
        return Ok(Arc::new(result));
    }
    if let Some(fa) = as_f64(a) {
        let result: Float64Array = (0..count)
            .map(|i| {
                if fa.is_null(i) || (!eq.is_null(i) && eq.value(i)) {
                    None
                } else {
                    Some(fa.value(i))
                }
            })
            .collect();
        return Ok(Arc::new(result));
    }
    if let Some(sa) = as_str(a) {
        let result: StringArray = (0..count)
            .map(|i| {
                if sa.is_null(i) || (!eq.is_null(i) && eq.value(i)) {
                    None
                } else {
                    Some(sa.value(i).to_string())
                }
            })
            .collect();
        return Ok(Arc::new(result));
    }
    if let Some(ba_arr) = as_bool(a) {
        let result: BooleanArray = (0..count)
            .map(|i| {
                if ba_arr.is_null(i) || (!eq.is_null(i) && eq.value(i)) {
                    None
                } else {
                    Some(ba_arr.value(i))
                }
            })
            .collect();
        return Ok(Arc::new(result));
    }
    if is_null_array(a) {
        return Ok(typed_nulls(&DataType::Int64, count));
    }
    Err(EvalError {
        message: format!("nullif: unsupported type {:?}", a.data_type()),
    })
}

// ─── Date/time helpers ──────────────────────────────────────────────────────

/// Convert epoch millis to NaiveDateTime, returning None for out-of-range.
fn millis_to_datetime(ms: i64) -> Option<NaiveDateTime> {
    let secs = ms.div_euclid(1_000);
    let nsecs = (ms.rem_euclid(1_000) * 1_000_000) as u32;
    DateTime::from_timestamp(secs, nsecs).map(|dt| dt.naive_utc())
}

/// Convert NaiveDateTime to epoch millis.
fn datetime_to_millis(dt: &NaiveDateTime) -> i64 {
    dt.and_utc().timestamp_millis()
}

/// Parse a temporal unit string into milliseconds multiplier.
/// Returns None for units that need special handling (microsecond, month/quarter/year).
fn unit_to_millis(unit: &str) -> Option<i64> {
    match unit {
        "millisecond" => Some(1),
        "second" => Some(1_000),
        "minute" => Some(60_000),
        "hour" => Some(3_600_000),
        "day" => Some(86_400_000),
        "week" => Some(604_800_000),
        _ => None, // microsecond, month, quarter, year need special handling
    }
}

fn eval_make_date(y: &ArrayRef, m: &ArrayRef, d: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let ya = as_i64(y).ok_or_else(|| EvalError { message: "make_date: year must be integer".into() })?;
    let ma = as_i64(m).ok_or_else(|| EvalError { message: "make_date: month must be integer".into() })?;
    let da = as_i64(d).ok_or_else(|| EvalError { message: "make_date: day must be integer".into() })?;
    let result: TimestampMillisecondArray = (0..ya.len())
        .map(|i| {
            if ya.is_null(i) || ma.is_null(i) || da.is_null(i) { return None; }
            let yv = i32::try_from(ya.value(i)).ok()?;
            let mv = u32::try_from(ma.value(i)).ok()?;
            let dv = u32::try_from(da.value(i)).ok()?;
            let dt = NaiveDate::from_ymd_opt(yv, mv, dv)?;
            Some(datetime_to_millis(&dt.and_hms_opt(0, 0, 0)?))
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_make_time(h: &ArrayRef, m: &ArrayRef, s: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let ha = as_i64(h).ok_or_else(|| EvalError { message: "make_time: hour must be integer".into() })?;
    let ma = as_i64(m).ok_or_else(|| EvalError { message: "make_time: minute must be integer".into() })?;
    let sa = as_i64(s).ok_or_else(|| EvalError { message: "make_time: second must be integer".into() })?;
    // Returns millis since midnight
    let result: Int64Array = (0..ha.len())
        .map(|i| {
            if ha.is_null(i) || ma.is_null(i) || sa.is_null(i) { return None; }
            let h = ha.value(i);
            let m = ma.value(i);
            let s = sa.value(i);
            if !(0..24).contains(&h) || !(0..60).contains(&m) || !(0..60).contains(&s) {
                return None;
            }
            Some(h * 3_600_000 + m * 60_000 + s * 1_000)
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_make_datetime(
    y: &ArrayRef, mo: &ArrayRef, d: &ArrayRef,
    h: &ArrayRef, mi: &ArrayRef, s: &ArrayRef,
) -> Result<ArrayRef, EvalError> {
    let ya = as_i64(y).ok_or_else(|| EvalError { message: "make_datetime: year must be integer".into() })?;
    let moa = as_i64(mo).ok_or_else(|| EvalError { message: "make_datetime: month must be integer".into() })?;
    let da = as_i64(d).ok_or_else(|| EvalError { message: "make_datetime: day must be integer".into() })?;
    let ha = as_i64(h).ok_or_else(|| EvalError { message: "make_datetime: hour must be integer".into() })?;
    let mia = as_i64(mi).ok_or_else(|| EvalError { message: "make_datetime: minute must be integer".into() })?;
    let sa = as_i64(s).ok_or_else(|| EvalError { message: "make_datetime: second must be integer".into() })?;
    let result: TimestampMillisecondArray = (0..ya.len())
        .map(|i| {
            if ya.is_null(i) || moa.is_null(i) || da.is_null(i)
                || ha.is_null(i) || mia.is_null(i) || sa.is_null(i) { return None; }
            let yv = i32::try_from(ya.value(i)).ok()?;
            let mov = u32::try_from(moa.value(i)).ok()?;
            let dv = u32::try_from(da.value(i)).ok()?;
            let hv = u32::try_from(ha.value(i)).ok()?;
            let miv = u32::try_from(mia.value(i)).ok()?;
            let sv = u32::try_from(sa.value(i)).ok()?;
            let date = NaiveDate::from_ymd_opt(yv, mov, dv)?;
            let dt = date.and_hms_opt(hv, miv, sv)?;
            Some(datetime_to_millis(&dt))
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_make_duration(n: &ArrayRef, unit: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let na = as_i64(n).ok_or_else(|| EvalError { message: "make_duration: n must be integer".into() })?;
    let ua = as_str(unit).ok_or_else(|| EvalError { message: "make_duration: unit must be string".into() })?;
    let result: Int64Array = (0..na.len())
        .map(|i| {
            if na.is_null(i) || ua.is_null(i) { return None; }
            let val = na.value(i);
            let u = ua.value(i);
            if let Some(m) = unit_to_millis(u) {
                return Some(val * m);
            }
            match u {
                "microsecond" => Some(val / 1_000), // truncate sub-ms
                "month" => Some(val * 30 * 86_400_000),
                "quarter" => Some(val * 91 * 86_400_000),
                "year" => Some(val * 365 * 86_400_000),
                _ => None,
            }
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_to_date(s: &ArrayRef, fmt: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let sa = as_str(s).ok_or_else(|| EvalError { message: "to_date: expected string".into() })?;
    let fa = as_str(fmt).ok_or_else(|| EvalError { message: "to_date: format must be string".into() })?;
    let result: TimestampMillisecondArray = (0..sa.len())
        .map(|i| {
            if sa.is_null(i) || fa.is_null(i) { return None; }
            let date = NaiveDate::parse_from_str(sa.value(i), fa.value(i)).ok()?;
            Some(datetime_to_millis(&date.and_hms_opt(0, 0, 0)?))
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_to_datetime(s: &ArrayRef, fmt: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let sa = as_str(s).ok_or_else(|| EvalError { message: "to_datetime: expected string".into() })?;
    let fa = as_str(fmt).ok_or_else(|| EvalError { message: "to_datetime: format must be string".into() })?;
    let result: TimestampMillisecondArray = (0..sa.len())
        .map(|i| {
            if sa.is_null(i) || fa.is_null(i) { return None; }
            let dt = NaiveDateTime::parse_from_str(sa.value(i), fa.value(i)).ok()?;
            Some(datetime_to_millis(&dt))
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_epoch_seconds(arr: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let millis = require_millis(arr, "epoch_seconds")?;
    let result: Float64Array = millis.into_iter()
        .map(|v| v.map(|m| m as f64 / 1_000.0))
        .collect();
    Ok(Arc::new(result))
}

fn eval_from_epoch(arr: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let v = to_f64_vec(arr).map_err(|e| EvalError {
        message: format!("from_epoch: {e}"),
    })?;
    let result: TimestampMillisecondArray = v.into_iter()
        .map(|opt| opt.map(|secs| (secs * 1_000.0) as i64))
        .collect();
    Ok(Arc::new(result))
}

/// Generic date field extraction: convert millis → NaiveDateTime → extract component.
fn eval_date_extract(
    arr: &ArrayRef,
    fname: &str,
    f: fn(&NaiveDateTime) -> i64,
) -> Result<ArrayRef, EvalError> {
    let millis = require_millis(arr, fname)?;
    let result: Int64Array = millis.into_iter()
        .map(|v| v.and_then(|m| millis_to_datetime(m).map(|dt| f(&dt))))
        .collect();
    Ok(Arc::new(result))
}

fn eval_date_add_sub(
    d: &ArrayRef,
    n: &ArrayRef,
    unit: &ArrayRef,
    is_add: bool,
) -> Result<ArrayRef, EvalError> {
    let fname = if is_add { "date_add" } else { "date_sub" };
    let dm = require_millis(d, fname)?;
    let na = as_i64(n).ok_or_else(|| EvalError {
        message: format!("{fname}: n must be integer"),
    })?;
    let ua = as_str(unit).ok_or_else(|| EvalError {
        message: format!("{fname}: unit must be string"),
    })?;

    let result: TimestampMillisecondArray = (0..dm.len())
        .map(|i| {
            let ms = dm[i]?;
            if na.is_null(i) || ua.is_null(i) { return None; }
            let amount = if is_add { na.value(i) } else { -na.value(i) };
            let unit_str = ua.value(i);

            if let Some(unit_ms) = unit_to_millis(unit_str) {
                return Some(ms + amount * unit_ms);
            }

            // Sub-millisecond: truncate to nearest ms
            if unit_str == "microsecond" {
                return Some(ms + amount / 1_000);
            }

            // Calendar-based arithmetic for month/quarter/year
            let dt = millis_to_datetime(ms)?;
            match unit_str {
                "month" => {
                    let total_months = dt.year() as i64 * 12 + (dt.month() as i64 - 1) + amount;
                    let new_year = total_months.div_euclid(12) as i32;
                    let new_month = (total_months.rem_euclid(12) + 1) as u32;
                    let max_day = days_in_month(new_year, new_month);
                    let new_day = dt.day().min(max_day);
                    let new_dt = NaiveDate::from_ymd_opt(new_year, new_month, new_day)?
                        .and_hms_milli_opt(dt.hour(), dt.minute(), dt.second(), dt.and_utc().timestamp_subsec_millis())?;
                    Some(datetime_to_millis(&new_dt))
                }
                "quarter" => {
                    let total_months = dt.year() as i64 * 12 + (dt.month() as i64 - 1) + amount * 3;
                    let new_year = total_months.div_euclid(12) as i32;
                    let new_month = (total_months.rem_euclid(12) + 1) as u32;
                    let max_day = days_in_month(new_year, new_month);
                    let new_day = dt.day().min(max_day);
                    let new_dt = NaiveDate::from_ymd_opt(new_year, new_month, new_day)?
                        .and_hms_milli_opt(dt.hour(), dt.minute(), dt.second(), dt.and_utc().timestamp_subsec_millis())?;
                    Some(datetime_to_millis(&new_dt))
                }
                "year" => {
                    let new_year = dt.year() + amount as i32;
                    let max_day = days_in_month(new_year, dt.month());
                    let new_day = dt.day().min(max_day);
                    let new_dt = NaiveDate::from_ymd_opt(new_year, dt.month(), new_day)?
                        .and_hms_milli_opt(dt.hour(), dt.minute(), dt.second(), dt.and_utc().timestamp_subsec_millis())?;
                    Some(datetime_to_millis(&new_dt))
                }
                _ => None,
            }
        })
        .collect();
    Ok(Arc::new(result))
}

/// Number of days in a given month (handles leap years).
fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 { 29 } else { 28 }
        }
        _ => 30,
    }
}

fn eval_date_diff(d1: &ArrayRef, d2: &ArrayRef, unit: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let m1 = require_millis(d1, "date_diff")?;
    let m2 = require_millis(d2, "date_diff")?;
    let ua = as_str(unit).ok_or_else(|| EvalError {
        message: "date_diff: unit must be string".into(),
    })?;

    let result: Int64Array = (0..m1.len())
        .map(|i| {
            let ms1 = m1[i]?;
            let ms2 = m2[i]?;
            if ua.is_null(i) { return None; }
            let diff_ms = ms1 - ms2;
            let unit_str = ua.value(i);

            if let Some(unit_ms) = unit_to_millis(unit_str) {
                return Some(diff_ms / unit_ms);
            }

            if unit_str == "microsecond" {
                return Some(diff_ms * 1_000); // approximate: ms → µs
            }

            let dt1 = millis_to_datetime(ms1)?;
            let dt2 = millis_to_datetime(ms2)?;
            match unit_str {
                "month" => {
                    Some((dt1.year() as i64 - dt2.year() as i64) * 12
                        + dt1.month() as i64 - dt2.month() as i64)
                }
                "quarter" => {
                    let months = (dt1.year() as i64 - dt2.year() as i64) * 12
                        + dt1.month() as i64 - dt2.month() as i64;
                    Some(months / 3)
                }
                "year" => Some(dt1.year() as i64 - dt2.year() as i64),
                _ => None,
            }
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_duration_add(d: &ArrayRef, dur: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let dm = require_millis(d, "duration_add")?;
    let dur_m = require_millis(dur, "duration_add")?;
    let result: TimestampMillisecondArray = dm.iter().zip(dur_m.iter())
        .map(|(a, b)| match (a, b) {
            (Some(a), Some(b)) => Some(a + b),
            _ => None,
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_start_of(d: &ArrayRef, unit: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let dm = require_millis(d, "start_of")?;
    let ua = as_str(unit).ok_or_else(|| EvalError {
        message: "start_of: unit must be string".into(),
    })?;

    let result: TimestampMillisecondArray = (0..dm.len())
        .map(|i| {
            let ms = dm[i]?;
            if ua.is_null(i) { return None; }
            let dt = millis_to_datetime(ms)?;
            let truncated = match ua.value(i) {
                "second" => dt.date().and_hms_opt(dt.hour(), dt.minute(), dt.second())?,
                "minute" => dt.date().and_hms_opt(dt.hour(), dt.minute(), 0)?,
                "hour" => dt.date().and_hms_opt(dt.hour(), 0, 0)?,
                "day" => dt.date().and_hms_opt(0, 0, 0)?,
                "week" => {
                    let weekday = dt.weekday().num_days_from_monday();
                    let start = dt.date() - chrono::Duration::days(weekday as i64);
                    start.and_hms_opt(0, 0, 0)?
                }
                "month" => NaiveDate::from_ymd_opt(dt.year(), dt.month(), 1)?
                    .and_hms_opt(0, 0, 0)?,
                "quarter" => {
                    let q_month = (dt.month() - 1) / 3 * 3 + 1;
                    NaiveDate::from_ymd_opt(dt.year(), q_month, 1)?
                        .and_hms_opt(0, 0, 0)?
                }
                "year" => NaiveDate::from_ymd_opt(dt.year(), 1, 1)?
                    .and_hms_opt(0, 0, 0)?,
                _ => return None,
            };
            Some(datetime_to_millis(&truncated))
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_end_of(d: &ArrayRef, unit: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let dm = require_millis(d, "end_of")?;
    let ua = as_str(unit).ok_or_else(|| EvalError {
        message: "end_of: unit must be string".into(),
    })?;

    let result: TimestampMillisecondArray = (0..dm.len())
        .map(|i| {
            let ms = dm[i]?;
            if ua.is_null(i) { return None; }
            let dt = millis_to_datetime(ms)?;
            let end = match ua.value(i) {
                "second" => dt.date().and_hms_milli_opt(dt.hour(), dt.minute(), dt.second(), 999)?,
                "minute" => dt.date().and_hms_milli_opt(dt.hour(), dt.minute(), 59, 999)?,
                "hour" => dt.date().and_hms_milli_opt(dt.hour(), 59, 59, 999)?,
                "day" => dt.date().and_hms_milli_opt(23, 59, 59, 999)?,
                "week" => {
                    let weekday = dt.weekday().num_days_from_monday();
                    let end_date = dt.date() + chrono::Duration::days(6 - weekday as i64);
                    end_date.and_hms_milli_opt(23, 59, 59, 999)?
                }
                "month" => {
                    let last_day = days_in_month(dt.year(), dt.month());
                    NaiveDate::from_ymd_opt(dt.year(), dt.month(), last_day)?
                        .and_hms_milli_opt(23, 59, 59, 999)?
                }
                "quarter" => {
                    let q_end_month = ((dt.month() - 1) / 3 + 1) * 3;
                    let last_day = days_in_month(dt.year(), q_end_month);
                    NaiveDate::from_ymd_opt(dt.year(), q_end_month, last_day)?
                        .and_hms_milli_opt(23, 59, 59, 999)?
                }
                "year" => NaiveDate::from_ymd_opt(dt.year(), 12, 31)?
                    .and_hms_milli_opt(23, 59, 59, 999)?,
                _ => return None,
            };
            Some(datetime_to_millis(&end))
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_format_date(d: &ArrayRef, fmt: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let dm = require_millis(d, "format_date")?;
    let fa = as_str(fmt).ok_or_else(|| EvalError {
        message: "format_date: format must be string".into(),
    })?;

    let result: StringArray = (0..dm.len())
        .map(|i| {
            let ms = dm[i]?;
            if fa.is_null(i) { return None; }
            let dt = millis_to_datetime(ms)?;
            Some(dt.format(fa.value(i)).to_string())
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_format_duration(d: &ArrayRef, style: &ArrayRef) -> Result<ArrayRef, EvalError> {
    let dm = require_millis(d, "format_duration")?;
    let sa = as_str(style).ok_or_else(|| EvalError {
        message: "format_duration: style must be string".into(),
    })?;

    let result: StringArray = (0..dm.len())
        .map(|i| {
            let ms = dm[i]?;
            if sa.is_null(i) { return None; }
            let total_secs = ms / 1_000;
            let remaining_ms = ms % 1_000;
            match sa.value(i) {
                "hms" => {
                    let h = total_secs / 3600;
                    let m = (total_secs % 3600) / 60;
                    let s = total_secs % 60;
                    Some(format!("{h:02}:{m:02}:{s:02}"))
                }
                "human" => {
                    let days = total_secs / 86400;
                    let h = (total_secs % 86400) / 3600;
                    let m = (total_secs % 3600) / 60;
                    let s = total_secs % 60;
                    if days > 0 {
                        Some(format!("{days}d {h}h {m}m {s}s"))
                    } else if h > 0 {
                        Some(format!("{h}h {m}m {s}s"))
                    } else if m > 0 {
                        Some(format!("{m}m {s}s"))
                    } else {
                        Some(format!("{s}s"))
                    }
                }
                "iso" => {
                    let h = total_secs / 3600;
                    let m = (total_secs % 3600) / 60;
                    let s = total_secs % 60;
                    if remaining_ms > 0 {
                        Some(format!("PT{h}H{m}M{s}.{remaining_ms:03}S"))
                    } else {
                        Some(format!("PT{h}H{m}M{s}S"))
                    }
                }
                _ => None,
            }
        })
        .collect();
    Ok(Arc::new(result))
}

// ─── Random helpers ─────────────────────────────────────────────────────────

/// Fast, deterministic mixing function (splitmix64).
/// Produces a uniformly distributed u64 from any input.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// Allocate a unique call-site ID for this random invocation.
fn next_call_id(ctx: &EvalContext<'_>) -> u64 {
    let id = ctx.call_counter.get();
    ctx.call_counter.set(id + 1);
    id
}

/// Derive a per-row seed from the context seed, call ID, and row index.
fn row_seed(ctx: &EvalContext<'_>, call_id: u64, row: usize) -> u64 {
    splitmix64(ctx.seed ^ call_id.wrapping_mul(0x517C_C1B7_2722_0A95) ^ (ctx.row_offset + row as u64))
}

fn eval_random_int(
    min_arr: &ArrayRef,
    max_arr: &ArrayRef,
    ctx: &EvalContext<'_>,
) -> Result<ArrayRef, EvalError> {
    let mins = as_i64(min_arr).ok_or_else(|| EvalError {
        message: "random_int: min must be integer".into(),
    })?;
    let maxs = as_i64(max_arr).ok_or_else(|| EvalError {
        message: "random_int: max must be integer".into(),
    })?;
    let call_id = next_call_id(ctx);
    let result: Int64Array = (0..ctx.row_count)
        .map(|i| {
            if mins.is_null(i) || maxs.is_null(i) {
                return None;
            }
            let lo = mins.value(i);
            let hi = maxs.value(i);
            if lo > hi {
                return None;
            }
            // Use u128 to avoid overflow for large ranges (e.g., [0, i64::MAX])
            let range = (hi as u128).wrapping_sub(lo as u128) + 1;
            let r = row_seed(ctx, call_id, i) as u128;
            Some(lo.wrapping_add((r % range) as i64))
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_random_float(
    min_arr: &ArrayRef,
    max_arr: &ArrayRef,
    ctx: &EvalContext<'_>,
) -> Result<ArrayRef, EvalError> {
    let mins = to_f64_vec(min_arr).map_err(|e| EvalError {
        message: format!("random_float: {e}"),
    })?;
    let maxs = to_f64_vec(max_arr).map_err(|e| EvalError {
        message: format!("random_float: {e}"),
    })?;
    let call_id = next_call_id(ctx);
    let result: Float64Array = (0..ctx.row_count)
        .map(|i| {
            let lo = mins[i]?;
            let hi = maxs[i]?;
            if lo > hi {
                return None;
            }
            let r = row_seed(ctx, call_id, i);
            // Convert u64 to [0, 1) float
            let t = (r >> 11) as f64 / (1u64 << 53) as f64;
            Some(lo + t * (hi - lo))
        })
        .collect();
    Ok(Arc::new(result))
}

fn eval_random_duration(
    min_arr: &ArrayRef,
    max_arr: &ArrayRef,
    ctx: &EvalContext<'_>,
) -> Result<ArrayRef, EvalError> {
    let mins = require_millis(min_arr, "random_duration")?;
    let maxs = require_millis(max_arr, "random_duration")?;
    let call_id = next_call_id(ctx);
    let result: Int64Array = (0..ctx.row_count)
        .map(|i| {
            let lo = mins[i]?;
            let hi = maxs[i]?;
            if lo > hi {
                return None;
            }
            // Use u128 to avoid overflow for large ranges
            let range = (hi as u128).wrapping_sub(lo as u128) + 1;
            let r = row_seed(ctx, call_id, i) as u128;
            Some(lo.wrapping_add((r % range) as i64))
        })
        .collect();
    Ok(Arc::new(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::expr::parser;

    fn eval_expr(expr_str: &str, columns: HashMap<String, ArrayRef>) -> ArrayRef {
        let ast = parser::parse(expr_str).unwrap();
        let params = HashMap::new();
        let row_count = columns.values().next().map(|a| a.len()).unwrap_or(0);
        let ctx = EvalContext {
            columns: &columns,
            params: &params,
            row_count,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        evaluate(&ast, &ctx).unwrap()
    }

    #[test]
    fn add_int() {
        let mut cols = HashMap::new();
        cols.insert(
            "a".into(),
            Arc::new(Int64Array::from(vec![1, 2, 3])) as ArrayRef,
        );
        cols.insert(
            "b".into(),
            Arc::new(Int64Array::from(vec![10, 20, 30])) as ArrayRef,
        );
        let result = eval_expr("${a} + ${b}", cols);
        let fa = as_f64(&result).unwrap();
        assert_eq!(fa.values(), &[11.0, 22.0, 33.0]);
    }

    #[test]
    fn mul_float() {
        let mut cols = HashMap::new();
        cols.insert(
            "price".into(),
            Arc::new(Float64Array::from(vec![10.0, 20.0, 30.0])) as ArrayRef,
        );
        cols.insert(
            "qty".into(),
            Arc::new(Int64Array::from(vec![2, 3, 4])) as ArrayRef,
        );
        let result = eval_expr("${price} * ${qty}", cols);
        let fa = as_f64(&result).unwrap();
        assert_eq!(fa.values(), &[20.0, 60.0, 120.0]);
    }

    #[test]
    fn comparison() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Int64Array::from(vec![1, 5, 10])) as ArrayRef,
        );
        let result = eval_expr("${x} > 3", cols);
        let ba = as_bool(&result).unwrap();
        assert!(!ba.value(0));
        assert!(ba.value(1));
        assert!(ba.value(2));
    }

    #[test]
    fn abs_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Int64Array::from(vec![-5, 0, 3])) as ArrayRef,
        );
        let result = eval_expr("abs(${x})", cols);
        let ia = as_i64(&result).unwrap();
        assert_eq!(ia.values(), &[5, 0, 3]);
    }

    #[test]
    fn round_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Float64Array::from(vec![3.14159, 2.71828])) as ArrayRef,
        );
        let result = eval_expr("round(${x}, 2)", cols);
        let fa = as_f64(&result).unwrap();
        assert!((fa.value(0) - 3.14).abs() < 1e-10);
        assert!((fa.value(1) - 2.72).abs() < 1e-10);
    }

    #[test]
    fn upper_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "name".into(),
            Arc::new(StringArray::from(vec!["hello", "world"])) as ArrayRef,
        );
        let result = eval_expr("upper(${name})", cols);
        let sa = as_str(&result).unwrap();
        assert_eq!(sa.value(0), "HELLO");
        assert_eq!(sa.value(1), "WORLD");
    }

    #[test]
    fn concat_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "first".into(),
            Arc::new(StringArray::from(vec!["John", "Jane"])) as ArrayRef,
        );
        cols.insert(
            "last".into(),
            Arc::new(StringArray::from(vec!["Doe", "Smith"])) as ArrayRef,
        );
        let result = eval_expr("concat(${first}, \" \", ${last})", cols);
        let sa = as_str(&result).unwrap();
        assert_eq!(sa.value(0), "John Doe");
        assert_eq!(sa.value(1), "Jane Smith");
    }

    #[test]
    fn if_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "age".into(),
            Arc::new(Int64Array::from(vec![15, 25])) as ArrayRef,
        );
        let result = eval_expr("if(${age} >= 18, \"adult\", \"minor\")", cols);
        let sa = as_str(&result).unwrap();
        assert_eq!(sa.value(0), "minor");
        assert_eq!(sa.value(1), "adult");
    }

    #[test]
    fn coalesce_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Int64Array::from(vec![None, Some(5), None])) as ArrayRef,
        );
        cols.insert(
            "y".into(),
            Arc::new(Int64Array::from(vec![Some(10), None, Some(20)])) as ArrayRef,
        );
        let result = eval_expr("coalesce(${x}, ${y})", cols);
        let ia = as_i64(&result).unwrap();
        assert_eq!(ia.value(0), 10);
        assert_eq!(ia.value(1), 5);
        assert_eq!(ia.value(2), 20);
    }

    #[test]
    fn modulo() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Int64Array::from(vec![7, 10, 15])) as ArrayRef,
        );
        let result = eval_expr("${x} % 3", cols);
        let ia = as_i64(&result).unwrap();
        assert_eq!(ia.values(), &[1, 1, 0]);
    }

    #[test]
    fn unary_neg() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Int64Array::from(vec![1, -2, 3])) as ArrayRef,
        );
        let result = eval_expr("-${x}", cols);
        let ia = as_i64(&result).unwrap();
        assert_eq!(ia.values(), &[-1, 2, -3]);
    }

    #[test]
    fn complex_expression() {
        let mut cols = HashMap::new();
        cols.insert(
            "price".into(),
            Arc::new(Float64Array::from(vec![100.0, 200.0])) as ArrayRef,
        );
        cols.insert(
            "qty".into(),
            Arc::new(Int64Array::from(vec![2, 3])) as ArrayRef,
        );
        let result = eval_expr("round(${price} * ${qty} * 1.1, 2)", cols);
        let fa = as_f64(&result).unwrap();
        assert!((fa.value(0) - 220.0).abs() < 1e-10);
        assert!((fa.value(1) - 660.0).abs() < 1e-10);
    }

    #[test]
    fn len_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "s".into(),
            Arc::new(StringArray::from(vec!["hi", "hello"])) as ArrayRef,
        );
        let result = eval_expr("len(${s})", cols);
        let ia = as_i64(&result).unwrap();
        assert_eq!(ia.values(), &[2, 5]);
    }

    #[test]
    fn string_add_concat() {
        let mut cols = HashMap::new();
        cols.insert(
            "a".into(),
            Arc::new(StringArray::from(vec!["hello", "foo"])) as ArrayRef,
        );
        cols.insert(
            "b".into(),
            Arc::new(StringArray::from(vec![" world", "bar"])) as ArrayRef,
        );
        let result = eval_expr("${a} + ${b}", cols);
        let sa = as_str(&result).unwrap();
        assert_eq!(sa.value(0), "hello world");
        assert_eq!(sa.value(1), "foobar");
    }

    #[test]
    fn param_ref() {
        let ast = parser::parse("${price} * ${param.tax_rate}").unwrap();
        let mut cols = HashMap::new();
        cols.insert(
            "price".into(),
            Arc::new(Float64Array::from(vec![100.0, 200.0])) as ArrayRef,
        );
        let mut params = HashMap::new();
        params.insert("tax_rate".into(), "0.08".into());
        let ctx = EvalContext {
            columns: &cols,
            params: &params,
            row_count: 2,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        let fa = as_f64(&result).unwrap();
        assert!((fa.value(0) - 8.0).abs() < 1e-10);
        assert!((fa.value(1) - 16.0).abs() < 1e-10);
    }

    #[test]
    fn cast_int_from_float() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Float64Array::from(vec![3.7, 5.2])) as ArrayRef,
        );
        let result = eval_expr("cast_int(${x})", cols);
        let ia = as_i64(&result).unwrap();
        assert_eq!(ia.values(), &[3, 5]);
    }

    #[test]
    fn nullif_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Int64Array::from(vec![1, 0, 3])) as ArrayRef,
        );
        let result = eval_expr("nullif(${x}, 0)", cols);
        let ia = as_i64(&result).unwrap();
        assert_eq!(ia.value(0), 1);
        assert!(ia.is_null(1));
        assert_eq!(ia.value(2), 3);
    }

    #[test]
    fn min_max_functions() {
        let mut cols = HashMap::new();
        cols.insert(
            "a".into(),
            Arc::new(Int64Array::from(vec![1, 5, 3])) as ArrayRef,
        );
        cols.insert(
            "b".into(),
            Arc::new(Int64Array::from(vec![4, 2, 6])) as ArrayRef,
        );
        let result_min = eval_expr("min(${a}, ${b})", cols.clone());
        let result_max = eval_expr("max(${a}, ${b})", cols);
        let fa_min = as_f64(&result_min).unwrap();
        let fa_max = as_f64(&result_max).unwrap();
        assert_eq!(fa_min.values(), &[1.0, 2.0, 3.0]);
        assert_eq!(fa_max.values(), &[4.0, 5.0, 6.0]);
    }

    #[test]
    fn clamp_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Float64Array::from(vec![-5.0, 50.0, 150.0])) as ArrayRef,
        );
        let result = eval_expr("clamp(${x}, 0.0, 100.0)", cols);
        let fa = as_f64(&result).unwrap();
        assert_eq!(fa.values(), &[0.0, 50.0, 100.0]);
    }

    #[test]
    fn substr_utf8_safe() {
        let mut cols = HashMap::new();
        // "héllo" has a multi-byte char at position 1
        cols.insert(
            "s".into(),
            Arc::new(StringArray::from(vec!["héllo", "日本語テスト"])) as ArrayRef,
        );
        // substr(s, 1, 3) should return chars 1..4 (character-based)
        let result = eval_expr("substr(${s}, 1, 3)", cols);
        let sa = as_str(&result).unwrap();
        assert_eq!(sa.value(0), "éll");
        assert_eq!(sa.value(1), "本語テ");
    }

    #[test]
    fn coalesce_with_null_literal() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(StringArray::from(vec![None, Some("val")])) as ArrayRef,
        );
        let result = eval_expr("coalesce(null, ${x})", cols);
        let sa = as_str(&result).unwrap();
        // First row: null coalesces to "val"... wait, both null and x[0] are null
        // so result should be null for row 0
        assert!(sa.is_null(0));
        assert_eq!(sa.value(1), "val");
    }

    #[test]
    fn coalesce_boolean() {
        let mut cols = HashMap::new();
        cols.insert(
            "a".into(),
            Arc::new(BooleanArray::from(vec![None, Some(false)])) as ArrayRef,
        );
        cols.insert(
            "b".into(),
            Arc::new(BooleanArray::from(vec![Some(true), Some(true)])) as ArrayRef,
        );
        let result = eval_expr("coalesce(${a}, ${b})", cols);
        let ba = as_bool(&result).unwrap();
        assert_eq!(ba.value(0), true);
        assert_eq!(ba.value(1), false);
    }

    #[test]
    fn nullif_boolean() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(BooleanArray::from(vec![true, false, true])) as ArrayRef,
        );
        let result = eval_expr("nullif(${x}, true)", cols);
        let ba = as_bool(&result).unwrap();
        assert!(ba.is_null(0));
        assert_eq!(ba.value(1), false);
        assert!(ba.is_null(2));
    }

    #[test]
    fn null_arith_propagation() {
        let cols = HashMap::new();
        let ast = parser::parse("null + 1").unwrap();
        let ctx = EvalContext {
            columns: &cols,
            params: &HashMap::new(),
            row_count: 2,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        let fa = as_f64(&result).unwrap();
        assert!(fa.is_null(0));
        assert!(fa.is_null(1));
    }

    // ─── Phase 2 tests ─────────────────────────────────────────────────────

    #[test]
    fn sqrt_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Float64Array::from(vec![Some(4.0), Some(9.0), Some(-1.0), None])) as ArrayRef,
        );
        let result = eval_expr("sqrt(${x})", cols);
        let fa = as_f64(&result).unwrap();
        assert!((fa.value(0) - 2.0).abs() < 1e-10);
        assert!((fa.value(1) - 3.0).abs() < 1e-10);
        assert!(fa.is_null(2)); // sqrt(-1) → null
        assert!(fa.is_null(3)); // null propagation
    }

    #[test]
    fn pow_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "base".into(),
            Arc::new(Float64Array::from(vec![2.0, 10.0, 0.0])) as ArrayRef,
        );
        cols.insert(
            "exp".into(),
            Arc::new(Float64Array::from(vec![3.0, 2.0, 0.0])) as ArrayRef,
        );
        let result = eval_expr("pow(${base}, ${exp})", cols);
        let fa = as_f64(&result).unwrap();
        assert!((fa.value(0) - 8.0).abs() < 1e-10);
        assert!((fa.value(1) - 100.0).abs() < 1e-10);
        assert!((fa.value(2) - 1.0).abs() < 1e-10); // 0^0 = 1
    }

    #[test]
    fn ln_and_exp_functions() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Float64Array::from(vec![Some(1.0), Some(std::f64::consts::E), Some(0.0), Some(-1.0)])) as ArrayRef,
        );
        let result = eval_expr("ln(${x})", cols.clone());
        let fa = as_f64(&result).unwrap();
        assert!((fa.value(0) - 0.0).abs() < 1e-10);
        assert!((fa.value(1) - 1.0).abs() < 1e-10);
        assert!(fa.is_null(2)); // ln(0) → null
        assert!(fa.is_null(3)); // ln(-1) → null

        let result2 = eval_expr("exp(${x})", cols);
        let fa2 = as_f64(&result2).unwrap();
        assert!((fa2.value(0) - std::f64::consts::E).abs() < 1e-10);
    }

    #[test]
    fn log_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Float64Array::from(vec![8.0, 100.0])) as ArrayRef,
        );
        cols.insert(
            "b".into(),
            Arc::new(Float64Array::from(vec![2.0, 10.0])) as ArrayRef,
        );
        let result = eval_expr("log(${x}, ${b})", cols);
        let fa = as_f64(&result).unwrap();
        assert!((fa.value(0) - 3.0).abs() < 1e-10);
        assert!((fa.value(1) - 2.0).abs() < 1e-10);
    }

    #[test]
    fn left_right_functions() {
        let mut cols = HashMap::new();
        cols.insert(
            "s".into(),
            Arc::new(StringArray::from(vec!["hello", "日本語テスト"])) as ArrayRef,
        );
        let result_l = eval_expr("left(${s}, 3)", cols.clone());
        let sa = as_str(&result_l).unwrap();
        assert_eq!(sa.value(0), "hel");
        assert_eq!(sa.value(1), "日本語");

        let result_r = eval_expr("right(${s}, 3)", cols);
        let sa = as_str(&result_r).unwrap();
        assert_eq!(sa.value(0), "llo");
        assert_eq!(sa.value(1), "テスト");
    }

    #[test]
    fn pad_left_right() {
        let mut cols = HashMap::new();
        cols.insert(
            "s".into(),
            Arc::new(StringArray::from(vec!["42", "hello"])) as ArrayRef,
        );
        let result = eval_expr("pad_left(${s}, 5, \"0\")", cols.clone());
        let sa = as_str(&result).unwrap();
        assert_eq!(sa.value(0), "00042");
        assert_eq!(sa.value(1), "hello"); // already 5 chars

        let result2 = eval_expr("pad_right(${s}, 6, \".\")", cols);
        let sa2 = as_str(&result2).unwrap();
        assert_eq!(sa2.value(0), "42....");
        assert_eq!(sa2.value(1), "hello.");
    }

    #[test]
    fn starts_ends_contains() {
        let mut cols = HashMap::new();
        cols.insert(
            "s".into(),
            Arc::new(StringArray::from(vec!["hello world", "foobar"])) as ArrayRef,
        );
        let r1 = eval_expr("starts_with(${s}, \"hello\")", cols.clone());
        let ba1 = as_bool(&r1).unwrap();
        assert!(ba1.value(0));
        assert!(!ba1.value(1));

        let r2 = eval_expr("ends_with(${s}, \"bar\")", cols.clone());
        let ba2 = as_bool(&r2).unwrap();
        assert!(!ba2.value(0));
        assert!(ba2.value(1));

        let r3 = eval_expr("contains(${s}, \"oo\")", cols);
        let ba3 = as_bool(&r3).unwrap();
        assert!(!ba3.value(0));
        assert!(ba3.value(1));
    }

    #[test]
    fn hash_function() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(StringArray::from(vec![Some("hello"), Some("world"), None])) as ArrayRef,
        );
        let result = eval_expr("hash(${x})", cols.clone());
        let ia = as_i64(&result).unwrap();
        assert!(!ia.is_null(0));
        assert!(!ia.is_null(1));
        assert!(ia.is_null(2)); // null → null
        assert_ne!(ia.value(0), ia.value(1)); // different inputs → different hashes

        // Deterministic: same input → same hash
        let result2 = eval_expr("hash(${x})", cols);
        let ia2 = as_i64(&result2).unwrap();
        assert_eq!(ia.value(0), ia2.value(0));
    }

    #[test]
    fn row_number_function() {
        let ast = parser::parse("row_number()").unwrap();
        let cols = HashMap::new();
        let ctx = EvalContext {
            columns: &cols,
            params: &HashMap::new(),
            row_count: 5,
            row_offset: 100,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        let ia = as_i64(&result).unwrap();
        assert_eq!(ia.values(), &[100, 101, 102, 103, 104]);
    }

    #[test]
    fn case_function_string() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Int64Array::from(vec![5, -3, 0])) as ArrayRef,
        );
        let result = eval_expr(
            "case(${x} > 0, \"positive\", ${x} < 0, \"negative\", \"zero\")",
            cols,
        );
        let sa = as_str(&result).unwrap();
        assert_eq!(sa.value(0), "positive");
        assert_eq!(sa.value(1), "negative");
        assert_eq!(sa.value(2), "zero");
    }

    #[test]
    fn case_function_no_default() {
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Int64Array::from(vec![1, 2, 3])) as ArrayRef,
        );
        let result = eval_expr("case(${x} == 1, \"one\", ${x} == 2, \"two\")", cols);
        let sa = as_str(&result).unwrap();
        assert_eq!(sa.value(0), "one");
        assert_eq!(sa.value(1), "two");
        assert!(sa.is_null(2)); // no match, no default → null
    }

    #[test]
    fn case_function_numeric() {
        let mut cols = HashMap::new();
        cols.insert(
            "grade".into(),
            Arc::new(StringArray::from(vec!["A", "B", "C"])) as ArrayRef,
        );
        let result = eval_expr(
            "case(${grade} == \"A\", 4.0, ${grade} == \"B\", 3.0, 2.0)",
            cols,
        );
        let fa = as_f64(&result).unwrap();
        assert!((fa.value(0) - 4.0).abs() < 1e-10);
        assert!((fa.value(1) - 3.0).abs() < 1e-10);
        assert!((fa.value(2) - 2.0).abs() < 1e-10);
    }

    #[test]
    fn case_mixed_int_float() {
        // case(false, 1, 2.5) — Int64 then branch, Float64 default
        // Should promote to Float64
        let mut cols = HashMap::new();
        cols.insert(
            "x".into(),
            Arc::new(Int64Array::from(vec![0, 1])) as ArrayRef,
        );
        let result = eval_expr("case(${x} == 1, 1, 2.5)", cols);
        let fa = as_f64(&result).unwrap();
        assert!((fa.value(0) - 2.5).abs() < 1e-10); // default
        assert!((fa.value(1) - 1.0).abs() < 1e-10); // matched
    }

    // ─── Date/time tests ───────────────────────────────────────────────────

    #[test]
    fn make_date_function() {
        let cols = HashMap::new();
        let ast = parser::parse("make_date(2024, 3, 15)").unwrap();
        let ctx = EvalContext {
            columns: &cols,
            params: &HashMap::new(),
            row_count: 1,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        // Should be a timestamp, extract year to verify
        let millis = as_millis(&result).unwrap();
        let dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 3);
        assert_eq!(dt.day(), 15);
    }

    #[test]
    fn make_date_invalid_returns_null() {
        let cols = HashMap::new();
        // Feb 30 doesn't exist
        let ast = parser::parse("make_date(2024, 2, 30)").unwrap();
        let ctx = EvalContext {
            columns: &cols,
            params: &HashMap::new(),
            row_count: 1,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        assert!(result.is_null(0));
    }

    #[test]
    fn make_time_function() {
        let cols = HashMap::new();
        let ast = parser::parse("make_time(14, 30, 45)").unwrap();
        let ctx = EvalContext {
            columns: &cols,
            params: &HashMap::new(),
            row_count: 1,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        let ia = as_i64(&result).unwrap();
        // 14*3600000 + 30*60000 + 45*1000 = 52245000
        assert_eq!(ia.value(0), 52_245_000);
    }

    #[test]
    fn make_time_invalid_returns_null() {
        let cols = HashMap::new();
        let ast = parser::parse("make_time(25, 0, 0)").unwrap();
        let ctx = EvalContext {
            columns: &cols,
            params: &HashMap::new(),
            row_count: 1,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        assert!(result.is_null(0));
    }

    #[test]
    fn make_datetime_function() {
        let cols = HashMap::new();
        let ast = parser::parse("make_datetime(2024, 3, 15, 14, 30, 0)").unwrap();
        let ctx = EvalContext {
            columns: &cols,
            params: &HashMap::new(),
            row_count: 1,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        let millis = as_millis(&result).unwrap();
        let dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 3);
        assert_eq!(dt.day(), 15);
        assert_eq!(dt.hour(), 14);
        assert_eq!(dt.minute(), 30);
    }

    #[test]
    fn make_duration_function() {
        let cols = HashMap::new();
        let ast = parser::parse("make_duration(30, \"day\")").unwrap();
        let ctx = EvalContext {
            columns: &cols,
            params: &HashMap::new(),
            row_count: 1,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        let ia = as_i64(&result).unwrap();
        assert_eq!(ia.value(0), 30 * 86_400_000);
    }

    #[test]
    fn to_date_function() {
        let cols = HashMap::new();
        let ast = parser::parse("to_date(\"2024-03-15\", \"%Y-%m-%d\")").unwrap();
        let ctx = EvalContext {
            columns: &cols,
            params: &HashMap::new(),
            row_count: 1,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        let millis = as_millis(&result).unwrap();
        let dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 3);
        assert_eq!(dt.day(), 15);
    }

    #[test]
    fn to_datetime_function() {
        let cols = HashMap::new();
        let ast = parser::parse("to_datetime(\"2024-03-15 14:30\", \"%Y-%m-%d %H:%M\")").unwrap();
        let ctx = EvalContext {
            columns: &cols,
            params: &HashMap::new(),
            row_count: 1,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        let millis = as_millis(&result).unwrap();
        let dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.hour(), 14);
        assert_eq!(dt.minute(), 30);
    }

    #[test]
    fn epoch_seconds_function() {
        let mut cols = HashMap::new();
        // 2024-01-01 00:00:00 UTC = 1704067200000 ms
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![1704067200000i64])) as ArrayRef,
        );
        let result = eval_expr("epoch_seconds(${ts})", cols);
        let fa = as_f64(&result).unwrap();
        assert!((fa.value(0) - 1704067200.0).abs() < 1e-3);
    }

    #[test]
    fn from_epoch_function() {
        let cols = HashMap::new();
        let ast = parser::parse("from_epoch(1704067200)").unwrap();
        let ctx = EvalContext {
            columns: &cols,
            params: &HashMap::new(),
            row_count: 1,
            row_offset: 0,
            seed: 0,
            call_counter: Cell::new(0),
        };
        let result = evaluate(&ast, &ctx).unwrap();
        let millis = as_millis(&result).unwrap();
        let dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(dt.year(), 2024);
        assert_eq!(dt.month(), 1);
        assert_eq!(dt.day(), 1);
    }

    #[test]
    fn date_extraction_functions() {
        let mut cols = HashMap::new();
        // 2024-03-15 14:30:45 UTC
        let dt = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap()
            .and_hms_opt(14, 30, 45).unwrap();
        let ms = datetime_to_millis(&dt);
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![ms])) as ArrayRef,
        );

        let r = eval_expr("year(${ts})", cols.clone());
        assert_eq!(as_i64(&r).unwrap().value(0), 2024);

        let r = eval_expr("month(${ts})", cols.clone());
        assert_eq!(as_i64(&r).unwrap().value(0), 3);

        let r = eval_expr("day(${ts})", cols.clone());
        assert_eq!(as_i64(&r).unwrap().value(0), 15);

        let r = eval_expr("hour(${ts})", cols.clone());
        assert_eq!(as_i64(&r).unwrap().value(0), 14);

        let r = eval_expr("minute(${ts})", cols.clone());
        assert_eq!(as_i64(&r).unwrap().value(0), 30);

        let r = eval_expr("second(${ts})", cols.clone());
        assert_eq!(as_i64(&r).unwrap().value(0), 45);

        // 2024-03-15 is a Friday → day_of_week = 4 (Mon=0)
        let r = eval_expr("day_of_week(${ts})", cols.clone());
        assert_eq!(as_i64(&r).unwrap().value(0), 4);

        // Day 75 of 2024 (leap year)
        let r = eval_expr("day_of_year(${ts})", cols.clone());
        assert_eq!(as_i64(&r).unwrap().value(0), 75);

        let r = eval_expr("quarter(${ts})", cols.clone());
        assert_eq!(as_i64(&r).unwrap().value(0), 1);

        let r = eval_expr("week_of_year(${ts})", cols);
        assert_eq!(as_i64(&r).unwrap().value(0), 11);
    }

    #[test]
    fn date_add_sub_fixed_units() {
        let mut cols = HashMap::new();
        let dt = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap()
            .and_hms_opt(12, 0, 0).unwrap();
        let ms = datetime_to_millis(&dt);
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![ms])) as ArrayRef,
        );

        // Add 7 days
        let r = eval_expr("date_add(${ts}, 7, \"day\")", cols.clone());
        let millis = as_millis(&r).unwrap();
        let result_dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(result_dt.day(), 22);

        // Subtract 2 hours
        let r = eval_expr("date_sub(${ts}, 2, \"hour\")", cols);
        let millis = as_millis(&r).unwrap();
        let result_dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(result_dt.hour(), 10);
    }

    #[test]
    fn date_add_month_clamp() {
        // Jan 31 + 1 month should clamp to Feb 29 (2024 is leap year)
        let mut cols = HashMap::new();
        let dt = NaiveDate::from_ymd_opt(2024, 1, 31).unwrap()
            .and_hms_opt(0, 0, 0).unwrap();
        let ms = datetime_to_millis(&dt);
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![ms])) as ArrayRef,
        );
        let r = eval_expr("date_add(${ts}, 1, \"month\")", cols);
        let millis = as_millis(&r).unwrap();
        let result_dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(result_dt.month(), 2);
        assert_eq!(result_dt.day(), 29); // clamped to leap year Feb
    }

    #[test]
    fn date_diff_function() {
        let mut cols = HashMap::new();
        let dt1 = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap()
            .and_hms_opt(0, 0, 0).unwrap();
        let dt2 = NaiveDate::from_ymd_opt(2024, 1, 15).unwrap()
            .and_hms_opt(0, 0, 0).unwrap();
        cols.insert(
            "d1".into(),
            Arc::new(TimestampMillisecondArray::from(vec![datetime_to_millis(&dt1)])) as ArrayRef,
        );
        cols.insert(
            "d2".into(),
            Arc::new(TimestampMillisecondArray::from(vec![datetime_to_millis(&dt2)])) as ArrayRef,
        );

        let r = eval_expr("date_diff(${d1}, ${d2}, \"day\")", cols.clone());
        assert_eq!(as_i64(&r).unwrap().value(0), 60); // 2024 is leap year: Jan has 31, Feb has 29

        let r = eval_expr("date_diff(${d1}, ${d2}, \"month\")", cols);
        assert_eq!(as_i64(&r).unwrap().value(0), 2);
    }

    #[test]
    fn duration_add_function() {
        let mut cols = HashMap::new();
        let dt = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()
            .and_hms_opt(0, 0, 0).unwrap();
        let ms = datetime_to_millis(&dt);
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![ms])) as ArrayRef,
        );
        cols.insert(
            "dur".into(),
            Arc::new(Int64Array::from(vec![3_600_000i64])) as ArrayRef, // 1 hour in ms
        );
        let r = eval_expr("duration_add(${ts}, ${dur})", cols);
        let millis = as_millis(&r).unwrap();
        let result_dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(result_dt.hour(), 1);
    }

    #[test]
    fn start_of_function() {
        let mut cols = HashMap::new();
        let dt = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap()
            .and_hms_opt(14, 30, 45).unwrap();
        let ms = datetime_to_millis(&dt);
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![ms])) as ArrayRef,
        );

        let r = eval_expr("start_of(${ts}, \"day\")", cols.clone());
        let millis = as_millis(&r).unwrap();
        let result_dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(result_dt.hour(), 0);
        assert_eq!(result_dt.minute(), 0);
        assert_eq!(result_dt.second(), 0);
        assert_eq!(result_dt.day(), 15);

        let r = eval_expr("start_of(${ts}, \"month\")", cols.clone());
        let millis = as_millis(&r).unwrap();
        let result_dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(result_dt.day(), 1);
        assert_eq!(result_dt.month(), 3);

        let r = eval_expr("start_of(${ts}, \"year\")", cols);
        let millis = as_millis(&r).unwrap();
        let result_dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(result_dt.day(), 1);
        assert_eq!(result_dt.month(), 1);
    }

    #[test]
    fn end_of_function() {
        let mut cols = HashMap::new();
        let dt = NaiveDate::from_ymd_opt(2024, 2, 15).unwrap()
            .and_hms_opt(10, 0, 0).unwrap();
        let ms = datetime_to_millis(&dt);
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![ms])) as ArrayRef,
        );

        let r = eval_expr("end_of(${ts}, \"month\")", cols.clone());
        let millis = as_millis(&r).unwrap();
        let result_dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(result_dt.day(), 29); // Feb 2024 is leap year
        assert_eq!(result_dt.hour(), 23);
        assert_eq!(result_dt.minute(), 59);

        let r = eval_expr("end_of(${ts}, \"day\")", cols);
        let millis = as_millis(&r).unwrap();
        let result_dt = millis_to_datetime(millis[0].unwrap()).unwrap();
        assert_eq!(result_dt.day(), 15);
        assert_eq!(result_dt.hour(), 23);
        assert_eq!(result_dt.minute(), 59);
        assert_eq!(result_dt.second(), 59);
    }

    #[test]
    fn format_date_function() {
        let mut cols = HashMap::new();
        let dt = NaiveDate::from_ymd_opt(2024, 3, 15).unwrap()
            .and_hms_opt(14, 30, 0).unwrap();
        let ms = datetime_to_millis(&dt);
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![ms])) as ArrayRef,
        );

        let r = eval_expr("format_date(${ts}, \"%Y-%m\")", cols.clone());
        let sa = as_str(&r).unwrap();
        assert_eq!(sa.value(0), "2024-03");

        let r = eval_expr("format_date(${ts}, \"%Y-%m-%d %H:%M:%S\")", cols);
        let sa = as_str(&r).unwrap();
        assert_eq!(sa.value(0), "2024-03-15 14:30:00");
    }

    #[test]
    fn format_duration_function() {
        let mut cols = HashMap::new();
        // 1 hour, 30 minutes, 45 seconds = 5445000 ms
        cols.insert(
            "dur".into(),
            Arc::new(Int64Array::from(vec![5_445_000i64])) as ArrayRef,
        );

        let r = eval_expr("format_duration(${dur}, \"hms\")", cols.clone());
        let sa = as_str(&r).unwrap();
        assert_eq!(sa.value(0), "01:30:45");

        let r = eval_expr("format_duration(${dur}, \"human\")", cols.clone());
        let sa = as_str(&r).unwrap();
        assert_eq!(sa.value(0), "1h 30m 45s");

        let r = eval_expr("format_duration(${dur}, \"iso\")", cols);
        let sa = as_str(&r).unwrap();
        assert_eq!(sa.value(0), "PT1H30M45S");
    }

    #[test]
    fn timestamp_precision_handling() {
        // Test that different timestamp precisions are handled correctly
        let mut cols = HashMap::new();
        // 2024-01-01 00:00:00 UTC in different precisions
        let epoch_ms = 1704067200000i64;
        cols.insert(
            "ts_ms".into(),
            Arc::new(TimestampMillisecondArray::from(vec![epoch_ms])) as ArrayRef,
        );
        cols.insert(
            "ts_s".into(),
            Arc::new(TimestampSecondArray::from(vec![epoch_ms / 1_000])) as ArrayRef,
        );
        cols.insert(
            "ts_us".into(),
            Arc::new(TimestampMicrosecondArray::from(vec![epoch_ms * 1_000])) as ArrayRef,
        );
        cols.insert(
            "ts_ns".into(),
            Arc::new(TimestampNanosecondArray::from(vec![epoch_ms * 1_000_000])) as ArrayRef,
        );

        // All should extract year=2024
        for field in &["ts_ms", "ts_s", "ts_us", "ts_ns"] {
            let r = eval_expr(&format!("year(${{{field}}})"), cols.clone());
            let ia = as_i64(&r).unwrap();
            assert_eq!(ia.value(0), 2024, "year extraction failed for {field}");
        }
    }

    #[test]
    fn datetime_null_propagation() {
        let mut cols = HashMap::new();
        cols.insert(
            "ts".into(),
            Arc::new(TimestampMillisecondArray::from(vec![None::<i64>])) as ArrayRef,
        );
        let r = eval_expr("year(${ts})", cols.clone());
        assert!(r.is_null(0));

        let r = eval_expr("format_date(${ts}, \"%Y\")", cols);
        assert!(r.is_null(0));
    }

    // ─── Random function tests ─────────────────────────────────────

    fn eval_seeded(expr_str: &str, seed: u64, row_count: usize) -> ArrayRef {
        let ast = parser::parse(expr_str).unwrap();
        let cols = HashMap::new();
        let params = HashMap::new();
        let ctx = EvalContext {
            columns: &cols,
            params: &params,
            row_count,
            row_offset: 0,
            seed,
            call_counter: Cell::new(0),
        };
        evaluate(&ast, &ctx).unwrap()
    }

    #[test]
    fn random_int_in_range() {
        let r = eval_seeded("random_int(1, 10)", 42, 100);
        let arr = r.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..100 {
            let v = arr.value(i);
            assert!(v >= 1 && v <= 10, "value {v} out of range [1, 10]");
        }
    }

    #[test]
    fn random_int_deterministic() {
        let r1 = eval_seeded("random_int(0, 1000)", 123, 50);
        let r2 = eval_seeded("random_int(0, 1000)", 123, 50);
        let a1 = r1.as_any().downcast_ref::<Int64Array>().unwrap();
        let a2 = r2.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..50 {
            assert_eq!(a1.value(i), a2.value(i));
        }
    }

    #[test]
    fn random_int_different_seeds() {
        let r1 = eval_seeded("random_int(0, 1000000)", 1, 10);
        let r2 = eval_seeded("random_int(0, 1000000)", 2, 10);
        let a1 = r1.as_any().downcast_ref::<Int64Array>().unwrap();
        let a2 = r2.as_any().downcast_ref::<Int64Array>().unwrap();
        // With different seeds, at least one value should differ
        let any_diff = (0..10).any(|i| a1.value(i) != a2.value(i));
        assert!(any_diff, "different seeds should produce different values");
    }

    #[test]
    fn random_float_in_range() {
        let r = eval_seeded("random_float(0.0, 1.0)", 42, 100);
        let arr = r.as_any().downcast_ref::<Float64Array>().unwrap();
        for i in 0..100 {
            let v = arr.value(i);
            assert!(v >= 0.0 && v < 1.0, "value {v} out of range [0, 1)");
        }
    }

    #[test]
    fn random_float_deterministic() {
        let r1 = eval_seeded("random_float(0.0, 100.0)", 77, 30);
        let r2 = eval_seeded("random_float(0.0, 100.0)", 77, 30);
        let a1 = r1.as_any().downcast_ref::<Float64Array>().unwrap();
        let a2 = r2.as_any().downcast_ref::<Float64Array>().unwrap();
        for i in 0..30 {
            assert_eq!(a1.value(i), a2.value(i));
        }
    }

    #[test]
    fn random_int_min_equals_max() {
        let r = eval_seeded("random_int(5, 5)", 42, 10);
        let arr = r.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..10 {
            assert_eq!(arr.value(i), 5);
        }
    }

    #[test]
    fn random_int_min_greater_than_max_null() {
        let r = eval_seeded("random_int(10, 1)", 42, 5);
        let arr = r.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..5 {
            assert!(arr.is_null(i));
        }
    }

    #[test]
    fn random_int_large_range_no_overflow() {
        // Test with full i64 range — should not panic
        let ast = parser::parse("random_int(${lo}, ${hi})").unwrap();
        let mut cols = HashMap::new();
        cols.insert(
            "lo".into(),
            Arc::new(Int64Array::from(vec![0i64, i64::MIN])) as ArrayRef,
        );
        cols.insert(
            "hi".into(),
            Arc::new(Int64Array::from(vec![i64::MAX, i64::MAX])) as ArrayRef,
        );
        let params = HashMap::new();
        let ctx = EvalContext {
            columns: &cols,
            params: &params,
            row_count: 2,
            row_offset: 0,
            seed: 42,
            call_counter: Cell::new(0),
        };
        let r = evaluate(&ast, &ctx).unwrap();
        let arr = r.as_any().downcast_ref::<Int64Array>().unwrap();
        // Row 0: [0, i64::MAX] — result should be in range
        assert!(arr.value(0) >= 0);
        // Row 1: [i64::MIN, i64::MAX] — full range, should not panic
        assert!(!arr.is_null(1));
    }

    #[test]
    fn multiple_random_calls_independent() {
        // Two random_int calls in the same expression should produce
        // different streams (different call-site IDs).
        let ast = parser::parse("random_int(0, 1000000) - random_int(0, 1000000)").unwrap();
        let cols = HashMap::new();
        let params = HashMap::new();
        let ctx = EvalContext {
            columns: &cols,
            params: &params,
            row_count: 20,
            row_offset: 0,
            seed: 99,
            call_counter: Cell::new(0),
        };
        let r = evaluate(&ast, &ctx).unwrap();
        let arr = as_f64(&r).unwrap();
        // If both calls used the same stream, all differences would be 0
        let any_nonzero = (0..20).any(|i| arr.value(i) != 0.0);
        assert!(any_nonzero, "two random_int calls should produce different values");
    }

    #[test]
    fn random_duration_in_range() {
        // random_duration between 0ms and 86400000ms (24h)
        let r = eval_seeded("random_duration(0, 86400000)", 42, 50);
        let arr = r.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..50 {
            let v = arr.value(i);
            assert!(v >= 0 && v <= 86_400_000, "duration {v} out of range");
        }
    }

    #[test]
    fn random_batch_size_independent() {
        // Row 5 should produce the same value regardless of batch size.
        // This is guaranteed because we use row_offset + row_index, not RNG state.
        let ast = parser::parse("random_int(0, 1000000)").unwrap();
        let cols = HashMap::new();
        let params = HashMap::new();

        // Batch of 10 starting at row 0 — get row 5
        let ctx1 = EvalContext {
            columns: &cols,
            params: &params,
            row_count: 10,
            row_offset: 0,
            seed: 42,
            call_counter: Cell::new(0),
        };
        let r1 = evaluate(&ast, &ctx1).unwrap();
        let a1 = r1.as_any().downcast_ref::<Int64Array>().unwrap();
        let val_from_big_batch = a1.value(5);

        // Batch of 3 starting at row 5 — get row 0 (which is absolute row 5)
        let ctx2 = EvalContext {
            columns: &cols,
            params: &params,
            row_count: 3,
            row_offset: 5,
            seed: 42,
            call_counter: Cell::new(0),
        };
        let r2 = evaluate(&ast, &ctx2).unwrap();
        let a2 = r2.as_any().downcast_ref::<Int64Array>().unwrap();
        let val_from_small_batch = a2.value(0);

        assert_eq!(val_from_big_batch, val_from_small_batch,
            "same absolute row with same seed should produce same value");
    }
}
