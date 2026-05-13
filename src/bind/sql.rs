//! SQL INSERT output sink.
//!
//! Serializes Arrow `RecordBatch`es as SQL INSERT statements. Supports
//! multi-row VALUES syntax, optional CREATE TABLE DDL, and optional
//! transaction wrapping. All identifiers are quoted to handle reserved
//! words and special characters safely.

use std::fmt::Write as FmtWrite;
use std::io::Write;
use std::sync::Arc;

use arrow::array::*;
use arrow::datatypes::{DataType, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use parking_lot::Mutex;
use tracing::debug;

use crate::bind::error::BindError;
use crate::bind::traits::{Sink, SinkStats};

/// Configuration for SQL output.
#[derive(Debug, Clone)]
pub struct SqlConfig {
    /// Table name for INSERT statements.
    pub table_name: String,
    /// Whether to emit CREATE TABLE DDL before data.
    pub create_table: bool,
    /// Whether to wrap output in BEGIN/COMMIT.
    pub transaction: bool,
    /// Number of rows per INSERT statement (multi-row VALUES).
    pub rows_per_insert: usize,
}

impl Default for SqlConfig {
    fn default() -> Self {
        Self {
            table_name: "data".to_string(),
            create_table: false,
            transaction: false,
            rows_per_insert: 100,
        }
    }
}

/// Sink that writes `RecordBatch`es as SQL INSERT statements.
pub struct SqlSink<W: Write + Send> {
    writer: W,
    schema: Arc<Schema>,
    config: SqlConfig,
    rows_written: u64,
    byte_count: Arc<Mutex<u64>>,
    header_written: bool,
}

impl<W: Write + Send> SqlSink<W> {
    /// Create a new SQL sink.
    pub fn new(writer: W, schema: Arc<Schema>, config: SqlConfig) -> Self {
        Self {
            writer,
            schema,
            config,
            rows_written: 0,
            byte_count: Arc::new(Mutex::new(0)),
            header_written: false,
        }
    }

    /// Write the optional DDL and transaction header.
    fn write_header(&mut self) -> Result<(), BindError> {
        if self.header_written {
            return Ok(());
        }
        self.header_written = true;

        let mut buf = String::new();

        if self.config.create_table {
            writeln!(
                buf,
                "CREATE TABLE {} (",
                quote_identifier(&self.config.table_name)
            )
            .unwrap();
            let fields = self.schema.fields();
            for (i, field) in fields.iter().enumerate() {
                let sql_type = arrow_type_to_sql(field.data_type());
                let nullable = if field.is_nullable() { "" } else { " NOT NULL" };
                let comma = if i + 1 < fields.len() { "," } else { "" };
                writeln!(
                    buf,
                    "  {} {}{}{}",
                    quote_identifier(field.name()),
                    sql_type,
                    nullable,
                    comma,
                )
                .unwrap();
            }
            writeln!(buf, ");\n").unwrap();
        }

        if self.config.transaction {
            writeln!(buf, "BEGIN;\n").unwrap();
        }

        let bytes = buf.as_bytes();
        self.writer.write_all(bytes)?;
        *self.byte_count.lock() += bytes.len() as u64;
        Ok(())
    }

    /// Write a chunk of rows as a single multi-row INSERT statement.
    fn write_insert(
        &mut self,
        batch: &RecordBatch,
        start: usize,
        end: usize,
    ) -> Result<(), BindError> {
        let mut buf = String::with_capacity(4096);

        // Column list
        write!(
            buf,
            "INSERT INTO {} (",
            quote_identifier(&self.config.table_name)
        )
        .unwrap();
        for (i, field) in self.schema.fields().iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            buf.push_str(&quote_identifier(field.name()));
        }
        buf.push_str(") VALUES\n");

        // Value rows
        for row in start..end {
            if row > start {
                buf.push_str(",\n");
            }
            buf.push('(');
            for (col, field) in self.schema.fields().iter().enumerate() {
                if col > 0 {
                    buf.push_str(", ");
                }
                let array = batch.column(col);
                format_value(&mut buf, array, row, field.data_type());
            }
            buf.push(')');
        }
        buf.push_str(";\n");

        let bytes = buf.as_bytes();
        self.writer.write_all(bytes)?;
        *self.byte_count.lock() += bytes.len() as u64;
        Ok(())
    }
}

