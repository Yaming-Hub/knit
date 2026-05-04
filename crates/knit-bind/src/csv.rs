//! CSV output sink using Arrow's CSV writer.

use std::io::Write;

use parking_lot::Mutex;
use std::sync::Arc;

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
        *self.count.lock() += n as u64;
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
        // Drop the Arrow CSV writer to trigger its internal flush.
        // Errors during drop-based flush are silently discarded by Rust,
        // but the CountingWriter has already tracked all successful writes.
        drop(self.writer);
        let bytes_written = *self.byte_count.lock();
        debug!(rows = self.rows_written, bytes = bytes_written, "csv sink finished");
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

    /// A writer backed by a shared buffer so we can inspect output after the sink
    /// consumes the writer.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }
        fn to_string(&self) -> String {
            String::from_utf8(self.0.lock().clone()).unwrap()
        }
    }

    impl Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn sample_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            ArrowField::new("id", DataType::Int32, false),
            ArrowField::new("name", DataType::Utf8, false),
        ]));
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int32Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec!["alice", "bob", "carol"])),
            ],
        )
        .unwrap()
    }

    #[test]
    fn csv_write_single_batch() {
        let buf = SharedBuf::new();
        let config = CsvSinkConfig::default();
        let mut sink = CsvSink::new(buf.clone(), &config);
        sink.write_batch(&sample_batch()).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 3);
        assert!(stats.bytes_written > 0);
        assert_eq!(stats.files_created, 1);
        let content = buf.to_string();
        assert!(content.contains("id,name"), "should contain CSV header");
        assert!(content.contains("1,alice"), "should contain first data row");
    }

    #[test]
    fn csv_write_multiple_batches() {
        let buf = SharedBuf::new();
        let config = CsvSinkConfig::default();
        let mut sink = CsvSink::new(buf, &config);
        sink.write_batch(&sample_batch()).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 6);
    }

    #[test]
    fn csv_custom_delimiter_in_output() {
        let buf = SharedBuf::new();
        let config = CsvSinkConfig {
            delimiter: b'\t',
            header: true,
            null_value: String::new(),
        };
        let mut sink = CsvSink::new(buf.clone(), &config);
        sink.write_batch(&sample_batch()).unwrap();
        Box::new(sink).finish().unwrap();
        let content = buf.to_string();
        assert!(content.contains("id\tname"), "header should use tab delimiter");
        assert!(content.contains("1\talice"), "data should use tab delimiter");
        assert!(!content.contains("id,name"), "should not contain comma delimiter");
    }

    #[test]
    fn csv_no_header_omits_header_row() {
        let buf = SharedBuf::new();
        let config = CsvSinkConfig {
            header: false,
            ..Default::default()
        };
        let mut sink = CsvSink::new(buf.clone(), &config);
        sink.write_batch(&sample_batch()).unwrap();
        Box::new(sink).finish().unwrap();
        let content = buf.to_string();
        assert!(!content.contains("id,name"), "should not contain header row");
        assert!(content.starts_with("1,alice"), "should start with data row");
    }
}
