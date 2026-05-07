//! Behavioral profiling — per-actor feature extraction from record streams.
//!
//! This module implements streaming per-actor accumulators that build behavioral
//! profiles from data. Each actor (identified by an actor column value) gets an
//! accumulator that tracks temporal patterns and field preferences with bounded
//! memory.
//!
//! ## Memory Model
//!
//! Each [`ActorAccumulator`] uses ~300–500 bytes plus bounded-size field
//! accumulators. At 100K actors this is roughly 30–50 MB. The
//! [`ActorProfiler`] enforces a configurable cap on tracked actors.
//!
//! ## Usage
//!
//! ```rust,no_run
//! use knit_learn::behavioral::ActorProfiler;
//! use arrow::record_batch::RecordBatch;
//!
//! let mut profiler = ActorProfiler::new("user_id".into(), Some("created_at".into()));
//! // profiler.observe_batch(&batch, &["status", "amount"]);
//! // let profiles = profiler.finalize();
//! ```

use std::collections::{BTreeMap, HashMap};

use arrow::array::{
    Array, Float32Array, Float64Array, Int32Array, Int64Array,
    StringArray, TimestampMicrosecondArray, TimestampMillisecondArray,
    TimestampNanosecondArray, TimestampSecondArray,
};
use arrow::datatypes::{DataType, TimeUnit};
use arrow::record_batch::RecordBatch;
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::streaming::{NumericState, TopKTracker};

/// Default maximum number of actors to track before overflow.
const DEFAULT_MAX_ACTORS: usize = 100_000;

/// Default capacity for per-field categorical top-K trackers.
const FIELD_TOPK_CAPACITY: usize = 50;

/// Streaming per-actor accumulator with bounded memory.
///
/// Tracks temporal patterns (hour-of-day, day-of-week histograms) and
/// per-field preferences (categorical top-K, numeric mean/variance) for
/// a single actor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ActorAccumulator {
    /// Total number of records observed for this actor.
    pub activity_count: u64,
    /// Earliest timestamp seen (epoch seconds), or `f64::INFINITY` if none.
    pub first_seen: f64,
    /// Latest timestamp seen (epoch seconds), or `f64::NEG_INFINITY` if none.
    pub last_seen: f64,
    /// Hour-of-day activity histogram (24 slots, UTC).
    pub hourly_counts: [u64; 24],
    /// Day-of-week activity histogram (7 slots, Mon=0..Sun=6).
    pub daily_counts: [u64; 7],
    /// Per-field accumulators, keyed by field name.
    pub fields: BTreeMap<String, FieldAccumulator>,
}

/// Per-field accumulator for an actor's behavior on one column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum FieldAccumulator {
    /// Categorical field: bounded top-K frequency tracker.
    Categorical(TopKTracker),
    /// Numeric field: online mean/variance via Welford's algorithm.
    Numeric(NumericState),
}

/// Finalized behavioral profile for one actor.
#[derive(Debug, Clone)]
pub struct ActorProfile {
    /// The actor identifier.
    pub actor_id: String,
    /// Total records attributed to this actor.
    pub activity_count: u64,
    /// Time span from first to last activity, in days.
    pub active_span_days: f64,
    /// Normalized hour-of-day distribution (sums to 1.0).
    pub active_hours: [f64; 24],
    /// Normalized day-of-week distribution (sums to 1.0).
    pub active_days: [f64; 7],
    /// Per-field behavioral preferences.
    pub field_preferences: BTreeMap<String, FieldPreference>,
}

/// Finalized preference for one field.
#[derive(Debug, Clone)]
pub struct FieldPreference {
    /// For categorical fields: top categories with normalized probabilities.
    pub category_dist: Option<Vec<(String, f64)>>,
    /// For numeric fields: personal mean.
    pub numeric_mean: Option<f64>,
    /// For numeric fields: personal standard deviation.
    pub numeric_std: Option<f64>,
}