impl<W: Write + Send> Sink for SqlSink<W> {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<(), BindError> {
        self.write_header()?;

        let num_rows = batch.num_rows();
        let chunk = self.config.rows_per_insert.max(1);

        let mut start = 0;
        while start < num_rows {
            let end = (start + chunk).min(num_rows);
            self.write_insert(batch, start, end)?;
            start = end;
        }

        self.rows_written += num_rows as u64;
        debug!(
            rows = num_rows,
            total = self.rows_written,
            "wrote sql batch"
        );
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<SinkStats, BindError> {
        if self.config.transaction && self.header_written {
            let trailer = b"\nCOMMIT;\n";
            self.writer.write_all(trailer)?;
            *self.byte_count.lock() += trailer.len() as u64;
        }
        self.writer.flush()?;
        let bytes_written = *self.byte_count.lock();
        debug!(
            rows = self.rows_written,
            bytes = bytes_written,
            "sql sink finished"
        );
        Ok(SinkStats {
            rows_written: self.rows_written,
            bytes_written,
            files_created: 1,
        })
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Quote a SQL identifier with double-quotes, escaping embedded double-quotes.
fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Map Arrow DataType to a SQL column type string.
fn arrow_type_to_sql(dt: &DataType) -> &'static str {
    match dt {
        DataType::Boolean => "BOOLEAN",
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::UInt8 | DataType::UInt16 => {
            "INTEGER"
        }
        DataType::Int64 | DataType::UInt32 | DataType::UInt64 => "BIGINT",
        DataType::Float16 | DataType::Float32 => "REAL",
        DataType::Float64 => "DOUBLE PRECISION",
        DataType::Utf8 | DataType::LargeUtf8 => "TEXT",
        DataType::Binary | DataType::LargeBinary | DataType::FixedSizeBinary(_) => "TEXT",
        DataType::Date32 | DataType::Date64 => "DATE",
        DataType::Timestamp(_, _) => "TIMESTAMP",
        DataType::Time32(_) | DataType::Time64(_) => "TIME",
        DataType::Duration(_) => "TEXT",
        DataType::Decimal128(_, _) | DataType::Decimal256(_, _) => "NUMERIC",
        // Complex types serialized as JSON text
        DataType::Struct(_) | DataType::List(_) | DataType::LargeList(_) | DataType::Map(_, _) => {
            "TEXT"
        }
        _ => "TEXT",
    }
}

/// Format a single cell value as a SQL literal, appending to `buf`.
fn format_value(buf: &mut String, array: &ArrayRef, row: usize, dt: &DataType) {
    if array.is_null(row) {
        buf.push_str("NULL");
        return;
    }

    match dt {
        DataType::Boolean => {
            let v = array.as_any().downcast_ref::<BooleanArray>().unwrap();
            buf.push_str(if v.value(row) { "TRUE" } else { "FALSE" });
        }
        DataType::Int8 => {
            write!(
                buf,
                "{}",
                array
                    .as_any()
                    .downcast_ref::<Int8Array>()
                    .unwrap()
                    .value(row)
            )
            .unwrap();
        }
        DataType::Int16 => {
            write!(
                buf,
                "{}",
                array
                    .as_any()
                    .downcast_ref::<Int16Array>()
                    .unwrap()
                    .value(row)
            )
            .unwrap();
        }
        DataType::Int32 => {
            write!(
                buf,
                "{}",
                array
                    .as_any()
                    .downcast_ref::<Int32Array>()
                    .unwrap()
                    .value(row)
            )
            .unwrap();
        }
        DataType::Int64 => {
            write!(
                buf,
                "{}",
                array
                    .as_any()
                    .downcast_ref::<Int64Array>()
                    .unwrap()
                    .value(row)
            )
            .unwrap();
        }
        DataType::UInt8 => {
            write!(
                buf,
                "{}",
                array
                    .as_any()
                    .downcast_ref::<UInt8Array>()
                    .unwrap()
                    .value(row)
            )
            .unwrap();
        }
        DataType::UInt16 => {
            write!(
                buf,
                "{}",
                array
                    .as_any()
                    .downcast_ref::<UInt16Array>()
                    .unwrap()
                    .value(row)
            )
            .unwrap();
        }
        DataType::UInt32 => {
            write!(
                buf,
                "{}",
                array
                    .as_any()
                    .downcast_ref::<UInt32Array>()
                    .unwrap()
                    .value(row)
            )
            .unwrap();
        }
        DataType::UInt64 => {
            write!(
                buf,
                "{}",
                array
                    .as_any()
                    .downcast_ref::<UInt64Array>()
                    .unwrap()
                    .value(row)
            )
            .unwrap();
        }
        DataType::Float32 => {
            let v = array
                .as_any()
                .downcast_ref::<Float32Array>()
                .unwrap()
                .value(row);
            write_float(buf, v as f64);
        }
        DataType::Float16 => {
            // Float16 → cast to f64 for SQL literal
            let v = array
                .as_any()
                .downcast_ref::<Float16Array>()
                .unwrap()
                .value(row);
            write_float(buf, f64::from(v));
        }
        DataType::Float64 => {
            let v = array
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap()
                .value(row);
            write_float(buf, v);
        }
        DataType::Utf8 => {
            let v = array
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(row);
            write_sql_string(buf, v);
        }
        DataType::LargeUtf8 => {
            let v = array
                .as_any()
                .downcast_ref::<LargeStringArray>()
                .unwrap()
                .value(row);
            write_sql_string(buf, v);
        }
        DataType::Date32 => {
            let v = array
                .as_any()
                .downcast_ref::<Date32Array>()
                .unwrap()
                .value(row);
            // Date32 is days since epoch
            let date = chrono::NaiveDate::from_num_days_from_ce_opt(v + 719_163);
            if let Some(d) = date {
                write!(buf, "'{}'", d.format("%Y-%m-%d")).unwrap();
            } else {
                buf.push_str("NULL");
            }
        }
        DataType::Date64 => {
            let v = array
                .as_any()
                .downcast_ref::<Date64Array>()
                .unwrap()
                .value(row);
            let date = chrono::DateTime::from_timestamp_millis(v);
            if let Some(d) = date {
                write!(buf, "'{}'", d.format("%Y-%m-%d")).unwrap();
            } else {
                buf.push_str("NULL");
            }
        }
        DataType::Timestamp(unit, _tz) => {
            let v = match unit {
                TimeUnit::Second => {
                    let arr = array
                        .as_any()
                        .downcast_ref::<TimestampSecondArray>()
                        .unwrap();
                    chrono::DateTime::from_timestamp(arr.value(row), 0)
                }
                TimeUnit::Millisecond => {
                    let arr = array
                        .as_any()
                        .downcast_ref::<TimestampMillisecondArray>()
                        .unwrap();
                    chrono::DateTime::from_timestamp_millis(arr.value(row))
                }
                TimeUnit::Microsecond => {
                    let arr = array
                        .as_any()
                        .downcast_ref::<TimestampMicrosecondArray>()
                        .unwrap();
                    chrono::DateTime::from_timestamp_micros(arr.value(row))
                }
                TimeUnit::Nanosecond => {
                    let arr = array
                        .as_any()
                        .downcast_ref::<TimestampNanosecondArray>()
                        .unwrap();
                    let nanos = arr.value(row);
                    // Use Euclidean division to handle negative timestamps correctly
                    let secs = nanos.div_euclid(1_000_000_000);
                    let sub_nanos = nanos.rem_euclid(1_000_000_000) as u32;
                    chrono::DateTime::from_timestamp(secs, sub_nanos)
                }
            };
            if let Some(dt) = v {
                write!(buf, "'{}'", dt.format("%Y-%m-%d %H:%M:%S%.f")).unwrap();
            } else {
                buf.push_str("NULL");
            }
        }
        DataType::Time32(unit) => {
            let secs = match unit {
                TimeUnit::Second => array
                    .as_any()
                    .downcast_ref::<Time32SecondArray>()
                    .unwrap()
                    .value(row) as i64,
                TimeUnit::Millisecond => {
                    let ms = array
                        .as_any()
                        .downcast_ref::<Time32MillisecondArray>()
                        .unwrap()
                        .value(row) as i64;
                    ms / 1000
                }
                _ => 0,
            };
            let h = secs / 3600;
            let m = (secs % 3600) / 60;
            let s = secs % 60;
            write!(buf, "'{:02}:{:02}:{:02}'", h, m, s).unwrap();
        }
        DataType::Time64(unit) => {
            let micros = match unit {
                TimeUnit::Microsecond => array
                    .as_any()
                    .downcast_ref::<Time64MicrosecondArray>()
                    .unwrap()
                    .value(row),
                TimeUnit::Nanosecond => {
                    array
                        .as_any()
                        .downcast_ref::<Time64NanosecondArray>()
                        .unwrap()
                        .value(row)
                        / 1000
                }
                _ => 0,
            };
            let total_secs = micros / 1_000_000;
            let h = total_secs / 3600;
            let m = (total_secs % 3600) / 60;
            let s = total_secs % 60;
            let us = micros % 1_000_000;
            write!(buf, "'{:02}:{:02}:{:02}.{:06}'", h, m, s, us).unwrap();
        }
        DataType::Binary => {
            let v = array
                .as_any()
                .downcast_ref::<BinaryArray>()
                .unwrap()
                .value(row);
            write_hex_string(buf, v);
        }
        DataType::LargeBinary => {
            let v = array
                .as_any()
                .downcast_ref::<LargeBinaryArray>()
                .unwrap()
                .value(row);
            write_hex_string(buf, v);
        }
        DataType::FixedSizeBinary(_) => {
            let v = array
                .as_any()
                .downcast_ref::<FixedSizeBinaryArray>()
                .unwrap()
                .value(row);
            write_hex_string(buf, v);
        }
        DataType::Decimal128(_, scale) => {
            let v = array
                .as_any()
                .downcast_ref::<Decimal128Array>()
                .unwrap()
                .value(row);
            let s = *scale as u32;
            if s == 0 {
                write!(buf, "{}", v).unwrap();
            } else {
                let divisor = 10i128.pow(s);
                let sign = if v < 0 { "-" } else { "" };
                let abs_v = v.unsigned_abs();
                let integer = abs_v / divisor as u128;
                let frac = abs_v % divisor as u128;
                write!(
                    buf,
                    "{}{}.{:0>width$}",
                    sign,
                    integer,
                    frac,
                    width = s as usize
                )
                .unwrap();
            }
        }
        DataType::Decimal256(_, _) => {
            // Decimal256: serialize as string literal (rare type)
            let json_val = array_value_to_json(array, row);
            buf.push_str(&json_val);
        }
        // Complex types: serialize as JSON text
        DataType::Struct(_) | DataType::List(_) | DataType::LargeList(_) | DataType::Map(_, _) => {
            // Use Arrow's JSON serialization for complex types
            let json_val = array_value_to_json(array, row);
            write_sql_string(buf, &json_val);
        }
        // Fallback: convert to string display
        _ => {
            let json_val = array_value_to_json(array, row);
            write_sql_string(buf, &json_val);
        }
    }
}

/// Write a float value, ensuring it always has a decimal point.
fn write_float(buf: &mut String, v: f64) {
    if v.is_nan() || v.is_infinite() {
        buf.push_str("NULL");
    } else if v == v.trunc() && v.abs() < 1e15 {
        write!(buf, "{:.1}", v).unwrap();
    } else {
        write!(buf, "{}", v).unwrap();
    }
}

/// Write a SQL single-quoted string, escaping single quotes by doubling.
fn write_sql_string(buf: &mut String, s: &str) {
    buf.push('\'');
    for c in s.chars() {
        if c == '\'' {
            buf.push_str("''");
        } else {
            buf.push(c);
        }
    }
    buf.push('\'');
}

/// Write binary data as a hex string literal.
fn write_hex_string(buf: &mut String, data: &[u8]) {
    buf.push_str("X'");
    for byte in data {
        write!(buf, "{:02X}", byte).unwrap();
    }
    buf.push('\'');
}

/// Convert a single array element to a JSON string (for complex types).
fn array_value_to_json(array: &ArrayRef, row: usize) -> String {
    // Use Arrow's built-in JSON writer to serialize a single-row batch
    use arrow::json::LineDelimitedWriter;

    let schema = Arc::new(Schema::new(vec![arrow::datatypes::Field::new(
        "v",
        array.data_type().clone(),
        true,
    )]));
    let col = array.slice(row, 1);
    if let Ok(batch) = RecordBatch::try_new(schema, vec![col]) {
        let mut buf = Vec::new();
        let mut writer = LineDelimitedWriter::new(&mut buf);
        if writer.write(&batch).is_ok() && writer.finish().is_ok() {
            if let Ok(s) = String::from_utf8(buf) {
                // Output is {"v": <value>}\n — extract the value part
                let s = s.trim();
                if let Some(start) = s.find(':') {
                    let val = s[start + 1..].trim();
                    // Strip trailing }
                    if let Some(val) = val.strip_suffix('}') {
                        return val.trim().to_string();
                    }
                }
            }
        }
    }
    "NULL".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::Field as ArrowField;

    /// A writer backed by a shared buffer.
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

    fn sample_schema() -> Arc<Schema> {
        Arc::new(Schema::new(vec![
            ArrowField::new("id", DataType::Int32, false),
            ArrowField::new("name", DataType::Utf8, true),
        ]))
    }

    fn sample_batch() -> RecordBatch {
        RecordBatch::try_new(
            sample_schema(),
            vec![
                Arc::new(Int32Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec![Some("alice"), Some("bob"), None])),
            ],
        )
        .unwrap()
    }

