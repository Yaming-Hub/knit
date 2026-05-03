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

use arrow::record_batch::RecordBatch;
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;
use tracing::{debug, info, instrument};

use crate::error::NoiseError;
use crate::traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};

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
    } else if invariants
        .intersects(InvariantSet::FK_INTEGRITY | InvariantSet::UNIQUE)
    {
        Stage::Breaking
    } else {
        Stage::Constrained
    }
}

/// Per-perturbator overrides for rate and column filter.
#[derive(Debug, Clone, Default)]
pub struct PerturbOverrides {
    /// Probability override (clamped to `[0.0, 1.0]`).
    pub probability: Option<f64>,
    /// Column filter override. If `None`, uses the pipeline default.
    pub columns: Option<ColumnFilter>,
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
/// ```ignore
/// let mut pipe = Pipeline::new(PerturbConfig::default());
/// pipe.add(Box::new(GaussianNoise::default()));
/// pipe.add_with_rate(Box::new(NullInjector::default()), 0.10);
/// let noisy = pipe.run(batch)?;
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
        self.perturbators.push((p, PerturbOverrides {
            probability: Some(rate.clamp(0.0, 1.0)),
            columns: None,
        }));
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

            debug!(
                perturbator = p.name(),
                stage = ?stage,
                probability = effective_config.probability,
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
    use crate::traits::ColumnFilter;
    use arrow::array::{Float64Array, Int32Array};
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
    struct ProbRecorder {
        received_prob: std::sync::Mutex<f64>,
    }

    impl ProbRecorder {
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
        use crate::NullInjector;

        let cfg = PerturbConfig::default()
            .with_probability(0.05)
            .with_seed(0);
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
}
