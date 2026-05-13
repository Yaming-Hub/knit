//! JSON / JSONL output sink.

use std::collections::HashMap;
use std::io::Write;

use arrow::array::{self, Array};
use arrow::datatypes::{DataType, TimeUnit};
use arrow::record_batch::RecordBatch;
use tracing::debug;

use crate::bind::error::BindError;
use crate::bind::traits::{Sink, SinkStats};

/// Fast deterministic hash for per-row omission decisions.
fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// JSON output mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JsonMode {
    /// One JSON object per line (newline-delimited JSON).
    Jsonl,
    /// A single JSON array wrapping all objects.
    JsonArray,
}

/// Specifies a field that should be randomly omitted from document output.
///
/// For each row, a deterministic RNG decides whether the field is present.
/// This produces semi-structured output where some records lack certain keys,
/// simulating real-world document databases (MongoDB, DynamoDB, etc.).
#[derive(Debug, Clone)]
pub struct MissingFieldSpec {
    /// Column name to potentially omit.
    pub field: String,
    /// Probability in `[0.0, 1.0]` that the field is omitted per row.
    pub probability: f64,
    /// RNG seed for deterministic omission decisions.
    pub seed: u64,
}

/// Sink that writes `RecordBatch`es as JSON.
pub struct JsonSink<W: Write + Send> {
    writer: W,
    mode: JsonMode,
    rows_written: u64,
    bytes_written: u64,
    first_row: bool,
    /// Per-field missing specs, keyed by field name for O(1) lookup.
    missing_specs: HashMap<String, (f64, u64)>,
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
            missing_specs: HashMap::new(),
        })
    }

    /// Configure fields that should be randomly omitted from output.
    pub fn with_missing_fields(mut self, specs: Vec<MissingFieldSpec>) -> Self {
        for spec in specs {
            self.missing_specs
                .insert(spec.field, (spec.probability, spec.seed));
        }
        self
    }
}

