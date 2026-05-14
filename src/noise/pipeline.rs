//! Three-stage perturbation pipeline executor.
//!
//! The [`Pipeline`] collects perturbators and runs them in three stages:
//!
//! 1. **Clean** — perturbators that break no invariants.
//! 2. **Constrained** — perturbators that break *some* invariants.
//! 3. **Breaking** — perturbators that intentionally violate hard constraints
//!    (FK integrity, uniqueness, etc.).
//!
//! Within each stage perturbators run in insertion order.

use arrow::array::{AsArray, BooleanArray};
use arrow::record_batch::RecordBatch;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, info, instrument};

use crate::gen::expr::ast::Expr;
use crate::gen::expr::eval::{evaluate, EvalContext};
use crate::noise::error::NoiseError;
use crate::noise::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

/// Categorises a perturbator into a pipeline stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Stage {
    Clean = 0,
    Constrained = 1,
    Breaking = 2,
}

fn classify(invariants: InvariantSet) -> Stage {
    if invariants.is_empty() {
        Stage::Clean
    } else if invariants.intersects(InvariantSet::FK_INTEGRITY | InvariantSet::UNIQUE) {
        Stage::Breaking
    } else {
        Stage::Constrained
    }
}

/// Per-perturbator overrides for rate, column filter, and scope.
#[derive(Debug, Clone, Default)]
pub struct PerturbOverrides {
    /// Probability override (clamped to `[0.0, 1.0]`).
    pub probability: Option<f64>,
    /// Column filter override. If `None`, uses the pipeline default.
    pub columns: Option<ColumnFilter>,
    /// Compiled scope predicate AST. Evaluated per-batch to produce
    /// a row-level boolean mask restricting which rows are eligible.
    pub scope_expr: Option<Expr>,
}

/// Evaluate a compiled scope expression against a `RecordBatch`,
/// producing a `BooleanArray` mask. Null results are treated as `false`.
fn evaluate_scope_mask(expr: &Expr, batch: &RecordBatch) -> Result<BooleanArray, NoiseError> {
    use arrow::array::Array;
    use arrow::datatypes::DataType;

    let schema = batch.schema();
    let mut columns: HashMap<String, arrow::array::ArrayRef> = HashMap::new();
    for (i, field) in schema.fields().iter().enumerate() {
        columns.insert(field.name().clone(), batch.column(i).clone());
    }
    let ctx = EvalContext {
        columns: &columns,
        params: &HashMap::new(),
        row_count: batch.num_rows(),
        row_offset: 0,
        seed: 0,
        call_counter: Cell::new(0),
    };
    let result = evaluate(expr, &ctx).map_err(|e| NoiseError::Scope(e.message))?;

    // Coerce to boolean
    match result.data_type() {
        DataType::Boolean => {
            let bool_arr = result.as_boolean().clone();
            // Replace nulls with false
            let mask: BooleanArray = (0..bool_arr.len())
                .map(|i| Some(bool_arr.is_valid(i) && bool_arr.value(i)))
                .collect();
            Ok(mask)
        }
        other => Err(NoiseError::Scope(format!(
            "scope predicate must evaluate to Boolean, got {other:?}"
        ))),
    }
}

/// Three-stage perturbation pipeline.
///
/// Add perturbators with [`Pipeline::add`], [`Pipeline::add_with_rate`], or
/// [`Pipeline::add_with_overrides`], then call [`Pipeline::run`] to apply
/// them all in the correct stage order.
///
/// Each perturbator can have its own probability and column filter overrides.
/// If not set, the pipeline's defaults from [`PerturbConfig`] are used.
///
/// # Example
///
/// ```no_run
/// use knit::noise::pipeline::Pipeline;
/// use knit::noise::traits::PerturbConfig;
/// use knit::noise::gaussian_noise::GaussianNoise;
/// use knit::noise::null_injector::NullInjector;
///
/// let mut pipe = Pipeline::new(PerturbConfig::default());
/// pipe.add(Box::new(GaussianNoise::default()));
/// pipe.add_with_rate(Box::new(NullInjector::default()), 0.10);
/// # let batch = arrow::record_batch::RecordBatch::new_empty(std::sync::Arc::new(arrow::datatypes::Schema::empty()));
/// let noisy = pipe.run(batch).unwrap();
/// ```
pub struct Pipeline {
    perturbators: Vec<(Box<dyn Perturbator>, PerturbOverrides)>,
    config: PerturbConfig,
}