    #[test]
    fn sql_basic_insert() {
        let buf = SharedBuf::new();
        let schema = sample_schema();
        let config = SqlConfig {
            table_name: "users".to_string(),
            rows_per_insert: 100,
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&sample_batch()).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 3);

        let output = buf.to_string();
        assert!(output.contains("INSERT INTO \"users\""));
        assert!(output.contains("\"id\", \"name\""));
        assert!(output.contains("1, 'alice'"));
        assert!(output.contains("2, 'bob'"));
        assert!(output.contains("3, NULL"));
    }

    #[test]
    fn sql_create_table() {
        let buf = SharedBuf::new();
        let schema = sample_schema();
        let config = SqlConfig {
            table_name: "users".to_string(),
            create_table: true,
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&sample_batch()).unwrap();
        Box::new(sink).finish().unwrap();

        let output = buf.to_string();
        assert!(output.contains("CREATE TABLE \"users\""));
        assert!(output.contains("\"id\" INTEGER NOT NULL"));
        assert!(output.contains("\"name\" TEXT"));
    }

    #[test]
    fn sql_transaction_wrapping() {
        let buf = SharedBuf::new();
        let schema = sample_schema();
        let config = SqlConfig {
            table_name: "users".to_string(),
            transaction: true,
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&sample_batch()).unwrap();
        Box::new(sink).finish().unwrap();

        let output = buf.to_string();
        assert!(output.starts_with("BEGIN;"));
        assert!(output.contains("COMMIT;"));
    }

