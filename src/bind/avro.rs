//! Apache Avro output sink.
//!
//! Converts Arrow `RecordBatch`es to Avro Object Container Format (OCF).
//! Supports Null, Deflate, and Snappy compression codecs.

use std::io::Write;

use apache_avro::types::Value as AvroValue;
use apache_avro::Schema as AvroSchema;
use arrow::array::*;
use arrow::datatypes::{DataType, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;
use parking_lot::Mutex;
use std::sync::Arc;
use tracing::debug;

use crate::bind::error::BindError;
use crate::bind::traits::{Sink, SinkStats};

/// Avro compression codec.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AvroCodec {
    /// No compression.
    #[default]
    Null,
    /// Deflate compression.
    Deflate,
    /// Snappy compression.
    Snappy,
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

/// Sink that writes `RecordBatch`es in Apache Avro format.
pub struct AvroSink<W: Write + Send> {
    writer: Option<apache_avro::Writer<'static, CountingWriter<W>>>,
    rows_written: u64,
    byte_count: Arc<Mutex<u64>>,
    field_names: Vec<String>,
    field_nullable: Vec<bool>,
}

impl<W: Write + Send> AvroSink<W> {
    /// Create a new `AvroSink` writing to the given writer.
    ///
    /// `record_name` is used as the top-level Avro record name.
    /// The Arrow schema is converted to an Avro schema at construction time.
    pub fn new(
        writer: W,
        arrow_schema: Arc<Schema>,
        record_name: &str,
        codec: AvroCodec,
    ) -> Result<Self, BindError> {
        let avro_schema = arrow_schema_to_avro(&arrow_schema, record_name)?;
        // Leak the schema so it has 'static lifetime for the Writer
        let schema_ref: &'static AvroSchema = Box::leak(Box::new(avro_schema));

        let avro_codec = match codec {
            AvroCodec::Null => apache_avro::Codec::Null,
            AvroCodec::Deflate => {
                apache_avro::Codec::Deflate(apache_avro::DeflateSettings::default())
            }
            AvroCodec::Snappy => apache_avro::Codec::Snappy,
        };

        let byte_count = Arc::new(Mutex::new(0u64));
        let counting = CountingWriter {
            inner: writer,
            count: Arc::clone(&byte_count),
        };

        let avro_writer = apache_avro::Writer::builder()
            .schema(schema_ref)
            .codec(avro_codec)
            .writer(counting)
            .build();

        let field_names: Vec<String> = arrow_schema
            .fields()
            .iter()
            .map(|f| f.name().clone())
            .collect();

        let field_nullable: Vec<bool> = arrow_schema
            .fields()
            .iter()
            .map(|f| f.is_nullable())
            .collect();

        Ok(Self {
            writer: Some(avro_writer),
            rows_written: 0,
            byte_count,
            field_names,
            field_nullable,
        })
    }
}

impl<W: Write + Send> Sink for AvroSink<W> {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<(), BindError> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| BindError::Other("sink already finished".into()))?;

        let num_rows = batch.num_rows();
        for row in 0..num_rows {
            let fields: Result<Vec<(String, AvroValue)>, BindError> = self
                .field_names
                .iter()
                .enumerate()
                .map(|(col, name)| {
                    let array = batch.column(col);
                    let nullable = self.field_nullable[col];
                    let value = arrow_value_to_avro(array, row, nullable)?;
                    Ok((name.clone(), value))
                })
                .collect();
            let fields = fields?;

            let record = AvroValue::Record(fields);
            writer
                .append(record)
                .map_err(|e| BindError::Other(format!("Avro write error: {e}")))?;
        }

        self.rows_written += num_rows as u64;
        debug!(
            rows = num_rows as u64,
            total = self.rows_written,
            "wrote avro batch"
        );
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<SinkStats, BindError> {
        let mut writer = self
            .writer
            .take()
            .ok_or_else(|| BindError::Other("sink already finished".into()))?;

        writer
            .flush()
            .map_err(|e| BindError::Other(format!("Avro flush error: {e}")))?;

        let bytes_written = *self.byte_count.lock();
        debug!(
            rows = self.rows_written,
            bytes = bytes_written,
            "avro sink finished"
        );
        Ok(SinkStats {
            rows_written: self.rows_written,
            bytes_written,
            files_created: 1,
        })
    }
}

