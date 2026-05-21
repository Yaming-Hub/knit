//! Factory for creating output sinks from configuration.

use std::io::Write;
use std::sync::Arc;

use arrow::datatypes::Schema;

use crate::bind::avro::{AvroCodec, AvroSink};
use crate::bind::csv::{CsvSink, CsvSinkConfig};
use crate::bind::error::BindError;
use crate::bind::ipc::ArrowIpcSink;
use crate::bind::json::{JsonMode, JsonSink, MissingFieldSpec};
use crate::bind::parquet::{Compression, ParquetSink};
use crate::bind::sql::{SqlConfig, SqlSink};
use crate::bind::template::{TemplateMode, TemplateSink};
use crate::bind::traits::Sink;

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
    /// Apache Avro Object Container Format.
    Avro,
    /// SQL INSERT statements.
    Sql,
    /// Template-based output rendered via MiniJinja.
    Template,
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
    /// MiniJinja template source string (used when format is `Template`).
    pub template_source: String,
    /// Template rendering mode (`None` for auto-detection).
    pub template_mode: Option<TemplateMode>,
    /// Avro compression codec (used when format is `Avro`).
    pub avro_codec: AvroCodec,
    /// Record name for Avro output (defaults to "Record").
    pub record_name: String,
    /// Fields to randomly omit from document output (JSON/JSONL only).
    pub missing_field_specs: Vec<MissingFieldSpec>,
    /// Whether to emit CREATE TABLE DDL (SQL format only).
    pub sql_create_table: bool,
    /// Whether to wrap SQL output in BEGIN/COMMIT.
    pub sql_transaction: bool,
    /// Number of rows per INSERT statement (SQL format only).
    pub sql_rows_per_insert: usize,
}

