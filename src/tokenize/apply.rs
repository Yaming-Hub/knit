//! File rewriting — applies token map to produce tokenized output.

use std::path::Path;

use anyhow::{Context, Result};

use crate::tokenize::mapper::TokenMapper;
use crate::tokenize::scanner::{FileEntry, FileFormat};
use crate::tokenize::TokenizeConfig;

/// Apply tokenization to a data or dictionary file.
pub fn apply_data_file(
    entry: &FileEntry,
    root: &Path,
    out_path: &Path,
    mapper: &TokenMapper,
    config: &TokenizeConfig,
) -> Result<()> {
    let src = root.join(&entry.rel_path);

    match entry.format {
        FileFormat::Csv | FileFormat::Tsv => {
            apply_csv(&src, out_path, entry.format, mapper, config)
        }
        FileFormat::Json | FileFormat::Jsonl => {
            apply_json(&src, out_path, entry.format, mapper, config)
        }
        FileFormat::Parquet => {
            apply_parquet(&src, out_path, mapper, config)
        }
        FileFormat::Other => {
            std::fs::copy(&src, out_path)?;
            Ok(())
        }
    }
}

/// Apply tokenization to a schema JSON file (selective field replacement).
pub fn apply_schema_file(
    entry: &FileEntry,
    root: &Path,
    out_path: &Path,
    mapper: &TokenMapper,
) -> Result<()> {
    let src = root.join(&entry.rel_path);
    let content = std::fs::read_to_string(&src)
        .with_context(|| format!("reading schema {}", src.display()))?;

    let mut value: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("parsing schema JSON {}", src.display()))?;

    tokenize_schema_value(&mut value, mapper);

    let output = serde_json::to_string_pretty(&value)?;
    std::fs::write(out_path, output)?;
    Ok(())
}

fn apply_csv(
    src: &Path,
    out: &Path,
    format: FileFormat,
    mapper: &TokenMapper,
    config: &TokenizeConfig,
) -> Result<()> {
    let delimiter = if format == FileFormat::Tsv { b'\t' } else { b',' };

    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true)
        .from_path(src)
        .with_context(|| format!("opening {}", src.display()))?;

    let mut wtr = csv::WriterBuilder::new()
        .delimiter(delimiter)
        .from_path(out)
        .with_context(|| format!("creating {}", out.display()))?;

    // Write headers (tokenized if configured, respecting column filter)
    let headers = rdr.headers()?.clone();

    // Resolve original column names for filter matching.
    // During forward tokenization, headers are already original names.
    // During restore, headers may be tokenized — resolve via mapper to get originals.
    let col_flags: Vec<bool> = headers
        .iter()
        .map(|h| {
            // Try direct match first (works during forward tokenization)
            if config.should_tokenize_column(h) {
                return true;
            }
            // If no match, try resolving via mapper (works during restore
            // where headers are tokenized and mapper maps token→original)
            if let Some(original) = mapper.get(h) {
                return config.should_tokenize_column(original);
            }
            false
        })
        .collect();

    if config.tokenize_headers {
        let tokenized_headers: Vec<String> = headers
            .iter()
            .enumerate()
            .map(|(idx, h)| {
                if idx < col_flags.len() && col_flags[idx] {
                    mapper
                        .get(h)
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| h.to_string())
                } else {
                    h.to_string()
                }
            })
            .collect();
        wtr.write_record(&tokenized_headers)?;
    } else {
        wtr.write_record(&headers)?;
    }

    // Write tokenized records (respecting column filter)
    for result in rdr.records() {
        let record = result?;
        let tokenized: Vec<String> = record
            .iter()
            .enumerate()
            .map(|(idx, field)| {
                if idx < col_flags.len() && !col_flags[idx] {
                    return field.to_string();
                }
                mapper.get(field)
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| field.to_string())
            })
            .collect();
        wtr.write_record(&tokenized)?;
    }
    wtr.flush()?;
    Ok(())
}

fn apply_json(
    src: &Path,
    out: &Path,
    format: FileFormat,
    mapper: &TokenMapper,
    config: &TokenizeConfig,
) -> Result<()> {
    if format == FileFormat::Jsonl {
        apply_jsonl(src, out, mapper, config)
    } else {
        let content = std::fs::read_to_string(src)?;
        let mut value: serde_json::Value = serde_json::from_str(&content)?;
        tokenize_json_value(&mut value, mapper, config, true, false);
        let output = serde_json::to_string_pretty(&value)?;
        std::fs::write(out, output)?;
        Ok(())
    }
}

fn apply_jsonl(
    src: &Path,
    out: &Path,
    mapper: &TokenMapper,
    config: &TokenizeConfig,
) -> Result<()> {
    use std::io::{BufRead, BufReader, Write};

    let file = std::fs::File::open(src)?;
    let reader = BufReader::new(file);
    let mut writer = std::fs::File::create(out)?;

    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            writeln!(writer)?;
            continue;
        }
        let mut value: serde_json::Value = serde_json::from_str(trimmed)
            .with_context(|| format!("parsing JSONL line in {}", src.display()))?;
        tokenize_json_value(&mut value, mapper, config, true, false);
        serde_json::to_writer(&mut writer, &value)?;
        writeln!(writer)?;
    }
    Ok(())
}

