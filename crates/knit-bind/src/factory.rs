//! Factory for creating output sinks from configuration.

use std::io::Write;
use std::sync::Arc;

use arrow::datatypes::Schema;

use crate::csv::{CsvSink, CsvSinkConfig};
use crate::error::BindError;
use crate::ipc::ArrowIpcSink;
use crate::json::{JsonMode, JsonSink};
use crate::parquet::{Compression, ParquetSink};
use crate::traits::Sink;

/// Supported output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputFormat {
    /// Apache Parquet columnar format.
    Parquet,
    /// JSON array format.
    Json,
    /// Newline-delimited JSON (one object per line).
    Jsonl,
    /// Comma-separated values.
    Csv,
    /// Arrow IPC / Feather v2 format.
    ArrowIpc,
}

/// Configuration for creating an output sink.
#[derive(Debug, Clone)]
pub struct SinkConfig {
    /// Output format to use.
    pub format: OutputFormat,
    /// Compression algorithm (used by Parquet).
    pub compression: Compression,
    /// CSV column delimiter.
    pub csv_delimiter: u8,
    /// Whether CSV output includes a header row.
    pub csv_header: bool,
    /// String representation for null values in CSV.
    pub null_representation: String,
}

impl Default for SinkConfig {
    fn default() -> Self {
        Self {
            format: OutputFormat::Parquet,
            compression: Compression::default(),
            csv_delimiter: b',',
            csv_header: true,
            null_representation: String::new(),
        }
    }
}

/// Create a sink for the given writer and configuration.
///
/// The `schema` is required for formats that need it up front (Parquet, Arrow IPC).
pub fn create_sink(
    writer: Box<dyn Write + Send>,
    schema: Arc<Schema>,
    config: &SinkConfig,
) -> Result<Box<dyn Sink>, BindError> {
    match config.format {
        OutputFormat::Parquet => {
            let sink = ParquetSink::new(writer, schema, config.compression, None)?;
            Ok(Box::new(sink))
        }
        OutputFormat::Json => {
            let sink = JsonSink::new(writer, JsonMode::JsonArray)?;
            Ok(Box::new(sink))
        }
        OutputFormat::Jsonl => {
            let sink = JsonSink::new(writer, JsonMode::Jsonl)?;
            Ok(Box::new(sink))
        }
        OutputFormat::Csv => {
            let csv_config = CsvSinkConfig {
                delimiter: config.csv_delimiter,
                header: config.csv_header,
                null_value: config.null_representation.clone(),
            };
            let sink = CsvSink::new(writer, &csv_config);
            Ok(Box::new(sink))
        }
        OutputFormat::ArrowIpc => {
            let sink = ArrowIpcSink::new(writer, schema)?;
            Ok(Box::new(sink))
        }
    }
}
