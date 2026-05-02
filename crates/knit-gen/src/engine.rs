//! Generation engine — orchestrates plan execution with parallel partitions.
//!
//! The [`GenerationEngine`] takes an [`ExecutionPlan`] and materialises
//! synthetic data as Arrow [`RecordBatch`]es, calling a user-supplied callback
//! for each batch. Phases execute sequentially; entities and partitions within
//! a phase run in parallel via Rayon.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{Array, Int64Array};
use arrow::record_batch::RecordBatch;
use rand::{RngCore, SeedableRng};
use rand_chacha::ChaCha8Rng;
use rayon::prelude::*;

use knit_plan::{
    DeferralStrategy, DeferredRef, EntityPlan, ExecutionPlan, GeneratorPlan, KeyStoreKind,
    PartitionRange,
};

use crate::batch::assemble_batch;
use crate::context::GenContext;
use crate::error::GenError;
use crate::generators::create_generator;
use crate::generators::fk::ForeignKeyGenerator;
use crate::keystore::InMemoryKeyStore;
use crate::null_mask::apply_null_mask;
use crate::traits::{FieldGenerator, KeyStore};

/// Default number of rows per Arrow batch.
const DEFAULT_BATCH_SIZE: usize = 8192;

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
    /// Entity-name → shared key store for FK resolution.
    key_stores: HashMap<String, Arc<dyn KeyStore>>,
    /// Maximum rows per Arrow batch.
    batch_size: usize,
}

