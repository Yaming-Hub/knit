//! knit-bind: Output sinks for serializing Arrow `RecordBatch`es to various formats.
//!
//! Supported formats: Parquet, JSON, JSONL, CSV, Arrow IPC (Feather v2), and
//! MiniJinja templates.
//!
//! # Usage
//!
//! Use [`factory::create_sink`] to obtain a [`traits::Sink`] for a given
//! [`factory::OutputFormat`], then call [`Sink::write_batch`]
//! for each batch and [`Sink::finish`] to finalize.

#![warn(missing_docs)]

pub mod csv;
pub mod error;
pub mod factory;
pub mod helpers;
pub mod ipc;
pub mod json;
pub mod parquet;
pub mod template;
pub mod traits;

pub use error::BindError;
pub use factory::{create_sink, OutputFormat, SinkConfig};
pub use parquet::Compression;
pub use template::{TemplateSink, TemplateMode};
pub use traits::{Sink, SinkStats};
