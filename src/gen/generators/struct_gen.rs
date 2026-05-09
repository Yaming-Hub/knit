//! Struct (nested object) generator.
//!
//! Produces an Arrow [`StructArray`] by running child generators and assembling
//! their outputs as struct fields. Supports arbitrary nesting depth.

use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanArray, Int64Array, StructArray};
use arrow::datatypes::{DataType, Field as ArrowField};
use rand::RngCore;

use crate::gen::context::GenContext;
use crate::gen::null_mask::apply_null_mask;
use crate::gen::traits::FieldGenerator;
use crate::plan::NullPlan;

/// Post-processing configuration for a single child field within a struct.
#[derive(Debug, Clone)]
pub struct ChildPostProcess {
    /// Optional precision for float rounding.
    pub precision: Option<u8>,
    /// Declared data type for logical coercion (e.g., Bool, Int32).
    pub data_type: crate::core::DataType,
    /// Null plan for injecting nulls.
    pub null_plan: NullPlan,
}

/// Generator that produces an Arrow `StructArray` from child field generators.
pub struct StructGenerator {
    /// Child generators, one per sub-field.
    children: Vec<Box<dyn FieldGenerator>>,
    /// Child field names (parallel to `children`).
    field_names: Vec<String>,
    /// Post-processing config for each child (parallel to `children`).
    post_process: Vec<ChildPostProcess>,
}

impl StructGenerator {
    /// Create a new struct generator with the given child generators, names,
    /// and per-child post-processing configuration.
    pub fn new(
        children: Vec<Box<dyn FieldGenerator>>,
        field_names: Vec<String>,
        post_process: Vec<ChildPostProcess>,
    ) -> Self {
        Self {
            children,
            field_names,
            post_process,
        }
    }
}

/// Apply precision rounding to a float array.
fn apply_precision(arr: ArrayRef, precision: Option<u8>) -> ArrayRef {
    let Some(places) = precision else {
        return arr;
    };
    if let Some(float_arr) = arr.as_any().downcast_ref::<arrow::array::Float64Array>() {
        let factor = 10f64.powi(places as i32);
        let rounded: arrow::array::Float64Array = float_arr
            .iter()
            .map(|v| v.map(|x| (x * factor).round() / factor))
            .collect();
        Arc::new(rounded)
    } else {
        arr
    }
}

/// Coerce a generated array to match the declared logical data type.
fn coerce_to_logical_type(
    arr: ArrayRef,
    data_type: &crate::core::DataType,
) -> ArrayRef {
    match data_type {
        crate::core::DataType::Bool => {
            if arr.as_any().is::<BooleanArray>() {
                return arr;
            }
            if let Some(i64_arr) = arr.as_any().downcast_ref::<Int64Array>() {
                let bools: BooleanArray =
                    i64_arr.iter().map(|v| v.map(|x| x != 0)).collect();
                return Arc::new(bools);
            }
            arr
        }
        crate::core::DataType::Int32 => {
            if arr.as_any().is::<arrow::array::Int32Array>() {
                return arr;
            }
            if let Some(i64_arr) = arr.as_any().downcast_ref::<Int64Array>() {
                let i32s: arrow::array::Int32Array = i64_arr
                    .iter()
                    .map(|v| v.map(|x| x.clamp(i32::MIN as i64, i32::MAX as i64) as i32))
                    .collect();
                return Arc::new(i32s);
            }
            arr
        }
        _ => arr,
    }
}

impl FieldGenerator for StructGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let child_arrays: Vec<ArrayRef> = self
            .children
            .iter()
            .zip(self.post_process.iter())
            .map(|(gen, pp)| {
                let arr = gen.generate(rng, count, ctx);
                let arr = apply_precision(arr, pp.precision);
                let arr = coerce_to_logical_type(arr, &pp.data_type);
                apply_null_mask(arr, &pp.null_plan, rng, count).unwrap_or_else(|e| {
                    tracing::warn!("null mask failed in struct child: {}", e);
                    gen.generate(rng, count, ctx)
                })
            })
            .collect();

        let arrow_fields: Vec<ArrowField> = self
            .field_names
            .iter()
            .zip(child_arrays.iter())
            .map(|(name, arr)| ArrowField::new(name, arr.data_type().clone(), true))
            .collect();

        let struct_array =
            StructArray::try_new(arrow_fields.into(), child_arrays, None).unwrap_or_else(|e| {
                tracing::error!("Failed to create StructArray: {}", e);
                // Fallback: empty struct
                StructArray::new_empty_fields(count, None)
            });
        Arc::new(struct_array)
    }

    fn output_type(&self) -> DataType {
        let fields: Vec<ArrowField> = self
            .children
            .iter()
            .zip(self.field_names.iter())
            .map(|(gen, name)| ArrowField::new(name, gen.output_type(), true))
            .collect();
        DataType::Struct(fields.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::generators::constant::ConstantGenerator;
    use crate::gen::generators::sequence::SequenceGenerator;
    use arrow::array::{Array, Float64Array, Int64Array};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_pp(precision: Option<u8>, data_type: crate::core::DataType) -> ChildPostProcess {
        ChildPostProcess {
            precision,
            data_type,
            null_plan: NullPlan::Never,
        }
    }

    #[test]
    fn test_struct_generator_produces_struct_array() {
        let children: Vec<Box<dyn FieldGenerator>> = vec![
            Box::new(SequenceGenerator::new(1, 1)),
            Box::new(ConstantGenerator::new(crate::core::Value::Float(2.718))),
        ];
        let names = vec!["id".to_string(), "value".to_string()];
        let post_process = vec![
            make_pp(None, crate::core::DataType::Int),
            make_pp(Some(2), crate::core::DataType::Float),
        ];
        let gen = StructGenerator::new(children, names, post_process);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let cols = HashMap::new();
        let ctx = GenContext::new(&cols, 0, 0, 1, "test");
        let result = gen.generate(&mut rng, 5, &ctx);

        // Should be a StructArray
        assert!(matches!(result.data_type(), DataType::Struct(_)));
        assert_eq!(result.len(), 5);

        let struct_arr = result
            .as_any()
            .downcast_ref::<StructArray>()
            .expect("should be StructArray");
        assert_eq!(struct_arr.num_columns(), 2);

        // Check the id column (sequence 1,2,3,4,5)
        let id_col = struct_arr
            .column(0)
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("id should be Int64");
        assert_eq!(id_col.value(0), 1);
        assert_eq!(id_col.value(4), 5);

        // Check the value column (constant 2.718, rounded to 2 decimal places = 2.72)
        let val_col = struct_arr
            .column(1)
            .as_any()
            .downcast_ref::<Float64Array>()
            .expect("value should be Float64");
        assert!((val_col.value(0) - 2.72).abs() < 0.001);
    }

    #[test]
    fn test_struct_output_type() {
        let children: Vec<Box<dyn FieldGenerator>> = vec![
            Box::new(SequenceGenerator::new(0, 1)),
            Box::new(ConstantGenerator::new(crate::core::Value::String(
                "hello".to_string(),
            ))),
        ];
        let names = vec!["count".to_string(), "label".to_string()];
        let gen = StructGenerator::new(children, names, vec![
            make_pp(None, crate::core::DataType::Int),
            make_pp(None, crate::core::DataType::String),
        ]);

        let dt = gen.output_type();
        assert!(matches!(dt, DataType::Struct(_)));
    }
}
