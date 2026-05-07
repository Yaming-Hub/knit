//! # knit-learn
//!
//! Schema inference from existing data sources. Provides data ingestion
//! (CSV, Parquet, JSON/JSONL), sampling strategies, column profiling,
//! and semantic type inference.
//!
//! ## Quick start
//!
//! ```rust,no_run
//! use knit_learn::ingest;
//! use knit_learn::profile::compute_profiles;
//!
//! let batches = ingest::read_csv(
//!     std::path::Path::new("data.csv"),
//!     &ingest::CsvOptions::default(),
//! ).unwrap();
//! let profiles = compute_profiles(&batches).unwrap();
//! ```

#![warn(missing_docs)]

pub mod behavioral;
pub mod correlation;
pub mod error;
pub mod fitting;
pub mod incremental;
pub mod ingest;
pub mod profile;
pub mod relationships;
pub mod sampling;
pub mod schema_assembly;
pub mod streaming;
pub mod temporal;
pub mod type_inference;

pub use error::{LearnError, LearnResult};