// ── Arrow → Avro schema conversion ──────────────────────────────────

/// Convert an Arrow schema to an Avro record schema.
fn arrow_schema_to_avro(schema: &Schema, record_name: &str) -> Result<AvroSchema, BindError> {
    let mut fields = Vec::new();
    for field in schema.fields() {
        let base_schema = arrow_type_to_avro(field.data_type())?;
        let field_schema = if field.is_nullable() {
            // Avro union: ["null", type]
            AvroSchema::Union(
                apache_avro::schema::UnionSchema::new(vec![AvroSchema::Null, base_schema])
                    .map_err(|e| BindError::Other(format!("Avro union error: {e}")))?,
            )
        } else {
            base_schema
        };

        fields.push(avro_record_field(field.name(), field_schema));
    }

    let schema_json = serde_json::json!({
        "type": "record",
        "name": record_name,
        "fields": fields
    });

    AvroSchema::parse_str(&schema_json.to_string())
        .map_err(|e| BindError::Other(format!("Avro schema parse error: {e}")))
}

/// Build a JSON representation of an Avro record field.
fn avro_record_field(name: &str, schema: AvroSchema) -> serde_json::Value {
    let type_json = avro_schema_to_json(&schema);
    serde_json::json!({
        "name": name,
        "type": type_json
    })
}

/// Convert an Avro schema to its JSON representation.
fn avro_schema_to_json(schema: &AvroSchema) -> serde_json::Value {
    match schema {
        AvroSchema::Null => serde_json::json!("null"),
        AvroSchema::Boolean => serde_json::json!("boolean"),
        AvroSchema::Int => serde_json::json!("int"),
        AvroSchema::Long => serde_json::json!("long"),
        AvroSchema::Float => serde_json::json!("float"),
        AvroSchema::Double => serde_json::json!("double"),
        AvroSchema::String => serde_json::json!("string"),
        AvroSchema::Bytes => serde_json::json!("bytes"),
        AvroSchema::Union(union) => {
            let variants: Vec<serde_json::Value> =
                union.variants().iter().map(avro_schema_to_json).collect();
            serde_json::json!(variants)
        }
        AvroSchema::Array(inner) => {
            serde_json::json!({
                "type": "array",
                "items": avro_schema_to_json(&inner.items)
            })
        }
        AvroSchema::Record(record) => {
            let fields: Vec<serde_json::Value> = record
                .fields
                .iter()
                .map(|f| {
                    serde_json::json!({
                        "name": f.name,
                        "type": avro_schema_to_json(&f.schema)
                    })
                })
                .collect();
            serde_json::json!({
                "type": "record",
                "name": record.name.fullname(None),
                "fields": fields
            })
        }
        _ => serde_json::json!("string"), // fallback
    }
}

/// Map an Arrow DataType to an Avro Schema.
fn arrow_type_to_avro(dt: &DataType) -> Result<AvroSchema, BindError> {
    match dt {
        DataType::Null => Ok(AvroSchema::Null),
        DataType::Boolean => Ok(AvroSchema::Boolean),
        DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::UInt8 | DataType::UInt16 => {
            Ok(AvroSchema::Int)
        }
        DataType::Int64 | DataType::UInt32 | DataType::UInt64 => Ok(AvroSchema::Long),
        DataType::Float16 | DataType::Float32 => Ok(AvroSchema::Float),
        DataType::Float64 => Ok(AvroSchema::Double),
        DataType::Utf8 | DataType::LargeUtf8 => Ok(AvroSchema::String),
        DataType::Binary | DataType::LargeBinary => Ok(AvroSchema::Bytes),
        DataType::Date32 => Ok(AvroSchema::Int), // days since epoch
        DataType::Date64 => Ok(AvroSchema::Long), // millis since epoch
        DataType::Timestamp(_, _) => Ok(AvroSchema::Long), // millis since epoch
        DataType::Time32(_) | DataType::Time64(_) => Ok(AvroSchema::Long), // time as long
        DataType::Duration(_) => Ok(AvroSchema::Long), // duration as millis
        DataType::List(inner) => {
            let items = arrow_type_to_avro(inner.data_type())?;
            Ok(AvroSchema::array(items))
        }
        DataType::Struct(fields) => {
            // Build an Avro record schema for the nested struct.
            // Use a counter-suffixed name to avoid collisions when multiple
            // struct fields share the same child field names.
            use std::sync::atomic::{AtomicU32, Ordering};
            static STRUCT_COUNTER: AtomicU32 = AtomicU32::new(0);
            let id = STRUCT_COUNTER.fetch_add(1, Ordering::Relaxed);
            let record_name = format!(
                "struct_{}_{}",
                fields
                    .iter()
                    .map(|f| f.name().as_str())
                    .collect::<Vec<_>>()
                    .join("_"),
                id
            );
            let mut avro_fields = Vec::new();
            for field in fields.iter() {
                let base = arrow_type_to_avro(field.data_type())?;
                let field_schema = if field.is_nullable() {
                    AvroSchema::Union(
                        apache_avro::schema::UnionSchema::new(vec![AvroSchema::Null, base])
                            .map_err(|e| BindError::Other(format!("Avro union error: {e}")))?,
                    )
                } else {
                    base
                };
                avro_fields.push(avro_record_field(field.name(), field_schema));
            }
            let schema_json = serde_json::json!({
                "type": "record",
                "name": record_name,
                "fields": avro_fields
            });
            AvroSchema::parse_str(&schema_json.to_string())
                .map_err(|e| BindError::Other(format!("Avro nested record parse error: {e}")))
        }
        _ => {
            // Map, Struct, etc. → encode as JSON string
            Ok(AvroSchema::String)
        }
    }
}

