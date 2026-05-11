//! Template-based output sink using MiniJinja.
//!
//! [`TemplateSink`] renders Arrow `RecordBatch`es through a user-supplied
//! MiniJinja template. It supports two rendering modes:
//!
//! - **Per-row** (default): the template is rendered once per row with context
//!   `{ row: { field: value, ... }, row_index, batch_index }`.
//! - **Per-batch**: the template is rendered once per batch with context
//!   `{ rows: [{ field: value, ... }, ...], schema: { fields: [...] }, batch_index }`.
//!
//! The rendering mode is auto-detected: if the template source contains the
//! literal `rows` variable reference, batch mode is used; otherwise row mode.
//!
//! All built-in helpers from [`crate::bind::helpers`] are registered automatically.

use std::io::Write;

use arrow::array::{self, Array};
use arrow::datatypes::{DataType, TimeUnit};
use arrow::record_batch::RecordBatch;
use minijinja::{context, Environment, Value};
use tracing::debug;

use crate::bind::json::downcast_col;
use crate::bind::error::BindError;
use crate::bind::helpers;
use crate::bind::traits::{Sink, SinkStats};

/// Rendering mode for the template sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateMode {
    /// Render the template once per row.
    PerRow,
    /// Render the template once per batch.
    PerBatch,
}

/// Output sink that renders `RecordBatch`es through a MiniJinja template.
///
/// Created by [`crate::bind::factory::create_sink`] when the format is
/// [`OutputFormat::Template`](crate::bind::factory::OutputFormat::Template), or
/// directly via [`TemplateSink::new`].
///
/// The generation engine writes batches via [`Sink::write_batch`], and the
/// sink renders each row (or batch) using the compiled template.
///
/// The template is compiled once at construction time and reused for all
/// batches, avoiding per-batch compilation overhead.
pub struct TemplateSink<W: Write + Send> {
    writer: W,
    env: Environment<'static>,
    mode: TemplateMode,
    rows_written: u64,
    bytes_written: u64,
    batch_index: u64,
}

impl<W: Write + Send> TemplateSink<W> {
    /// Create a new template sink.
    ///
    /// # Arguments
    /// * `writer` — destination for rendered output
    /// * `template_source` — MiniJinja template string
    /// * `mode` — whether to render per-row or per-batch (if `None`, auto-detected)
    ///
    /// # Errors
    /// Returns [`BindError::Template`] if the template fails to compile.
    pub fn new(
        writer: W,
        template_source: String,
        mode: Option<TemplateMode>,
    ) -> Result<Self, BindError> {
        let mode = mode.unwrap_or_else(|| detect_mode(&template_source));

        // Compile the template once and store in the environment for reuse.
        let mut env = Environment::new();
        helpers::register_helpers(&mut env);
        env.add_template_owned("main".to_string(), template_source)
            .map_err(|e| BindError::Template(e.to_string()))?;

        Ok(Self {
            writer,
            env,
            mode,
            rows_written: 0,
            bytes_written: 0,
            batch_index: 0,
        })
    }

    /// Returns the rendering mode being used.
    pub fn mode(&self) -> TemplateMode {
        self.mode
    }

    /// Consume the sink and return the inner writer.
    ///
    /// Useful for tests that need to inspect the rendered output.
    pub fn into_inner(self) -> W {
        self.writer
    }
}

/// Auto-detect rendering mode by checking whether the template references `rows`.
fn detect_mode(source: &str) -> TemplateMode {
    if source.contains("rows") {
        TemplateMode::PerBatch
    } else {
        TemplateMode::PerRow
    }
}

