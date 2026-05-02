//! Arrow IPC (Feather v2) output sink.

use std::io::Write;

use arrow::ipc::writer::FileWriter;
use arrow::record_batch::RecordBatch;
use tracing::debug;

use crate::error::BindError;
use crate::traits::{Sink, SinkStats};

/// Sink that writes `RecordBatch`es in Arrow IPC format (Feather v2).
pub struct ArrowIpcSink<W: Write + Send> {
    writer: Option<FileWriter<W>>,
    rows_written: u64,
}

impl<W: Write + Send> ArrowIpcSink<W> {
    /// Create a new `ArrowIpcSink` writing to the given writer with the specified schema.
    pub fn new(
        writer: W,
        schema: std::sync::Arc<arrow::datatypes::Schema>,
    ) -> Result<Self, BindError> {
        let ipc_writer = FileWriter::try_new(writer, &schema)?;
        Ok(Self {
            writer: Some(ipc_writer),
            rows_written: 0,
        })
    }
}

impl<W: Write + Send> Sink for ArrowIpcSink<W> {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<(), BindError> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| BindError::Other("sink already finished".into()))?;
        let num_rows = batch.num_rows() as u64;
        writer.write(batch)?;
        self.rows_written += num_rows;
        debug!(rows = num_rows, total = self.rows_written, "wrote ipc batch");
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<SinkStats, BindError> {
        let writer = self
            .writer
            .take()
            .ok_or_else(|| BindError::Other("sink already finished".into()))?;
        let inner = writer.into_inner()?;
        // Get total bytes from the underlying writer if it supports it.
        // For generic writers we can't know, so we use stream_position if seekable.
        let bytes_written = get_stream_position(&inner);
        debug!(rows = self.rows_written, bytes = bytes_written, "ipc sink finished");
        Ok(SinkStats {
            rows_written: self.rows_written,
            bytes_written,
            files_created: 1,
        })
    }
}

/// Attempt to get the current position of a writer for byte counting.
fn get_stream_position<W: Write>(_writer: &W) -> u64 {
    // Generic writers don't necessarily implement Seek.
    // For Vec<u8> and Cursor types used in tests, the caller can check length.
    0
}
