//! Generation engine — orchestrates plan execution with parallel partitions.
//!
//! The [`GenerationEngine`] takes an [`ExecutionPlan`] and materialises
//! synthetic data as Arrow [`RecordBatch`]es, calling a user-supplied callback
//! for each batch. Phases execute sequentially; entities and partitions within
//! a phase run in parallel via Rayon.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{Array, Int64Array, StringArray};
use arrow::record_batch::RecordBatch;
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;

use knit_plan::{
    DeferralStrategy, DeferredRef, EntityPlan, ExecutionPlan, GeneratorPlan, KeyStoreKind,
    PartitionRange,
};

use crate::actor_pool::ActorPool;
use crate::batch::assemble_batch;
use crate::context::GenContext;
use crate::error::GenError;
use crate::generators::actor_fk::ActorForeignKeyGenerator;
use crate::generators::create_generator;
use crate::generators::fk::ForeignKeyGenerator;
use crate::generators::graph_fk::GraphTargetFkGenerator;
use crate::generators::string_fk::StringForeignKeyGenerator;
use crate::keystore::InMemoryKeyStore;
use crate::null_mask::apply_null_mask;
use crate::sampled_key_store::SampledKeyStore;
use crate::string_keystore::InMemoryStringKeyStore;
use crate::traits::{FieldGenerator, KeyStore, StringKeyStore};

/// Default number of rows per Arrow batch.
const DEFAULT_BATCH_SIZE: usize = 8192;

