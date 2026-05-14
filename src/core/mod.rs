//! # core — Shared Type Definitions
//!
//! This module provides the canonical data model types used by every other module
//! in knit:
//!
//! - **[`DataModel`]** — the root type representing a complete Weave schema
//! - **[`Entity`]**, **[`Field`]**, **[`Relationship`]** — the structural building blocks
//! - **[`GeneratorSpec`]**, **[`DistributionSpec`]**, **[`NullSpec`]** — generation configuration
//! - **[`NoiseProfile`]**, **[`Correlation`]**, **[`Constraint`]** — data quality and integrity rules
//! - **[`ModelError`]** — validation errors for structural and semantic issues
//!
//! ## Pipeline Position
//!
//! ```text
//! Weave Schema → blueprint (parse) → DataModel → plan (compile) → ExecutionPlan → gen
//! ```
//!
//! `core` types are **created** by the [`blueprint`](crate::blueprint) parser and
//! the [`learn`](crate::learn) inference engine, and **consumed** by the
//! [`plan`](crate::plan) compiler and [`gen`](crate::gen) engine. They are
//! serializable via `serde` so schemas can round-trip through TOML, JSON, or
//! any other format.

pub mod error;
pub mod types;

#[cfg(test)]
mod tests;

pub use error::*;
pub use types::*;
