//! Actor-temporal generator — timestamps biased toward actor's preferred hours.
//!
//! For each row, reads the actor FK column, resolves the actor index, looks up
//! the temporal trait (expected to be a float 0–23 representing preferred hour),
//! and generates a timestamp with a wrapped-normal distribution centered on that
//! hour.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{Array, ArrayRef, Int64Array, TimestampMillisecondArray};
use arrow::datatypes::{DataType, TimeUnit};
use rand::RngCore;
use rand_distr::{Distribution, Normal};

use crate::actor_pool::ActorPool;
use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Default start date: 2024-01-01 UTC (ms since epoch).
const DEFAULT_START_MS: i64 = 1_704_067_200_000;
/// Default span: 365 days in milliseconds.
const DEFAULT_SPAN_MS: i64 = 365 * 24 * 3_600 * 1_000;
/// Standard deviation in hours for the wrapped normal distribution.
const PEAK_HOUR_STD_DEV: f64 = 3.0;

/// Generates timestamps biased toward each actor's preferred activity hours.
///
/// The generator uses a wrapped-normal distribution centered on the actor's
/// `peak_hours` trait value to bias the hour-of-day while distributing the
/// date uniformly across the configured time span.
///
/// When `creation_times` is set, generated timestamps are constrained to be
/// **after** the actor's creation time (captured from a datetime field in the
/// actor entity during a preceding phase).
pub struct ActorTemporalGenerator {
    /// Actor pool to look up traits.
    actor_pool: Arc<ActorPool>,
    /// PK → actor index reverse map for the actor entity.
    pk_reverse_map: Arc<HashMap<i64, usize>>,
    /// Trait name for temporal bias (e.g. `"peak_hours"`).
    trait_name: String,
    /// Actor entity name.
    actor_entity: String,
    /// FK field name in the current entity that references the actor.
    actor_field: String,
    /// Per-actor creation timestamps (indexed by actor_index).
    /// When set, generated timestamps are >= the actor's creation time.
    creation_times: Option<Arc<Vec<Option<i64>>>>,
}

impl ActorTemporalGenerator {
    /// Create a new actor-temporal generator.
    ///
    /// `creation_times`: optional per-actor creation timestamps (ms since epoch).
    /// When provided, all generated timestamps will be >= the actor's creation time.
    pub fn new(
        actor_pool: Arc<ActorPool>,
        pk_reverse_map: Arc<HashMap<i64, usize>>,
        trait_name: String,
        actor_entity: String,
        actor_field: String,
        creation_times: Option<Arc<Vec<Option<i64>>>>,
    ) -> Self {
        Self {
            actor_pool,
            pk_reverse_map,
            trait_name,
            actor_entity,
            actor_field,
            creation_times,
        }
    }
}

