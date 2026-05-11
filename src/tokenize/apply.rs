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
            apply_json(&src, out_path, entry.format, mapper)
        }
        FileFormat::Parquet => {
            apply_parquet(&src, out_path, mapper)
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
    _config: &TokenizeConfig,
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

    // Write headers unchanged
    let headers = rdr.headers()?.clone();
    wtr.write_record(&headers)?;

    // Write tokenized records
    for result in rdr.records() {
        let record = result?;
        let tokenized: Vec<String> = record
            .iter()
            .map(|field| {
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
) -> Result<()> {
    if format == FileFormat::Jsonl {
        apply_jsonl(src, out, mapper)
    } else {
        let content = std::fs::read_to_string(src)?;
        let mut value: serde_json::Value = serde_json::from_str(&content)?;
        tokenize_json_value(&mut value, mapper);
        let output = serde_json::to_string_pretty(&value)?;
        std::fs::write(out, output)?;
        Ok(())
    }
}

fn apply_jsonl(src: &Path, out: &Path, mapper: &TokenMapper) -> Result<()> {
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
        tokenize_json_value(&mut value, mapper);
        serde_json::to_writer(&mut writer, &value)?;
        writeln!(writer)?;
    }
    Ok(())
}

/// Recursively tokenize all string values in a JSON value (for data files).
fn tokenize_json_value(value: &mut serde_json::Value, mapper: &TokenMapper) {
    match value {
        serde_json::Value::String(s) => {
            if let Some(token) = mapper.get(s) {
                *s = token.to_string();
            }
        }
        serde_json::Value::Object(map) => {
            for val in map.values_mut() {
                tokenize_json_value(val, mapper);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr.iter_mut() {
                tokenize_json_value(item, mapper);
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

fn apply_parquet(src: &Path, out: &Path, mapper: &TokenMapper) -> Result<()> {
    use arrow::array::{Array, AsArray, StringArray};
    use arrow::record_batch::RecordBatch;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use parquet::arrow::ArrowWriter;
    use std::sync::Arc;

    let file = std::fs::File::open(src)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;
    let schema = builder.schema().clone();
    let reader = builder.build()?;

    let out_file = std::fs::File::create(out)?;
    let mut writer = ArrowWriter::try_new(out_file, schema.clone(), None)?;

    for batch_result in reader {
        let batch = batch_result?;
        let mut columns: Vec<Arc<dyn Array>> = Vec::with_capacity(batch.num_columns());

        for col_idx in 0..batch.num_columns() {
            let col = batch.column(col_idx);
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

        let new_batch = RecordBatch::try_new(schema.clone(), columns)?;
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
}