impl Pipeline {
    /// Create a pipeline with the given default config.
    pub fn new(config: PerturbConfig) -> Self {
        Self {
            perturbators: Vec::new(),
            config,
        }
    }

    /// Append a perturbator using the pipeline's default probability and columns.
    pub fn add(&mut self, p: Box<dyn Perturbator>) {
        self.perturbators.push((p, PerturbOverrides::default()));
    }

    /// Append a perturbator with a specific probability override.
    ///
    /// The `rate` is clamped to `[0.0, 1.0]` and overrides `config.probability`
    /// for this perturbator only.
    pub fn add_with_rate(&mut self, p: Box<dyn Perturbator>, rate: f64) {
        self.perturbators.push((
            p,
            PerturbOverrides {
                probability: Some(rate.clamp(0.0, 1.0)),
                columns: None,
                scope_expr: None,
            },
        ));
    }

    /// Append a perturbator with full overrides for probability and column filter.
    pub fn add_with_overrides(&mut self, p: Box<dyn Perturbator>, overrides: PerturbOverrides) {
        let mut overrides = overrides;
        if let Some(rate) = overrides.probability {
            overrides.probability = Some(rate.clamp(0.0, 1.0));
        }
        self.perturbators.push((p, overrides));
    }

    /// Execute all perturbators in stage order against `batch`.
    ///
    /// Returns the final [`RecordBatch`] after all perturbations.
    ///
    /// # Errors
    ///
    /// Propagates the first [`NoiseError`] from any perturbator.
    #[instrument(skip_all, fields(num_perturbators = self.perturbators.len()))]
    pub fn run(&self, batch: RecordBatch) -> Result<RecordBatch, NoiseError> {
        self.run_with_offset(batch, 0)
    }

