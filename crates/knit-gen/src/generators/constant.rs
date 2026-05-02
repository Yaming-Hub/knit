//! Fixed-value generator.
//!
//! Produces the same constant value for every row in the batch. Useful for
//! status flags, type discriminators, or placeholder columns that will be
//! overridden by noise injection.

use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanArray, Float64Array, Int64Array, NullArray, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use knit_core::Value;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Produce the same [`Value`] for every row in the batch.
///
/// Handles all primitive `Value` variants: String, Int, Float, Bool, and Null.
/// Complex variants (DateTime, Array, Map) fall back to null arrays with a
/// tracing warning until dedicated generators are introduced.
pub struct ConstantGenerator {
    value: Value,
}

impl ConstantGenerator {
    /// Create a new constant generator.
    pub fn new(value: Value) -> Self {
        Self { value }
    }
}

impl FieldGenerator for ConstantGenerator {
    fn generate(&self, _rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        match &self.value {
            Value::String(s) => {
                let values: Vec<&str> = vec![s.as_str(); count];
                Arc::new(StringArray::from(values))
            }
            Value::Int(v) => {
                let values: Vec<i64> = vec![*v; count];
                Arc::new(Int64Array::from(values))
            }
            Value::Float(v) => {
                let values: Vec<f64> = vec![*v; count];
                Arc::new(Float64Array::from(values))
            }
            Value::Bool(v) => {
                let values: Vec<bool> = vec![*v; count];
                Arc::new(BooleanArray::from(values))
            }
            Value::Null => Arc::new(NullArray::new(count)),
            _ => {
                tracing::warn!("unsupported constant value type, producing nulls");
                Arc::new(NullArray::new(count))
            }
        }
    }

    fn output_type(&self) -> DataType {
        match &self.value {
            Value::String(_) => DataType::Utf8,
            Value::Int(_) => DataType::Int64,
            Value::Float(_) => DataType::Float64,
            Value::Bool(_) => DataType::Boolean,
            _ => DataType::Null,
        }
    }
}