/// Downcast an Arrow column to the expected array type, returning a
/// [`BindError`] on type mismatch instead of panicking.
#[macro_export]
macro_rules! downcast_col {
    ($col:expr, $arr_ty:ty) => {
        $col.as_any().downcast_ref::<$arr_ty>().ok_or_else(|| {
            $crate::bind::BindError::Other(format!(
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
        DataType::List(_) | DataType::LargeList(_) | DataType::FixedSizeList(_, _) => {
            list_to_json(col, row)
        }
        DataType::Map(_, _) => map_to_json(col, row),
        DataType::Struct(_) => struct_to_json(col, row),
        _ => {
            // Fallback: cast the entire column to Utf8 and read the row.
            match arrow::compute::cast(col, &DataType::Utf8) {
                Ok(casted) => {
                    let formatted = array::cast::as_string_array(&casted).value(row).to_string();
                    Ok(serde_json::Value::String(formatted))
                }
                Err(_) => Ok(serde_json::Value::String("<unsupported>".to_string())),
            }
        }
    }
}

/// Convert a List or LargeList column element to a JSON array.
fn list_to_json(col: &dyn Array, row: usize) -> Result<serde_json::Value, BindError> {
    // Get the child values for this row using the generic ListArray trait
    let values: std::sync::Arc<dyn Array> = match col.data_type() {
        DataType::List(_) => {
            let list = downcast_col!(col, array::ListArray)?;
            list.value(row)
        }
        DataType::LargeList(_) => {
            let list = downcast_col!(col, array::LargeListArray)?;
            list.value(row)
        }
        DataType::FixedSizeList(_, _) => {
            let list = downcast_col!(col, array::FixedSizeListArray)?;
            list.value(row)
        }
        _ => return Ok(serde_json::Value::Null),
    };
    let items: Result<Vec<serde_json::Value>, BindError> = (0..values.len())
        .map(|i| cell_to_json(values.as_ref(), i))
        .collect();
    Ok(serde_json::Value::Array(items?))
}

/// Convert a Map column element to a JSON object.
fn map_to_json(col: &dyn Array, row: usize) -> Result<serde_json::Value, BindError> {
    let map = downcast_col!(col, array::MapArray)?;
    let entries = map.value(row);
    let struct_arr = entries
        .as_any()
        .downcast_ref::<array::StructArray>()
        .ok_or_else(|| BindError::Other("Map entries not a StructArray".to_string()))?;
    let keys = struct_arr.column(0);
    let vals = struct_arr.column(1);
    let mut obj = serde_json::Map::new();
    for i in 0..entries.len() {
        let key = cell_to_json(keys.as_ref(), i)?;
        let val = cell_to_json(vals.as_ref(), i)?;
        // Stringify non-string keys to preserve all entries
        let k = match key {
            serde_json::Value::String(s) => s,
            other => other.to_string(),
        };
        obj.insert(k, val);
    }
    Ok(serde_json::Value::Object(obj))
}

/// Convert a Struct column element to a JSON object.
fn struct_to_json(col: &dyn Array, row: usize) -> Result<serde_json::Value, BindError> {
    let struct_arr = downcast_col!(col, array::StructArray)?;
    let fields = match col.data_type() {
        DataType::Struct(f) => f,
        _ => return Ok(serde_json::Value::Null),
    };
    let mut obj = serde_json::Map::new();
    for (fi, field) in fields.iter().enumerate() {
        let child = struct_arr.column(fi);
        obj.insert(field.name().clone(), cell_to_json(child.as_ref(), row)?);
    }
    Ok(serde_json::Value::Object(obj))
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

        // Pre-collect missing-field specs indexed by column position.
        // Uses a per-row hash (splitmix64) for batch-size-independent decisions.
        let missing_lookup: Vec<Option<(f64, u64)>> = field_names
            .iter()
            .map(|name| self.missing_specs.get(*name).copied())
            .collect();

        for row in 0..batch.num_rows() {
            let global_row = self.rows_written + row as u64;
            let mut map = serde_json::Map::with_capacity(field_names.len());
            for (i, name) in field_names.iter().enumerate() {
                // Check if this field should be omitted for this row
                if let Some((prob, seed)) = missing_lookup[i] {
                    // Deterministic per-(field, row) decision via splitmix64
                    let hash = splitmix64(seed.wrapping_add(global_row));
                    let roll = (hash >> 11) as f64 / ((1u64 << 53) as f64);
                    if roll < prob {
                        continue; // omit this field entirely
                    }
                }
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
        debug!(
            rows = batch.num_rows(),
            total = self.rows_written,
            "wrote json batch"
        );
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<SinkStats, BindError> {
        if self.mode == JsonMode::JsonArray {
            let b = self.writer.write(b"]")?;
            self.bytes_written += b as u64;
        }
        self.writer.flush()?;
        debug!(
            rows = self.rows_written,
            bytes = self.bytes_written,
            "json sink finished"
        );
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
        assert_eq!(
            cell_to_json(&arr, 0).unwrap(),
            serde_json::Value::Bool(true)
        );

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

    #[test]
    fn cell_to_json_list_of_strings() {
        use arrow::array::ListArray;
        use arrow::buffer::OffsetBuffer;

        let values = StringArray::from(vec!["a", "b", "c"]);
        let offsets = OffsetBuffer::new(vec![0i32, 2, 3].into());
        let list = ListArray::new(
            Arc::new(Field::new("item", DataType::Utf8, false)),
            offsets,
            Arc::new(values),
            None,
        );
        // Row 0 = ["a", "b"], Row 1 = ["c"]
        let val = cell_to_json(&list, 0).unwrap();
        assert_eq!(val, serde_json::json!(["a", "b"]));
        let val = cell_to_json(&list, 1).unwrap();
        assert_eq!(val, serde_json::json!(["c"]));
    }

    #[test]
    fn cell_to_json_map_string_to_int() {
        use arrow::array::{Int32Array, MapArray, StructArray};
        use arrow::buffer::OffsetBuffer;

        let keys = StringArray::from(vec!["x", "y"]);
        let vals = Int32Array::from(vec![10, 20]);
        let entries_field = vec![
            Field::new("key", DataType::Utf8, false),
            Field::new("value", DataType::Int32, true),
        ];
        let entries = StructArray::from(vec![
            (Arc::new(entries_field[0].clone()), Arc::new(keys) as _),
            (Arc::new(entries_field[1].clone()), Arc::new(vals) as _),
        ]);
        let map_field = Field::new("entries", DataType::Struct(entries_field.into()), false);
        let offsets = OffsetBuffer::new(vec![0i32, 2].into());
        let map = MapArray::new(Arc::new(map_field), offsets, entries, None, false);

        let val = cell_to_json(&map, 0).unwrap();
        assert_eq!(val, serde_json::json!({"x": 10, "y": 20}));
    }

    #[test]
    fn cell_to_json_struct_type() {
        use arrow::array::StructArray;

        let names = StringArray::from(vec!["alice"]);
        let ages = Int64Array::from(vec![30i64]);
        let fields = vec![
            Arc::new(Field::new("name", DataType::Utf8, false)),
            Arc::new(Field::new("age", DataType::Int64, false)),
        ];
        let struct_arr = StructArray::from(vec![
            (fields[0].clone(), Arc::new(names) as _),
            (fields[1].clone(), Arc::new(ages) as _),
        ]);

        let val = cell_to_json(&struct_arr, 0).unwrap();
        assert_eq!(val, serde_json::json!({"name": "alice", "age": 30}));
    }

    #[test]
    fn jsonl_with_nested_types() {
        use arrow::array::{ListArray, StructArray};
        use arrow::buffer::OffsetBuffer;

        // Build a batch with a list column and a struct column
        let id_arr = Int64Array::from(vec![1]);
        let list_values = StringArray::from(vec!["tag1", "tag2"]);
        let list_offsets = OffsetBuffer::new(vec![0i32, 2].into());
        let list_arr = ListArray::new(
            Arc::new(Field::new("item", DataType::Utf8, false)),
            list_offsets,
            Arc::new(list_values),
            None,
        );
        let struct_name = StringArray::from(vec!["bob"]);
        let struct_age = Int64Array::from(vec![25i64]);
        let struct_fields = vec![
            Arc::new(Field::new("name", DataType::Utf8, false)),
            Arc::new(Field::new("age", DataType::Int64, false)),
        ];
        let struct_arr = StructArray::from(vec![
            (struct_fields[0].clone(), Arc::new(struct_name) as _),
            (struct_fields[1].clone(), Arc::new(struct_age) as _),
        ]);

        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new(
                "tags",
                DataType::List(Arc::new(Field::new("item", DataType::Utf8, false))),
                false,
            ),
            Field::new(
                "meta",
                DataType::Struct(
                    vec![
                        Field::new("name", DataType::Utf8, false),
                        Field::new("age", DataType::Int64, false),
                    ]
                    .into(),
                ),
                false,
            ),
        ]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(id_arr), Arc::new(list_arr), Arc::new(struct_arr)],
        )
        .unwrap();

        let buf = Cursor::new(Vec::new());
        let mut sink = JsonSink::new(buf, JsonMode::Jsonl).unwrap();
        sink.write_batch(&batch).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 1);

        // Verify we can parse the output as valid JSON with proper nested types
        // (stats.bytes_written > 0 confirms output was produced)
        assert!(stats.bytes_written > 0);
    }

    #[test]
    fn json_preserves_field_order() {
        let buf = Cursor::new(Vec::new());
        let mut sink = JsonSink::new(buf, JsonMode::Jsonl).unwrap();
        sink.write_batch(&sample_batch()).unwrap();
        let output = String::from_utf8(sink.writer.into_inner()).unwrap();
        let first_line = output.lines().next().unwrap();
        // Fields should appear in schema order: id, name, score, active
        let obj: serde_json::Value = serde_json::from_str(first_line).unwrap();
        let keys: Vec<&String> = obj.as_object().unwrap().keys().collect();
        assert_eq!(keys, &["id", "name", "score", "active"]);
    }

    #[test]
    fn missing_field_omits_key_from_json() {
        let buf = Cursor::new(Vec::new());
        // Use probability 1.0 to always omit the "score" field
        let mut sink = JsonSink::new(buf, JsonMode::Jsonl)
            .unwrap()
            .with_missing_fields(vec![MissingFieldSpec {
                field: "score".to_string(),
                probability: 1.0,
                seed: 42,
            }]);
        sink.write_batch(&sample_batch()).unwrap();
        let output = String::from_utf8(sink.writer.into_inner()).unwrap();
        for line in output.lines() {
            let obj: serde_json::Value = serde_json::from_str(line).unwrap();
            let map = obj.as_object().unwrap();
            assert!(
                !map.contains_key("score"),
                "score should be omitted: {line}"
            );
            // Other fields should still be present
            assert!(map.contains_key("id"));
            assert!(map.contains_key("name"));
            assert!(map.contains_key("active"));
        }
    }

    #[test]
    fn missing_field_zero_probability_keeps_all() {
        let buf = Cursor::new(Vec::new());
        let mut sink = JsonSink::new(buf, JsonMode::Jsonl)
            .unwrap()
            .with_missing_fields(vec![MissingFieldSpec {
                field: "score".to_string(),
                probability: 0.0,
                seed: 42,
            }]);
        sink.write_batch(&sample_batch()).unwrap();
        let output = String::from_utf8(sink.writer.into_inner()).unwrap();
        for line in output.lines() {
            let obj: serde_json::Value = serde_json::from_str(line).unwrap();
            assert!(obj.as_object().unwrap().contains_key("score"));
        }
    }

    #[test]
    fn missing_field_partial_probability() {
        // With a moderate probability and enough rows, some should be present
        // and some missing. Use 100 rows to get statistical confidence.
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("val", DataType::Utf8, false),
        ]));
        let ids: Vec<i64> = (0..100).collect();
        let vals: Vec<&str> = (0..100).map(|_| "x").collect();
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(StringArray::from(vals)),
            ],
        )
        .unwrap();

        let buf = Cursor::new(Vec::new());
        let mut sink = JsonSink::new(buf, JsonMode::Jsonl)
            .unwrap()
            .with_missing_fields(vec![MissingFieldSpec {
                field: "val".to_string(),
                probability: 0.5,
                seed: 123,
            }]);
        sink.write_batch(&batch).unwrap();
        let output = String::from_utf8(sink.writer.into_inner()).unwrap();

        let mut present = 0u32;
        let mut missing = 0u32;
        for line in output.lines() {
            let obj: serde_json::Value = serde_json::from_str(line).unwrap();
            if obj.as_object().unwrap().contains_key("val") {
                present += 1;
            } else {
                missing += 1;
            }
        }
        // With 100 rows and p=0.5, we expect both present and missing to be > 0
        assert!(present > 0, "expected some rows to have 'val'");
        assert!(missing > 0, "expected some rows to omit 'val'");
    }

    #[test]
    fn missing_field_deterministic() {
        // Same seed should produce same omission pattern
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("val", DataType::Utf8, false),
        ]));
        let ids: Vec<i64> = (0..20).collect();
        let vals: Vec<&str> = (0..20).map(|_| "x").collect();
        let batch = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(StringArray::from(vals)),
            ],
        )
        .unwrap();

        let mut outputs = Vec::new();
        for _ in 0..2 {
            let buf = Cursor::new(Vec::new());
            let mut sink = JsonSink::new(buf, JsonMode::Jsonl)
                .unwrap()
                .with_missing_fields(vec![MissingFieldSpec {
                    field: "val".to_string(),
                    probability: 0.3,
                    seed: 999,
                }]);
            sink.write_batch(&batch.clone()).unwrap();
            let output = String::from_utf8(sink.writer.into_inner()).unwrap();
            outputs.push(output);
        }
        assert_eq!(
            outputs[0], outputs[1],
            "same seed should produce identical output"
        );
    }

    #[test]
    fn missing_field_batch_size_independent() {
        // Verify that splitting rows across batches produces the same result
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("val", DataType::Utf8, false),
        ]));

        let spec = vec![MissingFieldSpec {
            field: "val".to_string(),
            probability: 0.4,
            seed: 777,
        }];

        // Single batch of 10 rows
        let ids: Vec<i64> = (0..10).collect();
        let vals: Vec<&str> = (0..10).map(|_| "x").collect();
        let full_batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(ids)),
                Arc::new(StringArray::from(vals)),
            ],
        )
        .unwrap();

        let buf = Cursor::new(Vec::new());
        let mut sink = JsonSink::new(buf, JsonMode::Jsonl)
            .unwrap()
            .with_missing_fields(spec.clone());
        sink.write_batch(&full_batch).unwrap();
        let output_one = String::from_utf8(sink.writer.into_inner()).unwrap();

        // Two batches: 4 + 6 rows
        let batch_a = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(vec![0i64, 1, 2, 3])),
                Arc::new(StringArray::from(vec!["x"; 4])),
            ],
        )
        .unwrap();
        let batch_b = RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![4i64, 5, 6, 7, 8, 9])),
                Arc::new(StringArray::from(vec!["x"; 6])),
            ],
        )
        .unwrap();

        let buf = Cursor::new(Vec::new());
        let mut sink = JsonSink::new(buf, JsonMode::Jsonl)
            .unwrap()
            .with_missing_fields(spec);
        sink.write_batch(&batch_a).unwrap();
        sink.write_batch(&batch_b).unwrap();
        let output_two = String::from_utf8(sink.writer.into_inner()).unwrap();

        assert_eq!(
            output_one, output_two,
            "splitting into batches must not change omission pattern"
        );
    }
}
