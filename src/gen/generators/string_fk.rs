//! Foreign-key generator for string/UUID columns — samples from a parent
//! entity's [`StringKeyStore`].
//!
//! This is the string-typed counterpart to
//! [`ForeignKeyGenerator`](crate::gen::generators::fk::ForeignKeyGenerator) which
//! handles Int64 keys. Used for UUID and string primary-key relationships.

use std::sync::Arc;

use arrow::array::{ArrayRef, StringArray};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::r#gen::context::GenContext;
use crate::r#gen::traits::{FieldGenerator, StringKeyStore};

/// Generates a column of string/UUID foreign-key values by sampling from a
/// parent entity's [`StringKeyStore`].
///
/// # Empty store safety
///
/// If the parent key store is empty (should not happen with correct
/// topological ordering) the generator produces an all-null String column and
/// logs a warning.
pub struct StringForeignKeyGenerator {
    /// Shared reference to the parent entity's string key store.
    key_store: Arc<dyn StringKeyStore>,
}

impl StringForeignKeyGenerator {
    /// Create a new string foreign-key generator backed by the given key store.
    pub fn new(key_store: Arc<dyn StringKeyStore>) -> Self {
        Self { key_store }
    }
}

impl FieldGenerator for StringForeignKeyGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        let values: Vec<Option<String>> = (0..count).map(|_| self.key_store.sample(rng)).collect();

        // If all values are None, the key store was empty — warn once.
        if values.iter().all(|v| v.is_none()) && count > 0 {
            tracing::warn!(
                entity = ctx.entity_name,
                "String FK key store is empty — producing null column"
            );
        }

        Arc::new(StringArray::from(values))
    }

    fn output_type(&self) -> DataType {
        DataType::Utf8
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::r#gen::string_keystore::InMemoryStringKeyStore;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn test_ctx() -> GenContext<'static> {
        static EMPTY_COLS: std::sync::LazyLock<HashMap<String, ArrayRef>> =
            std::sync::LazyLock::new(HashMap::new);
        static EMPTY_PARAMS: std::sync::LazyLock<HashMap<String, String>> =
            std::sync::LazyLock::new(HashMap::new);
        GenContext {
            batch_columns: &EMPTY_COLS,
            row_offset: 0,
            partition_index: 0,
            partition_count: 1,
            entity_name: "test_entity",
            params: &EMPTY_PARAMS,
        }
    }

    #[test]
    fn generates_from_store() {
        let store = Arc::new(InMemoryStringKeyStore::new());
        store.insert("uuid-aaa".to_string());
        store.insert("uuid-bbb".to_string());
        store.insert("uuid-ccc".to_string());

        let r#gen = StringForeignKeyGenerator::new(store);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 10, &ctx);

        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(str_arr.len(), 10);
        for i in 0..10 {
            let v = str_arr.value(i);
            assert!(
                v == "uuid-aaa" || v == "uuid-bbb" || v == "uuid-ccc",
                "unexpected value: {v}"
            );
        }
    }

    #[test]
    fn empty_store_produces_nulls() {
        let store = Arc::new(InMemoryStringKeyStore::new());
        let r#gen = StringForeignKeyGenerator::new(store);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 5, &ctx);

        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(str_arr.len(), 5);
        for i in 0..5 {
            assert!(str_arr.is_null(i));
        }
    }

    #[test]
    fn output_type_is_utf8() {
        let store = Arc::new(InMemoryStringKeyStore::new());
        let r#gen = StringForeignKeyGenerator::new(store);
        assert_eq!(r#gen.output_type(), DataType::Utf8);
    }

    #[test]
    fn deterministic_output() {
        let store: Arc<dyn StringKeyStore> = Arc::new(InMemoryStringKeyStore::new());
        for i in 0..10 {
            store.insert(format!("key-{i}"));
        }
        let r#gen = StringForeignKeyGenerator::new(Arc::clone(&store));
        let ctx = test_ctx();

        let mut rng1 = ChaCha8Rng::seed_from_u64(123);
        let arr1 = r#gen.generate(&mut rng1, 20, &ctx);

        let mut rng2 = ChaCha8Rng::seed_from_u64(123);
        let arr2 = r#gen.generate(&mut rng2, 20, &ctx);

        let s1 = arr1.as_any().downcast_ref::<StringArray>().unwrap();
        let s2 = arr2.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..20 {
            assert_eq!(s1.value(i), s2.value(i));
        }
    }
}
