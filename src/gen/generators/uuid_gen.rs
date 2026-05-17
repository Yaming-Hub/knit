//! UUID v4 generator with deterministic output.
//!
//! Generates RFC 4122 compliant UUID v4 strings using bytes drawn from the
//! provided RNG, ensuring reproducibility for a given seed. Useful for
//! generating globally unique identifiers without relying on system randomness.

use std::sync::Arc;

use arrow::array::{ArrayRef, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::r#gen::context::GenContext;
use crate::r#gen::traits::FieldGenerator;

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

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::{HashMap, HashSet};

    fn make_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext::new(map, 0, 0, 1, "test")
    }

    fn gen_uuids(count: usize, seed: u64) -> Vec<String> {
        let g = UuidGenerator;
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(seed);
        let arr = g.generate(&mut rng, count, &ctx);
        let sa = arr.as_any().downcast_ref::<StringArray>().unwrap();
        (0..sa.len()).map(|i| sa.value(i).to_string()).collect()
    }

    #[test]
    fn uuid_format() {
        let vals = gen_uuids(100, 42);
        // UUID v4 format: 8-4-4-4-12 hex digits
        for v in &vals {
            let parts: Vec<&str> = v.split('-').collect();
            assert_eq!(parts.len(), 5, "UUID should have 5 parts: {v}");
            assert_eq!(parts[0].len(), 8, "first part should be 8 chars: {v}");
            assert_eq!(parts[1].len(), 4);
            assert_eq!(parts[2].len(), 4);
            assert_eq!(parts[3].len(), 4);
            assert_eq!(parts[4].len(), 12, "last part should be 12 chars: {v}");
            // All lowercase hex
            for part in &parts {
                assert!(
                    part.chars().all(|c| c.is_ascii_hexdigit()),
                    "UUID parts should be hex: {v}"
                );
            }
            assert_eq!(v, &v.to_ascii_lowercase(), "UUID should be lowercase: {v}");
        }
    }

    #[test]
    fn uuid_v4_version_bits() {
        let vals = gen_uuids(50, 1);
        for v in &vals {
            // Version 4: third group starts with '4'
            let parts: Vec<&str> = v.split('-').collect();
            assert!(
                parts[2].starts_with('4'),
                "UUID v4 third group should start with 4: {v}"
            );
            // Variant bits: fourth group starts with 8, 9, a, or b
            let variant_char = parts[3].chars().next().unwrap();
            assert!(
                "89ab".contains(variant_char),
                "UUID v4 variant char should be 8/9/a/b: {v}"
            );
        }
    }

    #[test]
    fn uuid_all_unique() {
        let vals = gen_uuids(1000, 7);
        let unique: HashSet<&str> = vals.iter().map(|s| s.as_str()).collect();
        assert_eq!(unique.len(), 1000, "all UUIDs should be unique");
    }

    #[test]
    fn uuid_deterministic() {
        let a = gen_uuids(20, 42);
        let b = gen_uuids(20, 42);
        assert_eq!(a, b, "same seed should produce same UUIDs");
    }

    #[test]
    fn uuid_different_seeds_differ() {
        let a = gen_uuids(10, 1);
        let b = gen_uuids(10, 2);
        assert_ne!(a, b, "different seeds should produce different UUIDs");
    }

    #[test]
    fn uuid_zero_count() {
        let vals = gen_uuids(0, 1);
        assert!(vals.is_empty());
    }

    #[test]
    fn uuid_output_type() {
        assert_eq!(UuidGenerator.output_type(), DataType::Utf8);
    }
}