impl FieldGenerator for ActorTemporalGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        // Read actor FK column
        let actor_col = ctx.batch_columns.get(&self.actor_field);

        let actor_pks: Vec<Option<i64>> = if let Some(col) = actor_col {
            if let Some(i64_arr) = col.as_any().downcast_ref::<Int64Array>() {
                (0..i64_arr.len())
                    .map(|i| {
                        if i64_arr.is_null(i) {
                            None
                        } else {
                            Some(i64_arr.value(i))
                        }
                    })
                    .collect()
            } else {
                vec![None; count]
            }
        } else {
            vec![None; count]
        };

        let normal_12 = Normal::new(12.0, PEAK_HOUR_STD_DEV).unwrap();

        let values: Vec<Option<i64>> = actor_pks
            .iter()
            .map(|pk_opt| {
                let actor_idx = pk_opt.and_then(|pk| self.pk_reverse_map.get(&pk).copied());

                let peak_hour = actor_idx
                    .and_then(|idx| {
                        self.actor_pool
                            .get_trait(&self.actor_entity, idx, &self.trait_name)
                    })
                    .and_then(|v| match v {
                        knit_core::Value::Float(f) => Some(*f),
                        knit_core::Value::Int(i) => Some(*i as f64),
                        _ => None,
                    });

                let peak = peak_hour.unwrap_or(12.0);

                // Determine lower bound: actor's creation time or DEFAULT_START.
                let lower_bound = actor_idx
                    .and_then(|idx| {
                        self.creation_times
                            .as_ref()
                            .and_then(|ct| ct.get(idx).copied().flatten())
                    })
                    .unwrap_or(DEFAULT_START_MS);

                // Upper bound is DEFAULT_START + SPAN or lower_bound + SPAN,
                // whichever is later.
                // Normalize lower_bound to the start of its day so that
                // day_offset + time_ms compose correctly regardless of when
                // the actor was created during the day.
                let day_start = (lower_bound / (24 * 3_600_000)) * (24 * 3_600_000);
                let upper_bound = lower_bound.max(DEFAULT_START_MS) + DEFAULT_SPAN_MS;
                let span = upper_bound - day_start;

                // Generate a random day offset within the available span
                let day_offset_ms = gen_range_i64(rng, span.max(1));

                // Generate hour biased toward peak using wrapped normal
                let normal = Normal::new(peak, PEAK_HOUR_STD_DEV).unwrap_or(normal_12);
                let raw_hour: f64 = normal.sample(rng);
                // Wrap to [0, 24)
                let hour = ((raw_hour % 24.0) + 24.0) % 24.0;

                let hour_int = hour as i64;
                let minute = ((hour - hour.floor()) * 60.0) as i64;

                // Combine: day start + day offset (day portion only) + biased hour
                let day_ms = (day_offset_ms / (24 * 3_600_000)) * (24 * 3_600_000);
                let time_ms = hour_int * 3_600_000 + minute * 60_000;

                let timestamp = day_start + day_ms + time_ms;

                // Hard constraint: timestamp must be >= lower_bound (the actual
                // creation time, not the normalized day start).
                let timestamp = if timestamp < lower_bound {
                    timestamp + 24 * 3_600_000
                } else {
                    timestamp
                };

                Some(timestamp)
            })
            .collect();

        Arc::new(TimestampMillisecondArray::from(values)) as ArrayRef
    }

    fn output_type(&self) -> DataType {
        DataType::Timestamp(TimeUnit::Millisecond, None)
    }
}

