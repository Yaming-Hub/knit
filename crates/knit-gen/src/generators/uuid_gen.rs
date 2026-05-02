//! UUID v4 generator with deterministic output.
//!
//! Generates RFC 4122 compliant UUID v4 strings using bytes drawn from the
//! provided RNG, ensuring reproducibility for a given seed. Useful for
//! generating globally unique identifiers without relying on system randomness.

use std::sync::Arc;

use arrow::array::{ArrayRef, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Generate deterministic UUID v4 strings.
///
/// Each call to [`generate`](FieldGenerator::generate) produces `count` unique
/// UUID v4 values formatted as lowercase hyphenated strings
/// (e.g. `550e8400-e29b-41d4-a716-446655440000`).
///
/// # Determinism
///
/// Output is fully determined by the RNG state — same seed yields same UUIDs.
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