impl GenerationEngine {
    /// Create a new engine with default batch size (8 192 rows).
    pub fn new() -> Self {
        Self {
            key_stores: HashMap::new(),
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }

    /// Create a new engine with a custom batch size.
    pub fn with_batch_size(batch_size: usize) -> Self {
        Self {
            key_stores: HashMap::new(),
            batch_size: batch_size.max(1),
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
                    self.ensure_key_store(&ep.entity_name, kind, ep.estimated_row_count);
                }
            }

            // Collect batches from all entities/partitions in this phase (parallel).
            let batches = self.generate_phase_batches(&plan, &phase.entity_plans)?;

            // Deliver batches through the callback (sequential, in order).
            for (entity_name, batch) in &batches {
                on_batch(entity_name, batch.clone())?;
            }

            // Resolve deferred FK references (cyclic/self-ref backpatch).
            if !phase.deferred_refs.is_empty() {
                let deferred_batches =
                    self.resolve_deferred_refs(&phase.deferred_refs, &plan, &batches)?;
                for (entity_name, batch) in &deferred_batches {
                    on_batch(entity_name, batch.clone())?;
                }
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
            // For now, all variants fall back to in-memory.
            KeyStoreKind::MemoryMapped | KeyStoreKind::SampledSubset { .. } => {
                tracing::warn!(
                    entity = entity_name,
                    "mmap/sampled key store not yet implemented — falling back to in-memory"
                );
                Arc::new(InMemoryKeyStore::with_capacity(estimated_rows as usize))
            }
        };
        self.key_stores.insert(entity_name.to_string(), store);
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
        // Identify the primary-key field index (first field with a Sequence generator is PK).
        let pk_field_idx = self.find_pk_field_index(ep);
        let key_store = self.key_stores.get(&ep.entity_name).cloned();

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

            // Insert PK values into the key store.
            if let (Some(idx), Some(ref ks)) = (pk_field_idx, &key_store) {
                let col = batch.column(idx);
                if let Some(i64_arr) = col.as_any().downcast_ref::<Int64Array>() {
                    for v in i64_arr.values().iter() {
                        ks.insert(*v);
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
        _plan: &ExecutionPlan,
    ) -> Vec<Box<dyn FieldGenerator>> {
        ep.field_plans
            .iter()
            .map(|fp| match &fp.generator_plan {
                GeneratorPlan::ForeignKey {
                    target_entity,
                    key_store_kind: _,
                    ..
                } => {
                    if let Some(ks) = self.key_stores.get(target_entity) {
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
                other => create_generator(other),
            })
            .collect()
    }

    /// Find the index of the primary-key field (first Sequence field).
    fn find_pk_field_index(&self, ep: &EntityPlan) -> Option<usize> {
        ep.field_plans
            .iter()
            .position(|fp| matches!(&fp.generator_plan, GeneratorPlan::Sequence { .. }))
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
            let ctx = GenContext {
                batch_columns: &batch_columns,
                row_offset,
                partition_index,
                partition_count: ep.partitions.len(),
                entity_name: &ep.entity_name,
            };

            let arr = generators[i].generate(rng, count, &ctx);
            let arr = apply_null_mask(arr, &fp.null_plan, rng, count);

            batch_columns.insert(fp.field_name.clone(), Arc::clone(&arr));
            field_names.push(fp.field_name.clone());
            field_arrays.push(arr);
        }

        assemble_batch(&field_names, field_arrays)
    }

    /// Resolve deferred FK references by backpatching null columns with sampled
    /// values from now-populated key stores.
    fn resolve_deferred_refs(
        &self,
        deferred_refs: &[DeferredRef],
        _plan: &ExecutionPlan,
        _existing_batches: &[(String, RecordBatch)],
    ) -> Result<Vec<(String, RecordBatch)>, GenError> {
        // For each deferred ref, we generate a standalone "patch" batch containing
        // the FK column with valid values sampled from the target key store.
        let mut patch_batches = Vec::new();

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

            // Find existing batches for the from_entity and produce patch columns.
            let source_batches: Vec<&RecordBatch> = _existing_batches
                .iter()
                .filter(|(name, _)| name == &dr.from_entity)
                .map(|(_, b)| b)
                .collect();

            let base_seed: u64 = 0xDEFE_AAED;

            for (batch_idx, batch) in source_batches.iter().enumerate() {
                // Per-batch deterministic seed ensures order-independent FK assignment
                let mut rng = ChaCha8Rng::seed_from_u64(base_seed ^ (batch_idx as u64));
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

                let arr: arrow::array::ArrayRef = Arc::new(Int64Array::from(fk_values));
                let patch = assemble_batch(
                    &[dr.from_field.clone()],
                    vec![arr],
                )?;
                patch_batches.push((dr.from_entity.clone(), patch));
            }
        }

        Ok(patch_batches)
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
                                generator_plan: GeneratorPlan::Sequence { start: 1, step: 1 },
                                null_plan: NullPlan::Never,
                                dependency_order: 0,
                            },
                            FieldPlan {
                                field_name: "value".into(),
                                generator_plan: GeneratorPlan::Constant(knit_core::Value::Int(99)),
                                null_plan: NullPlan::Never,
                                dependency_order: 1,
                            },
                        ],
                        estimated_row_count: 100,
                        estimated_byte_size: 800,
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
                                generator_plan: GeneratorPlan::Sequence { start: 1, step: 1 },
                                null_plan: NullPlan::Never,
                                dependency_order: 0,
                            },
                            FieldPlan {
                                field_name: "parent_id".into(),
                                generator_plan: GeneratorPlan::ForeignKey {
                                    target_entity: "parent".into(),
                                    target_field: "id".into(),
                                    key_store_kind: KeyStoreKind::InMemoryVec,
                                },
                                null_plan: NullPlan::Never,
                                dependency_order: 1,
                            },
                        ],
                        estimated_row_count: 500,
                        estimated_byte_size: 4000,
                    }],
                    deferred_refs: vec![],
                },
            ],
            rng_tree: RngTree {
                global_seed: 12345,
                entity_nodes,
            },
            index_strategy: IndexStrategy { per_entity },
            metadata: PlanMetadata {
                schema_name: "test".into(),
                total_entities: 2,
                total_phases: 2,
                total_partitions: 2,
                estimated_total_rows: 600,
                estimated_total_bytes: 4800,
                has_cycles: false,
                deferred_ref_count: 0,
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
                    field_plans: vec![FieldPlan {
                        field_name: "id".into(),
                        generator_plan: GeneratorPlan::Sequence { start: 1, step: 1 },
                        null_plan: NullPlan::Never,
                        dependency_order: 0,
                    }],
                    estimated_row_count: 50,
                    estimated_byte_size: 400,
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
            metadata: PlanMetadata {
                schema_name: "self_ref_test".into(),
                total_entities: 1,
                total_phases: 1,
                total_partitions: 1,
                estimated_total_rows: 50,
                estimated_total_bytes: 400,
                has_cycles: true,
                deferred_ref_count: 1,
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

        // Collect employee PKs.
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

        // Deferred ref patch batches: manager_id values.
        let deferred: Vec<&RecordBatch> = batches
            .iter()
            .filter(|(_, b): &&(String, RecordBatch)| {
                b.schema().fields().len() == 1
                    && b.schema().field(0).name() == "manager_id"
            })
            .map(|(_, b): &(String, RecordBatch)| b)
            .collect();

        assert!(!deferred.is_empty(), "should have deferred patch batches");

        for batch in deferred {
            let col = batch.column(0);
            let arr = col.as_any().downcast_ref::<Int64Array>().unwrap();
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
                        generator_plan: GeneratorPlan::Sequence { start: 1, step: 1 },
                        null_plan: NullPlan::Never,
                        dependency_order: 0,
                    }],
                    estimated_row_count: 1000,
                    estimated_byte_size: 8000,
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
            metadata: PlanMetadata {
                schema_name: "parallel_test".into(),
                total_entities: 1,
                total_phases: 1,
                total_partitions: 4,
                estimated_total_rows: 1000,
                estimated_total_bytes: 8000,
                has_cycles: false,
                deferred_ref_count: 0,
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
}