    #[test]
    fn sql_multi_row_chunking() {
        let buf = SharedBuf::new();
        let schema = sample_schema();
        let config = SqlConfig {
            table_name: "t".to_string(),
            rows_per_insert: 2,
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&sample_batch()).unwrap();
        Box::new(sink).finish().unwrap();

        let output = buf.to_string();
        // 3 rows with chunk size 2 → 2 INSERT statements
        let insert_count = output.matches("INSERT INTO").count();
        assert_eq!(insert_count, 2);
    }

    #[test]
    fn sql_string_escaping() {
        let schema = Arc::new(Schema::new(vec![ArrowField::new(
            "msg",
            DataType::Utf8,
            false,
        )]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(StringArray::from(vec!["it's a test"]))],
        )
        .unwrap();

        let buf = SharedBuf::new();
        let config = SqlConfig {
            table_name: "t".to_string(),
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&batch).unwrap();
        Box::new(sink).finish().unwrap();

        let output = buf.to_string();
        assert!(
            output.contains("'it''s a test'"),
            "single quotes should be doubled: {}",
            output
        );
    }

    #[test]
    fn sql_identifier_quoting() {
        let schema = Arc::new(Schema::new(vec![ArrowField::new(
            "select",
            DataType::Int32,
            false,
        )]));
        let batch =
            RecordBatch::try_new(schema.clone(), vec![Arc::new(Int32Array::from(vec![42]))])
                .unwrap();

        let buf = SharedBuf::new();
        let config = SqlConfig {
            table_name: "order".to_string(),
            create_table: true,
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&batch).unwrap();
        Box::new(sink).finish().unwrap();

        let output = buf.to_string();
        // Reserved words should be safely quoted
        assert!(output.contains("\"order\""));
        assert!(output.contains("\"select\""));
    }

