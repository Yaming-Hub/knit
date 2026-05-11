//! File scanning and string extraction for tokenization.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use tracing::debug;

use crate::tokenize::mapper::TokenMapper;
use crate::tokenize::TokenizeConfig;

/// Classification of a file in the dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Data file (CSV, TSV, Parquet, JSON, JSONL).
    Data,
    /// Schema metadata file (schema.json).
    Schema,
    /// Dictionary/mapping file (in Mappings/ or Dictionaries/ folders).
    Dictionary,
    /// Other companion files (copied unchanged).
    Companion,
}

/// A classified file entry in the dataset.
#[derive(Debug, Clone)]
pub struct FileEntry {
    /// Path relative to the dataset root.
    pub rel_path: PathBuf,
    /// Classified type.
    pub kind: FileKind,
    /// Detected format for data/dictionary files.
    pub format: FileFormat,
}

/// Supported file formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileFormat {
    Csv,
    Tsv,
    Parquet,
    Json,
    Jsonl,
    Other,
}

impl FileFormat {
    fn from_extension(ext: &str) -> Self {
        match ext.to_lowercase().as_str() {
            "csv" => Self::Csv,
            "tsv" => Self::Tsv,
            "parquet" | "pq" => Self::Parquet,
            "json" => Self::Json,
            "jsonl" | "ndjson" => Self::Jsonl,
            _ => Self::Other,
        }
    }
}

/// Recursively scan a directory and classify all files.
pub fn scan_directory(root: &Path) -> Result<Vec<FileEntry>> {
    let mut entries = Vec::new();
    walk_dir(root, root, &mut entries)?;
    entries.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
    Ok(entries)
}

fn walk_dir(root: &Path, current: &Path, entries: &mut Vec<FileEntry>) -> Result<()> {
    let read_dir = std::fs::read_dir(current)
        .with_context(|| format!("reading directory {}", current.display()))?;

    for entry in read_dir {
        let entry = entry?;
        let path = entry.path();

        if path.is_dir() {
            walk_dir(root, &path, entries)?;
        } else {
            let rel_path = path.strip_prefix(root)
                .unwrap_or(&path)
                .to_path_buf();

            let kind = classify_file(&rel_path);
            let ext = path.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("");
            let format = FileFormat::from_extension(ext);

            entries.push(FileEntry { rel_path, kind, format });
        }
    }
    Ok(())
}

/// Classify a file based on its path and name.
fn classify_file(rel_path: &Path) -> FileKind {
    let path_str = rel_path.to_string_lossy().to_lowercase();
    let file_name = rel_path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();

    // Schema files
    if file_name == "schema.json" || path_str.contains("schema") && file_name.ends_with(".json") {
        return FileKind::Schema;
    }

    // Dictionary/mapping files
    if path_str.starts_with("mappings") || path_str.starts_with("dictionaries")
        || path_str.contains("\\mappings\\") || path_str.contains("/mappings/")
        || path_str.contains("\\dictionaries\\") || path_str.contains("/dictionaries/")
    {
        let ext = rel_path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if matches!(FileFormat::from_extension(ext), FileFormat::Csv | FileFormat::Tsv | FileFormat::Json) {
            return FileKind::Dictionary;
        }
    }

    // Data files by extension
    let ext = rel_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match FileFormat::from_extension(ext) {
        FileFormat::Csv | FileFormat::Tsv | FileFormat::Parquet | FileFormat::Json | FileFormat::Jsonl => {
            FileKind::Data
        }
        FileFormat::Other => FileKind::Companion,
    }
}

/// Extract all string values from a file and register them in the token mapper.
pub fn extract_strings(
    entry: &FileEntry,
    root: &Path,
    mapper: &mut TokenMapper,
    config: &TokenizeConfig,
) -> Result<()> {
    let path = root.join(&entry.rel_path);

    match entry.format {
        FileFormat::Csv | FileFormat::Tsv => extract_csv_strings(&path, entry.format, mapper, config),
        FileFormat::Json | FileFormat::Jsonl => extract_json_strings(&path, entry.kind, mapper, config),
        FileFormat::Parquet => extract_parquet_strings(&path, mapper, config),
        FileFormat::Other => Ok(()),
    }
}

