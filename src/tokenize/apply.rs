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
    let col_flags: Vec<bool> = headers
        .iter()
        .map(|h| config.should_tokenize_column(h))
        .collect();

    if config.tokenize_headers {
        let tokenized_headers: Vec<String> = headers
            .iter()
            .map(|h| {
                if config.should_tokenize_header(h) {
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
        tokenize_json_value(&mut value, mapper, config, true);
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
        tokenize_json_value(&mut value, mapper, config, true);
        serde_json::to_writer(&mut writer, &value)?;
        writeln!(writer)?;
    }
    Ok(())
}

/// Recursively tokenize string values, optionally keys, and optionally numbers in JSON.
/// `should_tokenize` tracks whether values in the current subtree should be tokenized
/// (controlled by column filter). At the top level it is `true`.
fn tokenize_json_value(
    value: &mut serde_json::Value,
    mapper: &TokenMapper,
    config: &TokenizeConfig,
    should_tokenize: bool,
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
                        let new_key = if config.should_tokenize_header(k) {
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
                // Check for duplicate keys after tokenization
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
                let child_tokenize = if config.has_column_filter() {
                    config.should_tokenize_column(&key)
                } else {
                    should_tokenize
                };
                if let Some(val) = map.get_mut(&key) {
                    tokenize_json_value(val, mapper, config, child_tokenize);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                tokenize_json_value(item, mapper, config, should_tokenize);
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

    // Build per-column tokenization flags
    let col_flags: Vec<bool> = schema
        .fields()
        .iter()
        .map(|f| config.should_tokenize_column(f.name()))
        .collect();

    // Optionally tokenize column names in schema (respecting column filter)
    let output_schema = if config.tokenize_headers {
        let new_fields: Vec<Arc<Field>> = schema
            .fields()
            .iter()
            .map(|f| {
                if config.should_tokenize_header(f.name()) {
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
                // Non-string columns: pass through unchanged
                columns.push(col.clone());
            }
        }

        let new_batch = RecordBatch::try_new(output_schema.clone(), columns)?;
        writer.write(&new_batch)?;
    }
    writer.close()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        mapper.register("alice@test.com");
        // user subtree values NOT registered because user column is preserved

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
        // user subtree should be preserved
        assert!(content.contains("Alice"));
        assert!(content.contains("Smith"));
        // email should be tokenized
        assert!(!content.contains("alice@test.com"));
    }
}