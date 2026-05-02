//! UUID v4 generator with deterministic output.

use std::sync::Arc;

use arrow::array::{ArrayRef, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Generates random UUID v4 strings.
///
/// Uses the provided RNG to construct UUID bytes, ensuring deterministic
/// output for a given seed.
pub struct UuidGenerator;

impl FieldGenerator for UuidGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        let values: Vec<String> = (0..count)
            .map(|_| {
                let mut bytes = [0u8; 16];
                rng.fill_bytes(&mut bytes);
                let uuid = uuid::Builder::from_random_bytes(bytes).into_uuid();
                uuid.to_string()
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
