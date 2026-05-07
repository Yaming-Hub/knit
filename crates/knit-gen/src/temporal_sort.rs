//! Post-generation pass that enforces per-actor inter-event minimum gaps
//! and chronological ordering of timestamps.
//!
//! After all batches for an entity are generated, this module groups rows
//! by actor FK, sorts their timestamps, and ensures a configurable minimum
//! gap between consecutive events from the same actor.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{Array, Int64Array, TimestampMillisecondArray};
use arrow::record_batch::RecordBatch;

/// Default minimum gap between consecutive events from the same actor: 60 seconds.
pub const DEFAULT_MIN_GAP_MS: i64 = 60_000;

/// Entry tracking a timestamp's location across batches.
#[derive(Debug)]
struct TimestampEntry {
    batch_idx: usize,
    row_idx: usize,
    value: i64,
}

/// Enforce per-actor inter-event minimum gaps across all batches
/// for a single entity.
///
/// This pass ensures that no two events from the same actor have timestamps
/// closer than `min_gap_ms`. It does **not** reorder rows — the original
/// row positions are preserved, and only timestamp values are adjusted
/// (shifted forward) when they violate the minimum gap constraint.
///
/// - `batches`: all `(entity_name, RecordBatch)` pairs for this entity
/// - `actor_fk_col`: name of the FK column that identifies the actor
/// - `timestamp_col`: name of the timestamp column to adjust
/// - `min_gap_ms`: minimum milliseconds between consecutive events per actor
///
/// Returns new batches with adjusted timestamps. Other columns are unchanged.
pub fn enforce_inter_event_gaps(
    batches: &[(String, RecordBatch)],
    actor_fk_col: &str,
    timestamp_col: &str,
    min_gap_ms: i64,
) -> Vec<(String, RecordBatch)> {
    // Collect per-actor timestamp entries across all batches.
    let mut actor_entries: HashMap<i64, Vec<TimestampEntry>> = HashMap::new();

    for (batch_idx, (_entity, batch)) in batches.iter().enumerate() {
        let fk_col_idx = batch.schema().index_of(actor_fk_col);
        let ts_col_idx = batch.schema().index_of(timestamp_col);

        let (fk_idx, ts_idx) = match (fk_col_idx, ts_col_idx) {
            (Ok(f), Ok(t)) => (f, t),
            _ => continue, // columns not found — skip this batch
        };

        let fk_arr = batch.column(fk_idx);
        let ts_arr = batch.column(ts_idx);

        let fk_i64 = fk_arr.as_any().downcast_ref::<Int64Array>();
        let ts_i64 = ts_arr
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>();

        let (fk_arr, ts_arr) = match (fk_i64, ts_i64) {
            (Some(f), Some(t)) => (f, t),
            _ => continue, // unsupported column types
        };

        for row in 0..batch.num_rows() {
            if fk_arr.is_null(row) || ts_arr.is_null(row) {
                continue;
            }
            let actor_pk = fk_arr.value(row);
            let ts = ts_arr.value(row);
            actor_entries
                .entry(actor_pk)
                .or_default()
                .push(TimestampEntry {
                    batch_idx,
                    row_idx: row,
                    value: ts,
                });
        }
    }

    // Build a write-back map: (batch_idx, row_idx) → new_timestamp
    let mut adjustments: HashMap<(usize, usize), i64> = HashMap::new();

    for entries in actor_entries.values_mut() {
        if entries.len() <= 1 {
            continue;
        }
        // Sort by timestamp.
        entries.sort_by_key(|e| e.value);

        // Enforce minimum gaps: walk forward, shifting as needed.
        let mut prev_ts = entries[0].value;
        for entry in entries.iter_mut().skip(1) {
            let required = prev_ts + min_gap_ms;
            if entry.value < required {
                entry.value = required;
                adjustments.insert((entry.batch_idx, entry.row_idx), entry.value);
            }
            prev_ts = entry.value;
        }
    }

    if adjustments.is_empty() {
        return batches.to_vec();
    }

    // Rebuild batches with adjusted timestamps.
    batches
        .iter()
        .enumerate()
        .map(|(batch_idx, (entity, batch))| {
            let ts_col_idx = match batch.schema().index_of(timestamp_col) {
                Ok(idx) => idx,
                Err(_) => return (entity.clone(), batch.clone()),
            };

            // Check if any rows in this batch need adjustment.
            let needs_adjustment = (0..batch.num_rows())
                .any(|row| adjustments.contains_key(&(batch_idx, row)));

            if !needs_adjustment {
                return (entity.clone(), batch.clone());
            }

            // Build new timestamp column.
            let old_ts = batch
                .column(ts_col_idx)
                .as_any()
                .downcast_ref::<TimestampMillisecondArray>()
                .unwrap();

            let new_values: Vec<Option<i64>> = (0..old_ts.len())
                .map(|row| {
                    if old_ts.is_null(row) {
                        None
                    } else if let Some(&new_val) = adjustments.get(&(batch_idx, row)) {
                        Some(new_val)
                    } else {
                        Some(old_ts.value(row))
                    }
                })
                .collect();

            let new_ts_arr =
                Arc::new(TimestampMillisecondArray::from(new_values)) as Arc<dyn Array>;

            // Replace the timestamp column in the batch.
            let columns: Vec<Arc<dyn Array>> = (0..batch.num_columns())
                .map(|i| {
                    if i == ts_col_idx {
                        new_ts_arr.clone()
                    } else {
                        batch.column(i).clone()
                    }
                })
                .collect();

            let new_batch =
                RecordBatch::try_new(batch.schema(), columns).unwrap_or_else(|_| batch.clone());

            (entity.clone(), new_batch)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

    fn make_batch(
        entity: &str,
        actor_fks: Vec<i64>,
        timestamps: Vec<i64>,
    ) -> (String, RecordBatch) {
        let schema = Schema::new(vec![
            Field::new("user_id", DataType::Int64, false),
            Field::new(
                "created_at",
                DataType::Timestamp(TimeUnit::Millisecond, None),
                false,
            ),
        ]);
        let fk_arr = Int64Array::from(actor_fks);
        let ts_arr = TimestampMillisecondArray::from(timestamps);
        let batch =
            RecordBatch::try_new(Arc::new(schema), vec![Arc::new(fk_arr), Arc::new(ts_arr)])
                .unwrap();
        (entity.to_string(), batch)
    }

    #[test]
    fn no_adjustment_when_gaps_sufficient() {
        let batches = vec![make_batch(
            "posts",
            vec![1, 1, 2],
            vec![1000, 1000 + DEFAULT_MIN_GAP_MS, 500],
        )];
        let result = enforce_inter_event_gaps(&batches, "user_id", "created_at", DEFAULT_MIN_GAP_MS);
        let ts = result[0]
            .1
            .column(1)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        assert_eq!(ts.value(0), 1000);
        assert_eq!(ts.value(1), 1000 + DEFAULT_MIN_GAP_MS);
        assert_eq!(ts.value(2), 500);
    }

    #[test]
    fn duplicate_timestamps_get_separated() {
        let batches = vec![make_batch("posts", vec![1, 1, 1], vec![1000, 1000, 1000])];
        let result = enforce_inter_event_gaps(&batches, "user_id", "created_at", DEFAULT_MIN_GAP_MS);
        let ts = result[0]
            .1
            .column(1)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        assert_eq!(ts.value(0), 1000); // first stays
        assert_eq!(ts.value(1), 1000 + DEFAULT_MIN_GAP_MS); // shifted
        assert_eq!(ts.value(2), 1000 + 2 * DEFAULT_MIN_GAP_MS); // shifted more
    }

    #[test]
    fn cross_batch_enforcement() {
        let batch1 = make_batch("posts", vec![1], vec![1000]);
        let batch2 = make_batch("posts", vec![1], vec![1000]); // duplicate across batches
        let result =
            enforce_inter_event_gaps(&[batch1, batch2], "user_id", "created_at", DEFAULT_MIN_GAP_MS);
        let ts0 = result[0]
            .1
            .column(1)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        let ts1 = result[1]
            .1
            .column(1)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        assert_eq!(ts0.value(0), 1000);
        assert_eq!(ts1.value(0), 1000 + DEFAULT_MIN_GAP_MS);
    }

    #[test]
    fn different_actors_independent() {
        let batches = vec![make_batch("posts", vec![1, 2, 1], vec![1000, 1000, 1000])];
        let result = enforce_inter_event_gaps(&batches, "user_id", "created_at", DEFAULT_MIN_GAP_MS);
        let ts = result[0]
            .1
            .column(1)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        // Actor 1 row 0: 1000 (first), actor 2 row 1: 1000 (only event, unchanged)
        // Actor 1 row 2: 1000 → shifted to 1000 + gap
        assert_eq!(ts.value(0), 1000);
        assert_eq!(ts.value(1), 1000); // different actor, no shift
        assert_eq!(ts.value(2), 1000 + DEFAULT_MIN_GAP_MS);
    }

    #[test]
    fn nullable_timestamps_skipped() {
        let schema = Schema::new(vec![
            Field::new("user_id", DataType::Int64, true),
            Field::new(
                "created_at",
                DataType::Timestamp(TimeUnit::Millisecond, None),
                true,
            ),
        ]);
        let fk_arr = Int64Array::from(vec![Some(1), Some(1), None]);
        let ts_arr = TimestampMillisecondArray::from(vec![Some(1000), None, Some(500)]);
        let batch =
            RecordBatch::try_new(Arc::new(schema), vec![Arc::new(fk_arr), Arc::new(ts_arr)])
                .unwrap();
        let batches = vec![("posts".to_string(), batch)];
        let result = enforce_inter_event_gaps(&batches, "user_id", "created_at", DEFAULT_MIN_GAP_MS);
        let ts = result[0]
            .1
            .column(1)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        // Only one non-null timestamp for actor 1, so no shift needed
        assert_eq!(ts.value(0), 1000);
        assert!(ts.is_null(1));
    }

    #[test]
    fn custom_min_gap() {
        let gap = 1000; // 1 second
        let batches = vec![make_batch("posts", vec![1, 1], vec![5000, 5000])];
        let result = enforce_inter_event_gaps(&batches, "user_id", "created_at", gap);
        let ts = result[0]
            .1
            .column(1)
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        assert_eq!(ts.value(0), 5000);
        assert_eq!(ts.value(1), 6000);
    }
}
