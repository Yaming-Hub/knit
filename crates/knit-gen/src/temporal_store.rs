//! Per-actor temporal baseline store — maps actor indices to creation timestamps.
//!
//! After an actor entity is generated, the engine captures a datetime column
//! from the batches and stores per-actor timestamps. Subsequent generators
//! (e.g. [`ActorTemporalGenerator`](crate::generators::actor_temporal::ActorTemporalGenerator))
//! use these baselines as lower bounds so behavioral timestamps always occur
//! after the actor's creation time.

use std::collections::HashMap;

use arrow::array::{Array, Int64Array};
use arrow::record_batch::RecordBatch;

/// Stores per-actor creation timestamps, keyed by (entity_name, field_name).
///
/// Values are milliseconds since epoch, indexed by actor insertion order
/// (matching the PK key store order).
#[derive(Debug, Default)]
pub struct TemporalStore {
    /// (entity_name, field_name) → Vec<Option<i64>> indexed by actor_index.
    stores: HashMap<(String, String), Vec<Option<i64>>>,
}

impl TemporalStore {
    /// Create an empty temporal store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up an actor's baseline timestamp.
    pub fn get(&self, entity: &str, field: &str, actor_idx: usize) -> Option<i64> {
        self.stores
            .get(&(entity.to_string(), field.to_string()))
            .and_then(|v| v.get(actor_idx).copied())
            .flatten()
    }

    /// Populate the store from generated batches for an actor entity.
    ///
    /// Extracts values from the specified `datetime_field` column in all batches
    /// belonging to `entity_name`. Values are stored in insertion order (matching
    /// the key store's actor index ordering).
    ///
    /// The column must be a Timestamp type (any resolution) or Date32/Int64.
    pub fn capture_from_batches(
        &mut self,
        entity_name: &str,
        datetime_field: &str,
        batches: &[(String, RecordBatch)],
    ) {
        let key = (entity_name.to_string(), datetime_field.to_string());
        let mut values: Vec<Option<i64>> = Vec::new();

        for (name, batch) in batches {
            if name != entity_name {
                continue;
            }
            let col_idx = match batch.schema().index_of(datetime_field) {
                Ok(idx) => idx,
                Err(_) => {
                    // Column missing in this batch — pad with None to maintain alignment
                    values.extend(std::iter::repeat_n(None, batch.num_rows()));
                    continue;
                }
            };
            let col = batch.column(col_idx);

            // Handle Timestamp(Millisecond, _) columns
            if let Some(ts_arr) = col
                .as_any()
                .downcast_ref::<arrow::array::TimestampMillisecondArray>()
            {
                for i in 0..ts_arr.len() {
                    if ts_arr.is_null(i) {
                        values.push(None);
                    } else {
                        values.push(Some(ts_arr.value(i)));
                    }
                }
                continue;
            }

            // Handle Timestamp(Microsecond, _) columns
            if let Some(ts_arr) = col
                .as_any()
                .downcast_ref::<arrow::array::TimestampMicrosecondArray>()
            {
                for i in 0..ts_arr.len() {
                    if ts_arr.is_null(i) {
                        values.push(None);
                    } else {
                        values.push(Some(ts_arr.value(i).div_euclid(1000)));
                    }
                }
                continue;
            }

            // Handle Timestamp(Nanosecond, _) columns
            if let Some(ts_arr) = col
                .as_any()
                .downcast_ref::<arrow::array::TimestampNanosecondArray>()
            {
                for i in 0..ts_arr.len() {
                    if ts_arr.is_null(i) {
                        values.push(None);
                    } else {
                        values.push(Some(ts_arr.value(i).div_euclid(1_000_000)));
                    }
                }
                continue;
            }

            // Handle Timestamp(Second, _) columns
            if let Some(ts_arr) = col
                .as_any()
                .downcast_ref::<arrow::array::TimestampSecondArray>()
            {
                for i in 0..ts_arr.len() {
                    if ts_arr.is_null(i) {
                        values.push(None);
                    } else {
                        values.push(Some(ts_arr.value(i) * 1000));
                    }
                }
                continue;
            }

            // Handle Date32 columns (days since epoch → ms)
            if let Some(d32_arr) = col
                .as_any()
                .downcast_ref::<arrow::array::Date32Array>()
            {
                for i in 0..d32_arr.len() {
                    if d32_arr.is_null(i) {
                        values.push(None);
                    } else {
                        let days = d32_arr.value(i) as i64;
                        values.push(Some(days * 86_400_000));
                    }
                }
                continue;
            }

            // Handle Int64 columns (assume ms since epoch)
            if let Some(i64_arr) = col.as_any().downcast_ref::<Int64Array>() {
                for i in 0..i64_arr.len() {
                    if i64_arr.is_null(i) {
                        values.push(None);
                    } else {
                        values.push(Some(i64_arr.value(i)));
                    }
                }
                continue;
            }

            // Unsupported column type — fill with None
            let len = col.len();
            values.extend(std::iter::repeat_n(None, len));
        }

        // Only store if we found matching batches (avoid creating empty entries
        // that block later capture attempts from the correct phase).
        if !values.is_empty() {
            self.stores.insert(key, values);
        }
    }

