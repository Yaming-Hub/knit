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
//! - [`RelationshipEvidence`] — HLL-based FK relationship detection
//! - [`PairwiseCorrelation`] — Streaming Pearson correlation

mod hll;
mod numeric;
pub mod relationships;
mod reservoir;
pub mod state;
mod topk;

pub use hll::HyperLogLog;
pub use numeric::NumericState;
pub use relationships::{
    detect_candidates, finalize_relationships, FinalizedRelationship, IncrementalRelColumn,
    PairwiseCorrelation, RelKind, RelationshipEvidence,
};
pub use reservoir::ReservoirSample;
pub use state::{ChunkRecord, ColumnDataType, ColumnState, LearnState, StateError, TableState};
pub use topk::TopKTracker;