/// Recursively tokenize string values, optionally keys, and optionally numbers in JSON.
/// `should_tokenize` tracks whether values in the current subtree should be tokenized.
/// `filter_applied` tracks whether the column filter has been evaluated — if true,
/// nested keys inherit the parent's decision instead of re-checking the filter.
fn tokenize_json_value(
    value: &mut serde_json::Value,
    mapper: &TokenMapper,
    config: &TokenizeConfig,
    should_tokenize: bool,
    filter_applied: bool,
) {
    match value {
        serde_json::Value::String(s) => {
            if should_tokenize {
                if let Some(token) = mapper.get(s) {
                    *s = token.to_string();
                }
            }
        }
        serde_json::Value::Number(n) => {
            if should_tokenize && config.tokenize_numbers {
                let s = n.to_string();
                if let Some(token) = mapper.get(&s) {
                    // Preserve integer vs float type with precision
                    if let Ok(i) = token.parse::<i64>() {
                        if !token.contains('.') {
                            *value = serde_json::Value::Number(i.into());
                        } else if let Some(num) = serde_json::Number::from_f64(i as f64) {
                            *value = serde_json::Value::Number(num);
                        }
                    } else if let Ok(u) = token.parse::<u64>() {
                        if !token.contains('.') {
                            *value = serde_json::Value::Number(u.into());
                        }
                    } else if let Ok(f) = token.parse::<f64>() {
                        if let Some(num) = serde_json::Number::from_f64(f) {
                            *value = serde_json::Value::Number(num);
                        }
                    }
                }
            }
        }
        serde_json::Value::Object(map) => {
            if config.tokenize_headers {
                let mut seen = std::collections::HashSet::new();
                let entries: Vec<(String, serde_json::Value)> = map
                    .into_iter()
                    .map(|(k, v)| {
                        // Check if this key's column should be tokenized
                        let should = if config.should_tokenize_column(k) {
                            true
                        } else if let Some(orig) = mapper.get(k) {
                            config.should_tokenize_column(orig)
                        } else {
                            false
                        };
                        let new_key = if config.tokenize_headers && should {
                            mapper
                                .get(k)
                                .map(|t| t.to_string())
                                .unwrap_or_else(|| k.clone())
                        } else {
                            k.clone()
                        };
                        (new_key, v.clone())
                    })
                    .collect();
                let mut deduped = serde_json::Map::new();
                for (key, val) in entries {
                    if !seen.insert(key.clone()) {
                        tracing::warn!(key = %key, "duplicate JSON key after tokenization; last value wins");
                    }
                    deduped.insert(key, val);
                }
                *map = deduped;
            }
            // Collect keys before iterating to satisfy borrow checker
            let keys: Vec<String> = map.keys().cloned().collect();
            for key in keys {
                // Apply column filter only at the first object level
                let (child_tokenize, child_filter_applied) = if !filter_applied && config.has_column_filter() {
                    let should = if config.should_tokenize_column(&key) {
                        true
                    } else if let Some(orig) = mapper.get(&key) {
                        config.should_tokenize_column(orig)
                    } else {
                        false
                    };
                    (should, true)
                } else {
                    (should_tokenize, filter_applied)
                };
                if let Some(val) = map.get_mut(&key) {
                    tokenize_json_value(val, mapper, config, child_tokenize, child_filter_applied);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                tokenize_json_value(item, mapper, config, should_tokenize, filter_applied);
            }
        }
        _ => {}
    }
}

/// Tokenize schema JSON selectively — only description/display name fields.
fn tokenize_schema_value(value: &mut serde_json::Value, mapper: &TokenMapper) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                let key_lower = key.to_lowercase();
                if key_lower == "description" || key_lower == "displayname"
                    || key_lower == "display_name" || key_lower == "tablename"
                    || key_lower == "table_name"
                {
                    if let serde_json::Value::String(s) = val {
                        if let Some(token) = mapper.get(s) {
                            *s = token.to_string();
                        }
                    }
                } else {
                    tokenize_schema_value(val, mapper);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                tokenize_schema_value(item, mapper);
            }
        }
        _ => {}
    }
}