    /// Check if baselines exist for a given entity + field.
    pub fn has(&self, entity: &str, field: &str) -> bool {
        self.stores
            .contains_key(&(entity.to_string(), field.to_string()))
    }

    /// Get the number of stored entries for a given entity + field.
    pub fn len(&self, entity: &str, field: &str) -> usize {
        self.stores
            .get(&(entity.to_string(), field.to_string()))
            .map(|v| v.len())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use arrow::array::TimestampMillisecondArray;
    use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

    fn make_batch(entity: &str, timestamps: Vec<Option<i64>>) -> (String, RecordBatch) {
        let schema = Schema::new(vec![Field::new(
            "created_at",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            true,
        )]);
        let arr = TimestampMillisecondArray::from(timestamps);
        let batch = RecordBatch::try_new(Arc::new(schema), vec![Arc::new(arr)]).unwrap();
        (entity.to_string(), batch)
    }

    #[test]
    fn capture_and_retrieve() {
        let batches = vec![
            make_batch("users", vec![Some(1000), Some(2000), Some(3000)]),
        ];

        let mut store = TemporalStore::new();
        store.capture_from_batches("users", "created_at", &batches);

        assert_eq!(store.get("users", "created_at", 0), Some(1000));
        assert_eq!(store.get("users", "created_at", 1), Some(2000));
        assert_eq!(store.get("users", "created_at", 2), Some(3000));
        assert_eq!(store.get("users", "created_at", 3), None); // out of bounds
    }

    #[test]
    fn null_timestamps_stored_as_none() {
        let batches = vec![
            make_batch("users", vec![Some(1000), None, Some(3000)]),
        ];

        let mut store = TemporalStore::new();
        store.capture_from_batches("users", "created_at", &batches);

        assert_eq!(store.get("users", "created_at", 0), Some(1000));
        assert_eq!(store.get("users", "created_at", 1), None);
        assert_eq!(store.get("users", "created_at", 2), Some(3000));
    }

    #[test]
    fn multiple_batches_concatenated() {
        let batches = vec![
            make_batch("users", vec![Some(100)]),
            make_batch("users", vec![Some(200), Some(300)]),
        ];

        let mut store = TemporalStore::new();
        store.capture_from_batches("users", "created_at", &batches);

        assert_eq!(store.get("users", "created_at", 0), Some(100));
        assert_eq!(store.get("users", "created_at", 1), Some(200));
        assert_eq!(store.get("users", "created_at", 2), Some(300));
    }

    #[test]
    fn missing_field_yields_empty() {
        let batches = vec![
            make_batch("users", vec![Some(1000)]),
        ];

        let mut store = TemporalStore::new();
        store.capture_from_batches("users", "nonexistent", &batches);

        // Key exists but no values were captured (field missing in schema)
        assert_eq!(store.get("users", "nonexistent", 0), None);
    }

    #[test]
    fn different_entities_independent() {
        let batches = vec![
            make_batch("users", vec![Some(100)]),
            make_batch("admins", vec![Some(999)]),
        ];

        let mut store = TemporalStore::new();
        store.capture_from_batches("users", "created_at", &batches);
        store.capture_from_batches("admins", "created_at", &batches);

        assert_eq!(store.get("users", "created_at", 0), Some(100));
        assert_eq!(store.get("admins", "created_at", 0), Some(999));
    }
}
