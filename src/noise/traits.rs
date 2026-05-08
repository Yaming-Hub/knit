//! Core trait and supporting types for the perturbation pipeline.
//!
//! Every noise strategy implements [`Perturbator`]. The [`InvariantSet`]
//! bitflags declare which data invariants a perturbator *may* violate,
//! enabling [`Pipeline`](crate::noise::Pipeline) to order execution into
//! clean → constrained → breaking stages.

use arrow::record_batch::RecordBatch;
use bitflags::bitflags;
use rand::RngCore;

use crate::noise::error::NoiseError;

bitflags! {
    /// Describes which data invariants a [`Perturbator`] may violate.
    ///
    /// The pipeline uses these flags to sort perturbators into stages:
    /// - **Clean** (`InvariantSet::empty()`) — no invariants broken.
    /// - **Constrained** — breaks some invariants.
    /// - **Breaking** — intentionally violates hard constraints.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
    pub struct InvariantSet: u32 {
        /// May introduce null values where none existed.
        const NOT_NULL      = 0b0000_0001;
        /// May introduce duplicate values in unique columns.
        const UNIQUE        = 0b0000_0010;
        /// May break foreign-key referential integrity.
        const FK_INTEGRITY  = 0b0000_0100;
        /// May produce values outside the valid range for a type.
        const TYPE_RANGE    = 0b0000_1000;
        /// May corrupt the expected string format.
        const FORMAT        = 0b0001_0000;
    }
}

/// Controls which columns a perturbator targets.
#[derive(Debug, Clone, Default)]
pub enum ColumnFilter {
    /// Apply to all columns whose Arrow data type is compatible.
    #[default]
    All,
    /// Apply only to columns whose names are in this list.
    ByName(Vec<String>),
}

/// Per-invocation configuration for a perturbator.
///
/// Created by the caller (typically [`Pipeline`](crate::noise::Pipeline)) and passed
/// into every [`Perturbator::perturb`] call.
#[derive(Debug, Clone)]
pub struct PerturbConfig {
    /// Probability in `[0.0, 1.0]` that each eligible cell is perturbed.
    pub probability: f64,
    /// Which columns to target.
    pub columns: ColumnFilter,
    /// Optional RNG seed for reproducibility.
    pub seed: Option<u64>,
}

impl Default for PerturbConfig {
    fn default() -> Self {
        Self {
            probability: 0.05,
            columns: ColumnFilter::All,
            seed: None,
        }
    }
}

impl PerturbConfig {
    /// Create a new config with the given probability.
    pub fn with_probability(mut self, p: f64) -> Self {
        self.probability = p;
        self
    }

    /// Restrict perturbation to the named columns.
    pub fn with_columns(mut self, names: Vec<String>) -> Self {
        self.columns = ColumnFilter::ByName(names);
        self
    }

    /// Set the column filter directly.
    pub fn with_columns_filter(mut self, filter: ColumnFilter) -> Self {
        self.columns = filter;
        self
    }

    /// Set an explicit RNG seed for reproducibility.
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = Some(seed);
        self
    }
}

/// A single noise strategy that can be applied to a [`RecordBatch`].
///
/// Implementations must be `Send + Sync` so the pipeline can run them
/// across threads. Each perturbator declares which invariants it may
/// break via [`Perturbator::breaks`], and the pipeline uses that to
/// determine execution order.
pub trait Perturbator: Send + Sync {
    /// Human-readable name used in tracing spans and diagnostics.
    fn name(&self) -> &str;

    /// Invariants this perturbator may violate.
    fn breaks(&self) -> InvariantSet;

    /// Apply noise to `batch`, returning a new (potentially modified) batch.
    ///
    /// # Errors
    ///
    /// Returns [`NoiseError`] if the batch schema is incompatible or an
    /// Arrow operation fails.
    fn perturb(
        &self,
        batch: RecordBatch,
        rng: &mut dyn RngCore,
        config: &PerturbConfig,
    ) -> Result<RecordBatch, NoiseError>;
}