    /// Execute all perturbators with a seed offset for batch-level entropy.
    ///
    /// Use this when applying noise to multiple batches from the same entity
    /// to avoid repeating the same corruption pattern. Pass a unique
    /// `batch_offset` (e.g. batch index or row offset) for each call.
    #[instrument(skip_all, fields(num_perturbators = self.perturbators.len(), batch_offset))]
    pub fn run_with_offset(
        &self,
        batch: RecordBatch,
        batch_offset: u64,
    ) -> Result<RecordBatch, NoiseError> {
        // Build (stage, index) pairs and sort by stage while preserving
        // insertion order within each stage.
        let mut order: Vec<(Stage, usize)> = self
            .perturbators
            .iter()
            .enumerate()
            .map(|(i, (p, _))| (classify(p.breaks()), i))
            .collect();
        order.sort_by_key(|(stage, idx)| (*stage, *idx));

        let base_seed = self.config.seed.unwrap_or(42).wrapping_add(batch_offset);
        let mut batch = batch;

        info!(
            stages = ?order.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            "starting noise pipeline"
        );

        // Pre-compute scope masks once from the original batch so that
        // perturbators that mutate scoped fields don't shift the target rows.
        let mut precomputed_scope: std::collections::HashMap<usize, Arc<BooleanArray>> =
            std::collections::HashMap::new();
        for &(_, idx) in &order {
            let (_, overrides) = &self.perturbators[idx];
            if let Some(ref expr) = overrides.scope_expr {
                if let std::collections::hash_map::Entry::Vacant(e) = precomputed_scope.entry(idx) {
                    let mask = evaluate_scope_mask(expr, &batch)?;
                    e.insert(Arc::new(mask));
                }
            }
        }

        for (stage, idx) in &order {
            let (p, overrides) = &self.perturbators[*idx];
            // Derive uncorrelated per-perturbator seed using XOR with rotated index
            let derived_seed = base_seed ^ (*idx as u64).wrapping_mul(0x9E3779B97F4A7C15);
            let mut rng = ChaCha8Rng::seed_from_u64(derived_seed);

            // Build effective config with per-perturbator overrides.
            let mut effective_config = self.config.clone();
            if let Some(rate) = overrides.probability {
                effective_config.probability = rate;
            }
            if let Some(ref cols) = overrides.columns {
                effective_config.columns = cols.clone();
            }

            // Use pre-computed scope mask (evaluated once against original batch).
            if let Some(mask) = precomputed_scope.get(idx) {
                effective_config.scope_mask = Some(Arc::clone(mask));
            }

            debug!(
                perturbator = p.name(),
                stage = ?stage,
                probability = effective_config.probability,
                scoped = effective_config.scope_mask.is_some(),
                "applying perturbator"
            );
            batch = p.perturb(batch, &mut rng, &effective_config)?;
        }

        info!("noise pipeline complete");
        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::noise::traits::ColumnFilter;
    use arrow::array::{Array, BooleanArray, Float64Array, Int32Array};
    use arrow::datatypes::{DataType, Field, Schema};
    use std::sync::Arc;

    /// Trivial perturbator that records its call order via a name.
    struct Named {
        n: &'static str,
        inv: InvariantSet,
    }

    impl Perturbator for Named {
        fn name(&self) -> &str {
            self.n
        }
        fn breaks(&self) -> InvariantSet {
            self.inv
        }
        fn perturb(
            &self,
            batch: RecordBatch,
            _rng: &mut dyn rand::RngCore,
            _cfg: &PerturbConfig,
        ) -> Result<RecordBatch, NoiseError> {
            Ok(batch)
        }
    }

    fn sample_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("x", DataType::Int32, true),
            Field::new("y", DataType::Float64, true),
        ]));
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(vec![1, 2, 3])),
                Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0])),
            ],
        )
        .unwrap()
    }

    #[test]
    fn stage_ordering() {
        let cfg = PerturbConfig {
            probability: 0.0,
            columns: ColumnFilter::All,
            seed: Some(0),
            scope_mask: None,
        };
        let mut pipe = Pipeline::new(cfg);
        // Add in reverse order: breaking, constrained, clean
        pipe.add(Box::new(Named {
            n: "breaker",
            inv: InvariantSet::FK_INTEGRITY,
        }));
        pipe.add(Box::new(Named {
            n: "constrained",
            inv: InvariantSet::NOT_NULL,
        }));
        pipe.add(Box::new(Named {
            n: "clean",
            inv: InvariantSet::empty(),
        }));

        let order: Vec<(Stage, usize)> = pipe
            .perturbators
            .iter()
            .enumerate()
            .map(|(i, (p, _))| (classify(p.breaks()), i))
            .collect();
        let mut sorted = order.clone();
        sorted.sort_by_key(|(s, i)| (*s, *i));

        assert_eq!(sorted[0].0, Stage::Clean);
        assert_eq!(sorted[1].0, Stage::Constrained);
        assert_eq!(sorted[2].0, Stage::Breaking);
    }

    #[test]
    fn run_empty_pipeline() {
        let pipe = Pipeline::new(PerturbConfig::default());
        let batch = sample_batch();
        let result = pipe.run(batch.clone()).unwrap();
        assert_eq!(result.num_rows(), batch.num_rows());
    }

    /// Perturbator that records the probability it received.
    #[allow(dead_code)] // Reserved for probability propagation tests in this module.
    struct ProbRecorder {
        received_prob: std::sync::Mutex<f64>,
    }

    impl ProbRecorder {
        #[allow(dead_code)] // Reserved for probability propagation tests in this module.
        fn new() -> Self {
            Self {
                received_prob: std::sync::Mutex::new(0.0),
            }
        }
    }

    impl Perturbator for ProbRecorder {
        fn name(&self) -> &str {
            "ProbRecorder"
        }
        fn breaks(&self) -> InvariantSet {
            InvariantSet::empty()
        }
        fn perturb(
            &self,
            batch: RecordBatch,
            _rng: &mut dyn rand::RngCore,
            cfg: &PerturbConfig,
        ) -> Result<RecordBatch, NoiseError> {
            *self.received_prob.lock().unwrap() = cfg.probability;
            Ok(batch)
        }
    }

    #[test]
    fn per_perturbator_rate_override() {
        use crate::noise::NullInjector;

        let cfg = PerturbConfig::default().with_probability(0.05).with_seed(0);
        let mut pipe = Pipeline::new(cfg);

        // NullInjector with pipeline default (0.05)
        pipe.add(Box::new(NullInjector::new()));
        // NullInjector with override (0.42)
        pipe.add_with_rate(Box::new(NullInjector::new()), 0.42);

        // Run on a batch with 1000 nullable rows; compare null counts
        let schema = Arc::new(Schema::new(vec![
            Field::new("a", DataType::Int32, true),
            Field::new("b", DataType::Int32, true),
        ]));
        let big_batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from((0..1000).collect::<Vec<_>>())),
                Arc::new(Int32Array::from((0..1000).collect::<Vec<_>>())),
            ],
        )
        .unwrap();

        let result = pipe.run(big_batch).unwrap();
        // Column "a" gets nulled at 0.05 then again at 0.42.
        // With 1000 rows and high rate on second pass, we expect significant nulls.
        let total_nulls_a = result.column(0).null_count();
        // At 0.42 rate on the second pass alone, we expect ~420 nulls.
        assert!(
            total_nulls_a > 200,
            "expected substantial nulls from 0.42-rate pass, got {total_nulls_a}"
        );
    }

    #[test]
    fn scope_mask_restricts_perturbation() {
        use crate::noise::NullInjector;

        // Create a batch with 100 rows, scope mask only allows rows 0-9
        let schema = Arc::new(Schema::new(vec![Field::new("val", DataType::Int32, true)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from((0..100).collect::<Vec<i32>>()))],
        )
        .unwrap();

        // Scope mask: first 10 rows in scope, rest out
        let mut mask_vals = [false; 100];
        for v in mask_vals.iter_mut().take(10) {
            *v = true;
        }
        let scope_mask = Arc::new(BooleanArray::from(
            mask_vals.iter().map(|b| Some(*b)).collect::<Vec<_>>(),
        ));

        let cfg = PerturbConfig::default()
            .with_probability(1.0) // perturb ALL in-scope rows
            .with_seed(42)
            .with_scope_mask(scope_mask);

        let mut pipe = Pipeline::new(cfg);
        pipe.add(Box::new(NullInjector::new()));
        let result = pipe.run(batch).unwrap();

        // All 10 in-scope rows should be null, 90 out-of-scope rows untouched
        let col = result.column(0);
        assert_eq!(col.null_count(), 10);
        // Verify out-of-scope rows still have values
        let arr = col.as_any().downcast_ref::<Int32Array>().unwrap();
        for i in 10..100 {
            assert!(arr.is_valid(i), "row {i} should be untouched");
        }
    }

    #[test]
    fn scope_expr_evaluated_per_batch() {
        use crate::gen::expr::ast::{BinOp, Expr, LiteralValue};
        use crate::noise::NullInjector;

        // Build a batch where column "status" has two values
        let schema = Arc::new(Schema::new(vec![
            Field::new("val", DataType::Int32, true),
            Field::new("status", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(vec![1, 2, 3, 4, 5])),
                Arc::new(arrow::array::StringArray::from(vec![
                    "active", "refunded", "active", "refunded", "active",
                ])),
            ],
        )
        .unwrap();

        // Scope: ${status} == "refunded" → rows 1 and 3
        let scope_ast = Expr::BinaryOp {
            left: Box::new(Expr::FieldRef("status".into())),
            op: BinOp::Eq,
            right: Box::new(Expr::Literal(LiteralValue::Str("refunded".into()))),
        };

        let cfg = PerturbConfig::default().with_probability(1.0).with_seed(42);
        let mut pipe = Pipeline::new(cfg);
        pipe.add_with_overrides(
            Box::new(NullInjector::new()),
            PerturbOverrides {
                probability: Some(1.0),
                columns: Some(ColumnFilter::ByName(vec!["val".into()])),
                scope_expr: Some(scope_ast),
            },
        );

        let result = pipe.run(batch).unwrap();
        let val_col = result
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();

        // Rows 0, 2, 4 (active) should be valid
        assert!(val_col.is_valid(0));
        assert!(val_col.is_valid(2));
        assert!(val_col.is_valid(4));
        // Rows 1, 3 (refunded) should be null
        assert!(!val_col.is_valid(1));
        assert!(!val_col.is_valid(3));
    }

    #[test]
    fn scope_mask_with_swap_injector() {
        use crate::noise::SwapInjector;

        // 10 rows, scope only rows 0-4
        let schema = Arc::new(Schema::new(vec![Field::new("val", DataType::Int32, false)]));
        let original_vals: Vec<i32> = (0..10).collect();
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(original_vals.clone()))],
        )
        .unwrap();

        let mut mask_vals = [false; 10];
        for v in mask_vals.iter_mut().take(5) {
            *v = true;
        }
        let scope_mask = Arc::new(BooleanArray::from(
            mask_vals.iter().map(|b| Some(*b)).collect::<Vec<_>>(),
        ));

        let cfg = PerturbConfig::default()
            .with_probability(1.0)
            .with_seed(42)
            .with_scope_mask(scope_mask);

        let mut pipe = Pipeline::new(cfg);
        pipe.add(Box::new(SwapInjector::new()));
        let result = pipe.run(batch).unwrap();

        let arr = result
            .column(0)
            .as_any()
            .downcast_ref::<Int32Array>()
            .unwrap();

        // Out-of-scope rows 5-9 should be unchanged
        for (i, expected) in original_vals.iter().enumerate().skip(5) {
            assert_eq!(arr.value(i), *expected, "row {i} should be unchanged");
        }
    }

    #[test]
    fn scope_mask_with_duplicate_injector() {
        use crate::noise::DuplicateInjector;

        // 10 rows, scope only rows 0-2
        let schema = Arc::new(Schema::new(vec![Field::new("val", DataType::Int32, false)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(Int32Array::from(vec![
                10, 20, 30, 40, 50, 60, 70, 80, 90, 100,
            ]))],
        )
        .unwrap();

        let mut mask_vals = [false; 10];
        for v in mask_vals.iter_mut().take(3) {
            *v = true;
        }
        let scope_mask = Arc::new(BooleanArray::from(
            mask_vals.iter().map(|b| Some(*b)).collect::<Vec<_>>(),
        ));

        let cfg = PerturbConfig::default()
            .with_probability(1.0)
            .with_seed(42)
            .with_scope_mask(scope_mask);

        let mut pipe = Pipeline::new(cfg);
        pipe.add(Box::new(DuplicateInjector::new()));
        let result = pipe.run(batch).unwrap();

        // Should have duplicated up to 3 rows (in-scope), so 10 + [1..3] rows
        assert!(
            (10..=13).contains(&result.num_rows()),
            "expected 10-13 rows, got {}",
            result.num_rows()
        );
    }
}
