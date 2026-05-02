//! Noise injection and data-quality simulation for the knit pipeline.
//!
//! This crate sits between **knit-gen** (which produces clean, schema-conformant
//! [`RecordBatch`]es) and **knit-bind** (which serialises the final output).
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

pub mod error;
pub mod traits;
pub mod pipeline;

pub mod null_injector;
pub mod gaussian_noise;
pub mod typo_injector;
pub mod outlier_injector;
pub mod duplicate_injector;
pub mod value_drifter;
pub mod format_corruptor;

pub use error::NoiseError;
pub use traits::{Perturbator, InvariantSet, PerturbConfig, ColumnFilter};
pub use pipeline::Pipeline;
pub use null_injector::NullInjector;
pub use gaussian_noise::GaussianNoise;
pub use typo_injector::TypoInjector;
pub use outlier_injector::OutlierInjector;
pub use duplicate_injector::DuplicateInjector;
pub use value_drifter::ValueDrifter;
pub use format_corruptor::FormatCorruptor;
