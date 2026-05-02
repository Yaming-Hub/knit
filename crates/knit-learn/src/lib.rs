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

pub mod error;
pub mod ingest;
pub mod profile;
pub mod sampling;
pub mod type_inference;

pub use error::{LearnError, LearnResult};
