//! Knit — a Rust toolset for generating large synthetic datasets.
//!
//! This crate provides:
//! - A schema language for defining data models with statistical specifications
//! - A generation engine that produces synthetic data from schemas
//! - Output binding to Parquet, CSV, JSON, and other formats
//! - A machine-learning tool to extract data models from existing datasets
//! - Configurable noise and anomaly injection

pub mod bind;
pub mod cli;
pub mod core;
pub mod enrich;
pub mod gen;
pub mod learn;
pub mod noise;
pub mod plan;
pub mod scale;
pub mod schema;
pub mod tokenize;