fn apply_parquet(
    src: &Path,
    out: &Path,
    mapper: &TokenMapper,
    config: &TokenizeConfig,
) -> Result<()> {
    use arrow::array::{Array, AsArray, StringArray};
    use arrow::datatypes::{Field, Schema};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ArrowWriter;
    use std::sync::Arc;

    let file = std::fs::File::open(src)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let schema = builder.schema().clone();
    let reader = builder.build()?;

    // Build per-column tokenization flags (resolve original names via mapper for restore)
    let col_flags: Vec<bool> = schema
        .fields()
        .iter()
        .map(|f| {
            if config.should_tokenize_column(f.name()) {
                return true;
            }
            if let Some(original) = mapper.get(f.name()) {
                return config.should_tokenize_column(original);
            }
            false
        })
        .collect();

    // Optionally tokenize column names in schema (respecting column filter)
    let output_schema = if config.tokenize_headers {
        let new_fields: Vec<Arc<Field>> = schema
            .fields()
            .iter()
            .enumerate()
            .map(|(idx, f)| {
                if idx < col_flags.len() && col_flags[idx] {
                    let new_name = mapper
                        .get(f.name())
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| f.name().clone());
                    Arc::new(f.as_ref().clone().with_name(new_name))
                } else {
                    f.clone()
                }
            })
            .collect();
        Arc::new(Schema::new_with_metadata(new_fields, schema.metadata().clone()))
    } else {
        schema.clone()
    };

    // Compute date shift for native timestamp columns.
    // Use explicit override (e.g., inverse shift during restore) if set,
    // otherwise compute from seed.
    let date_shift = if let Some(shift) = config.native_date_shift {
        Some(shift)
    } else if config.tokenize_dates {
        Some(super::scanner::compute_date_shift(config.seed))
    } else {
        None
    };

    // Compute numeric shift for native numeric columns.
    // Use explicit override during restore, otherwise compute from seed.
    let numeric_shift = if let Some(shift) = config.native_numeric_shift {
        Some(shift)
    } else if config.tokenize_numbers {
        Some(super::scanner::compute_numeric_shift(config.seed))
    } else {
        None
    };

    let out_file = std::fs::File::create(out)?;
    let mut writer = ArrowWriter::try_new(out_file, output_schema.clone(), None)?;

    for batch_result in reader {
        let batch = batch_result?;
        let mut columns: Vec<Arc<dyn Array>> = Vec::with_capacity(batch.num_columns());

        for col_idx in 0..batch.num_columns() {
            let col = batch.column(col_idx);

            // Skip replacement for preserved columns
            if col_idx < col_flags.len() && !col_flags[col_idx] {
                columns.push(col.clone());
                continue;
            }

            if let Some(str_arr) = col.as_string_opt::<i32>() {
                let tokenized: StringArray = str_arr
                    .iter()
                    .map(|opt| {
                        opt.map(|val| {
                            mapper.get(val).unwrap_or(val).to_string()
                        })
                    })
                    .collect();
                columns.push(Arc::new(tokenized));
            } else if let Some(str_arr) = col.as_string_opt::<i64>() {
                // LargeString: preserve as LargeStringArray to avoid i32 offset overflow
                use arrow::array::LargeStringArray;
                let tokenized: LargeStringArray = str_arr
                    .iter()
                    .map(|opt| {
                        opt.map(|val| {
                            mapper.get(val).unwrap_or(val).to_string()
                        })
                    })
                    .collect();
                columns.push(Arc::new(tokenized));
            } else {
                // Try native temporal shifting, then numeric shifting, then pass through
                let shifted = date_shift
                    .and_then(|shift| shift_native_temporal(col, shift))
                    .or_else(|| {
                        numeric_shift
                            .and_then(|offset| shift_native_numeric(col, offset))
                    });
                columns.push(shifted.unwrap_or_else(|| col.clone()));
            }
        }

        let new_batch = RecordBatch::try_new(output_schema.clone(), columns)?;
        writer.write(&new_batch)?;
    }
    writer.close()?;
    Ok(())
}

/// Shift native Arrow temporal columns (Date32, Date64, Timestamp) by `shift_days`.
///
/// Returns `Some(shifted_array)` if the column is a supported temporal type,
/// `None` otherwise (indicating the column should be passed through unchanged).
fn shift_native_temporal(
    col: &dyn arrow::array::Array,
    shift_days: i64,
) -> Option<std::sync::Arc<dyn arrow::array::Array>> {
    use arrow::array::{
        Date32Array, Date64Array, PrimitiveArray, TimestampMicrosecondArray,
        TimestampMillisecondArray, TimestampNanosecondArray, TimestampSecondArray,
    };
    use arrow::datatypes::DataType;
    use std::sync::Arc;

    match col.data_type() {
        DataType::Date32 => {
            // Date32: days since Unix epoch
            let arr = col.as_any().downcast_ref::<Date32Array>()?;
            let shifted: Date32Array = arr
                .iter()
                .map(|opt| opt.map(|days| days.saturating_add(shift_days as i32)))
                .collect();
            Some(Arc::new(shifted))
        }
        DataType::Date64 => {
            // Date64: milliseconds since Unix epoch
            let ms_per_day = 86_400_000i64;
            let offset = shift_days.saturating_mul(ms_per_day);
            let arr = col.as_any().downcast_ref::<Date64Array>()?;
            let shifted: Date64Array = arr
                .iter()
                .map(|opt| opt.map(|ms| ms.saturating_add(offset)))
                .collect();
            Some(Arc::new(shifted))
        }
        DataType::Timestamp(unit, tz) => {
            let tz = tz.clone();
            match unit {
                arrow::datatypes::TimeUnit::Second => {
                    let offset = shift_days.saturating_mul(86_400);
                    let arr = col.as_any().downcast_ref::<TimestampSecondArray>()?;
                    let shifted: PrimitiveArray<arrow::datatypes::TimestampSecondType> = arr
                        .iter()
                        .map(|opt| opt.map(|s| s.saturating_add(offset)))
                        .collect();
                    Some(Arc::new(shifted.with_timezone_opt(tz)))
                }
                arrow::datatypes::TimeUnit::Millisecond => {
                    let offset = shift_days.saturating_mul(86_400_000);
                    let arr = col.as_any().downcast_ref::<TimestampMillisecondArray>()?;
                    let shifted: PrimitiveArray<arrow::datatypes::TimestampMillisecondType> = arr
                        .iter()
                        .map(|opt| opt.map(|ms| ms.saturating_add(offset)))
                        .collect();
                    Some(Arc::new(shifted.with_timezone_opt(tz)))
                }
                arrow::datatypes::TimeUnit::Microsecond => {
                    let offset = shift_days.saturating_mul(86_400_000_000);
                    let arr = col.as_any().downcast_ref::<TimestampMicrosecondArray>()?;
                    let shifted: PrimitiveArray<arrow::datatypes::TimestampMicrosecondType> = arr
                        .iter()
                        .map(|opt| opt.map(|us| us.saturating_add(offset)))
                        .collect();
                    Some(Arc::new(shifted.with_timezone_opt(tz)))
                }
                arrow::datatypes::TimeUnit::Nanosecond => {
                    let offset = shift_days.saturating_mul(86_400_000_000_000);
                    let arr = col.as_any().downcast_ref::<TimestampNanosecondArray>()?;
                    let shifted: PrimitiveArray<arrow::datatypes::TimestampNanosecondType> = arr
                        .iter()
                        .map(|opt| opt.map(|ns| ns.saturating_add(offset)))
                        .collect();
                    Some(Arc::new(shifted.with_timezone_opt(tz)))
                }
            }
        }
        _ => None,
    }
}