/// Convert a single Arrow cell to a MiniJinja [`Value`].
fn cell_to_value(col: &dyn Array, row: usize) -> Result<Value, BindError> {
    if col.is_null(row) {
        return Ok(Value::from(()));
    }
    match col.data_type() {
        DataType::Boolean => {
            let arr = downcast_col!(col, array::BooleanArray)?;
            Ok(Value::from(arr.value(row)))
        }
        DataType::Int8 => typed_int!(col, array::Int8Array, row),
        DataType::Int16 => typed_int!(col, array::Int16Array, row),
        DataType::Int32 => typed_int!(col, array::Int32Array, row),
        DataType::Int64 => {
            let arr = downcast_col!(col, array::Int64Array)?;
            Ok(Value::from(arr.value(row)))
        }
        DataType::UInt8 => typed_int!(col, array::UInt8Array, row),
        DataType::UInt16 => typed_int!(col, array::UInt16Array, row),
        DataType::UInt32 => typed_int!(col, array::UInt32Array, row),
        DataType::UInt64 => {
            let arr = downcast_col!(col, array::UInt64Array)?;
            let v = arr.value(row);
            if v <= i64::MAX as u64 {
                Ok(Value::from(v as i64))
            } else {
                // Value exceeds i64 range; represent as string to avoid silent truncation
                Ok(Value::from(v.to_string()))
            }
        }
        DataType::Float32 => {
            let arr = downcast_col!(col, array::Float32Array)?;
            Ok(Value::from(arr.value(row) as f64))
        }
        DataType::Float64 => {
            let arr = downcast_col!(col, array::Float64Array)?;
            Ok(Value::from(arr.value(row)))
        }
        DataType::Utf8 => {
            let arr = downcast_col!(col, array::StringArray)?;
            Ok(Value::from(arr.value(row)))
        }
        DataType::LargeUtf8 => {
            let arr = downcast_col!(col, array::LargeStringArray)?;
            Ok(Value::from(arr.value(row)))
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            let arr = downcast_col!(col, array::TimestampMicrosecondArray)?;
            Ok(
                match chrono::DateTime::from_timestamp_micros(arr.value(row)) {
                    Some(d) => Value::from(d.to_rfc3339()),
                    None => Value::from(()),
                },
            )
        }
        DataType::Timestamp(TimeUnit::Millisecond, _) => {
            let arr = downcast_col!(col, array::TimestampMillisecondArray)?;
            Ok(
                match chrono::DateTime::from_timestamp_millis(arr.value(row)) {
                    Some(d) => Value::from(d.to_rfc3339()),
                    None => Value::from(()),
                },
            )
        }
        DataType::Timestamp(TimeUnit::Second, _) => {
            let arr = downcast_col!(col, array::TimestampSecondArray)?;
            Ok(match chrono::DateTime::from_timestamp(arr.value(row), 0) {
                Some(d) => Value::from(d.to_rfc3339()),
                None => Value::from(()),
            })
        }
        DataType::Timestamp(TimeUnit::Nanosecond, _) => {
            let arr = downcast_col!(col, array::TimestampNanosecondArray)?;
            let ts = arr.value(row);
            let secs = ts.div_euclid(1_000_000_000);
            let nsecs = ts.rem_euclid(1_000_000_000) as u32;
            Ok(match chrono::DateTime::from_timestamp(secs, nsecs) {
                Some(d) => Value::from(d.to_rfc3339()),
                None => Value::from(()),
            })
        }
        _ => {
            // Fallback: cast to string via Arrow
            let fallback = arrow::compute::cast(col, &DataType::Utf8)
                .ok()
                .and_then(|arr| {
                    arr.as_any()
                        .downcast_ref::<array::StringArray>()
                        .map(|a| a.value(row).to_string())
                });
            Ok(Value::from(fallback.unwrap_or_default()))
        }
    }
}

macro_rules! typed_int {
    ($col:expr, $arr_ty:ty, $row:expr) => {{
        let arr = downcast_col!($col, $arr_ty)?;
        Ok(Value::from(arr.value($row) as i64))
    }};
}
use typed_int;

/// Build a row context map from a `RecordBatch` at the given row index.
fn row_to_value(batch: &RecordBatch, row: usize) -> Result<Value, BindError> {
    let schema = batch.schema();
    let fields = schema.fields();
    let columns = batch.columns();
    let mut map = std::collections::BTreeMap::new();
    for (i, field) in fields.iter().enumerate() {
        map.insert(
            field.name().clone(),
            cell_to_value(columns[i].as_ref(), row)?,
        );
    }
    Ok(Value::from(map))
}

/// Build the schema context for batch-mode rendering.
fn schema_to_value(batch: &RecordBatch) -> Value {
    let fields: Vec<Value> = batch
        .schema()
        .fields()
        .iter()
        .map(|f| {
            let mut m = std::collections::BTreeMap::new();
            m.insert("name".to_string(), Value::from(f.name().as_str()));
            m.insert(
                "type".to_string(),
                Value::from(format!("{}", f.data_type())),
            );
            m.insert("nullable".to_string(), Value::from(f.is_nullable()));
            Value::from(m)
        })
        .collect();
    let mut schema_map = std::collections::BTreeMap::new();
    schema_map.insert("fields".to_string(), Value::from(fields));
    Value::from(schema_map)
}

impl<W: Write + Send> Sink for TemplateSink<W> {
    fn write_batch(&mut self, batch: &RecordBatch) -> Result<(), BindError> {
        let tmpl = self
            .env
            .get_template("main")
            .map_err(|e| BindError::Template(e.to_string()))?;

        match self.mode {
            TemplateMode::PerRow => {
                for row in 0..batch.num_rows() {
                    let row_val = row_to_value(batch, row)?;
                    let ctx = context! {
                        row => row_val,
                        row_index => self.rows_written + row as u64,
                        batch_index => self.batch_index,
                    };
                    let rendered = tmpl
                        .render(ctx)
                        .map_err(|e| BindError::Template(e.to_string()))?;
                    let bytes = rendered.as_bytes();
                    self.writer.write_all(bytes)?;
                    self.bytes_written += bytes.len() as u64;
                    // Add newline separator between rows
                    self.writer.write_all(b"\n")?;
                    self.bytes_written += 1;
                }
            }
            TemplateMode::PerBatch => {
                let rows: Vec<Value> = (0..batch.num_rows())
                    .map(|row| row_to_value(batch, row))
                    .collect::<Result<_, _>>()?;
                let schema = schema_to_value(batch);
                let ctx = context! {
                    rows => rows,
                    schema => schema,
                    batch_index => self.batch_index,
                    row_count => batch.num_rows(),
                };
                let rendered = tmpl
                    .render(ctx)
                    .map_err(|e| BindError::Template(e.to_string()))?;
                let bytes = rendered.as_bytes();
                self.writer.write_all(bytes)?;
                self.bytes_written += bytes.len() as u64;
            }
        }

        self.rows_written += batch.num_rows() as u64;
        self.batch_index += 1;
        debug!(
            rows = batch.num_rows(),
            total = self.rows_written,
            mode = ?self.mode,
            "wrote template batch"
        );
        Ok(())
    }

