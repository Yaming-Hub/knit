//! CSV output sink using Arrow's CSV writer.

use std::io::Write;
use std::sync::{Arc, Mutex};

use arrow::csv::WriterBuilder;
use arrow::record_batch::RecordBatch;
use tracing::debug;

use crate::error::BindError;
use crate::traits::{Sink, SinkStats};

/// Configuration for the CSV sink.
#[derive(Debug, Clone)]
pub struct CsvSinkConfig {
    /// Column delimiter (default: `b','`).
    pub delimiter: u8,
    /// Whether to write a header row (default: `true`).
    pub header: bool,
    /// Representation for null values (default: empty string).
    pub null_value: String,
}

impl Default for CsvSinkConfig {
    fn default() -> Self {
        Self {
            delimiter: b',',
            header: true,
            null_value: String::new(),
        }
    }
}

/// Wrapper that counts bytes written through it.
struct CountingWriter<W: Write> {
    inner: W,
    count: Arc<Mutex<u64>>,
}

impl<W: Write> Write for CountingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        *self.count.lock().unwrap() += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Sink that writes `RecordBatch`es in CSV format.
pub struct CsvSink<W: Write + Send> {
    writer: arrow::csv::Writer<CountingWriter<W>>,
    rows_written: u64,
    byte_count: Arc<Mutex<u64>>,
}

impl<W: Write + Send> CsvSink<W> {
    /// Create a new `CsvSink` with the given configuration.
    pub fn new(writer: W, config: &CsvSinkConfig) -> Self {
        let byte_count = Arc::new(Mutex::new(0u64));
        let counting = CountingWriter {
            inner: writer,
            count: Arc::clone(&byte_count),
        };

        let csv_writer = WriterBuilder::new()
            .with_delimiter(config.delimiter)
            .with_header(config.header)
            .with_null(config.null_value.clone())
            .build(counting);

        Self {
            writer: csv_writer,
            rows_written: 0,
            byte_count,
        }
    }
}

impl<W: Write + Send> Sink for CsvSink<W> {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<(), BindError> {
        let num_rows = batch.num_rows() as u64;
        self.writer.write(batch)?;
        self.rows_written += num_rows;
        debug!(rows = num_rows, total = self.rows_written, "wrote csv batch");
        Ok(())
    }

    fn finish(self: Box<Self>) -> Result<SinkStats, BindError> {
        let bytes_written = *self.byte_count.lock().unwrap();
        debug!(rows = self.rows_written, bytes = bytes_written, "csv sink finished");
        Ok(SinkStats {
            rows_written: self.rows_written,
            bytes_written,
            files_created: 1,
        })
    }
}
