//! Fixed-value generator.
//!
//! Produces the same constant value for every row in the batch. Useful for
//! status flags, type discriminators, or placeholder columns that will be
//! overridden by noise injection.

use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanArray, Float64Array, Int64Array, NullArray, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::core::Value;

use crate::r#gen::context::GenContext;
use crate::r#gen::traits::FieldGenerator;

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::r#gen::context::GenContext;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, 0, 0, 1, "test")
    }

    fn make_rng() -> ChaCha8Rng {
        ChaCha8Rng::seed_from_u64(42)
    }

    #[test]
    fn constant_string() {
        let r#gen = ConstantGenerator::new(Value::String("hello".to_string()));
        let arr = r#gen.generate(&mut make_rng(), 5, &make_ctx());
        assert_eq!(arr.len(), 5);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..5 {
            assert_eq!(str_arr.value(i), "hello");
        }
        assert_eq!(r#gen.output_type(), DataType::Utf8);
    }

    #[test]
    fn constant_int() {
        let r#gen = ConstantGenerator::new(Value::Int(42));
        let arr = r#gen.generate(&mut make_rng(), 3, &make_ctx());
        let int_arr = arr.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..3 {
            assert_eq!(int_arr.value(i), 42);
        }
        assert_eq!(r#gen.output_type(), DataType::Int64);
    }

    #[test]
    fn constant_float() {
        let r#gen = ConstantGenerator::new(Value::Float(std::f64::consts::PI));
        let arr = r#gen.generate(&mut make_rng(), 4, &make_ctx());
        let f_arr = arr.as_any().downcast_ref::<Float64Array>().unwrap();
        for i in 0..4 {
            assert!((f_arr.value(i) - std::f64::consts::PI).abs() < f64::EPSILON);
        }
        assert_eq!(r#gen.output_type(), DataType::Float64);
    }

    #[test]
    fn constant_bool() {
        let r#gen = ConstantGenerator::new(Value::Bool(true));
        let arr = r#gen.generate(&mut make_rng(), 3, &make_ctx());
        let b_arr = arr.as_any().downcast_ref::<BooleanArray>().unwrap();
        for i in 0..3 {
            assert!(b_arr.value(i));
        }
        assert_eq!(r#gen.output_type(), DataType::Boolean);
    }

    #[test]
    fn constant_null() {
        let r#gen = ConstantGenerator::new(Value::Null);
        let arr = r#gen.generate(&mut make_rng(), 5, &make_ctx());
        assert_eq!(arr.len(), 5);
        // NullArray data type is Null; every element is logically null
        assert_eq!(*arr.data_type(), DataType::Null);
        assert_eq!(r#gen.output_type(), DataType::Null);
    }

    #[test]
    fn constant_zero_count() {
        let r#gen = ConstantGenerator::new(Value::Int(1));
        let arr = r#gen.generate(&mut make_rng(), 0, &make_ctx());
        assert_eq!(arr.len(), 0);
    }
}
