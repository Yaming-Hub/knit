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
use crate::traits::{InvariantSet, PerturbConfig, Perturbator};

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

/// Three-stage perturbation pipeline.
///
/// Add perturbators with [`Pipeline::add`], then call [`Pipeline::run`] to
/// apply them all in the correct stage order.
///
/// # Example
///
/// ```ignore
/// let mut pipe = Pipeline::new(PerturbConfig::default());
/// pipe.add(Box::new(GaussianNoise::default()));
/// pipe.add(Box::new(NullInjector::default()));
/// let noisy = pipe.run(batch)?;
/// ```
pub struct Pipeline {
    perturbators: Vec<Box<dyn Perturbator>>,
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

    /// Append a perturbator to the pipeline.
    pub fn add(&mut self, p: Box<dyn Perturbator>) {
        self.perturbators.push(p);
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
        // Build (stage, index) pairs and sort by stage while preserving
        // insertion order within each stage.
        let mut order: Vec<(Stage, usize)> = self
            .perturbators
            .iter()
            .enumerate()
            .map(|(i, p)| (classify(p.breaks()), i))
            .collect();
        order.sort_by_key(|(stage, idx)| (*stage, *idx));

        let base_seed = self.config.seed.unwrap_or(42);
        let mut batch = batch;

        info!(
            stages = ?order.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            "starting noise pipeline"
        );

        for (stage, idx) in &order {
            let p = &self.perturbators[*idx];
            // Derive uncorrelated per-perturbator seed using XOR with rotated index
            let derived_seed = base_seed ^ (*idx as u64).wrapping_mul(0x9E3779B97F4A7C15);
            let mut rng = ChaCha8Rng::seed_from_u64(derived_seed);
            debug!(
                perturbator = p.name(),
                stage = ?stage,
                "applying perturbator"
            );
            batch = p.perturb(batch, &mut rng, &self.config)?;
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
            .map(|(i, p)| (classify(p.breaks()), i))
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
}
