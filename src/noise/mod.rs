//! Noise injection and data-quality simulation for the knit pipeline.
//!
//! This module sits between **[`gen`](crate::gen)** (which produces clean, schema-conformant
//! [`RecordBatch`](arrow::record_batch::RecordBatch)es) and **[`bind`](crate::bind)** (which serialises the final output).
//! It introduces controlled imperfections — nulls, typos, outliers, drift, etc.
//! — so that downstream consumers can test their resilience to real-world data
//! quality issues.
//!
//! # Key entry points
//!
//! * [`Perturbator`] — trait implemented by every noise strategy.
//! * [`InvariantSet`] — bitflags describing which data invariants a perturbator
//!   may violate.
//! * [`PerturbConfig`] — per-invocation knobs (probability, seed, column filter).
//! * [`Pipeline`] — three-stage executor that applies perturbators in
//!   *clean → constrained → breaking* order.
//!
//! # Built-in perturbators
//!
//! | Perturbator | Invariants broken |
//! |---|---|
//! | [`NullInjector`] | `NOT_NULL` |
//! | [`GaussianNoise`] | (none by default) |
//! | [`TypoInjector`] | `FORMAT` |
//! | [`OutlierInjector`] | `TYPE_RANGE` |
//! | [`DuplicateInjector`] | `UNIQUE` |
//! | [`ValueDrifter`] | `TYPE_RANGE` |
//! | [`FormatCorruptor`] | `FORMAT` |
//! | [`SwapInjector`] | (none — clean stage) |
//! | [`TruncateInjector`] | `FORMAT`, `UNIQUE` |
//! | [`FkViolateInjector`] | `FK_INTEGRITY` |
//! | [`TemporalSpikeInjector`] | `TYPE_RANGE` |
//!
//! The `missing_field` noise type is handled at the JSON serialization layer
//! (see [`crate::bind::json::MissingFieldSpec`]), not as a `Perturbator`.

pub mod error;
pub mod pipeline;
pub mod traits;

pub mod duplicate_injector;
pub mod fk_violate_injector;
pub mod format_corruptor;
pub mod gaussian_noise;
pub mod null_injector;
pub mod outlier_injector;
pub mod swap_injector;
pub mod temporal_spike_injector;
pub mod truncate_injector;
pub mod typo_injector;
pub mod value_drifter;

pub use duplicate_injector::DuplicateInjector;
pub use error::NoiseError;
pub use fk_violate_injector::FkViolateInjector;
pub use format_corruptor::FormatCorruptor;
pub use gaussian_noise::GaussianNoise;
pub use null_injector::NullInjector;
pub use outlier_injector::OutlierInjector;
pub use pipeline::{PerturbOverrides, Pipeline};
pub use swap_injector::SwapInjector;
pub use temporal_spike_injector::TemporalSpikeInjector;
pub use traits::{ColumnFilter, InvariantSet, PerturbConfig, Perturbator};
pub use truncate_injector::TruncateInjector;
pub use typo_injector::TypoInjector;
pub use value_drifter::ValueDrifter;
