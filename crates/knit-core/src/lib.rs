//! # knit-core — Shared Type Definitions
//!
#![warn(missing_docs)]
//! This crate provides the canonical data model types used by every other crate
//! in the knit workspace:
//!
//! - **[`DataModel`]** — the root type representing a complete Weave schema
//! - **[`Entity`]**, **[`Field`]**, **[`Relationship`]** — the structural building blocks
//! - **[`GeneratorSpec`]**, **[`DistributionSpec`]**, **[`NullSpec`]** — generation configuration
//! - **[`NoiseProfile`]**, **[`Correlation`]**, **[`Constraint`]** — data quality and integrity rules
//! - **[`ModelError`]** — validation errors produced by `knit-schema`
//!
//! ## Pipeline Position
//!
//! ```text
//! Weave Schema → knit-schema (parse) → DataModel → knit-plan (compile) → ExecutionPlan → knit-gen
//! ```
//!
//! `knit-core` types are **created** by the `knit-schema` parser and **consumed** by
//! the `knit-plan` compiler and `knit-gen` engine. They are serializable via
//! `serde` so schemas can round-trip through TOML, JSON, or any other format.

pub mod error;
pub mod types;

#[cfg(test)]
mod tests;

pub use error::*;
pub use types::*;
