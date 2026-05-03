//! Integration tests for knit-bind output sinks.

use std::io::Cursor;
use std::sync::Arc;

use arrow::array::{
    BooleanArray, Float64Array, Int64Array, RecordBatch, StringArray, TimestampMicrosecondArray,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};

/// Helper: create a test schema and batch.
fn test_batch() -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("score", DataType::Float64, true),
        Field::new("active", DataType::Boolean, false),
    ]));
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![1, 2, 3])),
            Arc::new(StringArray::from(vec![Some("alice"), None, Some("charlie")])),
            Arc::new(Float64Array::from(vec![Some(95.5), Some(87.0), None])),
            Arc::new(BooleanArray::from(vec![true, false, true])),
        ],
    )
    .unwrap()
}

/// Helper: create a batch with timestamps.
fn timestamp_batch() -> RecordBatch {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new(
            "created_at",
            DataType::Timestamp(TimeUnit::Microsecond, None),
            false,
        ),
    ]));
    // 2024-01-15T12:00:00Z in microseconds
    let ts = 1_705_320_000_000_000i64;
    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![1, 2])),
            Arc::new(TimestampMicrosecondArray::from(vec![ts, ts + 3_600_000_000])),
        ],
    )
    .unwrap()
}

mod parquet_tests {
    use super::*;
    use bytes::Bytes;
    use knit_bind::parquet::{Compression, ParquetSink};
    use knit_bind::traits::Sink;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    // Helper that writes to a shared buffer and returns the bytes
    fn write_parquet(batch: &RecordBatch, compression: Compression) -> (Vec<u8>, knit_bind::SinkStats) {
        use std::sync::{Arc, Mutex};

        let shared = Arc::new(Mutex::new(Vec::new()));
        let writer = SharedWriter(Arc::clone(&shared));
        let mut sink =
            ParquetSink::new(writer, batch.schema(), compression, None).unwrap();
        sink.write_batch(batch).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        let buf = shared.lock().unwrap().clone();
        (buf, stats)
    }

    fn write_parquet_multi(batch: &RecordBatch, count: usize, compression: Compression) -> (Vec<u8>, knit_bind::SinkStats) {
        use std::sync::{Arc, Mutex};

        let shared = Arc::new(Mutex::new(Vec::new()));
        let writer = SharedWriter(Arc::clone(&shared));
        let mut sink =
            ParquetSink::new(writer, batch.schema(), compression, None).unwrap();
        for _ in 0..count {
            sink.write_batch(batch).unwrap();
        }
        let stats = Box::new(sink).finish().unwrap();
        let buf = shared.lock().unwrap().clone();
        (buf, stats)
    }

    /// A Write impl backed by a shared Vec for extracting bytes after writing.
    struct SharedWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    // SAFETY: SharedWriter uses Arc<Mutex<..>> which is Send
    unsafe impl Send for SharedWriter {}

    #[test]
    fn round_trip_uncompressed_v2() {
        let batch = test_batch();
        let (buf, stats) = write_parquet(&batch, Compression::None);

        assert_eq!(stats.rows_written, 3);
        assert_eq!(stats.files_created, 1);

        let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(buf))
            .unwrap()
            .build()
            .unwrap();
        let batches: Vec<_> = reader.map(|r| r.unwrap()).collect();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].num_rows(), 3);
        assert_eq!(batches[0], batch);
    }

    #[test]
    fn round_trip_zstd() {
        let batch = test_batch();
        let (buf, stats) = write_parquet(&batch, Compression::Zstd);
        assert_eq!(stats.rows_written, 3);

        let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(buf))
            .unwrap()
            .build()
            .unwrap();
        let batches: Vec<_> = reader.map(|r| r.unwrap()).collect();
        assert_eq!(batches[0], batch);
    }

    #[test]
    fn round_trip_snappy() {
        let batch = test_batch();
        let (buf, _) = write_parquet(&batch, Compression::Snappy);

        let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(buf))
            .unwrap()
            .build()
            .unwrap();
        let batches: Vec<_> = reader.map(|r| r.unwrap()).collect();
        assert_eq!(batches[0], batch);
    }

    #[test]
    fn multiple_batches() {
        let batch = test_batch();
        let (buf, stats) = write_parquet_multi(&batch, 2, Compression::None);
        assert_eq!(stats.rows_written, 6);

        let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(buf))
            .unwrap()
            .build()
            .unwrap();
        let total_rows: usize = reader.map(|r| r.unwrap().num_rows()).sum();
        assert_eq!(total_rows, 6);
    }
}