    #[test]
    fn sql_float_formatting() {
        let schema = Arc::new(Schema::new(vec![ArrowField::new(
            "val",
            DataType::Float64,
            true,
        )]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Float64Array::from(vec![
                Some(3.14),
                Some(42.0),
                Some(f64::NAN),
                None,
            ]))],
        )
        .unwrap();

        let buf = SharedBuf::new();
        let config = SqlConfig {
            table_name: "t".to_string(),
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&batch).unwrap();
        Box::new(sink).finish().unwrap();

        let output = buf.to_string();
        assert!(output.contains("3.14"), "should have decimal: {}", output);
        assert!(
            output.contains("42.0"),
            "whole floats should have .0: {}",
            output
        );
        // NaN → NULL
        let values_part = output.split("VALUES").nth(1).unwrap();
        // Count NULLs in the values section (NaN + actual NULL = 2)
        let null_count = values_part.matches("NULL").count();
        assert!(null_count >= 2, "NaN and null both → NULL: {}", output);
    }

    #[test]
    fn sql_boolean_values() {
        let schema = Arc::new(Schema::new(vec![ArrowField::new(
            "active",
            DataType::Boolean,
            false,
        )]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(BooleanArray::from(vec![true, false]))],
        )
        .unwrap();

        let buf = SharedBuf::new();
        let config = SqlConfig {
            table_name: "t".to_string(),
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&batch).unwrap();
        Box::new(sink).finish().unwrap();

        let output = buf.to_string();
        assert!(output.contains("TRUE"));
        assert!(output.contains("FALSE"));
    }

    #[test]
    fn sql_date_values() {
        let schema = Arc::new(Schema::new(vec![ArrowField::new(
            "dt",
            DataType::Date32,
            false,
        )]));
        // 2024-01-15 = 19737 days since epoch
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Date32Array::from(vec![19737]))],
        )
        .unwrap();

        let buf = SharedBuf::new();
        let config = SqlConfig {
            table_name: "t".to_string(),
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&batch).unwrap();
        Box::new(sink).finish().unwrap();

        let output = buf.to_string();
        assert!(
            output.contains("'2024-01-15'"),
            "date should be ISO format: {}",
            output
        );
    }

    #[test]
    fn sql_timestamp_values() {
        let schema = Arc::new(Schema::new(vec![ArrowField::new(
            "ts",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            false,
        )]));
        // 2024-01-15 12:30:45 UTC in millis
        let ms = 1705321845000i64;
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(TimestampMillisecondArray::from(vec![ms]))],
        )
        .unwrap();

        let buf = SharedBuf::new();
        let config = SqlConfig {
            table_name: "t".to_string(),
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&batch).unwrap();
        Box::new(sink).finish().unwrap();

        let output = buf.to_string();
        assert!(
            output.contains("'2024-01-15 12:30:45"),
            "timestamp should be readable: {}",
            output
        );
    }

    #[test]
    fn sql_multiple_batches() {
        let buf = SharedBuf::new();
        let schema = sample_schema();
        let config = SqlConfig {
            table_name: "t".to_string(),
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&sample_batch()).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 6);
        assert!(stats.bytes_written > 0);

        let output = buf.to_string();
        let insert_count = output.matches("INSERT INTO").count();
        assert_eq!(insert_count, 2);
    }

    #[test]
    fn sql_empty_batch() {
        let schema = sample_schema();
        let batch = RecordBatch::new_empty(schema.clone());

        let buf = SharedBuf::new();
        let config = SqlConfig {
            table_name: "t".to_string(),
            create_table: true,
            ..Default::default()
        };
        let mut sink = SqlSink::new(buf.clone(), schema, config);
        sink.write_batch(&batch).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 0);

        let output = buf.to_string();
        // DDL still emitted
        assert!(output.contains("CREATE TABLE"));
        // No INSERT
        assert!(!output.contains("INSERT"));
    }
}