/// Generate a random i64 in [0, max) using the RNG.
fn gen_range_i64(rng: &mut dyn RngCore, max: i64) -> i64 {
    if max <= 0 {
        return 0;
    }
    (rng.next_u64() % (max as u64)) as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::actor_pool::ActorPool;
    use knit_plan::{ActorEntityPool, ActorPoolPlan, PersonaWeight};
    use rand::SeedableRng;
    use std::collections::BTreeMap;

    fn make_pool_and_reverse_map() -> (Arc<ActorPool>, Arc<HashMap<i64, usize>>) {
        use knit_core::Value;
        let plan = ActorPoolPlan {
            pools: vec![ActorEntityPool {
                entity_name: "users".into(),
                actor_count: 3,
                persona_weights: vec![
                    PersonaWeight {
                        name: "night_owl".into(),
                        weight: 1.0,
                        traits: {
                            let mut m = BTreeMap::new();
                            m.insert("peak_hours".into(), Value::Float(22.0));
                            m
                        },
                    },
                ],
            }],
            graph_plans: vec![],
        };
        let pool = Arc::new(ActorPool::from_plan(&plan, 42));
        let mut reverse = HashMap::new();
        reverse.insert(100, 0);
        reverse.insert(200, 1);
        reverse.insert(300, 2);
        (pool, Arc::new(reverse))
    }

    #[test]
    fn generates_timestamps_biased_toward_peak() {
        let (pool, rev) = make_pool_and_reverse_map();
        let gen = ActorTemporalGenerator::new(
            pool.clone(),
            rev.clone(),
            "peak_hours".into(),
            "users".into(),
            "user_id".into(),
            None,
        );

        let mut batch_columns = HashMap::new();
        // Use actor 0 (night_owl with peak=22) for all 100 rows
        let user_ids = Arc::new(Int64Array::from(vec![100; 100])) as ArrayRef;
        batch_columns.insert("user_id".to_string(), user_ids);

        let ctx = GenContext::new(&batch_columns, 0, 0, 1, "posts");
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(999);
        let result = gen.generate(&mut rng, 100, &ctx);

        let ts_arr = result
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        assert_eq!(ts_arr.len(), 100);

        // Extract hours and check bias toward peak (22:00)
        let mut hour_counts = [0u32; 24];
        for i in 0..ts_arr.len() {
            assert!(!ts_arr.is_null(i));
            let ms = ts_arr.value(i);
            let hour = ((ms % (24 * 3_600_000)) / 3_600_000) as usize;
            hour_counts[hour] += 1;
        }

        // The peak should be near 22:00 — check that hours 20-23 + 0-1
        // (the wrapped window) have more hits than hours 8-14
        let near_peak: u32 = hour_counts[20] + hour_counts[21] + hour_counts[22]
            + hour_counts[23] + hour_counts[0] + hour_counts[1];
        let far_from_peak: u32 =
            hour_counts[8] + hour_counts[9] + hour_counts[10]
            + hour_counts[11] + hour_counts[12] + hour_counts[13];

        assert!(
            near_peak > far_from_peak,
            "Expected more timestamps near peak (22:00), got near={near_peak}, far={far_from_peak}"
        );
    }

    #[test]
    fn null_actor_fk_still_produces_timestamp() {
        let (pool, rev) = make_pool_and_reverse_map();
        let gen = ActorTemporalGenerator::new(
            pool,
            rev,
            "peak_hours".into(),
            "users".into(),
            "user_id".into(),
            None,
        );

        let mut batch_columns = HashMap::new();
        let user_ids =
            Arc::new(Int64Array::from(vec![Some(100), None, Some(300)])) as ArrayRef;
        batch_columns.insert("user_id".to_string(), user_ids);

        let ctx = GenContext::new(&batch_columns, 0, 0, 1, "posts");
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(123);
        let result = gen.generate(&mut rng, 3, &ctx);

        let ts_arr = result
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        assert_eq!(ts_arr.len(), 3);
        // Null actor FK → falls back to peak=12.0, still produces a timestamp
        for i in 0..3 {
            assert!(!ts_arr.is_null(i));
        }
    }

    #[test]
    fn timestamps_after_actor_creation() {
        let (pool, rev) = make_pool_and_reverse_map();

        // Actor 0 (PK=100) created at 2024-06-15 00:00 UTC
        // Actor 1 (PK=200) created at 2024-09-01 00:00 UTC
        // Actor 2 (PK=300) created at 2024-03-01 00:00 UTC
        let creation_ms_actor0: i64 = 1_718_409_600_000; // 2024-06-15
        let creation_ms_actor1: i64 = 1_725_148_800_000; // 2024-09-01
        let creation_ms_actor2: i64 = 1_709_251_200_000; // 2024-03-01

        let creation_times = Arc::new(vec![
            Some(creation_ms_actor0),
            Some(creation_ms_actor1),
            Some(creation_ms_actor2),
        ]);

        let gen = ActorTemporalGenerator::new(
            pool,
            rev,
            "peak_hours".into(),
            "users".into(),
            "user_id".into(),
            Some(creation_times),
        );

        // Generate 300 rows: 100 per actor
        let mut user_ids_vec = Vec::new();
        for _ in 0..100 { user_ids_vec.push(100i64); }
        for _ in 0..100 { user_ids_vec.push(200i64); }
        for _ in 0..100 { user_ids_vec.push(300i64); }

        let mut batch_columns = HashMap::new();
        let user_ids = Arc::new(Int64Array::from(user_ids_vec)) as ArrayRef;
        batch_columns.insert("user_id".to_string(), user_ids);

        let ctx = GenContext::new(&batch_columns, 0, 0, 1, "posts");
        let mut rng = rand_chacha::ChaCha8Rng::seed_from_u64(42);
        let result = gen.generate(&mut rng, 300, &ctx);

        let ts_arr = result
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .unwrap();
        assert_eq!(ts_arr.len(), 300);

        // Verify all timestamps are >= their actor's creation time
        for i in 0..100 {
            let ts = ts_arr.value(i);
            assert!(
                ts >= creation_ms_actor0,
                "row {i}: timestamp {ts} < actor0 creation {creation_ms_actor0}"
            );
        }
        for i in 100..200 {
            let ts = ts_arr.value(i);
            assert!(
                ts >= creation_ms_actor1,
                "row {i}: timestamp {ts} < actor1 creation {creation_ms_actor1}"
            );
        }
        for i in 200..300 {
            let ts = ts_arr.value(i);
            assert!(
                ts >= creation_ms_actor2,
                "row {i}: timestamp {ts} < actor2 creation {creation_ms_actor2}"
            );
        }
    }
}