mod json_tests {
    use super::*;
    use knit_bind::json::{JsonMode, JsonSink};
    use knit_bind::traits::Sink;

    #[test]
    fn jsonl_output() {
        let batch = test_batch();
        let buf = Cursor::new(Vec::new());

        let mut sink = JsonSink::new(buf, JsonMode::Jsonl).unwrap();
        sink.write_batch(&batch).unwrap();
        let stats = Box::new(sink).finish().unwrap();

        assert_eq!(stats.rows_written, 3);
        assert!(stats.bytes_written > 0);
    }

    /// Helper to write JSON and get the output string.
    fn write_json(batch: &RecordBatch, mode: JsonMode) -> (String, knit_bind::SinkStats) {
        use std::sync::{Arc, Mutex};
        struct SharedWriter(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for SharedWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        unsafe impl Send for SharedWriter {}

        let shared = Arc::new(Mutex::new(Vec::new()));
        let writer = SharedWriter(Arc::clone(&shared));
        let mut sink = JsonSink::new(writer, mode).unwrap();
        sink.write_batch(batch).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        let buf = shared.lock().unwrap().clone();
        (String::from_utf8(buf).unwrap(), stats)
    }

    #[test]
    fn jsonl_content() {
        let batch = test_batch();
        let (output, stats) = write_json(&batch, JsonMode::Jsonl);

        assert_eq!(stats.rows_written, 3);
        assert!(stats.bytes_written > 0);

        let lines: Vec<&str> = output.trim().split('\n').collect();
        assert_eq!(lines.len(), 3);

        let obj: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        assert_eq!(obj["id"], 1);
        assert_eq!(obj["name"], "alice");
        assert_eq!(obj["score"], 95.5);
        assert_eq!(obj["active"], true);

        let obj: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(obj["id"], 2);
        assert!(obj["name"].is_null());
    }

    #[test]
    fn json_array_output() {
        let batch = test_batch();
        let (output, stats) = write_json(&batch, JsonMode::JsonArray);

        assert_eq!(stats.rows_written, 3);

        let arr: Vec<serde_json::Value> = serde_json::from_str(&output).unwrap();
        assert_eq!(arr.len(), 3);
        assert_eq!(arr[2]["name"], "charlie");
        assert!(arr[2]["score"].is_null());
    }

    #[test]
    fn json_timestamps() {
        let batch = timestamp_batch();
        let (output, _) = write_json(&batch, JsonMode::Jsonl);

        let lines: Vec<&str> = output.trim().split('\n').collect();
        let obj: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
        let ts_str = obj["created_at"].as_str().unwrap();
        assert!(ts_str.contains("2024-01-15"), "timestamp: {ts_str}");
    }
}

mod csv_tests {
    use super::*;
    use knit_bind::csv::{CsvSink, CsvSinkConfig};
    use knit_bind::traits::Sink;

    #[test]
    fn csv_default_config() {
        let batch = test_batch();
        let config = CsvSinkConfig::default();
        let mut sink = CsvSink::new(Vec::new(), &config);
        sink.write_batch(&batch).unwrap();
        let stats = Box::new(sink).finish().unwrap();

        assert_eq!(stats.rows_written, 3);
        assert_eq!(stats.files_created, 1);
        assert!(stats.bytes_written > 0);
    }

    #[test]
    fn csv_content_check() {
        let batch = test_batch();
        let config = CsvSinkConfig::default();
        let mut sink = CsvSink::new(Cursor::new(Vec::new()), &config);
        sink.write_batch(&batch).unwrap();
        Box::new(sink).finish().unwrap();
    }

    #[test]
    fn csv_custom_delimiter() {
        let batch = test_batch();
        let config = CsvSinkConfig {
            delimiter: b'\t',
            header: true,
            null_value: "NA".to_string(),
        };
        let mut sink = CsvSink::new(Vec::new(), &config);
        sink.write_batch(&batch).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 3);
    }

