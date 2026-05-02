//! JSON / JSONL output sink.

use std::io::Write;

use arrow::array::{self, Array};
use arrow::datatypes::{DataType, TimeUnit};
use arrow::record_batch::RecordBatch;
use tracing::debug;

use crate::error::BindError;
use crate::traits::{Sink, SinkStats};

/// JSON output mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonMode {
    /// One JSON object per line (newline-delimited JSON).
    Jsonl,
    /// A single JSON array wrapping all objects.
    JsonArray,
}

/// Sink that writes `RecordBatch`es as JSON.
pub struct JsonSink<W: Write + Send> {
    writer: W,
    mode: JsonMode,
    rows_written: u64,
    bytes_written: u64,
    first_row: bool,
}

impl<W: Write + Send> JsonSink<W> {
    /// Create a new `JsonSink` with the given mode.
    pub fn new(mut writer: W, mode: JsonMode) -> Result<Self, BindError> {
        let mut bytes_written = 0u64;
        if mode == JsonMode::JsonArray {
            let b = writer.write(b"[")?;
            bytes_written += b as u64;
        }
        Ok(Self {
            writer,
            mode,
            rows_written: 0,
            bytes_written,
            first_row: true,
        })
    }
}

/// Convert a single cell value to a `serde_json::Value`.
fn cell_to_json(col: &dyn Array, row: usize) -> serde_json::Value {
    if col.is_null(row) {
        return serde_json::Value::Null;
    }
    match col.data_type() {
        DataType::Boolean => {
            let arr = col.as_any().downcast_ref::<array::BooleanArray>().unwrap();
            serde_json::Value::Bool(arr.value(row))
        }
        DataType::Int8 => json_number!(col, array::Int8Array, row),
        DataType::Int16 => json_number!(col, array::Int16Array, row),
        DataType::Int32 => json_number!(col, array::Int32Array, row),
        DataType::Int64 => {
            let arr = col.as_any().downcast_ref::<array::Int64Array>().unwrap();
            serde_json::Value::Number(arr.value(row).into())
        }
        DataType::UInt8 => json_number!(col, array::UInt8Array, row),
        DataType::UInt16 => json_number!(col, array::UInt16Array, row),
        DataType::UInt32 => json_number!(col, array::UInt32Array, row),
        DataType::UInt64 => {
            let arr = col.as_any().downcast_ref::<array::UInt64Array>().unwrap();
            serde_json::Value::Number(arr.value(row).into())
        }
        DataType::Float32 => {
            let arr = col.as_any().downcast_ref::<array::Float32Array>().unwrap();
            let v = arr.value(row) as f64;
            serde_json::Number::from_f64(v)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        DataType::Float64 => {
            let arr = col.as_any().downcast_ref::<array::Float64Array>().unwrap();
            serde_json::Number::from_f64(arr.value(row))
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        DataType::Utf8 => {
            let arr = col.as_any().downcast_ref::<array::StringArray>().unwrap();
            serde_json::Value::String(arr.value(row).to_string())
        }
        DataType::LargeUtf8 => {
            let arr = col
                .as_any()
                .downcast_ref::<array::LargeStringArray>()
                .unwrap();
            serde_json::Value::String(arr.value(row).to_string())
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            let arr = col
                .as_any()
                .downcast_ref::<array::TimestampMicrosecondArray>()
                .unwrap();
            let ts = arr.value(row);
            let dt = chrono::DateTime::from_timestamp_micros(ts);
            match dt {
                Some(d) => serde_json::Value::String(d.to_rfc3339()),
                None => serde_json::Value::Null,
            }
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let arr = col
                .as_any()
                .downcast_ref::<array::TimestampMillisecondArray>()
                .unwrap();
            let ts = arr.value(row);
            let dt = chrono::DateTime::from_timestamp_millis(ts);
            match dt {
                Some(d) => serde_json::Value::String(d.to_rfc3339()),
                None => serde_json::Value::Null,
            }
        }
        DataType::Timestamp(TimeUnit::Second, _) => {
            let arr = col
                .as_any()
                .downcast_ref::<array::TimestampSecondArray>()
                .unwrap();
            let ts = arr.value(row) as i64;
            let dt = chrono::DateTime::from_timestamp(ts, 0);
            match dt {
                Some(d) => serde_json::Value::String(d.to_rfc3339()),
                None => serde_json::Value::Null,
            }
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            let arr = col
                .as_any()
                .downcast_ref::<array::TimestampNanosecondArray>()
                .unwrap();
            let ts = arr.value(row);
            let secs = ts / 1_000_000_000;
            let nsecs = (ts % 1_000_000_000) as u32;
            let dt = chrono::DateTime::from_timestamp(secs, nsecs);
            match dt {
                Some(d) => serde_json::Value::String(d.to_rfc3339()),
                None => serde_json::Value::Null,
            }
        }
        _ => {
            // Fallback: use Arrow's display formatting
            let formatted = array::cast::as_string_array(
                &arrow::compute::cast(col, &DataType::Utf8).unwrap_or_else(|_| {
                    std::sync::Arc::new(array::StringArray::from(vec!["<unsupported>"]))
                }),
            )
            .value(row)
            .to_string();
            serde_json::Value::String(formatted)
        }
    }
}

macro_rules! json_number {
    ($col:expr, $arr_ty:ty, $row:expr) => {{
        let arr = $col.as_any().downcast_ref::<$arr_ty>().unwrap();
        serde_json::Value::Number(arr.value($row).into())
    }};
}
use json_number;

impl<W: Write + Send> Sink for JsonSink<W> {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<(), BindError> {
        let schema = batch.schema();
        let columns = batch.columns();

        for row in 0..batch.num_rows() {
            let mut map = serde_json::Map::with_capacity(schema.fields().len());
            for (i, field) in schema.fields().iter().enumerate() {
                map.insert(field.name().clone(), cell_to_json(columns[i].as_ref(), row));
            }
            let obj = serde_json::Value::Object(map);

            match self.mode {
                JsonMode::Jsonl => {
                    let serialized = serde_json::to_string(&obj)?;
                    let b = self.writer.write(serialized.as_bytes())?;
                    self.bytes_written += b as u64;
                    let b = self.writer.write(b"\n")?;
                    self.bytes_written += b as u64;
                }
                JsonMode::JsonArray => {
                    if !self.first_row {
                        let b = self.writer.write(b",")?;
                        self.bytes_written += b as u64;
                    }
                    let serialized = serde_json::to_string(&obj)?;
                    let b = self.writer.write(serialized.as_bytes())?;
                    self.bytes_written += b as u64;
                }
            }
            self.first_row = false;
        }
        self.rows_written += batch.num_rows() as u64;
        debug!(rows = batch.num_rows(), total = self.rows_written, "wrote json batch");
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<SinkStats, BindError> {
        if self.mode == JsonMode::JsonArray {
            let b = self.writer.write(b"]")?;
            self.bytes_written += b as u64;
        }
        self.writer.flush()?;
        debug!(rows = self.rows_written, bytes = self.bytes_written, "json sink finished");
        Ok(SinkStats {
            rows_written: self.rows_written,
            bytes_written: self.bytes_written,
            files_created: 1,
        })
    }
}
