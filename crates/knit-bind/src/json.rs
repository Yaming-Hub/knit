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

/// Downcast an Arrow column to the expected array type, returning a
/// [`BindError`] on type mismatch instead of panicking.
#[macro_export]
macro_rules! downcast_col {
    ($col:expr, $arr_ty:ty) => {
        $col.as_any()
            .downcast_ref::<$arr_ty>()
            .ok_or_else(|| {
                $crate::BindError::Other(format!(
                    "Arrow type mismatch: expected {}, got {:?}",
                    stringify!($arr_ty),
                    $col.data_type(),
                ))
            })
    };
}
pub use downcast_col;

/// Convert a single cell value to a `serde_json::Value`.
fn cell_to_json(col: &dyn Array, row: usize) -> Result<serde_json::Value, BindError> {
    if col.is_null(row) {
        return Ok(serde_json::Value::Null);
    }
    match col.data_type() {
        DataType::Boolean => {
            let arr = downcast_col!(col, array::BooleanArray)?;
            Ok(serde_json::Value::Bool(arr.value(row)))
        }
        DataType::Int8 => json_number!(col, array::Int8Array, row),
        DataType::Int16 => json_number!(col, array::Int16Array, row),
        DataType::Int32 => json_number!(col, array::Int32Array, row),
        DataType::Int64 => {
            let arr = downcast_col!(col, array::Int64Array)?;
            Ok(serde_json::Value::Number(arr.value(row).into()))
        }
        DataType::UInt8 => json_number!(col, array::UInt8Array, row),
        DataType::UInt16 => json_number!(col, array::UInt16Array, row),
        DataType::UInt32 => json_number!(col, array::UInt32Array, row),
        DataType::UInt64 => {
            let arr = downcast_col!(col, array::UInt64Array)?;
            Ok(serde_json::Value::Number(arr.value(row).into()))
        }
        DataType::Float32 => {
            let arr = downcast_col!(col, array::Float32Array)?;
            let v = arr.value(row) as f64;
            Ok(serde_json::Number::from_f64(v)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null))
        }
        DataType::Float64 => {
            let arr = downcast_col!(col, array::Float64Array)?;
            Ok(serde_json::Number::from_f64(arr.value(row))
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null))
        }
        DataType::Utf8 => {
            let arr = downcast_col!(col, array::StringArray)?;
            Ok(serde_json::Value::String(arr.value(row).to_string()))
        }
        DataType::LargeUtf8 => {
            let arr = downcast_col!(col, array::LargeStringArray)?;
            Ok(serde_json::Value::String(arr.value(row).to_string()))
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            let arr = downcast_col!(col, array::TimestampMicrosecondArray)?;
            let ts = arr.value(row);
            let dt = chrono::DateTime::from_timestamp_micros(ts);
            Ok(match dt {
                Some(d) => serde_json::Value::String(d.to_rfc3339()),
                None => serde_json::Value::Null,
            })
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let arr = downcast_col!(col, array::TimestampMillisecondArray)?;
            let ts = arr.value(row);
            let dt = chrono::DateTime::from_timestamp_millis(ts);
            Ok(match dt {
                Some(d) => serde_json::Value::String(d.to_rfc3339()),
                None => serde_json::Value::Null,
            })
        }
        DataType::Timestamp(TimeUnit::Second, _) => {
            let arr = downcast_col!(col, array::TimestampSecondArray)?;
            let ts = arr.value(row);
            let dt = chrono::DateTime::from_timestamp(ts, 0);
            Ok(match dt {
                Some(d) => serde_json::Value::String(d.to_rfc3339()),
                None => serde_json::Value::Null,
            })
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            let arr = downcast_col!(col, array::TimestampNanosecondArray)?;
            let ts = arr.value(row);
            let secs = ts.div_euclid(1_000_000_000);
            let nsecs = ts.rem_euclid(1_000_000_000) as u32;
            let dt = chrono::DateTime::from_timestamp(secs, nsecs);
            Ok(match dt {
                Some(d) => serde_json::Value::String(d.to_rfc3339()),
                None => serde_json::Value::Null,
            })
        }
        _ => {
            // Fallback: use Arrow's display formatting
            let casted = arrow::compute::cast(col, &DataType::Utf8).unwrap_or_else(|_| {
                std::sync::Arc::new(array::StringArray::from(vec!["<unsupported>"]))
            });
            let formatted = array::cast::as_string_array(&casted).value(row).to_string();
            Ok(serde_json::Value::String(formatted))
        }
    }
}

macro_rules! json_number {
    ($col:expr, $arr_ty:ty, $row:expr) => {{
        let arr = downcast_col!($col, $arr_ty)?;
        Ok(serde_json::Value::Number(arr.value($row).into()))
    }};
}
use json_number;

impl<W: Write + Send> Sink for JsonSink<W> {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<(), BindError> {
        let schema = batch.schema();
        let columns = batch.columns();
        // Pre-collect field names to avoid per-row cloning
        let field_names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();

        for row in 0..batch.num_rows() {
            let mut map = serde_json::Map::with_capacity(field_names.len());
            for (i, name) in field_names.iter().enumerate() {
                map.insert((*name).to_string(), cell_to_json(columns[i].as_ref(), row)?);
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

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::{BooleanArray, Float64Array, Int64Array, StringArray};
    use arrow::datatypes::{Field, Schema};
    use std::io::Cursor;
    use std::sync::Arc;

    fn sample_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, false),
            Field::new("score", DataType::Float64, true),
            Field::new("active", DataType::Boolean, false),
        ]));
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![1, 2])),
                Arc::new(StringArray::from(vec!["alice", "bob"])),
                Arc::new(Float64Array::from(vec![Some(95.5), None])),
                Arc::new(BooleanArray::from(vec![true, false])),
            ],
        )
        .unwrap()
    }

    #[test]
    fn jsonl_basic() {
        let buf = Cursor::new(Vec::new());
        let mut sink = JsonSink::new(buf, JsonMode::Jsonl).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 2);
        assert!(stats.bytes_written > 0);
    }

    #[test]
    fn json_array_basic() {
        let buf = Cursor::new(Vec::new());
        let mut sink = JsonSink::new(buf, JsonMode::JsonArray).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 2);
    }

    #[test]
    fn cell_to_json_null_handling() {
        let arr = Float64Array::from(vec![None, Some(1.0)]);
        let result = cell_to_json(&arr, 0).unwrap();
        assert_eq!(result, serde_json::Value::Null);
        let result = cell_to_json(&arr, 1).unwrap();
        assert!(result.is_number());
    }

    #[test]
    fn cell_to_json_all_basic_types() {
        // Boolean
        let arr = BooleanArray::from(vec![true]);
        assert_eq!(cell_to_json(&arr, 0).unwrap(), serde_json::Value::Bool(true));

        // Int64
        let arr = Int64Array::from(vec![42]);
        assert_eq!(cell_to_json(&arr, 0).unwrap(), serde_json::json!(42));

        // Float64
        let arr = Float64Array::from(vec![3.14]);
        let val = cell_to_json(&arr, 0).unwrap();
        assert!(val.is_number());

        // Utf8
        let arr = StringArray::from(vec!["hello"]);
        assert_eq!(
            cell_to_json(&arr, 0).unwrap(),
            serde_json::Value::String("hello".to_string())
        );
    }

    #[test]
    fn cell_to_json_nan_becomes_null() {
        let arr = Float64Array::from(vec![f64::NAN]);
        let val = cell_to_json(&arr, 0).unwrap();
        assert_eq!(val, serde_json::Value::Null);
    }
}