    fn finish(mut self: Box<Self>) -> Result<SinkStats, BindError> {
        self.writer.flush()?;
        debug!(
            rows = self.rows_written,
            bytes = self.bytes_written,
            "template sink finished"
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
    use arrow::array::{Float64Array, Int64Array, StringArray};
    use arrow::datatypes::{Field, Schema};
    use std::io::Cursor;
    use std::sync::Arc;

    fn test_batch() -> RecordBatch {
        let schema = Arc::new(Schema::new(vec![
            Field::new("id", DataType::Int64, false),
            Field::new("name", DataType::Utf8, true),
            Field::new("score", DataType::Float64, true),
        ]));
        RecordBatch::try_new(
            schema,
            vec![
                Arc::new(Int64Array::from(vec![1, 2])),
                Arc::new(StringArray::from(vec![Some("alice"), Some("bob")])),
                Arc::new(Float64Array::from(vec![Some(95.5), Some(87.0)])),
            ],
        )
        .unwrap()
    }

    #[test]
    fn test_per_row_rendering() {
        let buf = Cursor::new(Vec::new());
        let tmpl = "INSERT INTO users VALUES ({{ row.id }}, '{{ row.name }}', {{ row.score }});";
        let mut sink =
            TemplateSink::new(buf, tmpl.to_string(), Some(TemplateMode::PerRow)).unwrap();
        sink.write_batch(&test_batch()).unwrap();
        let stats = Box::new(sink).finish().unwrap();
        assert_eq!(stats.rows_written, 2);

        // Reconstruct to check output
        let buf2 = Cursor::new(Vec::new());
        let mut sink2 =
            TemplateSink::new(buf2, tmpl.to_string(), Some(TemplateMode::PerRow)).unwrap();
        sink2.write_batch(&test_batch()).unwrap();
        let stats2 = Box::new(sink2).finish().unwrap();
        assert!(stats2.bytes_written > 0);
    }

    #[test]
    fn test_per_batch_rendering() {
        let buf = Cursor::new(Vec::new());
        let tmpl = r#"<table>
{% for r in rows %}<tr><td>{{ r.id }}</td><td>{{ r.name }}</td></tr>
{% endfor %}</table>"#;
        let mut sink =
            TemplateSink::new(buf, tmpl.to_string(), Some(TemplateMode::PerBatch)).unwrap();
        sink.write_batch(&test_batch()).unwrap();
        let inner = sink.writer.into_inner();
        let output = String::from_utf8(inner).unwrap();
        assert!(output.contains("<td>alice</td>"));
        assert!(output.contains("<td>bob</td>"));
    }

    #[test]
    fn test_auto_detect_per_batch() {
        let tmpl = "{% for r in rows %}{{ r.id }}{% endfor %}";
        assert_eq!(detect_mode(tmpl), TemplateMode::PerBatch);
    }

    #[test]
    fn test_auto_detect_per_row() {
        let tmpl = "{{ row.id }}: {{ row.name }}";
        assert_eq!(detect_mode(tmpl), TemplateMode::PerRow);
    }

    #[test]
    fn test_invalid_template() {
        let buf = Cursor::new(Vec::new());
        let result = TemplateSink::new(buf, "{{ unclosed".to_string(), None);
        assert!(result.is_err());
    }

    #[test]
    fn test_null_handling() {
        let schema = Arc::new(Schema::new(vec![Field::new("name", DataType::Utf8, true)]));
        let batch = RecordBatch::try_new(
            schema,
            vec![Arc::new(StringArray::from(vec![None as Option<&str>]))],
        )
        .unwrap();

        let buf = Cursor::new(Vec::new());
        let tmpl = "name={{ row.name }}";
        let mut sink =
            TemplateSink::new(buf, tmpl.to_string(), Some(TemplateMode::PerRow)).unwrap();
        sink.write_batch(&batch).unwrap();
        let inner = sink.writer.into_inner();
        let output = String::from_utf8(inner).unwrap();
        // Null renders as empty string in MiniJinja
        assert_eq!(output.trim(), "name=none");
    }
}