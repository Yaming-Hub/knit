//! Pattern-based string generator.
//!
//! Expands a simple pattern language into random strings. Supports digit,
//! letter, and uppercase placeholders with literal pass-through for any
//! other character.

use std::sync::Arc;

use arrow::array::{ArrayRef, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Generate strings by expanding a pattern template.
///
/// # Pattern language
///
/// | Token | Expansion |
/// |-------|-----------|
/// | `#`   | Random digit `0`–`9` |
/// | `?`   | Random lowercase letter `a`–`z` |
/// | `A`   | Random uppercase letter `A`–`Z` |
/// | other | Literal pass-through |
///
/// # Example
///
/// Pattern `"###-???-AAA"` might produce `"472-mxb-QWL"`.
pub struct PatternGenerator {
    pattern: String,
}

impl PatternGenerator {
    /// Create a new pattern generator.
    pub fn new(pattern: String) -> Self {
        Self { pattern }
    }

    /// Expand the pattern once using the given RNG.
    fn expand(&self, rng: &mut dyn RngCore) -> String {
        let mut result = String::with_capacity(self.pattern.len());
        for ch in self.pattern.chars() {
            match ch {
                '#' => {
                    let d = (rng.next_u32() % 10) as u8;
                    result.push((b'0' + d) as char);
                }
                '?' => {
                    let d = (rng.next_u32() % 26) as u8;
                    result.push((b'a' + d) as char);
                }
                'A' => {
                    let d = (rng.next_u32() % 26) as u8;
                    result.push((b'A' + d) as char);
                }
                other => result.push(other),
            }
        }
        result
    }
}

impl FieldGenerator for PatternGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        let values: Vec<String> = (0..count).map(|_| self.expand(rng)).collect();
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
    use arrow::array::{Array, ArrayRef};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn make_ctx() -> GenContext<'static> {
        let map: &'static HashMap<String, ArrayRef> = Box::leak(Box::new(HashMap::new()));
        GenContext {
            batch_columns: map,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test",
        }
    }

    #[test]
    fn pattern_format() {
        let gen = PatternGenerator::new("###-???-AAA".into());
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let arr = gen.generate(&mut rng, 100, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();

        for i in 0..str_arr.len() {
            let s = str_arr.value(i);
            assert_eq!(s.len(), 11, "wrong length: {s}");
            let chars: Vec<char> = s.chars().collect();
            // digits
            assert!(chars[0].is_ascii_digit());
            assert!(chars[1].is_ascii_digit());
            assert!(chars[2].is_ascii_digit());
            assert_eq!(chars[3], '-');
            // lowercase
            assert!(chars[4].is_ascii_lowercase());
            assert!(chars[5].is_ascii_lowercase());
            assert!(chars[6].is_ascii_lowercase());
            assert_eq!(chars[7], '-');
            // uppercase
            assert!(chars[8].is_ascii_uppercase());
            assert!(chars[9].is_ascii_uppercase());
            assert!(chars[10].is_ascii_uppercase());
        }
    }

    #[test]
    fn literal_passthrough() {
        let gen = PatternGenerator::new("hello-world".into());
        let ctx = make_ctx();
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let arr = gen.generate(&mut rng, 3, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..3 {
            assert_eq!(str_arr.value(i), "hello-world");
        }
    }

    #[test]
    fn deterministic() {
        let gen = PatternGenerator::new("##??AA".into());
        let ctx = make_ctx();
        let a = gen.generate(&mut ChaCha8Rng::seed_from_u64(99), 10, &ctx);
        let b = gen.generate(&mut ChaCha8Rng::seed_from_u64(99), 10, &ctx);
        let a_s = a.as_any().downcast_ref::<StringArray>().unwrap();
        let b_s = b.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..10 {
            assert_eq!(a_s.value(i), b_s.value(i));
        }
    }
}