/// Container that manages per-actor accumulators for a single actor column.
///
/// The profiler accepts Arrow `RecordBatch` data and routes each row to the
/// appropriate actor accumulator based on the actor column value.
#[derive(Debug)]
pub struct ActorProfiler {
    /// Name of the actor column to group by.
    actor_column: String,
    /// Name of the temporal column for timestamp extraction (if any).
    temporal_column: Option<String>,
    /// Per-actor accumulators, keyed by actor ID string.
    accumulators: HashMap<String, ActorAccumulator>,
    /// Maximum number of actors to track.
    max_actors: usize,
    /// Count of rows where the actor was not tracked due to overflow.
    overflow_rows: u64,
}

impl ActorAccumulator {
    /// Create a new empty accumulator.
    pub fn new() -> Self {
        Self {
            activity_count: 0,
            first_seen: f64::INFINITY,
            last_seen: f64::NEG_INFINITY,
            hourly_counts: [0; 24],
            daily_counts: [0; 7],
            fields: BTreeMap::new(),
        }
    }

    /// Record a timestamp observation (epoch seconds).
    pub fn observe_timestamp(&mut self, epoch_secs: f64) {
        if !epoch_secs.is_finite() {
            return;
        }
        if epoch_secs < self.first_seen {
            self.first_seen = epoch_secs;
        }
        if epoch_secs > self.last_seen {
            self.last_seen = epoch_secs;
        }

        // Decompose into hour-of-day and day-of-week (UTC)
        let secs = epoch_secs as i64;
        let hour = ((secs % 86400 + 86400) % 86400) / 3600;
        self.hourly_counts[hour as usize] += 1;

        // Days since Unix epoch; epoch was Thursday (3)
        let day_offset = secs.div_euclid(86400);
        let dow = ((day_offset + 3) % 7 + 7) % 7; // Mon=0..Sun=6
        self.daily_counts[dow as usize] += 1;
    }

    /// Record a categorical field observation.
    pub fn observe_categorical(&mut self, field: &str, value: &str) {
        let acc = self
            .fields
            .entry(field.to_string())
            .or_insert_with(|| FieldAccumulator::Categorical(TopKTracker::new(FIELD_TOPK_CAPACITY)));
        if let FieldAccumulator::Categorical(tracker) = acc {
            tracker.add(value);
        }
    }

    /// Record a numeric field observation.
    pub fn observe_numeric(&mut self, field: &str, value: f64) {
        let acc = self
            .fields
            .entry(field.to_string())
            .or_insert_with(|| FieldAccumulator::Numeric(NumericState::new()));
        if let FieldAccumulator::Numeric(state) = acc {
            state.update(value);
        }
    }

    /// Finalize this accumulator into a profile.
    pub fn finalize(&self, actor_id: String) -> ActorProfile {
        let active_span_days = if self.last_seen > self.first_seen {
            (self.last_seen - self.first_seen) / 86400.0
        } else {
            0.0
        };

        let active_hours = normalize_histogram_24(&self.hourly_counts);
        let active_days = normalize_histogram_7(&self.daily_counts);

        let mut field_preferences = BTreeMap::new();
        for (name, acc) in &self.fields {
            let pref = match acc {
                FieldAccumulator::Categorical(tracker) => {
                    let items = tracker.top_items();
                    let total: u64 = items.iter().map(|(_, c)| c).sum();
                    let dist = if total > 0 {
                        Some(
                            items
                                .into_iter()
                                .map(|(k, c)| (k, c as f64 / total as f64))
                                .collect(),
                        )
                    } else {
                        None
                    };
                    FieldPreference {
                        category_dist: dist,
                        numeric_mean: None,
                        numeric_std: None,
                    }
                }
                FieldAccumulator::Numeric(state) => FieldPreference {
                    category_dist: None,
                    numeric_mean: if state.count() > 0 {
                        Some(state.mean())
                    } else {
                        None
                    },
                    numeric_std: if state.count() > 1 {
                        Some(state.std_dev())
                    } else {
                        None
                    },
                },
            };
            field_preferences.insert(name.clone(), pref);
        }

        ActorProfile {
            actor_id,
            activity_count: self.activity_count,
            active_span_days,
            active_hours,
            active_days,
            field_preferences,
        }
    }
}