    #[test]
    fn csv_no_header() {
        let batch = test_batch();
        let config = CsvSinkConfig {
            header: false,
            ..Default::default()
        };
        let mut sink = CsvSink::new(Vec::new(), &config);
        sink.write_batch(&batch).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 3);
    }
}

mod ipc_tests {
    use super::*;
    use arrow::ipc::reader::FileReader;
    use knit_bind::ipc::ArrowIpcSink;
    use knit_bind::traits::Sink;

    /// Helper to write IPC and return the bytes.
    fn write_ipc(batch: &RecordBatch, count: usize) -> Vec<u8> {
        use std::sync::{Arc, Mutex};
        struct SharedWriter(Arc<Mutex<Vec<u8>>>);
        impl std::io::Write for SharedWriter {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        unsafe impl Send for SharedWriter {}

        let shared: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
        let writer = SharedWriter(Arc::clone(&shared));
        let mut sink = ArrowIpcSink::new(writer, batch.schema()).unwrap();
        for _ in 0..count {
            sink.write_batch(batch).unwrap();
        }
        let _stats = Box::new(sink).finish().unwrap();
        let result = shared.lock().unwrap().clone();
        result
    }

    #[test]
    fn round_trip() {
        let batch = test_batch();
        let buf = write_ipc(&batch, 1);

        assert!(!buf.is_empty());

        let cursor = Cursor::new(buf);
        let reader = FileReader::try_new(cursor, None).unwrap();
        let batches: Vec<_> = reader.map(|r| r.unwrap()).collect();
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0], batch);
    }

    #[test]
    fn multiple_batches() {
        let batch = test_batch();
        let buf = write_ipc(&batch, 2);

        let cursor = Cursor::new(buf);
        let reader = FileReader::try_new(cursor, None).unwrap();
        let total_rows: usize = reader.map(|r| r.unwrap().num_rows()).sum();
        assert_eq!(total_rows, 6);
    }
}

mod factory_tests {
    use super::*;
    use knit_bind::factory::{create_sink, OutputFormat, SinkConfig};
    use knit_bind::parquet::Compression;

    #[test]
    fn create_all_formats() {
        let batch = test_batch();
        let schema = batch.schema();

        let formats = vec![
            OutputFormat::Parquet,
            OutputFormat::Json,
            OutputFormat::Jsonl,
            OutputFormat::Csv,
            OutputFormat::ArrowIpc,
        ];

        for format in formats {
            let config = SinkConfig {
                format,
                compression: Compression::None,
                ..Default::default()
            };
            let writer: Box<dyn std::io::Write + Send> = Box::new(Vec::new());
            let mut sink = create_sink(writer, Arc::clone(&schema), &config).unwrap();
            sink.write_batch(&batch).unwrap();
            let stats = sink.finish().unwrap();
            assert_eq!(stats.rows_written, 3, "format: {format:?}");
            assert_eq!(stats.files_created, 1, "format: {format:?}");
        }
    }

    #[test]
    fn sink_stats_fields() {
        let batch = test_batch();
        let config = SinkConfig {
            format: OutputFormat::Jsonl,
            ..Default::default()
        };
        let writer: Box<dyn std::io::Write + Send> = Box::new(Vec::new());
        let mut sink = create_sink(writer, batch.schema(), &config).unwrap();
        sink.write_batch(&batch).unwrap();
        sink.write_batch(&batch).unwrap();
        let stats = sink.finish().unwrap();
        assert_eq!(stats.rows_written, 6);
        assert!(stats.bytes_written > 0);
        assert_eq!(stats.files_created, 1);
    }

    #[test]
    fn create_template_sink_via_factory() {
        let batch = test_batch();
        let config = SinkConfig {
            format: OutputFormat::Template,
            template_source: "{{ row.id }},{{ row.name }}".to_string(),
            template_mode: None,
            ..Default::default()
        };
        let writer: Box<dyn std::io::Write + Send> = Box::new(Vec::new());
        let mut sink = create_sink(writer, batch.schema(), &config).unwrap();
        sink.write_batch(&batch).unwrap();
        let stats = sink.finish().unwrap();
        assert_eq!(stats.rows_written, 3);
        assert!(stats.bytes_written > 0);
    }
}