fn extract_csv_strings(
    path: &Path,
    format: FileFormat,
    mapper: &mut TokenMapper,
    config: &TokenizeConfig,
) -> Result<()> {
    let delimiter = if format == FileFormat::Tsv { b'\t' } else { b',' };
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delimiter)
        .has_headers(true)
        .flexible(true)
        .from_path(path)
        .with_context(|| format!("opening CSV {}", path.display()))?;

    // Register headers if header tokenization is enabled
    if config.tokenize_headers {
        let headers = rdr.headers()?.clone();
        for h in headers.iter() {
            if should_tokenize_value(h) {
                mapper.register(h);
            }
        }
    }

    for result in rdr.records() {
        let record = result?;
        for field in record.iter() {
            if should_tokenize_value(field) {
                mapper.register(field);
            }
        }
    }
    Ok(())
}

fn extract_json_strings(
    path: &Path,
    kind: FileKind,
    mapper: &mut TokenMapper,
    config: &TokenizeConfig,
) -> Result<()> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("reading {}", path.display()))?;

    // Try parsing as a single JSON document first
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
        if kind == FileKind::Schema {
            extract_schema_json_strings(&value, mapper);
        } else {
            extract_data_json_strings(&value, mapper, config.tokenize_headers);
        }
        return Ok(());
    }

    // Fall back to line-by-line parsing (JSONL)
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let value: serde_json::Value = serde_json::from_str(trimmed)
            .with_context(|| format!("parsing JSONL line in {}", path.display()))?;
        extract_data_json_strings(&value, mapper, config.tokenize_headers);
    }
    Ok(())
}

/// For schema files, only tokenize data payload fields (descriptions, display names).
/// Preserve structural fields (field names, types, paths, IDs).
fn extract_schema_json_strings(value: &serde_json::Value, mapper: &mut TokenMapper) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                // Only tokenize display/description fields, not structural ones
                let key_lower = key.to_lowercase();
                if key_lower == "description" || key_lower == "displayname"
                    || key_lower == "display_name" || key_lower == "tablename"
                    || key_lower == "table_name"
                {
                    if let serde_json::Value::String(s) = val {
                        if should_tokenize_value(s) {
                            mapper.register(s);
                        }
                    }
                } else {
                    // Recurse into nested objects/arrays
                    extract_schema_json_strings(val, mapper);
                }
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                extract_schema_json_strings(item, mapper);
            }
        }
        _ => {}
    }
}

/// For data files, tokenize all string values (and optionally keys).
fn extract_data_json_strings(
    value: &serde_json::Value,
    mapper: &mut TokenMapper,
    tokenize_keys: bool,
) {
    match value {
        serde_json::Value::String(s) => {
            if should_tokenize_value(s) {
                mapper.register(s);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                if tokenize_keys && should_tokenize_value(key) {
                    mapper.register(key);
                }
                extract_data_json_strings(val, mapper, tokenize_keys);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                extract_data_json_strings(item, mapper, tokenize_keys);
            }
        }
        _ => {}
    }
}

