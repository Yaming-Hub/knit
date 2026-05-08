//! Arrow IPC (Feather v2) output sink.

use std::io::Write;

use parking_lot::Mutex;
use std::sync::Arc;

use arrow::ipc::writer::FileWriter;
use arrow::record_batch::RecordBatch;
use tracing::debug;

use crate::error::BindError;
use crate::traits::{Sink, SinkStats};

/// Wrapper that counts bytes written through it.
struct CountingWriter<W: Write> {
    inner: W,
    count: Arc<Mutex<u64>>,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        *self.count.lock() += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Sink that writes `RecordBatch`es in Arrow IPC format (Feather v2).
pub struct ArrowIpcSink<W: Write + Send> {
    writer: Option<FileWriter<CountingWriter<W>>>,
    rows_written: u64,
    byte_count: Arc<Mutex<u64>>,
}

impl<W: Write + Send> ArrowIpcSink<W> {
    /// Create a new `ArrowIpcSink` writing to the given writer with the specified schema.
    pub fn new(
        writer: W,
        schema: std::sync::Arc<arrow::datatypes::Schema>,
    ) -> Result<Self, BindError> {
        let byte_count = Arc::new(Mutex::new(0u64));
        let counting = CountingWriter {
            inner: writer,
            count: Arc::clone(&byte_count),
        };
        let ipc_writer = FileWriter::try_new(counting, &schema)?;
        Ok(Self {
            writer: Some(ipc_writer),
            rows_written: 0,
            byte_count,
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
        debug!(
            rows = num_rows,
            total = self.rows_written,
            "wrote ipc batch"
        );
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<SinkStats, BindError> {
        let writer = self
            .writer
            .take()
            .ok_or_else(|| BindError::Other("sink already finished".into()))?;
        let _inner = writer.into_inner()?;
        let bytes_written = *self.byte_count.lock();
        debug!(
            rows = self.rows_written,
            bytes = bytes_written,
            "ipc sink finished"
        );
        Ok(SinkStats {
            rows_written: self.rows_written,
            bytes_written,
            files_created: 1,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int32Array, StringArray};
    use arrow::datatypes::{DataType, Field as ArrowField, Schema};

    fn sample_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            ArrowField::new("id", DataType::Int32, false),
            ArrowField::new("name", DataType::Utf8, false),
        ]))
    }

    fn sample_batch() -> RecordBatch {
        RecordBatch::try_new(
            sample_schema(),
            vec![
                Arc::new(Int32Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec!["alice", "bob", "carol"])),
            ],
        )
        .unwrap()
    }

    #[test]
    fn ipc_write_and_finish() {
        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut sink = ArrowIpcSink::new(cursor, sample_schema()).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 3);
        assert!(stats.bytes_written > 0);
        assert_eq!(stats.files_created, 1);
    }

    #[test]
    fn ipc_multiple_batches() {
        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let mut sink = ArrowIpcSink::new(cursor, sample_schema()).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 6);
    }

    #[test]
    fn ipc_finish_twice_errors() {
        let buf = Vec::new();
        let cursor = std::io::Cursor::new(buf);
        let sink = ArrowIpcSink::new(cursor, sample_schema()).unwrap();
        // Manually take the writer to simulate a finished state
        let mut sink = sink;
        sink.writer = None;
        let result = Box::new(sink).finish();
        assert!(result.is_err());
    }
}
