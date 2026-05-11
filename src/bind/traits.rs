//! Core trait definitions for output sinks.

use arrow::record_batch::RecordBatch;

use crate::bind::error::BindError;

/// Statistics collected during sink operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkStats {
    /// Total number of rows written across all batches.
    pub rows_written: u64,
    /// Total bytes written to the output destination.
    pub bytes_written: u64,
    /// Number of output files created.
    pub files_created: u32,
}

/// Output sink that receives `RecordBatch`es and writes them to a destination.
///
/// Implementations handle format-specific serialization (Parquet, JSON, CSV, etc.).
/// The generation engine calls [`Sink::write_batch`] for each produced batch, then
/// [`Sink::finish`] to flush and close the output.
pub trait Sink: Send {
    /// Write a single `RecordBatch` to the output.
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<(), BindError>;

    /// Flush remaining data and close the sink. Returns statistics.
    fn finish(self: Box<Self>) -> Result<SinkStats, BindError>;
}