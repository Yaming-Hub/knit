//! Streaming statistics for incremental learning.
//!
//! This module provides bounded-memory data structures that compute
//! statistics over arbitrarily large data streams. Each structure supports
//! incremental updates and merging, enabling chunked processing of datasets
//! that don't fit in memory.
//!
//! ## Components
//!
//! - [`NumericState`] — Online mean, variance, min/max via Welford's algorithm
//! - [`ReservoirSample`] — Deterministic uniform random sampling (Algorithm R)
//! - [`TopKTracker`] — Approximate frequent-item tracking (Space-Saving)
//! - [`HyperLogLog`] — Probabilistic cardinality estimation
//! - [`LearnState`] — Top-level persistent state container

mod hll;
mod numeric;
mod reservoir;
pub mod state;
mod topk;

pub use hll::HyperLogLog;
pub use numeric::NumericState;
pub use reservoir::ReservoirSample;
pub use state::{ChunkRecord, ColumnDataType, ColumnState, LearnState, StateError, TableState};
pub use topk::TopKTracker;