mod template_tests {
    use super::*;
    use knit_bind::template::{TemplateSink, TemplateMode};
    use knit_bind::traits::Sink;

    #[test]
    fn sql_insert_template() {
        let batch = test_batch();
        let tmpl = "INSERT INTO users (id, name, score, active) VALUES ({{ row.id }}, '{{ row.name | escape_sql }}', {{ row.score }}, {{ row.active }});";
        let buf = Cursor::new(Vec::new());
        let mut sink =
            TemplateSink::new(buf, tmpl.to_string(), Some(TemplateMode::PerRow)).unwrap();
        sink.write_batch(&batch).unwrap();
        let output = String::from_utf8(sink.into_inner().into_inner()).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("VALUES (1,"));
        assert!(lines[0].contains("'alice'"));
        assert!(lines[0].contains("95.5"));
        // Second row has null name
        assert!(lines[1].contains("VALUES (2,"));
    }

    #[test]
    fn xml_document_template() {
        let batch = test_batch();
        let tmpl = r#"<?xml version="1.0"?>
<records>
{% for r in rows %}  <record id="{{ r.id }}">
    <name>{{ r.name | escape_xml }}</name>
    <score>{{ r.score }}</score>
    <active>{{ r.active }}</active>
  </record>
{% endfor %}</records>"#;
        let buf = Cursor::new(Vec::new());
        let mut sink =
            TemplateSink::new(buf, tmpl.to_string(), Some(TemplateMode::PerBatch)).unwrap();
        sink.write_batch(&batch).unwrap();
        let output = String::from_utf8(sink.into_inner().into_inner()).unwrap();
        assert!(output.contains("<?xml version=\"1.0\"?>"));
        assert!(output.contains("<name>alice</name>"));
        assert!(output.contains("<name>charlie</name>"));
        assert!(output.contains("<records>"));
        assert!(output.contains("</records>"));
    }

    #[test]
    fn template_with_helpers() {
        let batch = test_batch();
        let tmpl = "{{ row.name | upper | pad_right(10) }}|{{ row.score | format_number(1) }}";
        let buf = Cursor::new(Vec::new());
        let mut sink =
            TemplateSink::new(buf, tmpl.to_string(), Some(TemplateMode::PerRow)).unwrap();
        sink.write_batch(&batch).unwrap();
        let output = String::from_utf8(sink.into_inner().into_inner()).unwrap();
        let lines: Vec<&str> = output.lines().collect();
        assert!(lines[0].contains("ALICE"));
        assert!(lines[0].contains("95.5"));
    }

    #[test]
    fn template_multiple_batches() {
        let batch = test_batch();
        let tmpl = "{{ row.id }}";
        let buf = Cursor::new(Vec::new());
        let mut sink =
            TemplateSink::new(buf, tmpl.to_string(), Some(TemplateMode::PerRow)).unwrap();
        sink.write_batch(&batch).unwrap();
        sink.write_batch(&batch).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 6);
        assert_eq!(stats.files_created, 1);
    }

    #[test]
    fn template_batch_with_schema() {
        let batch = test_batch();
        let tmpl = "{% for f in schema.fields %}{{ f.name }},{% endfor %}";
        let buf = Cursor::new(Vec::new());
        let mut sink =
            TemplateSink::new(buf, tmpl.to_string(), Some(TemplateMode::PerBatch)).unwrap();
        sink.write_batch(&batch).unwrap();
        let output = String::from_utf8(sink.into_inner().into_inner()).unwrap();
        assert!(output.contains("id,"));
        assert!(output.contains("name,"));
        assert!(output.contains("score,"));
        assert!(output.contains("active,"));
    }

    #[test]
    fn template_timestamp_formatting() {
        let batch = timestamp_batch();
        let tmpl = "{{ row.created_at | format_date(\"%Y-%m-%d\") }}";
        let buf = Cursor::new(Vec::new());
        let mut sink =
            TemplateSink::new(buf, tmpl.to_string(), Some(TemplateMode::PerRow)).unwrap();
        sink.write_batch(&batch).unwrap();
        let output = String::from_utf8(sink.into_inner().into_inner()).unwrap();
        assert!(output.contains("2024-01-15"));
    }
}