impl Default for SinkConfig {
    fn default() -> Self {
        Self {
            format: OutputFormat::Parquet,
            compression: Compression::default(),
            csv_delimiter: b',',
            csv_header: true,
            null_representation: String::new(),
            template_source: String::new(),
            template_mode: None,
            avro_codec: AvroCodec::default(),
            record_name: "Record".to_string(),
            missing_field_specs: Vec::new(),
            sql_create_table: false,
            sql_transaction: false,
            sql_rows_per_insert: 100,
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
            let sink = JsonSink::new(writer, JsonMode::JsonArray)?
                .with_missing_fields(config.missing_field_specs.clone());
            Ok(Box::new(sink))
        }
        OutputFormat::Jsonl => {
            let sink = JsonSink::new(writer, JsonMode::Jsonl)?
                .with_missing_fields(config.missing_field_specs.clone());
            Ok(Box::new(sink))
        }
        OutputFormat::Csv => {
            let csv_config = CsvSinkConfig {
                delimiter: config.csv_delimiter,
                header: config.csv_header,
                null_value: config.null_representation.clone(),
                timestamp_format: Some("%Y-%m-%d %H:%M:%S".to_string()),
            };
            let sink = CsvSink::new(writer, &csv_config);
            Ok(Box::new(sink))
        }
        OutputFormat::ArrowIpc => {
            let sink = ArrowIpcSink::new(writer, schema)?;
            Ok(Box::new(sink))
        }
        OutputFormat::Avro => {
            let sink = AvroSink::new(writer, schema, &config.record_name, config.avro_codec)?;
            Ok(Box::new(sink))
        }
        OutputFormat::Sql => {
            let sql_config = SqlConfig {
                table_name: config.record_name.clone(),
                create_table: config.sql_create_table,
                transaction: config.sql_transaction,
                rows_per_insert: config.sql_rows_per_insert,
            };
            let sink = SqlSink::new(writer, schema, sql_config);
            Ok(Box::new(sink))
        }
        OutputFormat::Template => {
            let sink =
                TemplateSink::new(writer, config.template_source.clone(), config.template_mode)?;
            Ok(Box::new(sink))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{Int32Array, StringArray};
    use arrow::datatypes::{DataType, Field as ArrowField};
    use parking_lot::Mutex;

    /// A writer backed by a shared buffer so we can inspect output after the sink
    /// consumes the writer.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn new() -> Self {
            Self(Arc::new(Mutex::new(Vec::new())))
        }
        fn bytes(&self) -> Vec<u8> {
            self.0.lock().clone()
        }
        fn content(&self) -> String {
            String::from_utf8(self.bytes()).unwrap()
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

    fn sample_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            ArrowField::new("id", DataType::Int32, false),
            ArrowField::new("name", DataType::Utf8, false),
        ]))
    }

    fn sample_batch() -> arrow::record_batch::RecordBatch {
        arrow::record_batch::RecordBatch::try_new(
            sample_schema(),
            vec![
                Arc::new(Int32Array::from(vec![1, 2])),
                Arc::new(StringArray::from(vec!["a", "b"])),
            ],
        )
        .unwrap()
    }

    #[test]
    fn factory_csv() {
        let buf = SharedBuf::new();
        let config = SinkConfig {
            format: OutputFormat::Csv,
            ..Default::default()
        };
        let writer: Box<dyn Write + Send> = Box::new(buf.clone());
        let mut sink = create_sink(writer, sample_schema(), &config).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = sink.finish().unwrap();
        assert_eq!(stats.rows_written, 2);
        let content = buf.content();
        assert!(
            content.contains("id,name"),
            "CSV output should contain header"
        );
        assert!(content.contains("1,a"), "CSV output should contain data");
    }

    #[test]
    fn factory_json() {
        let buf = SharedBuf::new();
        let config = SinkConfig {
            format: OutputFormat::Json,
            ..Default::default()
        };
        let writer: Box<dyn Write + Send> = Box::new(buf.clone());
        let mut sink = create_sink(writer, sample_schema(), &config).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = sink.finish().unwrap();
        assert_eq!(stats.rows_written, 2);
        let content = buf.content();
        assert!(
            content.contains('['),
            "JSON array output should start with ["
        );
        assert!(
            content.contains("\"id\""),
            "JSON output should contain field names"
        );
    }

    #[test]
    fn factory_jsonl() {
        let buf = SharedBuf::new();
        let config = SinkConfig {
            format: OutputFormat::Jsonl,
            ..Default::default()
        };
        let writer: Box<dyn Write + Send> = Box::new(buf.clone());
        let mut sink = create_sink(writer, sample_schema(), &config).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = sink.finish().unwrap();
        assert_eq!(stats.rows_written, 2);
        let content = buf.content();
        let lines: Vec<&str> = content.trim().lines().collect();
        assert_eq!(lines.len(), 2, "JSONL should have one line per record");
        assert!(
            lines[0].contains("\"id\""),
            "JSONL line should contain field name"
        );
    }

    #[test]
    fn factory_parquet() {
        let buf = SharedBuf::new();
        let config = SinkConfig {
            format: OutputFormat::Parquet,
            ..Default::default()
        };
        let writer: Box<dyn Write + Send> = Box::new(buf.clone());
        let mut sink = create_sink(writer, sample_schema(), &config).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = sink.finish().unwrap();
        assert_eq!(stats.rows_written, 2);
        // Parquet magic bytes: PAR1
        let bytes = buf.bytes();
        assert!(bytes.len() >= 4, "Parquet output should not be empty");
        assert_eq!(
            &bytes[..4],
            b"PAR1",
            "Parquet output should start with magic bytes"
        );
    }

    #[test]
    fn factory_arrow_ipc() {
        let buf = SharedBuf::new();
        let config = SinkConfig {
            format: OutputFormat::ArrowIpc,
            ..Default::default()
        };
        let writer: Box<dyn Write + Send> = Box::new(buf.clone());
        let mut sink = create_sink(writer, sample_schema(), &config).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = sink.finish().unwrap();
        assert_eq!(stats.rows_written, 2);
        // Arrow IPC magic bytes: ARROW1
        let bytes = buf.bytes();
        assert!(bytes.len() >= 6, "IPC output should not be empty");
        assert_eq!(
            &bytes[..6],
            b"ARROW1",
            "IPC output should start with magic bytes"
        );
    }

    #[test]
    fn factory_template() {
        let buf = SharedBuf::new();
        let config = SinkConfig {
            format: OutputFormat::Template,
            template_source: "ROW:{{ row.id }},{{ row.name }}\n".to_string(),
            ..Default::default()
        };
        let writer: Box<dyn Write + Send> = Box::new(buf.clone());
        let mut sink = create_sink(writer, sample_schema(), &config).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = sink.finish().unwrap();
        assert_eq!(stats.rows_written, 2);
        let content = buf.content();
        assert!(
            content.contains("ROW:1,a"),
            "template output should contain rendered row"
        );
        assert!(
            content.contains("ROW:2,b"),
            "template output should contain second row"
        );
    }
}