// ── Arrow value → Avro value conversion ─────────────────────────────

/// Convert a single Arrow array value at `row` to an Avro value.
///
/// `nullable` is driven by the Arrow schema field's `is_nullable()`, not the
/// array's runtime null count, ensuring union wrapping is consistent regardless
/// of whether the current batch contains nulls.
fn arrow_value_to_avro(array: &ArrayRef, row: usize, nullable: bool) -> Result<AvroValue, BindError> {
    if array.is_null(row) {
        return Ok(if nullable {
            AvroValue::Union(0, Box::new(AvroValue::Null))
        } else {
            AvroValue::Null
        });
    }

    let value = arrow_value_to_avro_inner(array, row)?;
    Ok(if nullable {
        AvroValue::Union(1, Box::new(value))
    } else {
        value
    })
}

/// Convert a non-null Arrow array value at `row` to an Avro value (without union wrapping).
fn arrow_value_to_avro_inner(array: &ArrayRef, row: usize) -> Result<AvroValue, BindError> {
    let value = match array.data_type() {
        DataType::Null => AvroValue::Null,
        DataType::Boolean => {
            let a = downcast_array::<BooleanArray>(array, "BooleanArray")?;
            AvroValue::Boolean(a.value(row))
        }
        DataType::Int8 => {
            let a = downcast_array::<Int8Array>(array, "Int8Array")?;
            AvroValue::Int(a.value(row) as i32)
        }
        DataType::Int16 => {
            let a = downcast_array::<Int16Array>(array, "Int16Array")?;
            AvroValue::Int(a.value(row) as i32)
        }
        DataType::Int32 => {
            let a = downcast_array::<Int32Array>(array, "Int32Array")?;
            AvroValue::Int(a.value(row))
        }
        DataType::UInt8 => {
            let a = downcast_array::<UInt8Array>(array, "UInt8Array")?;
            AvroValue::Int(a.value(row) as i32)
        }
        DataType::UInt16 => {
            let a = downcast_array::<UInt16Array>(array, "UInt16Array")?;
            AvroValue::Int(a.value(row) as i32)
        }
        DataType::UInt32 => {
            let a = downcast_array::<UInt32Array>(array, "UInt32Array")?;
            AvroValue::Long(a.value(row) as i64)
        }
        DataType::Int64 => {
            let a = downcast_array::<Int64Array>(array, "Int64Array")?;
            AvroValue::Long(a.value(row))
        }
        DataType::UInt64 => {
            let a = downcast_array::<UInt64Array>(array, "UInt64Array")?;
            AvroValue::Long(a.value(row) as i64)
        }
        DataType::Float16 => {
            let a = downcast_array::<Float16Array>(array, "Float16Array")?;
            AvroValue::Float(a.value(row).to_f32())
        }
        DataType::Float32 => {
            let a = downcast_array::<Float32Array>(array, "Float32Array")?;
            AvroValue::Float(a.value(row))
        }
        DataType::Float64 => {
            let a = downcast_array::<Float64Array>(array, "Float64Array")?;
            AvroValue::Double(a.value(row))
        }
        DataType::Utf8 => {
            let a = downcast_array::<StringArray>(array, "StringArray")?;
            AvroValue::String(a.value(row).to_string())
        }
        DataType::LargeUtf8 => {
            let a = downcast_array::<LargeStringArray>(array, "LargeStringArray")?;
            AvroValue::String(a.value(row).to_string())
        }
        DataType::Binary => {
            let a = downcast_array::<BinaryArray>(array, "BinaryArray")?;
            AvroValue::Bytes(a.value(row).to_vec())
        }
        DataType::LargeBinary => {
            let a = downcast_array::<LargeBinaryArray>(array, "LargeBinaryArray")?;
            AvroValue::Bytes(a.value(row).to_vec())
        }
        DataType::Date32 => {
            let a = downcast_array::<Date32Array>(array, "Date32Array")?;
            AvroValue::Int(a.value(row))
        }
        DataType::Date64 => {
            let a = downcast_array::<Date64Array>(array, "Date64Array")?;
            AvroValue::Long(a.value(row))
        }
        DataType::Timestamp(unit, _) => match unit {
            TimeUnit::Second => {
                let a = downcast_array::<TimestampSecondArray>(array, "TimestampSecondArray")?;
                AvroValue::Long(a.value(row) * 1000)
            }
            TimeUnit::Millisecond => {
                let a = downcast_array::<TimestampMillisecondArray>(array, "TimestampMillisecondArray")?;
                AvroValue::Long(a.value(row))
            }
            TimeUnit::Microsecond => {
                let a = downcast_array::<TimestampMicrosecondArray>(array, "TimestampMicrosecondArray")?;
                AvroValue::Long(a.value(row) / 1_000)
            }
            TimeUnit::Nanosecond => {
                let a = downcast_array::<TimestampNanosecondArray>(array, "TimestampNanosecondArray")?;
                AvroValue::Long(a.value(row) / 1_000_000)
            }
        },
        DataType::Time32(unit) => match unit {
            TimeUnit::Second => {
                let a = downcast_array::<Time32SecondArray>(array, "Time32SecondArray")?;
                AvroValue::Long(a.value(row) as i64 * 1000)
            }
            TimeUnit::Millisecond => {
                let a = downcast_array::<Time32MillisecondArray>(array, "Time32MillisecondArray")?;
                AvroValue::Long(a.value(row) as i64)
            }
            _ => AvroValue::Long(0),
        },
        DataType::Time64(unit) => match unit {
            TimeUnit::Microsecond => {
                let a = downcast_array::<Time64MicrosecondArray>(array, "Time64MicrosecondArray")?;
                AvroValue::Long(a.value(row) / 1000)
            }
            TimeUnit::Nanosecond => {
                let a = downcast_array::<Time64NanosecondArray>(array, "Time64NanosecondArray")?;
                AvroValue::Long(a.value(row) / 1_000_000)
            }
            _ => AvroValue::Long(0),
        },
        DataType::Duration(unit) => match unit {
            TimeUnit::Second => {
                let a = downcast_array::<DurationSecondArray>(array, "DurationSecondArray")?;
                AvroValue::Long(a.value(row) * 1000)
            }
            TimeUnit::Millisecond => {
                let a = downcast_array::<DurationMillisecondArray>(array, "DurationMillisecondArray")?;
                AvroValue::Long(a.value(row))
            }
            TimeUnit::Microsecond => {
                let a = downcast_array::<DurationMicrosecondArray>(array, "DurationMicrosecondArray")?;
                AvroValue::Long(a.value(row) / 1000)
            }
            TimeUnit::Nanosecond => {
                let a = downcast_array::<DurationNanosecondArray>(array, "DurationNanosecondArray")?;
                AvroValue::Long(a.value(row) / 1_000_000)
            }
        },
        DataType::List(_) => {
            let list = downcast_array::<ListArray>(array, "ListArray")?;
            let inner = list.value(row);
            let items: Result<Vec<AvroValue>, BindError> = (0..inner.len())
                .map(|i| arrow_value_to_avro_inner(&inner, i))
                .collect();
            AvroValue::Array(items?)
        }
        DataType::Struct(fields) => {
            let struct_arr = downcast_array::<StructArray>(array, "StructArray")?;
            let record_fields: Result<Vec<(String, AvroValue)>, BindError> = fields
                .iter()
                .enumerate()
                .map(|(i, field)| {
                    let col = struct_arr.column(i);
                    let val = arrow_value_to_avro(col, row, field.is_nullable())?;
                    Ok((field.name().clone(), val))
                })
                .collect();
            AvroValue::Record(record_fields?)
        }
        _ => {
            let display = arrow::util::display::array_value_to_string(array, row)
                .map_err(|e| BindError::Other(format!("Avro display conversion error: {e}")))?;
            AvroValue::String(display)
        }
    };

    Ok(value)
}

