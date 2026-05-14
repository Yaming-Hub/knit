//! Output sinks for serializing Arrow `RecordBatch`es to various formats.
//!
//! Supported formats: Parquet, JSON, JSONL, CSV, Arrow IPC (Feather v2), Avro,
//! and MiniJinja templates.
//!
//! # Usage
//!
//! Use [`factory::create_sink`] to obtain a [`traits::Sink`] for a given
//! [`factory::OutputFormat`], then call [`Sink::write_batch`]
//! for each batch and [`Sink::finish`] to finalize.

pub mod avro;
pub mod csv;
pub mod error;
pub mod factory;
pub mod helpers;
pub mod ipc;
pub mod json;
pub mod parquet;
pub mod sql;
pub mod template;
pub mod traits;

pub use avro::AvroCodec;
pub use error::BindError;
pub use factory::{create_sink, OutputFormat, SinkConfig};
pub use json::MissingFieldSpec;
pub use parquet::Compression;
pub use sql::SqlConfig;
pub use template::{TemplateMode, TemplateSink};
pub use traits::{Sink, SinkStats};