fn extract_parquet_strings(
    path: &Path,
    mapper: &mut TokenMapper,
    config: &TokenizeConfig,
) -> Result<()> {
    use arrow::array::{Array, AsArray};
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    let file = std::fs::File::open(path)
        .with_context(|| format!("opening parquet {}", path.display()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;

    // Register column names from file schema (works even for empty files)
    if config.tokenize_headers {
        for field in builder.schema().fields() {
            if should_tokenize_value(field.name()) {
                mapper.register(field.name());
            }
        }
    }

    let reader = builder.build()?;

    for batch_result in reader {
        let batch = batch_result?;

        for col_idx in 0..batch.num_columns() {
            let col = batch.column(col_idx);
            if let Some(str_arr) = col.as_string_opt::<i32>() {
                for i in 0..str_arr.len() {
                    if !str_arr.is_null(i) {
                        let val = str_arr.value(i);
                        if should_tokenize_value(val) {
                            mapper.register(val);
                        }
                    }
                }
            } else if let Some(str_arr) = col.as_string_opt::<i64>() {
                for i in 0..str_arr.len() {
                    if !str_arr.is_null(i) {
                        let val = str_arr.value(i);
                        if should_tokenize_value(val) {
                            mapper.register(val);
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Determine if a string value should be tokenized.
/// Empty strings, pure whitespace, and very short single-char values are skipped.
fn should_tokenize_value(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Skip pure numeric values (handled by --tokenize-numbers flag)
    if trimmed.parse::<f64>().is_ok() {
        return false;
    }
    // Skip booleans
    if matches!(trimmed.to_lowercase().as_str(), "true" | "false" | "yes" | "no") {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_classify_data_files() {
        assert_eq!(classify_file(Path::new("data.csv")), FileKind::Data);
        assert_eq!(classify_file(Path::new("folder/events.parquet")), FileKind::Data);
        assert_eq!(classify_file(Path::new("logs.jsonl")), FileKind::Data);
    }

    #[test]
    fn test_classify_schema_files() {
        assert_eq!(classify_file(Path::new("Schema/schema.json")), FileKind::Schema);
        assert_eq!(classify_file(Path::new("schema.json")), FileKind::Schema);
    }

    #[test]
    fn test_classify_dictionary_files() {
        assert_eq!(classify_file(Path::new("Mappings/regions.csv")), FileKind::Dictionary);
        assert_eq!(classify_file(Path::new("Dictionaries/codes.json")), FileKind::Dictionary);
    }

    #[test]
    fn test_classify_companion_files() {
        assert_eq!(classify_file(Path::new("README.md")), FileKind::Companion);
        assert_eq!(classify_file(Path::new("config.toml")), FileKind::Companion);
    }

    #[test]
    fn test_should_tokenize() {
        assert!(should_tokenize_value("Hello World"));
        assert!(should_tokenize_value("US"));
        assert!(!should_tokenize_value(""));
        assert!(!should_tokenize_value("  "));
        assert!(!should_tokenize_value("123"));
        assert!(!should_tokenize_value("45.67"));
        assert!(!should_tokenize_value("true"));
    }

    #[test]
    fn test_scan_directory() {
        let dir = TempDir::new().unwrap();
        std::fs::File::create(dir.path().join("data.csv")).unwrap();
        std::fs::create_dir(dir.path().join("Mappings")).unwrap();
        std::fs::File::create(dir.path().join("Mappings").join("regions.csv")).unwrap();
        std::fs::File::create(dir.path().join("README.md")).unwrap();

        let entries = scan_directory(dir.path()).unwrap();
        assert_eq!(entries.len(), 3);

        let kinds: Vec<FileKind> = entries.iter().map(|e| e.kind).collect();
        assert!(kinds.contains(&FileKind::Data));
        assert!(kinds.contains(&FileKind::Dictionary));
        assert!(kinds.contains(&FileKind::Companion));
    }

    #[test]
    fn test_extract_csv_strings() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "id,name,value").unwrap();
        writeln!(f, "1,Alice,100").unwrap();
        writeln!(f, "2,Bob,200").unwrap();

        let mut mapper = TokenMapper::new(42);
        extract_csv_strings(&path, FileFormat::Csv, &mut mapper, &TokenizeConfig::default()).unwrap();

        // "Alice" and "Bob" should be registered, but not "1", "2", "100", "200" (numeric)
        assert!(mapper.contains("Alice"));
        assert!(mapper.contains("Bob"));
        assert!(!mapper.contains("100"));
    }

    #[test]
    fn test_extract_jsonl_strings() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("events.jsonl");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, r#"{{"name": "Alice", "score": 10}}"#).unwrap();
        writeln!(f, r#"{{"name": "Bob", "score": 20}}"#).unwrap();

        let mut mapper = TokenMapper::new(42);
        extract_json_strings(&path, FileKind::Data, &mut mapper, &TokenizeConfig::default()).unwrap();

        assert!(mapper.contains("Alice"));
        assert!(mapper.contains("Bob"));
        assert!(!mapper.contains("10"));
    }
}