impl Default for ActorAccumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl ActorProfiler {
    /// Create a new profiler for the given actor column.
    pub fn new(actor_column: String, temporal_column: Option<String>) -> Self {
        Self {
            actor_column,
            temporal_column,
            accumulators: HashMap::new(),
            max_actors: DEFAULT_MAX_ACTORS,
            overflow_rows: 0,
        }
    }

    /// Set the maximum number of actors to track.
    pub fn with_max_actors(mut self, max: usize) -> Self {
        self.max_actors = max.max(1);
        self
    }

    /// Process a RecordBatch, updating per-actor accumulators.
    ///
    /// `feature_columns` specifies which columns (besides the actor and temporal
    /// columns) to track as behavioral features. Pass an empty slice to skip
    /// field-level profiling.
    pub fn observe_batch(&mut self, batch: &RecordBatch, feature_columns: &[&str]) {
        let schema = batch.schema();

        // Locate the actor column
        let actor_idx = match schema.index_of(&self.actor_column) {
            Ok(idx) => idx,
            Err(_) => return, // actor column not in this batch
        };
        let actor_array = batch.column(actor_idx);

        // Locate the temporal column (if configured)
        let temporal_info = self.temporal_column.as_ref().and_then(|name| {
            schema.index_of(name).ok().map(|idx| {
                let dt = schema.field(idx).data_type().clone();
                (idx, dt)
            })
        });

        // Locate feature columns
        let feature_info: Vec<(usize, &str, DataType)> = feature_columns
            .iter()
            .filter_map(|name| {
                schema.index_of(name).ok().map(|idx| {
                    let dt = schema.field(idx).data_type().clone();
                    (idx, *name, dt)
                })
            })
            .collect();

        // Extract actor IDs as strings
        let actor_strings = extract_string_values(actor_array);

        let num_rows = batch.num_rows();
        for row in 0..num_rows {
            let actor_id = match &actor_strings {
                Some(arr) if !arr.is_null(row) => arr.value(row),
                _ => continue, // skip null/missing actor IDs
            };

            // Check overflow
            if !self.accumulators.contains_key(actor_id)
                && self.accumulators.len() >= self.max_actors
            {
                self.overflow_rows += 1;
                continue;
            }

            let acc = self
                .accumulators
                .entry(actor_id.to_string())
                .or_default();
            acc.activity_count += 1;

            // Extract and observe timestamp
            if let Some((t_idx, ref t_dt)) = temporal_info {
                if let Some(epoch_secs) = extract_epoch_secs(batch.column(t_idx), row, t_dt) {
                    acc.observe_timestamp(epoch_secs);
                }
            }

            // Extract and observe feature columns
            for &(f_idx, f_name, ref f_dt) in &feature_info {
                let col = batch.column(f_idx);
                if col.is_null(row) {
                    continue;
                }
                observe_field_value(acc, f_name, col, row, f_dt);
            }
        }
    }

    /// Finalize all accumulators into sorted actor profiles.
    pub fn finalize(self) -> Vec<ActorProfile> {
        let mut profiles: Vec<ActorProfile> = self
            .accumulators
            .into_iter()
            .map(|(id, acc)| acc.finalize(id))
            .collect();
        profiles.sort_by(|a, b| a.actor_id.cmp(&b.actor_id));

        debug!(
            actor_column = %self.actor_column,
            actors = profiles.len(),
            overflow_rows = self.overflow_rows,
            "finalized actor profiles"
        );

        profiles
    }

    /// Number of actors currently tracked.
    pub fn actor_count(&self) -> usize {
        self.accumulators.len()
    }

    /// Number of rows dropped due to actor overflow.
    pub fn overflow_rows(&self) -> u64 {
        self.overflow_rows
    }
}

// ── Helpers ────────────────────────────────────────────────────────────

