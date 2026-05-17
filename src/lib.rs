//! # Knit — High-Performance Synthetic Data Generation
//!
//! Knit is a Rust toolset for generating large synthetic datasets (100 GB+ in
//! hours). It combines a declarative blueprint language with a multi-stage
//! pipeline that compiles, generates, perturbs, and serializes data.
//!
//! ## Pipeline Overview
//!
//! ```text
//! Blueprint (TOML/JSON)
//!     → blueprint (parse & validate)
//!         → DataModel
//!             → plan (compile)
//!                 → ExecutionPlan
//!                     → gen (generate)
//!                         → RecordBatch stream
//!                             → noise (perturb)
//!                                 → bind (serialize)
//!                                     → Output files (Parquet/CSV/JSON/…)
//! ```
//!
//! ## Modules
//!
//! | Module | Role |
//! |--------|------|
//! | [`core`] | Shared type definitions (`DataModel`, `Entity`, `Field`, `Value`, …) |
//! | [`blueprint`] | Parse TOML/JSON blueprints and validate the resulting `DataModel` |
//! | [`plan`] | Compile a `DataModel` into a deterministic `ExecutionPlan` |
//! | [`gen`] | Execute the plan to produce Arrow `RecordBatch` streams |
//! | [`noise`] | Inject controlled imperfections (nulls, typos, outliers, …) |
//! | [`bind`] | Serialize `RecordBatch`es to Parquet, CSV, JSON, Avro, SQL, etc. |
//! | [`learn`] | Reverse-engineer a blueprint from existing data |
//! | [`scale`] | Multi-dimensional dataset scaling (actors, time, categories) |
//! | [`tokenize`] | Replace sensitive values with opaque tokens |
//! | [`enrich`] | Merge statistical knowledge from reference data into a model |
//! | [`model`] | Read/write structured model directories |
//! | [`decision`] | Decision logging and reporting |
//! | [`cli`] | CLI commands and configuration |
//!
//! ## Key Design Properties
//!
//! - **Deterministic** — same blueprint + seed always produces identical output
//! - **Columnar** — generates Arrow `RecordBatch`es for efficient Parquet serialization
//! - **Batch-oriented** — processes data in configurable batches (default 64K rows)
//! - **Parallel** — partitions execute concurrently via `rayon`
//! - **Extensible** — custom generators via the [`gen::FieldGenerator`] trait or
//!   WASM plugins loaded at runtime
//!
//! ## Quick Start (Library Usage)
//!
//! ```rust,no_run
//! use std::path::Path;
//! use knit::blueprint::{parse_toml_file, validate};
//! use knit::plan::compile;
//!
//! // Parse and validate a blueprint
//! let model = parse_toml_file(Path::new("schema.knit.toml"))
//!     .expect("parse error");
//! let errors = validate(&model);
//! assert!(errors.is_empty(), "validation errors: {:?}", errors);
//!
//! // Compile into an execution plan
//! let plan = compile(&model).expect("compilation error");
//! ```

#![warn(missing_docs)]
#![deny(unsafe_code)]

pub mod bind;
pub mod blueprint;
pub mod cli;
pub mod core;
pub mod decision;
pub mod enrich;
pub mod r#gen;
pub mod learn;
pub mod model;
pub mod noise;
pub mod plan;
pub mod scale;
pub mod tokenize;