/// Downcast an Arrow array to the expected concrete array type.
fn downcast_array<'a, T: 'static>(array: &'a ArrayRef, expected: &str) -> Result<&'a T, BindError> {
    array.as_any().downcast_ref::<T>().ok_or_else(|| {
        BindError::Other(format!(
            "avro sink expected {expected} for Arrow type {:?}",
            array.data_type()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use apache_avro::Reader;
    use arrow::datatypes::Field;
    use std::io::Cursor;

    fn make_test_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
            Field::new("score", DataType::Float64, false),
            Field::new("active", DataType::Boolean, true),
        ]));

        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![1, 2, 3])),
                Arc::new(StringArray::from(vec![
                    Some("alice"),
                    None,
                    Some("charlie"),
                ])),
                Arc::new(Float64Array::from(vec![95.5, 87.3, 91.0])),
                Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])),
            ],
        )
        .unwrap()
    }

    #[test]
    fn round_trip_basic_types() {
        let batch = make_test_batch();
        let mut buf = Vec::new();

        {
            let sink = AvroSink::new(
                Cursor::new(&mut buf),
                batch.schema(),
                "TestRecord",
                AvroCodec::Null,
            )
            .unwrap();
            let mut sink: Box<dyn Sink> = Box::new(sink);
            sink.write_batch(&batch).unwrap();
            let stats = sink.finish().unwrap();
            assert_eq!(stats.rows_written, 3);
            assert!(stats.bytes_written > 0);
        }

        // Read back with apache-avro Reader
        let reader = Reader::new(Cursor::new(&buf)).unwrap();
        let records: Vec<_> = reader.map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 3);

        // Verify first record
        if let AvroValue::Record(fields) = &records[0] {
            assert_eq!(fields[0].0, "id");
            assert_eq!(fields[0].1, AvroValue::Long(1));
            assert_eq!(fields[1].0, "name");
            assert_eq!(
                fields[1].1,
                AvroValue::Union(1, Box::new(AvroValue::String("alice".into())))
            );
            assert_eq!(fields[2].0, "score");
            assert_eq!(fields[2].1, AvroValue::Double(95.5));
            assert_eq!(fields[3].0, "active");
            assert_eq!(
                fields[3].1,
                AvroValue::Union(1, Box::new(AvroValue::Boolean(true)))
            );
        } else {
            panic!("expected Record");
        }

        // Verify null handling in second record
        if let AvroValue::Record(fields) = &records[1] {
            assert_eq!(fields[1].1, AvroValue::Union(0, Box::new(AvroValue::Null)));
        } else {
            panic!("expected Record");
        }
    }

    #[test]
    fn nullable_field_all_non_null_batch() {
        // Schema declares "name" as nullable, but batch has no nulls.
        // Union wrapping must still be applied based on schema, not data.
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true), // nullable in schema
        ]));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(Int64Array::from(vec![1, 2])),
                Arc::new(StringArray::from(vec!["alice", "bob"])), // no nulls in data
            ],
        )
        .unwrap();

        let mut buf = Vec::new();
        {
            let sink = AvroSink::new(
                Cursor::new(&mut buf),
                schema,
                "NullableTest",
                AvroCodec::Null,
            )
            .unwrap();
            let mut sink: Box<dyn Sink> = Box::new(sink);
            sink.write_batch(&batch).unwrap();
            sink.finish().unwrap();
        }

        let reader = Reader::new(Cursor::new(&buf)).unwrap();
        let records: Vec<_> = reader.map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 2);

        // Non-null value in nullable field must be Union(1, String)
        if let AvroValue::Record(fields) = &records[0] {
            assert_eq!(
                fields[1].1,
                AvroValue::Union(1, Box::new(AvroValue::String("alice".into())))
            );
        } else {
            panic!("expected Record");
        }
    }

    #[test]
    fn round_trip_with_compression() {
        let batch = make_test_batch();

        for codec in [AvroCodec::Null, AvroCodec::Deflate, AvroCodec::Snappy] {
            let mut buf = Vec::new();
            {
                let sink = AvroSink::new(
                    Cursor::new(&mut buf),
                    batch.schema(),
                    "CompressedRecord",
                    codec,
                )
                .unwrap();
                let mut sink: Box<dyn Sink> = Box::new(sink);
                sink.write_batch(&batch).unwrap();
                sink.finish().unwrap();
            }

            let reader = Reader::new(Cursor::new(&buf)).unwrap();
            let records: Vec<_> = reader.map(|r| r.unwrap()).collect();
            assert_eq!(records.len(), 3, "codec {:?} failed round-trip", codec);
        }
    }

    #[test]
    fn empty_batch() {
        let schema = Arc::new(Schema::new(vec![Field::new("x", DataType::Int32, false)]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(Int32Array::from(Vec::<i32>::new()))],
        )
        .unwrap();

        let mut buf = Vec::new();
        {
            let sink = AvroSink::new(
                Cursor::new(&mut buf),
                schema,
                "EmptyRecord",
                AvroCodec::Null,
            )
            .unwrap();
            let mut sink: Box<dyn Sink> = Box::new(sink);
            sink.write_batch(&batch).unwrap();
            let stats = sink.finish().unwrap();
            assert_eq!(stats.rows_written, 0);
        }

        let reader = Reader::new(Cursor::new(&buf)).unwrap();
        let records: Vec<_> = reader.map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 0);
    }

    #[test]
    fn timestamp_conversion() {
        let schema = Arc::new(Schema::new(vec![Field::new(
            "ts",
            DataType::Timestamp(TimeUnit::Nanosecond, None),
            false,
        )]));

        let ts_nanos = 1_700_000_000_000_000_000i64; // some timestamp in nanos
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(TimestampNanosecondArray::from(vec![ts_nanos]))],
        )
        .unwrap();

        let mut buf = Vec::new();
        {
            let sink =
                AvroSink::new(Cursor::new(&mut buf), schema, "TsRecord", AvroCodec::Null).unwrap();
            let mut sink: Box<dyn Sink> = Box::new(sink);
            sink.write_batch(&batch).unwrap();
            sink.finish().unwrap();
        }

        let reader = Reader::new(Cursor::new(&buf)).unwrap();
        let records: Vec<_> = reader.map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 1);

        if let AvroValue::Record(fields) = &records[0] {
            // Should be converted to millis
            assert_eq!(fields[0].1, AvroValue::Long(ts_nanos / 1_000_000));
        } else {
            panic!("expected Record");
        }
    }

    #[test]
    fn list_array_conversion() {
        let inner = Field::new("element", DataType::Int32, false);
        let schema = Arc::new(Schema::new(vec![Field::new(
            "tags",
            DataType::List(Arc::new(inner)),
            false,
        )]));

        let values = Int32Array::from(vec![1, 2, 3, 4, 5]);
        let offsets = arrow::buffer::OffsetBuffer::new(vec![0i32, 3, 5].into());
        let list = ListArray::new(
            Arc::new(Field::new("element", DataType::Int32, false)),
            offsets,
            Arc::new(values),
            None,
        );

        let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(list)]).unwrap();

        let mut buf = Vec::new();
        {
            let sink = AvroSink::new(Cursor::new(&mut buf), schema, "ListRecord", AvroCodec::Null)
                .unwrap();
            let mut sink: Box<dyn Sink> = Box::new(sink);
            sink.write_batch(&batch).unwrap();
            sink.finish().unwrap();
        }

        let reader = Reader::new(Cursor::new(&buf)).unwrap();
        let records: Vec<_> = reader.map(|r| r.unwrap()).collect();
        assert_eq!(records.len(), 2);

        if let AvroValue::Record(fields) = &records[0] {
            assert_eq!(
                fields[0].1,
                AvroValue::Array(vec![
                    AvroValue::Int(1),
                    AvroValue::Int(2),
                    AvroValue::Int(3),
                ])
            );
        } else {
            panic!("expected Record");
        }
    }
}