/// Extract string values from an Arrow array (handles Utf8, LargeUtf8, Dictionary).
fn extract_string_values(array: &dyn Array) -> Option<StringArray> {
    match array.data_type() {
        DataType::Utf8 => {
            let arr = array.as_any().downcast_ref::<StringArray>()?;
            Some(arr.clone())
        }
        DataType::LargeUtf8 => {
            // Convert LargeUtf8 to Utf8 for uniform handling
            let arr = array
                .as_any()
                .downcast_ref::<arrow::array::LargeStringArray>()?;
            let values: Vec<Option<&str>> = (0..arr.len())
                .map(|i| if arr.is_null(i) { None } else { Some(arr.value(i)) })
                .collect();
            let result: StringArray = values.into_iter().collect();
            Some(result)
        }
        DataType::Int32 | DataType::Int64 | DataType::UInt32 | DataType::UInt64 => {
            // Convert integer actor IDs to strings
            let strings: Vec<Option<String>> = (0..array.len())
                .map(|i| {
                    if array.is_null(i) {
                        None
                    } else {
                        Some(format_int_value(array, i))
                    }
                })
                .collect();
            let result: StringArray = strings.iter().map(|s| s.as_deref()).collect();
            Some(result)
        }
        _ => None,
    }
}

fn format_int_value(array: &dyn Array, idx: usize) -> String {
    if let Some(arr) = array.as_any().downcast_ref::<Int32Array>() {
        return arr.value(idx).to_string();
    }
    if let Some(arr) = array.as_any().downcast_ref::<Int64Array>() {
        return arr.value(idx).to_string();
    }
    if let Some(arr) = array.as_any().downcast_ref::<arrow::array::UInt32Array>() {
        return arr.value(idx).to_string();
    }
    if let Some(arr) = array.as_any().downcast_ref::<arrow::array::UInt64Array>() {
        return arr.value(idx).to_string();
    }
    String::new()
}

/// Extract epoch seconds from a temporal column at the given row.
fn extract_epoch_secs(array: &dyn Array, row: usize, dt: &DataType) -> Option<f64> {
    if array.is_null(row) {
        return None;
    }
    match dt {
        DataType::Timestamp(TimeUnit::Second, _) => {
            let arr = array.as_any().downcast_ref::<TimestampSecondArray>()?;
            Some(arr.value(row) as f64)
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let arr = array.as_any().downcast_ref::<TimestampMillisecondArray>()?;
            Some(arr.value(row) as f64 / 1_000.0)
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            let arr = array.as_any().downcast_ref::<TimestampMicrosecondArray>()?;
            Some(arr.value(row) as f64 / 1_000_000.0)
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            let arr = array.as_any().downcast_ref::<TimestampNanosecondArray>()?;
            Some(arr.value(row) as f64 / 1_000_000_000.0)
        }
        DataType::Date32 => {
            let arr = array.as_any().downcast_ref::<arrow::array::Date32Array>()?;
            Some(arr.value(row) as f64 * 86400.0)
        }
        DataType::Date64 => {
            let arr = array.as_any().downcast_ref::<arrow::array::Date64Array>()?;
            Some(arr.value(row) as f64 / 1_000.0)
        }
        _ => None,
    }
}

/// Observe a field value (categorical or numeric) for an actor.
fn observe_field_value(
    acc: &mut ActorAccumulator,
    field_name: &str,
    array: &dyn Array,
    row: usize,
    dt: &DataType,
) {
    match dt {
        DataType::Utf8 => {
            if let Some(arr) = array.as_any().downcast_ref::<StringArray>() {
                acc.observe_categorical(field_name, arr.value(row));
            }
        }
        DataType::LargeUtf8 => {
            if let Some(arr) = array.as_any().downcast_ref::<arrow::array::LargeStringArray>() {
                acc.observe_categorical(field_name, arr.value(row));
            }
        }
        DataType::Boolean => {
            if let Some(arr) = array.as_any().downcast_ref::<arrow::array::BooleanArray>() {
                let val = if arr.value(row) { "true" } else { "false" };
                acc.observe_categorical(field_name, val);
            }
        }
        DataType::Int32 => {
            if let Some(arr) = array.as_any().downcast_ref::<Int32Array>() {
                acc.observe_numeric(field_name, arr.value(row) as f64);
            }
        }
        DataType::Int64 => {
            if let Some(arr) = array.as_any().downcast_ref::<Int64Array>() {
                acc.observe_numeric(field_name, arr.value(row) as f64);
            }
        }
        DataType::Float32 => {
            if let Some(arr) = array.as_any().downcast_ref::<Float32Array>() {
                acc.observe_numeric(field_name, arr.value(row) as f64);
            }
        }
        DataType::Float64 => {
            if let Some(arr) = array.as_any().downcast_ref::<Float64Array>() {
                acc.observe_numeric(field_name, arr.value(row));
            }
        }
        _ => {} // skip unsupported types
    }
}

