//! # knit-plan — Execution Planner
//!
#![warn(missing_docs)]
//! Compiles a validated [`DataModel`](knit_core::DataModel) into an [`ExecutionPlan`]
//! that drives parallel data generation in `knit-gen`.
//!
//! ## Pipeline Position
//!
//! ```text
//! Weave Schema → knit-schema → DataModel → knit-plan → ExecutionPlan → knit-gen
//! ```
//!
//! ## Key Entry Point
//!
//! - [`compile()`] — takes a `DataModel` and produces an `ExecutionPlan`
//!
//! ## What the Planner Does
//!
//! 1. **Dependency analysis** — builds a directed graph from relationships, detects
//!    cycles via Tarjan's SCC algorithm
//! 2. **Phase assignment** — topologically sorts entities into generation phases;
//!    cyclic references become deferred backpatch operations
//! 3. **Partition planning** — divides large entities into parallel chunks (~1M rows each)
//! 4. **RNG tree** — derives deterministic per-field per-partition seeds via SipHash
//! 5. **Index strategy** — decides key store implementation based on entity size
//!
//! ## Determinism Guarantee
//!
//! The same `DataModel` always produces the same `ExecutionPlan`, regardless of
//! platform, thread count, or time of day. The planner performs no I/O and consumes
//! no randomness.

pub mod compiler;
pub mod error;
pub mod graph;
pub mod partition;
pub mod rng_tree;
pub mod types;

pub use compiler::compile;
pub use error::PlanError;
pub use types::*;