/// Round float values in an Arrow array to the specified number of decimal places.
///
/// Only applies to `Float64` arrays. Other array types pass through unchanged.
fn apply_precision(arr: arrow::array::ArrayRef, precision: Option<u8>) -> arrow::array::ArrayRef {
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
///
/// The Bernoulli distribution produces `Int64Array` with 0/1 values, but when
/// the schema declares a boolean field the output should be a `BooleanArray`
/// so that downstream sinks render `true`/`false` instead of `0`/`1`.
///
/// Similarly, Int32 fields that receive Int64 arrays are narrowed.
fn coerce_to_logical_type(
    arr: arrow::array::ArrayRef,
    data_type: &knit_core::DataType,
) -> arrow::array::ArrayRef {
    match data_type {
        knit_core::DataType::Bool => {
            if arr.as_any().is::<arrow::array::BooleanArray>() {
                return arr;
            }
            if let Some(i64_arr) = arr.as_any().downcast_ref::<Int64Array>() {
                let bools: arrow::array::BooleanArray = i64_arr
                    .iter()
                    .map(|v| v.map(|x| x != 0))
                    .collect();
                return Arc::new(bools);
            }
            arr
        }
        knit_core::DataType::Int32 => {
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

/// Top-level generation orchestrator.
///
/// Holds the key-store registry shared across entities and phases. Consumes an
/// [`ExecutionPlan`] and produces [`RecordBatch`]es through a caller-supplied
/// callback.
///
/// # Parallel execution model
///
/// - **Phases** run sequentially (phase *n* must complete before phase *n+1*).
/// - **Entities** within the same phase have no FK dependencies and run in
///   parallel.
/// - **Partitions** within an entity run in parallel, each with a
///   deterministic RNG derived from the plan's [`RngTree`](knit_plan::RngTree).
///
/// # Thread safety
///
/// Key stores are wrapped in `Arc` and use interior locking (`RwLock`) so that
/// concurrent partition workers can insert/sample without external
/// synchronisation.
pub struct GenerationEngine {
    /// Entity-name → shared key store for FK resolution (Int64 keys).
    key_stores: HashMap<String, Arc<dyn KeyStore>>,
    /// Entity-name → shared string key store for FK resolution (UUID/String keys).
    string_key_stores: HashMap<String, Arc<dyn StringKeyStore>>,
    /// Maximum rows per Arrow batch.
    batch_size: usize,
    /// User-supplied parameters passed to generators via GenContext.
    params: HashMap<String, String>,
    /// Optional actor pool for persona-weighted FK generation.
    actor_pool: Option<Arc<ActorPool>>,
    /// Generated relationship graphs: graph_name → adjacency list.
    graph_adjacency: HashMap<String, Arc<crate::generators::graph_fk::AdjacencyList>>,
    /// Reverse PK→index maps: entity_name → (PK → actor_index).
    pk_reverse_maps: HashMap<String, Arc<std::collections::HashMap<i64, usize>>>,
}

impl GenerationEngine {
    /// Create a new engine with default batch size (8 192 rows).
    pub fn new() -> Self {
        Self {
            key_stores: HashMap::new(),
            string_key_stores: HashMap::new(),
            batch_size: DEFAULT_BATCH_SIZE,
            params: HashMap::new(),
            actor_pool: None,
            graph_adjacency: HashMap::new(),
            pk_reverse_maps: HashMap::new(),
        }
    }

    /// Create a new engine with a custom batch size.
    pub fn with_batch_size(batch_size: usize) -> Self {
        Self {
            key_stores: HashMap::new(),
            string_key_stores: HashMap::new(),
            batch_size: batch_size.max(1),
            params: HashMap::new(),
            actor_pool: None,
            graph_adjacency: HashMap::new(),
            pk_reverse_maps: HashMap::new(),
        }
    }

    /// Set user-supplied parameters that will be available to generators.
    pub fn with_params(mut self, params: HashMap<String, String>) -> Self {
        self.params = params;
        self
    }

    /// Set the actor pool for persona-weighted FK generation.
    ///
    /// When set, FK fields marked `actor_column = true` targeting an entity
    /// in this pool will use activity-weighted sampling instead of uniform.
    pub fn with_actor_pool(mut self, pool: Arc<ActorPool>) -> Self {
        self.actor_pool = Some(pool);
        self
    }

    /// Generate relationship graphs and pre-build adjacency lists.
    ///
    /// Must be called after `with_actor_pool` and before `execute`. Generates
    /// all graphs from the plan's actor_pool.graph_plans and builds outbound
    /// adjacency lists for use by [`GraphTargetFkGenerator`].
    pub fn build_graphs(&mut self, plan: &ExecutionPlan) {
        let pool = match &self.actor_pool {
            Some(p) => p,
            None => return,
        };

        for graph_plan in &plan.actor_pool.graph_plans {
            let graph = crate::graph::generate_graph(graph_plan, pool, plan.rng_tree.global_seed);

            // Build outbound adjacency list (Vec<Vec<usize>> indexed by source actor)
            let source_count = graph.source_count as usize;
            let mut adjacency: Vec<Vec<usize>> = vec![Vec::new(); source_count];
            for edge in &graph.edges {
                if edge.from < source_count {
                    adjacency[edge.from].push(edge.to);
                }
            }

            tracing::info!(
                graph = %graph.name,
                from = %graph.from_entity,
                to = %graph.to_entity,
                edges = graph.edges.len(),
                "built graph adjacency"
            );

            self.graph_adjacency.insert(graph.name.clone(), Arc::new(adjacency));
        }
    }

    /// Build the reverse PK→index map for an entity's key store.
    ///
    /// Must be called after the entity's key store has been populated
    /// (i.e., after the actor entity's phase completes).
    fn ensure_pk_reverse_map(&mut self, entity_name: &str) {
        if self.pk_reverse_maps.contains_key(entity_name) {
            return;
        }
        if let Some(ks) = self.key_stores.get(entity_name) {
            let len = ks.len();
            let mut map = std::collections::HashMap::with_capacity(len);
            for i in 0..len {
                if let Some(pk) = ks.get_by_index(i) {
                    map.insert(pk, i);
                }
            }
            tracing::debug!(
                entity = entity_name,
                entries = map.len(),
                "built PK reverse map"
            );
            self.pk_reverse_maps.insert(entity_name.to_string(), Arc::new(map));
        }
    }

    /// Execute the full plan, calling `on_batch` for every produced
    /// [`RecordBatch`].
    ///
    /// The callback receives the entity name and the batch. Batches for the
    /// same entity arrive in partition-then-offset order when running
    /// single-threaded but may arrive in arbitrary order under parallelism.
    ///
    /// # Errors
    ///
    /// Propagates any error from generators, Arrow, or the callback.
    pub fn execute<F>(&mut self, plan: &ExecutionPlan, mut on_batch: F) -> Result<(), GenError>
    where
        F: FnMut(&str, RecordBatch) -> Result<(), GenError> + Send,
    {
        for (phase_idx, phase) in plan.phases.iter().enumerate() {
            tracing::info!(phase = phase_idx, entities = phase.entity_plans.len(), "starting phase");

            // Pre-create key stores for entities that need them.
            for ep in &phase.entity_plans {
                if let Some(kind) = plan.index_strategy.per_entity.get(&ep.entity_name) {
                    if self.is_string_pk_entity(ep) {
                        tracing::debug!(
                            entity = %ep.entity_name,
                            pk_idx = ?ep.primary_key_field_index,
                            "creating string key store"
                        );
                        self.ensure_string_key_store(&ep.entity_name, ep.estimated_row_count);
                    } else {
                        self.ensure_key_store(&ep.entity_name, kind, ep.estimated_row_count);
                    }
                }
            }

            // Collect batches from all entities/partitions in this phase (parallel).
            let batches = self.generate_phase_batches(plan, &phase.entity_plans)?;

            // Resolve deferred FK references (cyclic/self-ref backpatch) by
            // replacing the placeholder FK column in the original batches.
            let final_batches = if !phase.deferred_refs.is_empty() {
                self.apply_deferred_refs(&phase.deferred_refs, plan, batches)?
            } else {
                batches
            };

            // Deliver batches through the callback (sequential, in order).
            for (entity_name, batch) in &final_batches {
                on_batch(entity_name, batch.clone())?;
            }

            // Build PK reverse maps for entities completed in this phase
            // (needed by GraphTargetFkGenerator in subsequent phases).
            for ep in &phase.entity_plans {
                self.ensure_pk_reverse_map(&ep.entity_name);
            }

            tracing::info!(phase = phase_idx, "phase complete");
        }
        Ok(())
    }

    // ── internal helpers ─────────────────────────────────────────────

    /// Ensure a key store exists for the entity, creating one if necessary.
    fn ensure_key_store(&mut self, entity_name: &str, kind: &KeyStoreKind, estimated_rows: u64) {
        if self.key_stores.contains_key(entity_name) {
            return;
        }
        let store: Arc<dyn KeyStore> = match kind {
            KeyStoreKind::InMemoryVec => {
                Arc::new(InMemoryKeyStore::with_capacity(estimated_rows as usize))
            }
            KeyStoreKind::SampledSubset { sample_size } => {
                // Use a deterministic seed derived from entity name for reproducibility.
                let seed = entity_name.bytes().fold(0u64, |acc, b| {
                    acc.wrapping_mul(31).wrapping_add(b as u64)
                });
                tracing::info!(
                    entity = entity_name,
                    sample_size,
                    "using sampled key store (reservoir sampling)"
                );
                Arc::new(SampledKeyStore::new(*sample_size, seed))
            }
            KeyStoreKind::MemoryMapped => {
                tracing::warn!(
                    entity = entity_name,
                    "mmap key store not yet implemented — falling back to in-memory"
                );
                Arc::new(InMemoryKeyStore::with_capacity(estimated_rows as usize))
            }
        };
        self.key_stores.insert(entity_name.to_string(), store);
    }

    /// Ensure a string key store exists for the entity (UUID/String PKs).
    fn ensure_string_key_store(&mut self, entity_name: &str, estimated_rows: u64) {
        if self.string_key_stores.contains_key(entity_name) {
            return;
        }
        let store: Arc<dyn StringKeyStore> =
            Arc::new(InMemoryStringKeyStore::with_capacity(estimated_rows as usize));
        self.string_key_stores.insert(entity_name.to_string(), store);
    }

    /// Check if an entity has a string/UUID primary key (vs Int64).
    /// Determined by the PK field's generator plan rather than data_type,
    /// because Sequence generators always produce Int64 regardless of declared type.
    fn is_string_pk_entity(&self, ep: &EntityPlan) -> bool {
        if let Some(pk_idx) = ep.primary_key_field_index {
            if let Some(fp) = ep.field_plans.get(pk_idx) {
                return matches!(
                    &fp.generator_plan,
                    GeneratorPlan::Uuid
                        | GeneratorPlan::Faker { .. }
                        | GeneratorPlan::Pattern { .. }
                );
            }
        }
        // No explicit PK index — conservatively return false to avoid
        // misidentifying an Int64 PK entity as string-keyed.
        false
    }

    /// Generate all batches for every entity in a phase, using Rayon for
    /// partition-level parallelism.
    fn generate_phase_batches(
        &self,
        plan: &ExecutionPlan,
        entity_plans: &[EntityPlan],
    ) -> Result<Vec<(String, RecordBatch)>, GenError> {
        // Collect across entities (parallel).
        let results: Vec<Result<Vec<(String, RecordBatch)>, GenError>> = entity_plans
            .par_iter()
            .map(|ep| self.generate_entity_batches(plan, ep))
            .collect();

        let mut all_batches = Vec::new();
        for r in results {
            all_batches.extend(r?);
        }
        Ok(all_batches)
    }

    /// Generate all batches for a single entity across all its partitions.
    fn generate_entity_batches(
        &self,
        plan: &ExecutionPlan,
        ep: &EntityPlan,
    ) -> Result<Vec<(String, RecordBatch)>, GenError> {
        tracing::info!(
            entity = %ep.entity_name,
            rows = ep.estimated_row_count,
            partitions = ep.partitions.len(),
            "generating entity"
        );

        // Parallel across partitions.
        let partition_results: Vec<Result<Vec<(String, RecordBatch)>, GenError>> = ep
            .partitions
            .par_iter()
            .map(|part| self.generate_partition_batches(plan, ep, part))
            .collect();

        let mut entity_batches = Vec::new();
        for r in partition_results {
            entity_batches.extend(r?);
        }
        Ok(entity_batches)
    }

    /// Generate all batches for one partition of an entity.
    fn generate_partition_batches(
        &self,
        plan: &ExecutionPlan,
        ep: &EntityPlan,
        part: &PartitionRange,
    ) -> Result<Vec<(String, RecordBatch)>, GenError> {
        let total_rows = (part.end_row - part.start_row) as usize;
        let mut rng = ChaCha8Rng::seed_from_u64(part.seed);
        let mut row_offset = part.start_row;
        let mut batches = Vec::new();

        // Build field generators once for this partition.
        let generators = self.build_field_generators(ep, plan);
        // Identify the primary-key field index.
        let pk_field_idx = self.find_pk_field_index(ep);
        let key_store = self.key_stores.get(&ep.entity_name).cloned();
        let string_key_store = self.string_key_stores.get(&ep.entity_name).cloned();

        let mut remaining = total_rows;
        while remaining > 0 {
            let batch_rows = remaining.min(self.batch_size);
            remaining -= batch_rows;

            let batch = self.generate_single_batch(
                ep,
                &generators,
                &mut rng,
                batch_rows,
                row_offset,
                part.partition_id as usize,
            )?;

            // Insert PK values into the appropriate key store.
            if let Some(idx) = pk_field_idx {
                let col = batch.column(idx);
                tracing::trace!(
                    entity = %ep.entity_name,
                    pk_idx = idx,
                    col_type = ?col.data_type(),
                    "extracting PK values"
                );
                if let Some(ref ks) = key_store {
                    if let Some(i64_arr) = col.as_any().downcast_ref::<Int64Array>() {
                        for v in i64_arr.values().iter() {
                            ks.insert(*v);
                        }
                    }
                }
                if let Some(ref sks) = string_key_store {
                    if let Some(str_arr) = col.as_any().downcast_ref::<StringArray>() {
                        for i in 0..str_arr.len() {
                            if !str_arr.is_null(i) {
                                sks.insert(str_arr.value(i).to_string());
                            }
                        }
                        let inserted = str_arr.len() - str_arr.null_count();
                        tracing::debug!(
                            entity = %ep.entity_name,
                            keys_inserted = inserted,
                            "string PK values inserted"
                        );
                    } else {
                        tracing::warn!(
                            entity = %ep.entity_name,
                            actual_type = ?col.data_type(),
                            "string key store exists but PK column is not StringArray"
                        );
                    }
                }
            }

            batches.push((ep.entity_name.clone(), batch));
            row_offset += batch_rows as u64;
        }

        tracing::debug!(
            entity = %ep.entity_name,
            partition = part.partition_id,
            batches = batches.len(),
            "partition complete"
        );
        Ok(batches)
    }

    /// Build the vector of field generators for an entity plan.
    fn build_field_generators(
        &self,
        ep: &EntityPlan,
        plan: &ExecutionPlan,
    ) -> Vec<Box<dyn FieldGenerator>> {
        ep.field_plans
            .iter()
            .map(|fp| match &fp.generator_plan {
                GeneratorPlan::ForeignKey {
                    target_entity,
                    key_store_kind,
                    ..
                } => {
                    // Try string key store first (UUID/String FKs), then int key store.
                    if let Some(sks) = self.string_key_stores.get(target_entity) {
                        tracing::debug!(
                            entity = %ep.entity_name,
                            field = %fp.field_name,
                            target = %target_entity,
                            store_len = sks.len(),
                            "using string FK generator"
                        );
                        Box::new(StringForeignKeyGenerator::new(Arc::clone(sks)))
                            as Box<dyn FieldGenerator>
                    } else if let Some(ks) = self.key_stores.get(target_entity) {
                        // Use actor-aware FK if conditions are met:
                        // 1. Field is actor_column
                        // 2. Actor pool exists for the target entity
                        // 3. Target uses InMemoryVec (not sampled subset)
                        // 4. Target has single partition (insertion order is deterministic)
                        if fp.actor_column {
                            if let Some(ref pool) = self.actor_pool {
                                if pool.has_entity(target_entity) {
                                    let target_partitions = Self::count_entity_partitions(plan, target_entity);
                                    let is_sampled = matches!(key_store_kind, KeyStoreKind::SampledSubset { .. });

                                    if target_partitions > 1 {
                                        tracing::warn!(
                                            entity = %ep.entity_name,
                                            field = %fp.field_name,
                                            target = %target_entity,
                                            partitions = target_partitions,
                                            "actor entity has multiple partitions — falling back to uniform FK"
                                        );
                                    } else if is_sampled {
                                        tracing::warn!(
                                            entity = %ep.entity_name,
                                            field = %fp.field_name,
                                            target = %target_entity,
                                            "actor entity uses sampled key store — falling back to uniform FK"
                                        );
                                    } else {
                                        tracing::debug!(
                                            entity = %ep.entity_name,
                                            field = %fp.field_name,
                                            target = %target_entity,
                                            "using actor-aware FK generator (persona-weighted)"
                                        );
                                        return Box::new(ActorForeignKeyGenerator::new(
                                            Arc::clone(pool),
                                            target_entity.clone(),
                                            Arc::clone(ks),
                                        )) as Box<dyn FieldGenerator>;
                                    }
                                }
                            }
                        }
                        Box::new(ForeignKeyGenerator::new(Arc::clone(ks))) as Box<dyn FieldGenerator>
                    } else {
                        tracing::warn!(
                            entity = %ep.entity_name,
                            field = %fp.field_name,
                            target = %target_entity,
                            "FK target key store not found — using null generator"
                        );
                        Box::new(crate::generators::constant::ConstantGenerator::new(
                            knit_core::Value::Null,
                        )) as Box<dyn FieldGenerator>
                    }
                }
                GeneratorPlan::GraphTarget {
                    graph_name,
                    source_field,
                    from_entity,
                    target_entity,
                    ..
                } => {
                    if let Some(gen) = self.build_graph_target_generator(ep, fp, graph_name, source_field, from_entity, target_entity, plan) {
                        gen
                    } else if let Some(ks) = self.key_stores.get(target_entity) {
                        Box::new(ForeignKeyGenerator::new(Arc::clone(ks))) as Box<dyn FieldGenerator>
                    } else {
                        Box::new(crate::generators::constant::ConstantGenerator::new(
                            knit_core::Value::Null,
                        )) as Box<dyn FieldGenerator>
                    }
                }
                other => create_generator(other),
            })
            .collect()
    }

    /// Build a GraphTargetFkGenerator for a field with GraphTarget plan.
    /// Returns None if preconditions aren't met (falls back to uniform FK).
    fn build_graph_target_generator(
        &self,
        ep: &EntityPlan,
        fp: &knit_plan::FieldPlan,
        graph_name: &str,
        source_field: &str,
        from_entity: &str,
        target_entity: &str,
        plan: &ExecutionPlan,
    ) -> Option<Box<dyn FieldGenerator>> {
        // Safety check: source entity must be single-partition (same invariant as actor FK)
        let from_partitions = Self::count_entity_partitions(plan, from_entity);
        if from_partitions > 1 {
            tracing::warn!(
                entity = %ep.entity_name,
                field = %fp.field_name,
                from = from_entity,
                partitions = from_partitions,
                "graph source entity has multiple partitions — falling back to uniform FK"
            );
            return None;
        }

        // Safety check: target entity must also be single-partition
        let target_partitions = Self::count_entity_partitions(plan, target_entity);
        if target_partitions > 1 {
            tracing::warn!(
                entity = %ep.entity_name,
                field = %fp.field_name,
                target = target_entity,
                partitions = target_partitions,
                "graph target entity has multiple partitions — falling back to uniform FK"
            );
            return None;
        }

        // Need adjacency list for the graph
        let adjacency = match self.graph_adjacency.get(graph_name) {
            Some(adj) => Arc::clone(adj),
            None => {
                tracing::warn!(
                    entity = %ep.entity_name,
                    field = %fp.field_name,
                    graph = graph_name,
                    "graph adjacency not found — falling back to uniform FK"
                );
                return None;
            }
        };

        // Need PK reverse map for the source (from) entity
        let pk_reverse = match self.pk_reverse_maps.get(from_entity) {
            Some(m) => Arc::clone(m),
            None => {
                tracing::warn!(
                    entity = %ep.entity_name,
                    field = %fp.field_name,
                    from = from_entity,
                    "PK reverse map not found for source entity — falling back to uniform FK"
                );
                return None;
            }
        };

        // Need key store for target entity
        let target_ks = match self.key_stores.get(target_entity) {
            Some(ks) => Arc::clone(ks),
            None => {
                tracing::warn!(
                    entity = %ep.entity_name,
                    field = %fp.field_name,
                    target = target_entity,
                    "target key store not found — falling back to uniform FK"
                );
                return None;
            }
        };

        tracing::debug!(
            entity = %ep.entity_name,
            field = %fp.field_name,
            graph = graph_name,
            source = source_field,
            from = from_entity,
            target = target_entity,
            "using graph-aware FK generator"
        );

        Some(Box::new(GraphTargetFkGenerator::new(
            adjacency,
            pk_reverse,
            target_ks,
            source_field.to_string(),
        )))
    }

    /// Count the number of partitions for an entity in the plan.
    fn count_entity_partitions(plan: &ExecutionPlan, entity_name: &str) -> usize {
        for phase in &plan.phases {
            for ep in &phase.entity_plans {
                if ep.entity_name == entity_name {
                    return ep.partitions.len();
                }
            }
        }
        1 // Default: assume single partition if not found
    }

    /// Find the index of the primary-key field.
    /// Uses explicit `primary_key_field_index` from the plan, falling back to
    /// first Sequence/Uuid generator heuristic for backward compatibility.
    fn find_pk_field_index(&self, ep: &EntityPlan) -> Option<usize> {
        if let Some(idx) = ep.primary_key_field_index {
            return Some(idx);
        }
        // Fallback: first Sequence or Uuid field.
        ep.field_plans
            .iter()
            .position(|fp| {
                matches!(
                    &fp.generator_plan,
                    GeneratorPlan::Sequence { .. } | GeneratorPlan::Uuid
                )
            })
    }

    /// Generate a single batch for one partition slice.
    fn generate_single_batch(
        &self,
        ep: &EntityPlan,
        generators: &[Box<dyn FieldGenerator>],
        rng: &mut ChaCha8Rng,
        count: usize,
        row_offset: u64,
        partition_index: usize,
    ) -> Result<RecordBatch, GenError> {
        let mut batch_columns: HashMap<String, arrow::array::ArrayRef> = HashMap::new();
        let mut field_names = Vec::with_capacity(ep.field_plans.len());
        let mut field_arrays = Vec::with_capacity(ep.field_plans.len());

        for (i, fp) in ep.field_plans.iter().enumerate() {
            let ctx = GenContext::new(
                &batch_columns,
                row_offset,
                partition_index,
                ep.partitions.len(),
                &ep.entity_name,
            )
            .with_params(&self.params);

            let arr = generators[i].generate(rng, count, &ctx);
            let arr = apply_precision(arr, fp.precision);
            let arr = coerce_to_logical_type(arr, &fp.data_type);
            let arr = apply_null_mask(arr, &fp.null_plan, rng, count)?;

            batch_columns.insert(fp.field_name.clone(), Arc::clone(&arr));
            field_names.push(fp.field_name.clone());
            field_arrays.push(arr);
        }

        assemble_batch(&field_names, field_arrays)
    }

    /// Resolve deferred FK references by replacing the placeholder FK columns
    /// in the original batches with properly sampled values from the now-populated
    /// key stores. Returns the full set of batches with replacements applied.
    fn apply_deferred_refs(
        &self,
        deferred_refs: &[DeferredRef],
        _plan: &ExecutionPlan,
        mut batches: Vec<(String, RecordBatch)>,
    ) -> Result<Vec<(String, RecordBatch)>, GenError> {
        for dr in deferred_refs {
            let target_ks = match self.key_stores.get(&dr.to_entity) {
                Some(ks) => Arc::clone(ks),
                None => {
                    tracing::warn!(
                        from = %dr.from_entity,
                        to = %dr.to_entity,
                        "deferred ref target key store not found — skipping"
                    );
                    continue;
                }
            };

            if target_ks.is_empty() {
                tracing::warn!(
                    from = %dr.from_entity,
                    to = %dr.to_entity,
                    "deferred ref target key store empty — skipping"
                );
                continue;
            }

            let base_seed: u64 = 0xDEFE_AAED;
            let mut batch_counter = 0usize;

            for (entity_name, batch) in batches.iter_mut() {
                if entity_name != &dr.from_entity {
                    continue;
                }

                // Find the FK column index in this batch
                let col_idx = match batch.schema().index_of(&dr.from_field) {
                    Ok(idx) => idx,
                    Err(_) => continue, // field not in this batch
                };

                // Per-batch deterministic seed
                let mut rng =
                    ChaCha8Rng::seed_from_u64(base_seed ^ (batch_counter as u64));
                batch_counter += 1;

                let count = batch.num_rows();
                let fk_values: Vec<Option<i64>> = match &dr.strategy {
                    DeferralStrategy::SelfReference {
                        nullable_root_probability,
                    } => {
                        let p = *nullable_root_probability;
                        (0..count)
                            .map(|_| {
                                let r = (rng.next_u64() as f64) / (u64::MAX as f64);
                                if r < p {
                                    None // root node — null FK
                                } else {
                                    target_ks.sample(&mut rng)
                                }
                            })
                            .collect()
                    }
                    _ => (0..count)
                        .map(|_| target_ks.sample(&mut rng))
                        .collect(),
                };

                // Replace the FK column in the batch
                let new_arr: arrow::array::ArrayRef =
                    Arc::new(Int64Array::from(fk_values));

                // Build a new schema with the FK column typed as Int64
                let schema = batch.schema();
                let mut fields: Vec<arrow::datatypes::Field> =
                    schema.fields().iter().map(|f| f.as_ref().clone()).collect();
                fields[col_idx] =
                    arrow::datatypes::Field::new(&dr.from_field, arrow::datatypes::DataType::Int64, true);
                let new_schema = Arc::new(arrow::datatypes::Schema::new(fields));

                // Replace the column
                let mut columns: Vec<arrow::array::ArrayRef> = (0..batch.num_columns())
                    .map(|i| batch.column(i).clone())
                    .collect();
                columns[col_idx] = new_arr;

                *batch =
                    RecordBatch::try_new(new_schema, columns).map_err(GenError::Arrow)?;
            }
        }

        Ok(batches)
    }
}

impl Default for GenerationEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use knit_plan::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    /// Helper: build a minimal execution plan with parent → child FK.
    fn parent_child_plan() -> ExecutionPlan {
        let mut entity_nodes = BTreeMap::new();

        // Parent entity seed node
        let parent_field_seeds = {
            let mut m = BTreeMap::new();
            m.insert(
                "id".into(),
                FieldSeedNode {
                    field_seed: 100,
                    partition_seeds: vec![1000],
                },
            );
            m.insert(
                "value".into(),
                FieldSeedNode {
                    field_seed: 101,
                    partition_seeds: vec![1001],
                },
            );
            m
        };
        entity_nodes.insert(
            "parent".into(),
            EntitySeedNode {
                entity_seed: 10,
                field_seeds: parent_field_seeds,
            },
        );

        // Child entity seed node
        let child_field_seeds = {
            let mut m = BTreeMap::new();
            m.insert(
                "id".into(),
                FieldSeedNode {
                    field_seed: 200,
                    partition_seeds: vec![2000],
                },
            );
            m.insert(
                "parent_id".into(),
                FieldSeedNode {
                    field_seed: 201,
                    partition_seeds: vec![2001],
                },
            );
            m
        };
        entity_nodes.insert(
            "child".into(),
            EntitySeedNode {
                entity_seed: 20,
                field_seeds: child_field_seeds,
            },
        );

        let mut per_entity = BTreeMap::new();
        per_entity.insert("parent".into(), KeyStoreKind::InMemoryVec);

        ExecutionPlan {
            phases: vec![
                // Phase 0: parent
                Phase {
                    entity_plans: vec![EntityPlan {
                        entity_name: "parent".into(),
                        partitions: vec![PartitionRange {
                            partition_id: 0,
                            start_row: 0,
                            end_row: 100,
                            seed: 42,
                        }],
                        field_plans: vec![
                            FieldPlan {
                                field_name: "id".into(),
                                data_type: knit_core::DataType::Int,
                                generator_plan: GeneratorPlan::Sequence { start: 1, step: 1 },
                                null_plan: NullPlan::Never,
                                dependency_order: 0,
            precision: None,
            actor_column: false,
        },
                            FieldPlan {
                                field_name: "value".into(),
                                data_type: knit_core::DataType::Int,
                                generator_plan: GeneratorPlan::Constant(knit_core::Value::Int(99)),
                                null_plan: NullPlan::Never,
                                dependency_order: 1,
            precision: None,
            actor_column: false,
        },
                        ],
                        estimated_row_count: 100,
                        estimated_byte_size: 800,
                        primary_key_field_index: Some(0),
                    }],
                    deferred_refs: vec![],
                },
                // Phase 1: child (references parent)
                Phase {
                    entity_plans: vec![EntityPlan {
                        entity_name: "child".into(),
                        partitions: vec![PartitionRange {
                            partition_id: 0,
                            start_row: 0,
                            end_row: 500,
                            seed: 99,
                        }],
                        field_plans: vec![
                            FieldPlan {
                                field_name: "id".into(),
                                data_type: knit_core::DataType::Int,
                                generator_plan: GeneratorPlan::Sequence { start: 1, step: 1 },
                                null_plan: NullPlan::Never,
                                dependency_order: 0,
            precision: None,
            actor_column: false,
        },
                            FieldPlan {
                                field_name: "parent_id".into(),
                                data_type: knit_core::DataType::Int,
                                generator_plan: GeneratorPlan::ForeignKey {
                                    target_entity: "parent".into(),
                                    target_field: "id".into(),
                                    key_store_kind: KeyStoreKind::InMemoryVec,
                                },
                                null_plan: NullPlan::Never,
                                dependency_order: 1,
            precision: None,
            actor_column: false,
        },
                        ],
                        estimated_row_count: 500,
                        estimated_byte_size: 4000,
                        primary_key_field_index: Some(0),
                    }],
                    deferred_refs: vec![],
                },
            ],
            rng_tree: RngTree {
                global_seed: 12345,
                entity_nodes,
            },
            index_strategy: IndexStrategy { per_entity },
            actor_pool: knit_plan::ActorPoolPlan::default(),
            metadata: PlanMetadata {
                schema_name: "test".into(),
                total_entities: 2,
                total_phases: 2,
                total_partitions: 2,
                estimated_total_rows: 600,
                estimated_total_bytes: 4800,
                has_cycles: false,
                deferred_ref_count: 0,
                actor_entity_count: 0,
                persona_count: 0,
                actor_relationship_count: 0,
            },
        }
    }

    #[test]
    fn fk_referential_integrity() {
        let plan = parent_child_plan();
        let mut engine = GenerationEngine::new();

        let batches = Arc::new(Mutex::new(Vec::<(String, RecordBatch)>::new()));
        let batches_ref = Arc::clone(&batches);

        engine
            .execute(&plan, move |entity, batch| {
                batches_ref.lock().unwrap().push((entity.to_string(), batch));
                Ok(())
            })
            .expect("execution failed");

        let batches = batches.lock().unwrap();

        // Collect parent PKs.
        let parent_pks: std::collections::HashSet<i64> = batches
            .iter()
            .filter(|(name, _)| name == "parent")
            .flat_map(|(_, b)| {
                let col = b.column(0);
                let arr = col.as_any().downcast_ref::<Int64Array>().unwrap();
                arr.values().to_vec()
            })
            .collect();

        assert!(!parent_pks.is_empty(), "parent should have PKs");

        // Every child.parent_id must be in parent PKs.
        for (name, batch) in batches.iter() {
            if name != "child" {
                continue;
            }
            let fk_col = batch.column(1);
            let fk_arr = fk_col.as_any().downcast_ref::<Int64Array>().unwrap();
            for v in fk_arr.values().iter() {
                assert!(
                    parent_pks.contains(v),
                    "child FK {v} not found in parent PKs"
                );
            }
        }
    }

    #[test]
    fn parallel_determinism() {
        let plan = parent_child_plan();

        // Run twice and compare.
        let run = || {
            let mut engine = GenerationEngine::new();
            let batches = Arc::new(Mutex::new(Vec::<(String, RecordBatch)>::new()));
            let b = Arc::clone(&batches);
            engine
                .execute(&plan, move |entity, batch| {
                    b.lock().unwrap().push((entity.to_string(), batch));
                    Ok(())
                })
                .unwrap();
            Arc::try_unwrap(batches).unwrap().into_inner().unwrap()
        };

        let run1 = run();
        let run2 = run();

        assert_eq!(run1.len(), run2.len(), "batch counts differ");
        for ((n1, b1), (n2, b2)) in run1.iter().zip(run2.iter()) {
            assert_eq!(n1, n2, "entity names differ");
            assert_eq!(b1.num_rows(), b2.num_rows(), "row counts differ");
            for col_idx in 0..b1.num_columns() {
                assert_eq!(
                    b1.column(col_idx).as_ref(),
                    b2.column(col_idx).as_ref(),
                    "column {col_idx} differs for entity {n1}"
                );
            }
        }
    }

    #[test]
    fn self_referential_deferred_ref() {
        let mut entity_nodes = BTreeMap::new();
        entity_nodes.insert(
            "employee".into(),
            EntitySeedNode {
                entity_seed: 10,
                field_seeds: {
                    let mut m = BTreeMap::new();
                    m.insert(
                        "id".into(),
                        FieldSeedNode {
                            field_seed: 100,
                            partition_seeds: vec![1000],
                        },
                    );
                    m.insert(
                        "manager_id".into(),
                        FieldSeedNode {
                            field_seed: 200,
                            partition_seeds: vec![2000],
                        },
                    );
                    m
                },
            },
        );

        let mut per_entity = BTreeMap::new();
        per_entity.insert("employee".into(), KeyStoreKind::InMemoryVec);

        let plan = ExecutionPlan {
            phases: vec![Phase {
                entity_plans: vec![EntityPlan {
                    entity_name: "employee".into(),
                    partitions: vec![PartitionRange {
                        partition_id: 0,
                        start_row: 0,
                        end_row: 50,
                        seed: 77,
                    }],
                    field_plans: vec![
                        FieldPlan {
                            field_name: "id".into(),
                                data_type: knit_core::DataType::Int,
                            generator_plan: GeneratorPlan::Sequence { start: 1, step: 1 },
                            null_plan: NullPlan::Never,
                            dependency_order: 0,
            precision: None,
            actor_column: false,
        },
                        FieldPlan {
                            field_name: "manager_id".into(),
                                data_type: knit_core::DataType::Int,
                            generator_plan: GeneratorPlan::ForeignKey {
                                target_entity: "employee".into(),
                                target_field: "id".into(),
                                key_store_kind: KeyStoreKind::InMemoryVec,
                            },
                            null_plan: NullPlan::Never,
                            dependency_order: 1,
            precision: None,
            actor_column: false,
        },
                    ],
                    estimated_row_count: 50,
                    estimated_byte_size: 400,
                    primary_key_field_index: Some(0),
                }],
                deferred_refs: vec![DeferredRef {
                    from_entity: "employee".into(),
                    from_field: "manager_id".into(),
                    to_entity: "employee".into(),
                    to_field: "id".into(),
                    strategy: DeferralStrategy::SelfReference {
                        nullable_root_probability: 0.2,
                    },
                }],
            }],
            rng_tree: RngTree {
                global_seed: 42,
                entity_nodes,
            },
            index_strategy: IndexStrategy { per_entity },
            actor_pool: knit_plan::ActorPoolPlan::default(),
            metadata: PlanMetadata {
                schema_name: "self_ref_test".into(),
                total_entities: 1,
                total_phases: 1,
                total_partitions: 1,
                estimated_total_rows: 50,
                estimated_total_bytes: 400,
                has_cycles: true,
                deferred_ref_count: 1,
                actor_entity_count: 0,
                persona_count: 0,
                actor_relationship_count: 0,
            },
        };

        let mut engine = GenerationEngine::new();
        let batches = Arc::new(Mutex::new(Vec::<(String, RecordBatch)>::new()));
        let b = Arc::clone(&batches);

        engine
            .execute(&plan, move |entity, batch| {
                b.lock().unwrap().push((entity.to_string(), batch));
                Ok(())
            })
            .unwrap();

        let batches = batches.lock().unwrap();

        // Collect employee PKs from the first column (id).
        let pks: std::collections::HashSet<i64> = batches
            .iter()
            .filter(|(n, _): &&(String, RecordBatch)| n == "employee")
            .flat_map(|(_, b): &(String, RecordBatch)| {
                if let Some(arr) = b.column(0).as_any().downcast_ref::<Int64Array>() {
                    arr.values().to_vec()
                } else {
                    vec![]
                }
            })
            .collect();

        // With the in-place deferred ref resolution, each batch now contains
        // both the id column and the resolved manager_id column.
        let deferred_cols: Vec<&Int64Array> = batches
            .iter()
            .filter(|(n, _): &&(String, RecordBatch)| n == "employee")
            .filter_map(|(_, b): &(String, RecordBatch)| {
                let idx = b.schema().index_of("manager_id").ok()?;
                b.column(idx).as_any().downcast_ref::<Int64Array>()
            })
            .collect();

        assert!(!deferred_cols.is_empty(), "should have manager_id column in batches");

        for arr in deferred_cols {
            let mut has_null = false;
            let mut has_valid = false;
            for i in 0..arr.len() {
                if arr.is_null(i) {
                    has_null = true;
                } else {
                    has_valid = true;
                    assert!(
                        pks.contains(&arr.value(i)),
                        "deferred FK {} not in employee PKs",
                        arr.value(i)
                    );
                }
            }
            // With 50 rows and 0.2 null probability, we expect both nulls and values.
            assert!(has_null, "expected some null roots");
            assert!(has_valid, "expected some valid FKs");
        }
    }

    #[test]
    fn multi_partition_parallel() {
        let mut entity_nodes = BTreeMap::new();
        entity_nodes.insert(
            "items".into(),
            EntitySeedNode {
                entity_seed: 10,
                field_seeds: {
                    let mut m = BTreeMap::new();
                    m.insert(
                        "id".into(),
                        FieldSeedNode {
                            field_seed: 100,
                            partition_seeds: vec![1000, 1001, 1002, 1003],
                        },
                    );
                    m
                },
            },
        );

        let plan = ExecutionPlan {
            phases: vec![Phase {
                entity_plans: vec![EntityPlan {
                    entity_name: "items".into(),
                    partitions: vec![
                        PartitionRange { partition_id: 0, start_row: 0, end_row: 250, seed: 10 },
                        PartitionRange { partition_id: 1, start_row: 250, end_row: 500, seed: 20 },
                        PartitionRange { partition_id: 2, start_row: 500, end_row: 750, seed: 30 },
                        PartitionRange { partition_id: 3, start_row: 750, end_row: 1000, seed: 40 },
                    ],
                    field_plans: vec![FieldPlan {
                        field_name: "id".into(),
                                data_type: knit_core::DataType::Int,
                        generator_plan: GeneratorPlan::Sequence { start: 1, step: 1 },
                        null_plan: NullPlan::Never,
                        dependency_order: 0,
            precision: None,
            actor_column: false,
        }],
                    estimated_row_count: 1000,
                    estimated_byte_size: 8000,
                    primary_key_field_index: Some(0),
                }],
                deferred_refs: vec![],
            }],
            rng_tree: RngTree {
                global_seed: 99,
                entity_nodes,
            },
            index_strategy: IndexStrategy {
                per_entity: BTreeMap::new(),
            },
            actor_pool: knit_plan::ActorPoolPlan::default(),
            metadata: PlanMetadata {
                schema_name: "parallel_test".into(),
                total_entities: 1,
                total_phases: 1,
                total_partitions: 4,
                estimated_total_rows: 1000,
                estimated_total_bytes: 8000,
                has_cycles: false,
                deferred_ref_count: 0,
                actor_entity_count: 0,
                persona_count: 0,
                actor_relationship_count: 0,
            },
        };

        let mut engine = GenerationEngine::new();
        let total_rows = Arc::new(Mutex::new(0usize));
        let tr: Arc<Mutex<usize>> = Arc::clone(&total_rows);

        engine
            .execute(&plan, move |_entity, batch| {
                *tr.lock().unwrap() += batch.num_rows();
                Ok(())
            })
            .unwrap();

        assert_eq!(*total_rows.lock().unwrap(), 1000);
    }

    #[test]
    fn custom_batch_size() {
        // With batch_size=10 and 25 rows, should produce 3 batches (10+10+5)
        let mut entity_nodes = BTreeMap::new();
        entity_nodes.insert(
            "items".into(),
            EntitySeedNode {
                entity_seed: 10,
                field_seeds: {
                    let mut m = BTreeMap::new();
                    m.insert(
                        "id".into(),
                        FieldSeedNode {
                            field_seed: 100,
                            partition_seeds: vec![1000],
                        },
                    );
                    m
                },
            },
        );

        let plan = ExecutionPlan {
            phases: vec![Phase {
                entity_plans: vec![EntityPlan {
                    entity_name: "items".into(),
                    partitions: vec![PartitionRange {
                        partition_id: 0,
                        start_row: 0,
                        end_row: 25,
                        seed: 42,
                    }],
                    field_plans: vec![FieldPlan {
                        field_name: "id".into(),
                                data_type: knit_core::DataType::Int,
                        generator_plan: GeneratorPlan::Sequence { start: 1, step: 1 },
                        null_plan: NullPlan::Never,
                        dependency_order: 0,
            precision: None,
            actor_column: false,
        }],
                    estimated_row_count: 25,
                    estimated_byte_size: 200,
                    primary_key_field_index: Some(0),
                }],
                deferred_refs: vec![],
            }],
            rng_tree: RngTree {
                global_seed: 42,
                entity_nodes,
            },
            index_strategy: IndexStrategy {
                per_entity: BTreeMap::new(),
            },
            actor_pool: knit_plan::ActorPoolPlan::default(),
            metadata: PlanMetadata {
                schema_name: "batch_size_test".into(),
                total_entities: 1,
                total_phases: 1,
                total_partitions: 1,
                estimated_total_rows: 25,
                estimated_total_bytes: 200,
                has_cycles: false,
                deferred_ref_count: 0,
                actor_entity_count: 0,
                persona_count: 0,
                actor_relationship_count: 0,
            },
        };

        let mut engine = GenerationEngine::with_batch_size(10);
        let batch_sizes = Arc::new(Mutex::new(Vec::<usize>::new()));
        let all_ids = Arc::new(Mutex::new(Vec::<i64>::new()));
        let bs = Arc::clone(&batch_sizes);
        let ids = Arc::clone(&all_ids);

        engine
            .execute(&plan, move |_entity, batch| {
                bs.lock().unwrap().push(batch.num_rows());
                let col = batch
                    .column(0)
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap();
                ids.lock().unwrap().extend(col.values().iter().copied());
                Ok(())
            })
            .unwrap();

        let sizes = batch_sizes.lock().unwrap();
        assert_eq!(sizes.len(), 3, "25 rows / batch_size=10 → 3 batches");
        assert_eq!(sizes[0], 10);
        assert_eq!(sizes[1], 10);
        assert_eq!(sizes[2], 5);

        // Verify row offsets are correct: sequence 1..=25 with no gaps or duplicates
        let mut collected = all_ids.lock().unwrap().clone();
        collected.sort();
        let expected: Vec<i64> = (1..=25).collect();
        assert_eq!(collected, expected, "ids should be exactly 1..=25");
    }

    #[test]
    fn batch_size_clamped_to_minimum_one() {
        let engine = GenerationEngine::with_batch_size(0);
        assert_eq!(engine.batch_size, 1, "batch_size=0 should be clamped to 1");
    }

    #[test]
    fn callback_error_propagates() {
        let plan = parent_child_plan();
        let mut engine = GenerationEngine::new();

        let result = engine.execute(&plan, |_entity, _batch| {
            Err(GenError::Generation("intentional error".to_string()))
        });

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("intentional error"));
    }

    #[test]
    fn null_plan_injects_nulls() {
        let mut entity_nodes = BTreeMap::new();
        entity_nodes.insert(
            "items".into(),
            EntitySeedNode {
                entity_seed: 10,
                field_seeds: {
                    let mut m = BTreeMap::new();
                    m.insert(
                        "val".into(),
                        FieldSeedNode {
                            field_seed: 100,
                            partition_seeds: vec![1000],
                        },
                    );
                    m
                },
            },
        );

        let plan = ExecutionPlan {
            phases: vec![Phase {
                entity_plans: vec![EntityPlan {
                    entity_name: "items".into(),
                    partitions: vec![PartitionRange {
                        partition_id: 0,
                        start_row: 0,
                        end_row: 1000,
                        seed: 42,
                    }],
                    field_plans: vec![FieldPlan {
                        field_name: "val".into(),
                                data_type: knit_core::DataType::Int,
                        generator_plan: GeneratorPlan::Constant(knit_core::Value::Int(42)),
                        null_plan: NullPlan::Probability(0.5),
                        dependency_order: 0,
            precision: None,
            actor_column: false,
        }],
                    estimated_row_count: 1000,
                    estimated_byte_size: 8000,
                    primary_key_field_index: Some(0),
                }],
                deferred_refs: vec![],
            }],
            rng_tree: RngTree {
                global_seed: 42,
                entity_nodes,
            },
            index_strategy: IndexStrategy {
                per_entity: BTreeMap::new(),
            },
            actor_pool: knit_plan::ActorPoolPlan::default(),
            metadata: PlanMetadata {
                schema_name: "null_test".into(),
                total_entities: 1,
                total_phases: 1,
                total_partitions: 1,
                estimated_total_rows: 1000,
                estimated_total_bytes: 8000,
                has_cycles: false,
                deferred_ref_count: 0,
                actor_entity_count: 0,
                persona_count: 0,
                actor_relationship_count: 0,
            },
        };

        let mut engine = GenerationEngine::new();
        let null_count = Arc::new(Mutex::new(0usize));
        let total_count = Arc::new(Mutex::new(0usize));
        let nc = Arc::clone(&null_count);
        let tc = Arc::clone(&total_count);

        engine
            .execute(&plan, move |_entity, batch| {
                let col = batch.column(0);
                *nc.lock().unwrap() += col.null_count();
                *tc.lock().unwrap() += col.len();
                Ok(())
            })
            .unwrap();

        let nulls = *null_count.lock().unwrap();
        let total = *total_count.lock().unwrap();
        let ratio = nulls as f64 / total as f64;
        // With prob=0.5 and 1000 rows (deterministic seed), should be ~50% nulls (allow 45-55%)
        assert!(
            ratio > 0.45 && ratio < 0.55,
            "expected ~50% nulls, got {:.1}% ({nulls}/{total})",
            ratio * 100.0
        );
    }

    #[test]
    fn default_engine_has_default_batch_size() {
        let engine = GenerationEngine::default();
        assert_eq!(engine.batch_size, DEFAULT_BATCH_SIZE);
    }

    #[test]
    fn apply_precision_rounds_floats() {
        use arrow::array::Float64Array;

        let arr: arrow::array::ArrayRef =
            Arc::new(Float64Array::from(vec![1.23456, 99.999, 0.1]));
        let result = apply_precision(arr, Some(2));
        let float_arr = result
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(float_arr.value(0), 1.23);
        assert_eq!(float_arr.value(1), 100.0);
        assert_eq!(float_arr.value(2), 0.1);
    }

    #[test]
    fn apply_precision_none_is_noop() {
        use arrow::array::Float64Array;

        let arr: arrow::array::ArrayRef =
            Arc::new(Float64Array::from(vec![1.23456789]));
        let result = apply_precision(arr.clone(), None);
        let float_arr = result
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(float_arr.value(0), 1.23456789);
    }

    #[test]
    fn apply_precision_preserves_nulls() {
        use arrow::array::Float64Array;

        let arr: arrow::array::ArrayRef =
            Arc::new(Float64Array::from(vec![Some(3.14159), None, Some(2.71828)]));
        let result = apply_precision(arr, Some(2));
        let float_arr = result
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap();
        assert_eq!(float_arr.value(0), 3.14);
        assert!(float_arr.is_null(1));
        assert_eq!(float_arr.value(2), 2.72);
    }

    #[test]
    fn coerce_bool_from_int64() {
        use arrow::array::BooleanArray;

        let arr: arrow::array::ArrayRef = Arc::new(Int64Array::from(vec![1, 0, 1, 0]));
        let result = coerce_to_logical_type(arr, &knit_core::DataType::Bool);
        let bool_arr = result.as_any().downcast_ref::<BooleanArray>().unwrap();
        assert_eq!(bool_arr.value(0), true);
        assert_eq!(bool_arr.value(1), false);
        assert_eq!(bool_arr.value(2), true);
        assert_eq!(bool_arr.value(3), false);
    }

    #[test]
    fn coerce_bool_preserves_nulls() {
        use arrow::array::BooleanArray;

        let arr: arrow::array::ArrayRef =
            Arc::new(Int64Array::from(vec![Some(1), None, Some(0)]));
        let result = coerce_to_logical_type(arr, &knit_core::DataType::Bool);
        let bool_arr = result.as_any().downcast_ref::<BooleanArray>().unwrap();
        assert_eq!(bool_arr.value(0), true);
        assert!(bool_arr.is_null(1));
        assert_eq!(bool_arr.value(2), false);
    }

    #[test]
    fn coerce_noop_for_non_bool() {
        let arr: arrow::array::ArrayRef = Arc::new(Int64Array::from(vec![1, 2, 3]));
        let result = coerce_to_logical_type(arr.clone(), &knit_core::DataType::Int);
        // Should pass through unchanged (still Int64)
        assert!(result.as_any().downcast_ref::<Int64Array>().is_some());
    }
}