/// Normalize a 24-slot histogram to a probability distribution.
fn normalize_histogram_24(counts: &[u64; 24]) -> [f64; 24] {
    let total: u64 = counts.iter().sum();
    let mut result = [0.0; 24];
    if total > 0 {
        for (i, &c) in counts.iter().enumerate() {
            result[i] = c as f64 / total as f64;
        }
    }
    result
}

/// Normalize a 7-slot histogram to a probability distribution.
fn normalize_histogram_7(counts: &[u64; 7]) -> [f64; 7] {
    let total: u64 = counts.iter().sum();
    let mut result = [0.0; 7];
    if total > 0 {
        for (i, &c) in counts.iter().enumerate() {
            result[i] = c as f64 / total as f64;
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int64Array, StringArray, TimestampSecondArray};
    use arrow::datatypes::{Field, Schema, TimeUnit};
    use arrow::record_batch::RecordBatch;
    use std::sync::Arc;

    /// Helper: build a RecordBatch with actor_id (string), timestamp (epoch secs), and features.
    fn make_batch(
        actor_ids: &[&str],
        timestamps: &[i64],
        statuses: &[&str],
        amounts: &[f64],
    ) -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("user_id", DataType::Utf8, false),
            Field::new(
                "created_at",
                DataType::Timestamp(TimeUnit::Second, None),
                false,
            ),
            Field::new("status", DataType::Utf8, false),
            Field::new("amount", DataType::Float64, false),
        ]));

        let actor_arr = Arc::new(StringArray::from(
            actor_ids.iter().map(|s| *s).collect::<Vec<_>>(),
        ));
        let ts_arr = Arc::new(TimestampSecondArray::from(timestamps.to_vec()));
        let status_arr = Arc::new(StringArray::from(
            statuses.iter().map(|s| *s).collect::<Vec<_>>(),
        ));
        let amount_arr = Arc::new(Float64Array::from(amounts.to_vec()));

        RecordBatch::try_new(schema, vec![actor_arr, ts_arr, status_arr, amount_arr]).unwrap()
    }

    // Monday 2024-01-01 00:00:00 UTC = 1704067200
    const MON_MIDNIGHT: i64 = 1704067200;
    // Tuesday 2024-01-02 10:30:00 UTC
    const TUE_1030: i64 = 1704067200 + 86400 + 10 * 3600 + 30 * 60;
    // Wednesday 2024-01-03 15:00:00 UTC
    const WED_1500: i64 = 1704067200 + 2 * 86400 + 15 * 3600;

    #[test]
    fn accumulator_timestamp_updates_histograms() {
        let mut acc = ActorAccumulator::new();
        // Monday midnight UTC
        acc.observe_timestamp(MON_MIDNIGHT as f64);
        assert_eq!(acc.hourly_counts[0], 1); // hour 0
        assert_eq!(acc.daily_counts[0], 1); // Monday

        // Tuesday 10:30 UTC
        acc.observe_timestamp(TUE_1030 as f64);
        assert_eq!(acc.hourly_counts[10], 1); // hour 10
        assert_eq!(acc.daily_counts[1], 1); // Tuesday
    }

    #[test]
    fn accumulator_categorical_tracks_counts() {
        let mut acc = ActorAccumulator::new();
        acc.observe_categorical("status", "active");
        acc.observe_categorical("status", "active");
        acc.observe_categorical("status", "inactive");

        if let Some(FieldAccumulator::Categorical(tracker)) = acc.fields.get("status") {
            assert_eq!(tracker.get_count("active"), Some(2));
            assert_eq!(tracker.get_count("inactive"), Some(1));
        } else {
            panic!("expected categorical accumulator");
        }
    }

    #[test]
    fn accumulator_numeric_computes_mean_variance() {
        let mut acc = ActorAccumulator::new();
        acc.observe_numeric("amount", 10.0);
        acc.observe_numeric("amount", 20.0);
        acc.observe_numeric("amount", 30.0);

        if let Some(FieldAccumulator::Numeric(state)) = acc.fields.get("amount") {
            assert!((state.mean() - 20.0).abs() < 1e-10);
            assert!(state.std_dev() > 0.0);
        } else {
            panic!("expected numeric accumulator");
        }
    }

    #[test]
    fn accumulator_finalize_normalizes_histograms() {
        let mut acc = ActorAccumulator::new();
        acc.activity_count = 3;
        acc.observe_timestamp(MON_MIDNIGHT as f64);
        acc.observe_timestamp(MON_MIDNIGHT as f64);
        acc.observe_timestamp(TUE_1030 as f64);

        let profile = acc.finalize("user1".into());
        // 2/3 at hour 0, 1/3 at hour 10
        assert!((profile.active_hours[0] - 2.0 / 3.0).abs() < 1e-10);
        assert!((profile.active_hours[10] - 1.0 / 3.0).abs() < 1e-10);
        // Sum should be 1.0
        let sum: f64 = profile.active_hours.iter().sum();
        assert!((sum - 1.0).abs() < 1e-10);
    }

    #[test]
    fn accumulator_active_span_days() {
        let mut acc = ActorAccumulator::new();
        acc.activity_count = 2;
        acc.observe_timestamp(MON_MIDNIGHT as f64);
        acc.observe_timestamp(WED_1500 as f64);

        let profile = acc.finalize("user1".into());
        // Should be ~2.625 days
        let expected = (WED_1500 - MON_MIDNIGHT) as f64 / 86400.0;
        assert!((profile.active_span_days - expected).abs() < 1e-6);
    }

    #[test]
    fn accumulator_finalize_field_preferences() {
        let mut acc = ActorAccumulator::new();
        acc.activity_count = 3;
        acc.observe_categorical("status", "active");
        acc.observe_categorical("status", "active");
        acc.observe_categorical("status", "inactive");
        acc.observe_numeric("amount", 100.0);
        acc.observe_numeric("amount", 200.0);

        let profile = acc.finalize("user1".into());

        let status_pref = &profile.field_preferences["status"];
        let dist = status_pref.category_dist.as_ref().unwrap();
        let active_prob = dist.iter().find(|(k, _)| k == "active").unwrap().1;
        assert!((active_prob - 2.0 / 3.0).abs() < 1e-10);

        let amount_pref = &profile.field_preferences["amount"];
        assert!((amount_pref.numeric_mean.unwrap() - 150.0).abs() < 1e-10);
        assert!(amount_pref.numeric_std.unwrap() > 0.0);
    }

    #[test]
    fn profiler_observe_batch_separates_actors() {
        let batch = make_batch(
            &["alice", "bob", "alice"],
            &[MON_MIDNIGHT, TUE_1030, WED_1500],
            &["open", "closed", "open"],
            &[10.0, 20.0, 30.0],
        );

        let mut profiler = ActorProfiler::new("user_id".into(), Some("created_at".into()));
        profiler.observe_batch(&batch, &["status", "amount"]);

        assert_eq!(profiler.actor_count(), 2);

        let profiles = profiler.finalize();
        assert_eq!(profiles.len(), 2);

        let alice = profiles.iter().find(|p| p.actor_id == "alice").unwrap();
        assert_eq!(alice.activity_count, 2);

        let bob = profiles.iter().find(|p| p.actor_id == "bob").unwrap();
        assert_eq!(bob.activity_count, 1);
    }

    #[test]
    fn profiler_max_actors_cap() {
        let batch = make_batch(
            &["alice", "bob", "charlie"],
            &[MON_MIDNIGHT, TUE_1030, WED_1500],
            &["a", "b", "c"],
            &[1.0, 2.0, 3.0],
        );

        let mut profiler = ActorProfiler::new("user_id".into(), Some("created_at".into()))
            .with_max_actors(2);
        profiler.observe_batch(&batch, &[]);

        assert_eq!(profiler.actor_count(), 2);
        assert_eq!(profiler.overflow_rows(), 1);
    }

    #[test]
    fn profiler_missing_actor_column_skips_batch() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("other_col", DataType::Utf8, false),
        ]));
        let arr = Arc::new(StringArray::from(vec!["x"]));
        let batch = RecordBatch::try_new(schema, vec![arr]).unwrap();

        let mut profiler = ActorProfiler::new("user_id".into(), None);
        profiler.observe_batch(&batch, &[]);
        assert_eq!(profiler.actor_count(), 0);
    }

    #[test]
    fn profiler_finalize_empty_returns_empty() {
        let profiler = ActorProfiler::new("user_id".into(), None);
        let profiles = profiler.finalize();
        assert!(profiles.is_empty());
    }

    #[test]
    fn profiler_integer_actor_ids() {
        let schema = Arc::new(Schema::new(vec![
            Field::new("user_id", DataType::Int64, false),
            Field::new("value", DataType::Float64, false),
        ]));
        let id_arr = Arc::new(Int64Array::from(vec![1001, 1002, 1001]));
        let val_arr = Arc::new(Float64Array::from(vec![10.0, 20.0, 30.0]));
        let batch = RecordBatch::try_new(schema, vec![id_arr, val_arr]).unwrap();

        let mut profiler = ActorProfiler::new("user_id".into(), None);
        profiler.observe_batch(&batch, &["value"]);

        assert_eq!(profiler.actor_count(), 2);
        let profiles = profiler.finalize();
        let u1001 = profiles.iter().find(|p| p.actor_id == "1001").unwrap();
        assert_eq!(u1001.activity_count, 2);
    }

    #[test]
    fn profiler_no_temporal_column() {
        let batch = make_batch(
            &["alice", "bob"],
            &[MON_MIDNIGHT, TUE_1030],
            &["open", "closed"],
            &[10.0, 20.0],
        );

        let mut profiler = ActorProfiler::new("user_id".into(), None);
        profiler.observe_batch(&batch, &["status"]);

        let profiles = profiler.finalize();
        let alice = profiles.iter().find(|p| p.actor_id == "alice").unwrap();
        // Without temporal column, all hourly/daily counts should be 0
        assert_eq!(alice.active_span_days, 0.0);
        let sum: f64 = alice.active_hours.iter().sum();
        assert_eq!(sum, 0.0);
    }

    #[test]
    fn profiler_multiple_batches() {
        let batch1 = make_batch(
            &["alice", "bob"],
            &[MON_MIDNIGHT, TUE_1030],
            &["open", "closed"],
            &[10.0, 20.0],
        );
        let batch2 = make_batch(
            &["alice", "charlie"],
            &[WED_1500, MON_MIDNIGHT],
            &["open", "pending"],
            &[30.0, 40.0],
        );

        let mut profiler = ActorProfiler::new("user_id".into(), Some("created_at".into()));
        profiler.observe_batch(&batch1, &["status", "amount"]);
        profiler.observe_batch(&batch2, &["status", "amount"]);

        assert_eq!(profiler.actor_count(), 3);
        let profiles = profiler.finalize();
        let alice = profiles.iter().find(|p| p.actor_id == "alice").unwrap();
        assert_eq!(alice.activity_count, 2);
        assert!(alice.active_span_days > 0.0);
    }
}
