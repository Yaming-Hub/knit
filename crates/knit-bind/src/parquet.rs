//! Parquet output sink using Arrow's native Parquet writer.

use std::io::Write;
use std::sync::Arc;

use arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression as PqCompression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use tracing::debug;

use crate::error::BindError;
use crate::traits::{Sink, SinkStats};

/// Compression algorithm for Parquet output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Compression {
    /// No compression.
    #[default]
    None,
    /// Snappy compression (fast, moderate ratio).
    Snappy,
    /// LZ4 compression.
    Lz4,
    /// Zstd compression (good ratio, configurable level).
    Zstd,
}

/// Sink that writes `RecordBatch`es in Parquet format.
pub struct ParquetSink<W: Write + Send> {
    writer: Option<ArrowWriter<W>>,
    rows_written: u64,
}

impl<W: Write + Send> ParquetSink<W> {
    /// Create a new `ParquetSink` writing to the given writer.
    ///
    /// The schema is inferred from the first batch written. `compression` selects
    /// the codec and `row_group_size` controls the maximum rows per row group
    /// (defaults to 1,048,576 if `None`).
    pub fn new(
        writer: W,
        schema: Arc<arrow::datatypes::Schema>,
        compression: Compression,
        row_group_size: Option<usize>,
    ) -> Result<Self, BindError> {
        let pq_compression = match compression {
            Compression::None => PqCompression::UNCOMPRESSED,
            Compression::Snappy => PqCompression::SNAPPY,
            Compression::Lz4 => PqCompression::LZ4,
            Compression::Zstd => PqCompression::ZSTD(ZstdLevel::try_new(3)?),
        };

        let mut props_builder = WriterProperties::builder().set_compression(pq_compression);
        if let Some(rg_size) = row_group_size {
            props_builder = props_builder.set_max_row_group_size(rg_size);
        }
        let props = props_builder.build();

        let arrow_writer = ArrowWriter::try_new(writer, schema, Some(props))?;
        Ok(Self {
            writer: Some(arrow_writer),
            rows_written: 0,
        })
    }
}

impl<W: Write + Send> Sink for ParquetSink<W> {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<(), BindError> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| BindError::Other("sink already finished".into()))?;
        let num_rows = batch.num_rows() as u64;
        writer.write(batch)?;
        self.rows_written += num_rows;
        debug!(rows = num_rows, total = self.rows_written, "wrote parquet batch");
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<SinkStats, BindError> {
        let writer = self
            .writer
            .take()
            .ok_or_else(|| BindError::Other("sink already finished".into()))?;
        let metadata = writer.close()?;
        let bytes_written = metadata
            .row_groups
            .iter()
            .map(|rg| rg.total_byte_size as u64)
            .sum();
        debug!(rows = self.rows_written, bytes = bytes_written, "parquet sink finished");
        Ok(SinkStats {
            rows_written: self.rows_written,
            bytes_written,
            files_created: 1,
        })
    }
}