/// Shift native Arrow numeric columns for tokenization.
///
/// Integers use wrapping arithmetic (always exactly reversible).
/// Floats use additive offset (v + offset as f64/f32).
/// Returns `Some(shifted_array)` if the column is a supported numeric type.
fn shift_native_numeric(
    col: &dyn arrow::array::Array,
    offset: i64,
) -> Option<std::sync::Arc<dyn arrow::array::Array>> {
    use arrow::array::{
        Float32Array, Float64Array, Int8Array, Int16Array, Int32Array, Int64Array,
        UInt8Array, UInt16Array, UInt32Array, UInt64Array,
    };
    use arrow::datatypes::DataType;
    use std::sync::Arc;

    match col.data_type() {
        DataType::Int8 => {
            let arr = col.as_any().downcast_ref::<Int8Array>()?;
            let shifted: Int8Array = arr
                .iter()
                .map(|opt| opt.map(|v| v.wrapping_add(offset as i8)))
                .collect();
            Some(Arc::new(shifted))
        }
        DataType::Int16 => {
            let arr = col.as_any().downcast_ref::<Int16Array>()?;
            let shifted: Int16Array = arr
                .iter()
                .map(|opt| opt.map(|v| v.wrapping_add(offset as i16)))
                .collect();
            Some(Arc::new(shifted))
        }
        DataType::Int32 => {
            let arr = col.as_any().downcast_ref::<Int32Array>()?;
            let shifted: Int32Array = arr
                .iter()
                .map(|opt| opt.map(|v| v.wrapping_add(offset as i32)))
                .collect();
            Some(Arc::new(shifted))
        }
        DataType::Int64 => {
            let arr = col.as_any().downcast_ref::<Int64Array>()?;
            let shifted: Int64Array = arr
                .iter()
                .map(|opt| opt.map(|v| v.wrapping_add(offset)))
                .collect();
            Some(Arc::new(shifted))
        }
        DataType::UInt8 => {
            let arr = col.as_any().downcast_ref::<UInt8Array>()?;
            let shifted: UInt8Array = arr
                .iter()
                .map(|opt| opt.map(|v| v.wrapping_add(offset as u8)))
                .collect();
            Some(Arc::new(shifted))
        }
        DataType::UInt16 => {
            let arr = col.as_any().downcast_ref::<UInt16Array>()?;
            let shifted: UInt16Array = arr
                .iter()
                .map(|opt| opt.map(|v| v.wrapping_add(offset as u16)))
                .collect();
            Some(Arc::new(shifted))
        }
        DataType::UInt32 => {
            let arr = col.as_any().downcast_ref::<UInt32Array>()?;
            let shifted: UInt32Array = arr
                .iter()
                .map(|opt| opt.map(|v| v.wrapping_add(offset as u32)))
                .collect();
            Some(Arc::new(shifted))
        }
        DataType::UInt64 => {
            let arr = col.as_any().downcast_ref::<UInt64Array>()?;
            let shifted: UInt64Array = arr
                .iter()
                .map(|opt| opt.map(|v| v.wrapping_add(offset as u64)))
                .collect();
            Some(Arc::new(shifted))
        }
        DataType::Float32 => {
            let arr = col.as_any().downcast_ref::<Float32Array>()?;
            let shifted: Float32Array = arr
                .iter()
                .map(|opt| opt.map(|v| v + offset as f32))
                .collect();
            Some(Arc::new(shifted))
        }
        DataType::Float64 => {
            let arr = col.as_any().downcast_ref::<Float64Array>()?;
            let shifted: Float64Array = arr
                .iter()
                .map(|opt| opt.map(|v| v + offset as f64))
                .collect();
            Some(Arc::new(shifted))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array as _;
    use crate::tokenize::mapper::TokenMapper;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_apply_csv_preserves_headers() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("input.csv");
        let out = dir.path().join("output.csv");

        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "id,name,city").unwrap();
        writeln!(f, "1,Alice,Seattle").unwrap();
        writeln!(f, "2,Bob,Portland").unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");
        mapper.register("Bob");
        mapper.register("Seattle");
        mapper.register("Portland");

        let entry = FileEntry {
            rel_path: "input.csv".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Csv,
        };
        let config = TokenizeConfig::default();
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let result = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        assert_eq!(lines[0], "id,name,city"); // headers preserved
        assert!(!lines[1].contains("Alice"));
        assert!(!lines[2].contains("Bob"));
    }

    #[test]
    fn test_apply_json_data() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("data.json");
        let out = dir.path().join("output.json");

        std::fs::write(&src, r#"[{"name": "Alice", "age": 30}, {"name": "Bob", "age": 25}]"#).unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");
        mapper.register("Bob");

        let entry = FileEntry {
            rel_path: "data.json".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Json,
        };
        let config = TokenizeConfig::default();
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(!content.contains("Alice"));
        assert!(!content.contains("Bob"));
        // Numbers preserved
        assert!(content.contains("30"));
        assert!(content.contains("25"));
    }

    #[test]
    fn test_apply_schema_selective() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("schema.json");
        let out = dir.path().join("schema_out.json");

        let schema_json = r#"{
            "tableName": "Customers",
            "description": "Customer records",
            "columns": [
                {"name": "id", "dataType": "int"},
                {"name": "email", "dataType": "string", "description": "Contact email"}
            ]
        }"#;
        std::fs::write(&src, schema_json).unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Customers");
        mapper.register("Customer records");
        mapper.register("Contact email");

        let entry = FileEntry {
            rel_path: "schema.json".into(),
            kind: crate::tokenize::scanner::FileKind::Schema,
            format: FileFormat::Json,
        };
        apply_schema_file(&entry, dir.path(), &out, &mapper).unwrap();

        let content = std::fs::read_to_string(&out).unwrap();
        // Structural fields preserved
        assert!(content.contains("\"name\""));
        assert!(content.contains("\"dataType\""));
        assert!(content.contains("\"int\""));
        assert!(content.contains("\"string\""));
        // Data payloads tokenized
        assert!(!content.contains("Customers"));
        assert!(!content.contains("Customer records"));
        assert!(!content.contains("Contact email"));
    }

    #[test]
    fn test_apply_csv_tokenizes_headers() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("input.csv");
        let out = dir.path().join("output.csv");

        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "id,name,city").unwrap();
        writeln!(f, "1,Alice,Seattle").unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("name");
        mapper.register("city");
        mapper.register("Alice");
        mapper.register("Seattle");

        let entry = FileEntry {
            rel_path: "input.csv".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Csv,
        };
        let config = TokenizeConfig {
            tokenize_headers: true,
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let result = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        // Headers should be tokenized
        assert!(!lines[0].contains("name"));
        assert!(!lines[0].contains("city"));
        // "id" is too short to be tokenized, so it stays
        assert!(lines[0].contains("id"));
        // Values also tokenized
        assert!(!lines[1].contains("Alice"));
        assert!(!lines[1].contains("Seattle"));
    }

    #[test]
    fn test_apply_json_tokenizes_keys() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("data.json");
        let out = dir.path().join("output.json");

        std::fs::write(&src, r#"{"username": "Alice", "age": 30}"#).unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("username");
        mapper.register("Alice");

        let entry = FileEntry {
            rel_path: "data.json".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Json,
        };
        let config = TokenizeConfig {
            tokenize_headers: true,
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let content = std::fs::read_to_string(&out).unwrap();
        // Key and value should be tokenized
        assert!(!content.contains("username"));
        assert!(!content.contains("Alice"));
        // Number preserved
        assert!(content.contains("30"));
    }

    #[test]
    fn test_apply_jsonl_tokenizes_keys() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("data.jsonl");
        let out = dir.path().join("output.jsonl");

        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, r#"{{"username":"Alice","score":10}}"#).unwrap();
        writeln!(f, r#"{{"username":"Bob","score":20}}"#).unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("username");
        mapper.register("Alice");
        mapper.register("Bob");

        let entry = FileEntry {
            rel_path: "data.jsonl".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Jsonl,
        };
        let config = TokenizeConfig {
            tokenize_headers: true,
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(!content.contains("username"));
        assert!(!content.contains("Alice"));
        assert!(!content.contains("Bob"));
    }

    #[test]
    fn test_apply_parquet_tokenizes_columns() {
        use arrow::array::StringArray;
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let src = dir.path().join("input.parquet");
        let out = dir.path().join("output.parquet");

        // Create a parquet file with known column names
        let schema = Arc::new(Schema::new(vec![
            Field::new("username", DataType::Utf8, false),
            Field::new("city", DataType::Utf8, false),
        ]));
        let names = StringArray::from(vec!["Alice", "Bob"]);
        let cities = StringArray::from(vec!["Seattle", "Portland"]);
        let batch = RecordBatch::try_new(schema.clone(), vec![Arc::new(names), Arc::new(cities)]).unwrap();
        let file = std::fs::File::create(&src).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("username");
        mapper.register("city");
        mapper.register("Alice");
        mapper.register("Bob");
        mapper.register("Seattle");
        mapper.register("Portland");

        let entry = FileEntry {
            rel_path: "input.parquet".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Parquet,
        };
        let config = TokenizeConfig {
            tokenize_headers: true,
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        // Read back and verify column names are tokenized
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        let file = std::fs::File::open(&out).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(file).unwrap().build().unwrap();
        let batches: Vec<_> = reader.into_iter().collect::<Result<Vec<_>, _>>().unwrap();
        let out_schema = batches[0].schema();
        // Column names should NOT be the originals
        assert_ne!(out_schema.field(0).name(), "username");
        assert_ne!(out_schema.field(1).name(), "city");
        // Values should also be tokenized
        let col0 = batches[0].column(0).as_any().downcast_ref::<StringArray>().unwrap();
        assert_ne!(col0.value(0), "Alice");
        assert_ne!(col0.value(1), "Bob");
    }

    #[test]
    fn test_apply_csv_tokenizes_numbers() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("input.csv");
        let out = dir.path().join("output.csv");

        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "id,name,score").unwrap();
        writeln!(f, "1,Alice,42.5").unwrap();
        writeln!(f, "2,Bob,-100").unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");
        mapper.register("Bob");
        mapper.register("42.5");
        mapper.register("-100");

        let entry = FileEntry {
            rel_path: "input.csv".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Csv,
        };
        let config = TokenizeConfig {
            tokenize_numbers: true,
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let result = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        // Numbers should be tokenized
        assert!(!lines[1].contains("42.5"));
        assert!(!lines[2].contains("-100"));
        // Strings also tokenized
        assert!(!lines[1].contains("Alice"));
    }

    #[test]
    fn test_apply_json_tokenizes_numbers() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("data.json");
        let out = dir.path().join("output.json");

        std::fs::write(&src, r#"{"name": "Alice", "age": 30, "score": 99.5}"#).unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");
        mapper.register("30");
        mapper.register("99.5");

        let entry = FileEntry {
            rel_path: "data.json".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Json,
        };
        let config = TokenizeConfig {
            tokenize_numbers: true,
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let content = std::fs::read_to_string(&out).unwrap();
        assert!(!content.contains("Alice"));
        // JSON numeric scalars should be replaced
        let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
        let age = parsed.get("age").unwrap().as_f64().unwrap();
        assert_ne!(age, 30.0);
        let score = parsed.get("score").unwrap().as_f64().unwrap();
        assert_ne!(score, 99.5);
    }

    #[test]
    fn test_csv_numbers_preserved_without_flag() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("input.csv");
        let out = dir.path().join("output.csv");

        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "id,name,score").unwrap();
        writeln!(f, "1,Alice,42.5").unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");

        let entry = FileEntry {
            rel_path: "input.csv".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Csv,
        };
        let config = TokenizeConfig::default(); // tokenize_numbers = false
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let result = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        // Numbers should NOT be tokenized (flag off)
        assert!(lines[1].contains("42.5"));
        // But strings should be
        assert!(!lines[1].contains("Alice"));
    }

    #[test]
    fn test_apply_csv_with_shifted_dates() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("input.csv");
        let out = dir.path().join("output.csv");

        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "name,event_date").unwrap();
        writeln!(f, "Alice,2024-01-15").unwrap();
        writeln!(f, "Bob,2024-03-20").unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");
        mapper.register("Bob");
        // Register shifted dates (shift = +100 days for this test)
        mapper.register_with_value("2024-01-15", "2024-04-24");
        mapper.register_with_value("2024-03-20", "2024-06-28");

        let entry = FileEntry {
            rel_path: "input.csv".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Csv,
        };
        let config = TokenizeConfig {
            tokenize_dates: true,
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let result = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        // Dates should be replaced with shifted values
        assert!(lines[1].contains("2024-04-24"));
        assert!(lines[2].contains("2024-06-28"));
        // Original dates should not appear
        assert!(!lines[1].contains("2024-01-15"));
        assert!(!lines[2].contains("2024-03-20"));
    }

    #[test]
    fn test_dates_preserved_without_flag() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("input.csv");
        let out = dir.path().join("output.csv");

        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "name,date").unwrap();
        writeln!(f, "Alice,2024-01-15").unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");
        // Don't register date — it should be preserved

        let entry = FileEntry {
            rel_path: "input.csv".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Csv,
        };
        let config = TokenizeConfig::default(); // tokenize_dates = false
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let result = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        // Date should be preserved (not in mapper)
        assert!(lines[1].contains("2024-01-15"));
    }

    #[test]
    fn test_apply_csv_tokenize_columns_whitelist() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("input.csv");
        let out = dir.path().join("output.csv");

        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "name,city,country").unwrap();
        writeln!(f, "Alice,Seattle,USA").unwrap();
        writeln!(f, "Bob,Portland,Canada").unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");
        mapper.register("Bob");
        // city and country values NOT registered because only "name" is whitelisted

        let entry = FileEntry {
            rel_path: "input.csv".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Csv,
        };
        let config = TokenizeConfig {
            tokenize_columns: Some(["name".to_string()].into_iter().collect()),
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let result = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        // Name should be tokenized
        assert!(!lines[1].contains("Alice"));
        assert!(!lines[2].contains("Bob"));
        // City and country should be preserved
        assert!(lines[1].contains("Seattle"));
        assert!(lines[2].contains("Portland"));
        assert!(lines[1].contains("USA"));
        assert!(lines[2].contains("Canada"));
    }

    #[test]
    fn test_apply_csv_preserve_columns_blacklist() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("input.csv");
        let out = dir.path().join("output.csv");

        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "name,city,country").unwrap();
        writeln!(f, "Alice,Seattle,USA").unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");
        mapper.register("Seattle");
        // country NOT registered because it's preserved

        let entry = FileEntry {
            rel_path: "input.csv".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Csv,
        };
        let config = TokenizeConfig {
            preserve_columns: Some(["country".to_string()].into_iter().collect()),
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let result = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        // Name and city should be tokenized
        assert!(!lines[1].contains("Alice"));
        assert!(!lines[1].contains("Seattle"));
        // Country should be preserved
        assert!(lines[1].contains("USA"));
    }

    #[test]
    fn test_apply_csv_headers_respect_column_filter() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("input.csv");
        let out = dir.path().join("output.csv");

        let mut f = std::fs::File::create(&src).unwrap();
        writeln!(f, "name,city").unwrap();
        writeln!(f, "Alice,Seattle").unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("name");
        mapper.register("Alice");
        // city header NOT registered because it's preserved

        let entry = FileEntry {
            rel_path: "input.csv".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Csv,
        };
        let config = TokenizeConfig {
            tokenize_headers: true,
            tokenize_columns: Some(["name".to_string()].into_iter().collect()),
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let result = std::fs::read_to_string(&out).unwrap();
        let lines: Vec<&str> = result.lines().collect();
        // "name" header should be tokenized
        assert!(!lines[0].contains("name"));
        // "city" header should be preserved (column not in whitelist)
        assert!(lines[0].contains("city"));
    }

    #[test]
    fn test_apply_json_preserve_columns() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("data.json");
        let out = dir.path().join("output.json");

        std::fs::write(
            &src,
            r#"[{"name": "Alice", "country": "USA"}, {"name": "Bob", "country": "UK"}]"#,
        )
        .unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");
        mapper.register("Bob");
        // country values NOT registered because preserved

        let entry = FileEntry {
            rel_path: "data.json".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Json,
        };
        let config = TokenizeConfig {
            preserve_columns: Some(["country".to_string()].into_iter().collect()),
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let content = std::fs::read_to_string(&out).unwrap();
        // name values should be tokenized
        assert!(!content.contains("Alice"));
        assert!(!content.contains("Bob"));
        // country values should be preserved
        assert!(content.contains("USA"));
        assert!(content.contains("UK"));
    }

    #[test]
    fn test_apply_json_nested_subtree_preserved() {
        let dir = TempDir::new().unwrap();
        let src = dir.path().join("data.json");
        let out = dir.path().join("output.json");

        std::fs::write(
            &src,
            r#"{"user": {"first": "Alice", "last": "Smith"}, "email": "alice@test.com"}"#,
        )
        .unwrap();

        let mut mapper = TokenMapper::new(42);
        // Register ALL values — the filter should prevent user subtree from being replaced
        mapper.register("Alice");
        mapper.register("Smith");
        mapper.register("alice@test.com");

        let entry = FileEntry {
            rel_path: "data.json".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Json,
        };
        let config = TokenizeConfig {
            preserve_columns: Some(["user".to_string()].into_iter().collect()),
            ..Default::default()
        };
        apply_data_file(&entry, dir.path(), &out, &mapper, &config).unwrap();

        let content = std::fs::read_to_string(&out).unwrap();
        // user subtree should be preserved (even though values are in mapper)
        assert!(content.contains("Alice"));
        assert!(content.contains("Smith"));
        // email should be tokenized
        assert!(!content.contains("alice@test.com"));
    }

    #[test]
    fn test_shift_native_date32() {
        use arrow::array::{Array, Date32Array};
        let arr = Date32Array::from(vec![Some(19000), Some(19100), None]);
        let shifted = shift_native_temporal(&arr, 10).unwrap();
        let result = shifted
            .as_any()
            .downcast_ref::<Date32Array>()
            .unwrap();
        assert_eq!(result.value(0), 19010);
        assert_eq!(result.value(1), 19110);
        assert!(result.is_null(2));
    }

    #[test]
    fn test_shift_native_date64() {
        use arrow::array::{Array, Date64Array};
        let ms_per_day = 86_400_000i64;
        let arr = Date64Array::from(vec![Some(1_000 * ms_per_day), None]);
        let shifted = shift_native_temporal(&arr, -5).unwrap();
        let result = shifted
            .as_any()
            .downcast_ref::<Date64Array>()
            .unwrap();
        assert_eq!(result.value(0), 995 * ms_per_day);
        assert!(result.is_null(1));
    }

    #[test]
    fn test_shift_native_timestamp_us() {
        use arrow::array::TimestampMicrosecondArray;
        let us_per_day = 86_400_000_000i64;
        let arr = TimestampMicrosecondArray::from(vec![Some(100 * us_per_day), Some(200 * us_per_day)]);
        let shifted = shift_native_temporal(&arr, 7).unwrap();
        let result = shifted
            .as_any()
            .downcast_ref::<TimestampMicrosecondArray>()
            .unwrap();
        assert_eq!(result.value(0), 107 * us_per_day);
        assert_eq!(result.value(1), 207 * us_per_day);
    }

    #[test]
    fn test_shift_non_temporal_returns_none() {
        use arrow::array::Int32Array;
        let arr = Int32Array::from(vec![1, 2, 3]);
        assert!(shift_native_temporal(&arr, 10).is_none());
    }

    #[test]
    fn test_apply_parquet_native_timestamps() {
        use arrow::array::{Array, Date32Array, StringArray, TimestampMicrosecondArray};
        use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let src = dir.path().join("input.parquet");

        // Create a Parquet file with string, Date32, and Timestamp columns
        let schema = Arc::new(Schema::new(vec![
            Field::new("name", DataType::Utf8, false),
            Field::new("birth_date", DataType::Date32, true),
            Field::new("created_at", DataType::Timestamp(TimeUnit::Microsecond, None), true),
        ]));

        let names = StringArray::from(vec!["Alice", "Bob"]);
        let dates = Date32Array::from(vec![Some(19000), Some(19100)]);
        let us_per_day = 86_400_000_000i64;
        let timestamps = TimestampMicrosecondArray::from(vec![
            Some(100 * us_per_day),
            Some(200 * us_per_day),
        ]);

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![Arc::new(names), Arc::new(dates), Arc::new(timestamps)],
        )
        .unwrap();

        let file = std::fs::File::create(&src).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        // Tokenize with date shifting enabled
        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");
        mapper.register("Bob");

        let config = TokenizeConfig {
            tokenize_dates: true,
            seed: 42,
            ..TokenizeConfig::default()
        };

        let entry = FileEntry {
            rel_path: "input.parquet".into(),
            kind: crate::tokenize::scanner::FileKind::Data,
            format: FileFormat::Parquet,
        };

        let out_file = dir.path().join("output.parquet");
        apply_data_file(&entry, dir.path(), &out_file, &mapper, &config).unwrap();

        // Read output and verify dates were shifted
        let file = std::fs::File::open(&out_file).unwrap();
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();

        let shift = super::super::scanner::compute_date_shift(42);

        for batch in reader {
            let batch = batch.unwrap();
            let date_col = batch
                .column(1)
                .as_any()
                .downcast_ref::<Date32Array>()
                .unwrap();
            // Dates should be shifted
            assert_eq!(date_col.value(0), 19000 + shift as i32);
            assert_eq!(date_col.value(1), 19100 + shift as i32);

            let ts_col = batch
                .column(2)
                .as_any()
                .downcast_ref::<TimestampMicrosecondArray>()
                .unwrap();
            assert_eq!(ts_col.value(0), 100 * us_per_day + shift * 86_400_000_000);
            assert_eq!(ts_col.value(1), 200 * us_per_day + shift * 86_400_000_000);

            // String column should be tokenized (not original values)
            let name_col = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            assert_ne!(name_col.value(0), "Alice");
            assert_ne!(name_col.value(1), "Bob");
        }
    }

    #[test]
    fn test_shift_native_int32() {
        use arrow::array::Int32Array;
        let arr = Int32Array::from(vec![Some(100), None, Some(-50)]);
        let shifted = shift_native_numeric(&arr, 42).unwrap();
        let result = shifted.as_any().downcast_ref::<Int32Array>().unwrap();
        assert_eq!(result.value(0), 142);
        assert!(result.is_null(1));
        assert_eq!(result.value(2), -8);
    }

    #[test]
    fn test_shift_native_int64() {
        use arrow::array::Int64Array;
        let arr = Int64Array::from(vec![Some(1000), Some(-500)]);
        let shifted = shift_native_numeric(&arr, -200).unwrap();
        let result = shifted.as_any().downcast_ref::<Int64Array>().unwrap();
        assert_eq!(result.value(0), 800);
        assert_eq!(result.value(1), -700);
    }

    #[test]
    fn test_shift_native_uint32() {
        use arrow::array::UInt32Array;
        let arr = UInt32Array::from(vec![Some(100), Some(0)]);
        let shifted = shift_native_numeric(&arr, 50).unwrap();
        let result = shifted.as_any().downcast_ref::<UInt32Array>().unwrap();
        assert_eq!(result.value(0), 150);
        assert_eq!(result.value(1), 50);
    }

    #[test]
    fn test_shift_native_float64() {
        use arrow::array::Float64Array;
        let arr = Float64Array::from(vec![Some(100.0), Some(-50.0), None]);
        let shifted = shift_native_numeric(&arr, 42).unwrap();
        let result = shifted.as_any().downcast_ref::<Float64Array>().unwrap();
        assert!((result.value(0) - 142.0).abs() < 0.001);
        assert!((result.value(1) - (-8.0)).abs() < 0.001);
        assert!(result.is_null(2));
    }

    #[test]
    fn test_shift_native_numeric_passthrough_string() {
        use arrow::array::StringArray;
        let arr = StringArray::from(vec!["hello", "world"]);
        assert!(shift_native_numeric(&arr, 42).is_none());
    }

    #[test]
    fn test_shift_native_numeric_wrapping() {
        use arrow::array::Int8Array;
        // Wrapping: 120 + 42 wraps around in i8
        let arr = Int8Array::from(vec![Some(120i8)]);
        let shifted = shift_native_numeric(&arr, 42).unwrap();
        let result = shifted.as_any().downcast_ref::<Int8Array>().unwrap();
        let expected = 120i8.wrapping_add(42);
        assert_eq!(result.value(0), expected);
        // Verify roundtrip: shift back by -42
        let restored = shift_native_numeric(&*shifted, -42).unwrap();
        let result2 = restored.as_any().downcast_ref::<Int8Array>().unwrap();
        assert_eq!(result2.value(0), 120);
    }

    #[test]
    fn test_shift_native_uint_wrapping_roundtrip() {
        use arrow::array::UInt8Array;
        let arr = UInt8Array::from(vec![Some(10u8), Some(250u8)]);
        let shifted = shift_native_numeric(&arr, 100).unwrap();
        // Verify roundtrip
        let restored = shift_native_numeric(&*shifted, -100).unwrap();
        let result = restored.as_any().downcast_ref::<UInt8Array>().unwrap();
        assert_eq!(result.value(0), 10);
        assert_eq!(result.value(1), 250);
    }

    #[test]
    fn test_apply_parquet_native_numeric_shift() {
        use arrow::array::{Int32Array, Float64Array, StringArray};
        use arrow::datatypes::{Field, Schema};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let input = dir.path().join("data.parquet");
        let output = dir.path().join("tokenized.parquet");

        let schema = Arc::new(Schema::new(vec![
            Field::new("name", arrow::datatypes::DataType::Utf8, false),
            Field::new("score", arrow::datatypes::DataType::Int32, false),
            Field::new("value", arrow::datatypes::DataType::Float64, false),
        ]));

        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec!["Alice", "Bob"])),
                Arc::new(Int32Array::from(vec![100, 200])),
                Arc::new(Float64Array::from(vec![1.5, 2.5])),
            ],
        ).unwrap();

        let file = std::fs::File::create(&input).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        let mut mapper = TokenMapper::new(42);
        mapper.register("Alice");
        mapper.register("Bob");

        let config = TokenizeConfig {
            tokenize_numbers: true,
            seed: 42,
            ..TokenizeConfig::default()
        };
        apply_parquet(&input, &output, &mapper, &config).unwrap();

        let file = std::fs::File::open(&output).unwrap();
        let reader = ParquetRecordBatchReaderBuilder::try_new(file).unwrap().build().unwrap();

        let offset = super::super::scanner::compute_numeric_shift(42);

        for batch in reader {
            let batch = batch.unwrap();
            let score_col = batch
                .column(1)
                .as_any()
                .downcast_ref::<Int32Array>()
                .unwrap();
            assert_eq!(score_col.value(0), (100i32).wrapping_add(offset as i32));
            assert_eq!(score_col.value(1), (200i32).wrapping_add(offset as i32));

            let value_col = batch
                .column(2)
                .as_any()
                .downcast_ref::<Float64Array>()
                .unwrap();
            assert!((value_col.value(0) - (1.5 + offset as f64)).abs() < 0.001);
            assert!((value_col.value(1) - (2.5 + offset as f64)).abs() < 0.001);

            let name_col = batch
                .column(0)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            assert_ne!(name_col.value(0), "Alice");
            assert_ne!(name_col.value(1), "Bob");
        }
    }
}