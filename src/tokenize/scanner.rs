//! File scanning and string extraction for tokenization.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{NaiveDate, NaiveDateTime, Duration};
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

    let headers = rdr.headers()?.clone();

    // Build per-column tokenization flags
    let col_flags: Vec<bool> = headers
        .iter()
        .map(|h| config.should_tokenize_column(h))
        .collect();

    // Register headers if header tokenization is enabled
    if config.tokenize_headers {
        for h in headers.iter() {
            if config.should_tokenize_header(h) && should_tokenize_value(h) {
                mapper.register(h);
            }
        }
    }

    let shift_days = if config.tokenize_dates {
        Some(compute_date_shift(config.seed))
    } else {
        None
    };

    for result in rdr.records() {
        let record = result?;
        for (idx, field) in record.iter().enumerate() {
            // Skip columns that should not be tokenized
            if idx < col_flags.len() && !col_flags[idx] {
                continue;
            }
            // Try date shifting first (if enabled)
            if let Some(shift) = shift_days {
                if try_register_shifted_date(field, mapper, shift) {
                    continue;
                }
            }
            if should_tokenize_value_with_config(field, config.tokenize_numbers) {
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

    let date_shift = if config.tokenize_dates {
        Some(compute_date_shift(config.seed))
    } else {
        None
    };

    // Try parsing as a single JSON document first
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(&content) {
        if kind == FileKind::Schema {
            extract_schema_json_strings(&value, mapper);
        } else {
            extract_data_json_strings(
                &value, mapper, config, date_shift, true, false,
            );
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
        extract_data_json_strings(
            &value, mapper, config, date_shift, true, false,
        );
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

/// For data files, tokenize all string values (and optionally keys, numbers, and dates).
/// `should_tokenize` tracks whether the current value is in a column that should be
/// tokenized. `filter_applied` tracks whether the column filter has already been
/// evaluated for this subtree — if true, nested keys inherit instead of re-checking.
fn extract_data_json_strings(
    value: &serde_json::Value,
    mapper: &mut TokenMapper,
    config: &TokenizeConfig,
    date_shift: Option<i64>,
    should_tokenize: bool,
    filter_applied: bool,
) {
    match value {
        serde_json::Value::String(s) => {
            if !should_tokenize {
                return;
            }
            // Try date shifting first
            if let Some(shift) = date_shift {
                if try_register_shifted_date(s, mapper, shift) {
                    return;
                }
            }
            if should_tokenize_value_with_config(s, config.tokenize_numbers) {
                mapper.register(s);
            }
        }
        serde_json::Value::Number(n) => {
            if should_tokenize && config.tokenize_numbers {
                let s = n.to_string();
                mapper.register(&s);
            }
        }
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                // Apply column filter only at the first object level
                let (child_tokenize, child_filter_applied) = if !filter_applied && config.has_column_filter() {
                    (config.should_tokenize_column(key), true)
                } else {
                    (should_tokenize, filter_applied)
                };

                if config.tokenize_headers && child_tokenize && should_tokenize_value(key) {
                    mapper.register(key);
                }
                extract_data_json_strings(val, mapper, config, date_shift, child_tokenize, child_filter_applied);
            }
        }
        serde_json::Value::Array(arr) => {
            for item in arr {
                extract_data_json_strings(item, mapper, config, date_shift, should_tokenize, filter_applied);
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

    // Build per-column tokenization flags from schema
    let schema = builder.schema();
    let col_flags: Vec<bool> = schema
        .fields()
        .iter()
        .map(|f| config.should_tokenize_column(f.name()))
        .collect();

    // Register column names from file schema (works even for empty files)
    if config.tokenize_headers {
        for field in schema.fields() {
            if config.should_tokenize_header(field.name()) && should_tokenize_value(field.name()) {
                mapper.register(field.name());
            }
        }
    }

    let reader = builder.build()?;

    let date_shift = if config.tokenize_dates {
        Some(compute_date_shift(config.seed))
    } else {
        None
    };

    let mut warned_native_numerics = false;
    for batch_result in reader {
        let batch = batch_result?;

        for col_idx in 0..batch.num_columns() {
            // Skip columns that should not be tokenized
            if col_idx < col_flags.len() && !col_flags[col_idx] {
                continue;
            }

            let col = batch.column(col_idx);
            if let Some(str_arr) = col.as_string_opt::<i32>() {
                for i in 0..str_arr.len() {
                    if !str_arr.is_null(i) {
                        let val = str_arr.value(i);
                        if let Some(shift) = date_shift {
                            if try_register_shifted_date(val, mapper, shift) {
                                continue;
                            }
                        }
                        if should_tokenize_value_with_config(val, config.tokenize_numbers) {
                            mapper.register(val);
                        }
                    }
                }
            } else if let Some(str_arr) = col.as_string_opt::<i64>() {
                for i in 0..str_arr.len() {
                    if !str_arr.is_null(i) {
                        let val = str_arr.value(i);
                        if let Some(shift) = date_shift {
                            if try_register_shifted_date(val, mapper, shift) {
                                continue;
                            }
                        }
                        if should_tokenize_value_with_config(val, config.tokenize_numbers) {
                            mapper.register(val);
                        }
                    }
                }
            } else if config.tokenize_numbers && !warned_native_numerics {
                use arrow::datatypes::DataType;
                let dt = col.data_type();
                if matches!(dt, DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64
                    | DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64
                    | DataType::Float16 | DataType::Float32 | DataType::Float64)
                {
                    tracing::warn!(
                        file = %path.display(),
                        "native numeric Parquet columns are not yet tokenized by --tokenize-numbers; \
                         only string-encoded numbers are replaced"
                    );
                    warned_native_numerics = true;
                }
            }
        }
    }
    Ok(())
}

/// Determine if a string value should be tokenized.
/// Empty strings, pure whitespace, and very short single-char values are skipped.
/// Numeric values are skipped by default (handled by --tokenize-numbers flag).
fn should_tokenize_value(s: &str) -> bool {
    should_tokenize_value_with_config(s, false)
}

/// Core tokenization check with numeric and date overrides.
fn should_tokenize_value_with_config(s: &str, tokenize_numbers: bool) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Skip booleans
    if matches!(trimmed.to_lowercase().as_str(), "true" | "false" | "yes" | "no") {
        return false;
    }
    // Skip date/timestamp strings — handled separately by --tokenize-dates
    if is_date_string(trimmed).is_some() {
        return false;
    }
    // Skip pure numeric values unless --tokenize-numbers is enabled
    if !tokenize_numbers && is_numeric_string(trimmed) {
        return false;
    }
    true
}

/// Check if a string represents a numeric value (integer or float).
/// Recognizes: integers, decimals, negative numbers, leading +, scientific notation.
/// Does NOT match: NaN, inf, -inf (these are preserved unchanged).
fn is_numeric_string(s: &str) -> bool {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return false;
    }
    // Reject NaN, inf, -inf — these are not "numbers" for tokenization purposes
    let lower = trimmed.to_lowercase();
    if matches!(lower.as_str(), "nan" | "inf" | "-inf" | "+inf" | "infinity" | "-infinity" | "+infinity") {
        return false;
    }
    trimmed.parse::<f64>().is_ok()
}

/// Recognized date format patterns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DateFormat {
    /// YYYY-MM-DD (e.g., 2024-01-15)
    IsoDate,
    /// YYYY-MM-DDThh:mm:ss (e.g., 2024-01-15T10:30:00)
    IsoDateTimeT,
    /// YYYY-MM-DD hh:mm:ss (e.g., 2024-01-15 10:30:00)
    IsoDateTimeSpace,
    /// YYYY-MM-DDThh:mm:ssZ (e.g., 2024-01-15T10:30:00Z)
    IsoDateTimeZ,
    /// YYYY-MM-DDThh:mm:ss+HH:MM (e.g., 2024-01-15T10:30:00+05:30)
    IsoDateTimeOffset,
    /// YYYYMMDD (e.g., 20240115) — only 8-digit strings starting with 19/20
    Compact,
}

/// Parsed date information.
struct DateInfo {
    /// The datetime value (date + optional time component).
    datetime: NaiveDateTime,
    /// Which format was detected.
    format: DateFormat,
    /// Timezone suffix (e.g., "Z", "+05:30") for formats that include it.
    tz_suffix: String,
    /// Fractional seconds string (e.g., ".123") if present, empty otherwise.
    frac_seconds: String,
}

/// Try to parse a string as a recognized date/timestamp format.
/// Returns None if the string doesn't match any supported format.
/// Only recognizes unambiguous ISO 8601 and compact (YYYYMMDD) formats.
fn is_date_string(s: &str) -> Option<DateInfo> {
    let trimmed = s.trim();

    // Try ISO date: YYYY-MM-DD
    if trimmed.len() == 10 {
        if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
            return Some(DateInfo {
                datetime: d.and_hms_opt(0, 0, 0).unwrap(),
                format: DateFormat::IsoDate,
                tz_suffix: String::new(),
                frac_seconds: String::new(),
            });
        }
    }

    // Try ISO datetime with T separator
    if trimmed.contains('T') {
        // Strip timezone suffix for parsing
        let (base, tz) = strip_tz_suffix(trimmed);

        // Extract fractional seconds if present
        let (base_no_frac, frac) = extract_frac_seconds(base);

        if let Ok(dt) = NaiveDateTime::parse_from_str(base_no_frac, "%Y-%m-%dT%H:%M:%S") {
            let fmt = if tz == "Z" {
                DateFormat::IsoDateTimeZ
            } else if tz.is_empty() {
                DateFormat::IsoDateTimeT
            } else {
                DateFormat::IsoDateTimeOffset
            };
            return Some(DateInfo {
                datetime: dt,
                format: fmt,
                tz_suffix: tz.to_string(),
                frac_seconds: frac.to_string(),
            });
        }
    }

    // Try ISO datetime with space separator: YYYY-MM-DD HH:MM:SS
    if trimmed.len() >= 19 && &trimmed[10..11] == " " {
        let (base_no_frac, frac) = extract_frac_seconds(trimmed);

        if let Ok(dt) = NaiveDateTime::parse_from_str(base_no_frac, "%Y-%m-%d %H:%M:%S") {
            return Some(DateInfo {
                datetime: dt,
                format: DateFormat::IsoDateTimeSpace,
                tz_suffix: String::new(),
                frac_seconds: frac.to_string(),
            });
        }
    }

    // Try compact: YYYYMMDD (only 8 digits, starts with 19 or 20)
    if trimmed.len() == 8 && trimmed.chars().all(|c| c.is_ascii_digit()) {
        if trimmed.starts_with("19") || trimmed.starts_with("20") {
            if let Ok(d) = NaiveDate::parse_from_str(trimmed, "%Y%m%d") {
                return Some(DateInfo {
                    datetime: d.and_hms_opt(0, 0, 0).unwrap(),
                    format: DateFormat::Compact,
                    tz_suffix: String::new(),
                    frac_seconds: String::new(),
                });
            }
        }
    }

    None
}

/// Strip timezone suffix (Z, +HH:MM, -HH:MM, +HHMM, -HHMM) from a datetime string.
/// Returns (base, tz_suffix).
fn strip_tz_suffix(s: &str) -> (&str, &str) {
    if s.ends_with('Z') {
        (&s[..s.len() - 1], "Z")
    } else if s.len() > 6 {
        // Try +HH:MM or -HH:MM (6 chars)
        let last6 = &s[s.len() - 6..];
        if (last6.starts_with('+') || last6.starts_with('-')) && &last6[3..4] == ":" {
            return (&s[..s.len() - 6], last6);
        }
        // Try +HHMM or -HHMM (5 chars)
        if s.len() > 5 {
            let last5 = &s[s.len() - 5..];
            if (last5.starts_with('+') || last5.starts_with('-'))
                && last5[1..].chars().all(|c| c.is_ascii_digit())
            {
                return (&s[..s.len() - 5], last5);
            }
        }
        (s, "")
    } else {
        (s, "")
    }
}

/// Extract fractional seconds from a datetime string.
/// Returns (base_without_frac, frac_part) where frac_part includes the dot.
fn extract_frac_seconds(s: &str) -> (&str, &str) {
    // Look for ".NNN" after the seconds (position 19 for T-separated, 19 for space)
    // Find the last '.' that's followed only by digits
    if let Some(dot_pos) = s.rfind('.') {
        let after_dot = &s[dot_pos + 1..];
        if !after_dot.is_empty() && after_dot.chars().all(|c| c.is_ascii_digit()) {
            return (&s[..dot_pos], &s[dot_pos..]);
        }
    }
    (s, "")
}

/// Format a shifted datetime back to its original format, preserving fractional seconds.
fn format_shifted_date(dt: &NaiveDateTime, info: &DateInfo) -> String {
    let base = match info.format {
        DateFormat::IsoDate => dt.format("%Y-%m-%d").to_string(),
        DateFormat::IsoDateTimeT => format!(
            "{}{}",
            dt.format("%Y-%m-%dT%H:%M:%S"),
            info.frac_seconds,
        ),
        DateFormat::IsoDateTimeSpace => format!(
            "{}{}",
            dt.format("%Y-%m-%d %H:%M:%S"),
            info.frac_seconds,
        ),
        DateFormat::IsoDateTimeZ => format!(
            "{}{}Z",
            dt.format("%Y-%m-%dT%H:%M:%S"),
            info.frac_seconds,
        ),
        DateFormat::IsoDateTimeOffset => {
            format!(
                "{}{}{}",
                dt.format("%Y-%m-%dT%H:%M:%S"),
                info.frac_seconds,
                info.tz_suffix,
            )
        }
        DateFormat::Compact => dt.format("%Y%m%d").to_string(),
    };
    base
}

/// Compute a deterministic date shift offset (in days) from a seed.
/// Returns an offset between -1825 and +1825 days (±5 years), never zero.
fn compute_date_shift(seed: u64) -> i64 {
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    let mut rng = StdRng::seed_from_u64(seed.wrapping_add(0xDA7E_5EED));
    let offset: i64 = rng.gen_range(-1825..=1825);
    if offset == 0 { 1 } else { offset }
}

/// Register a date string with its shifted value in the mapper.
/// Returns true if the value was detected as a date and registered.
fn try_register_shifted_date(
    s: &str,
    mapper: &mut TokenMapper,
    shift_days: i64,
) -> bool {
    if let Some(info) = is_date_string(s) {
        let shifted = info.datetime + Duration::days(shift_days);
        let shifted_str = format_shifted_date(&shifted, &info);
        mapper.register_with_value(s, &shifted_str);
        true
    } else {
        false
    }
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

    #[test]
    fn test_is_numeric_string() {
        assert!(is_numeric_string("42"));
        assert!(is_numeric_string("-100"));
        assert!(is_numeric_string("3.14"));
        assert!(is_numeric_string("+7"));
        assert!(is_numeric_string("1e6"));
        assert!(is_numeric_string("-0.0"));
        // NaN and inf are NOT numeric for tokenization
        assert!(!is_numeric_string("NaN"));
        assert!(!is_numeric_string("inf"));
        assert!(!is_numeric_string("-inf"));
        assert!(!is_numeric_string("Infinity"));
        assert!(!is_numeric_string(""));
    }

    #[test]
    fn test_extract_csv_with_tokenize_numbers() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("data.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "name,score").unwrap();
        writeln!(f, "Alice,42.5").unwrap();
        writeln!(f, "Bob,-100").unwrap();

        let config = TokenizeConfig {
            tokenize_numbers: true,
            ..Default::default()
        };
        let mut mapper = TokenMapper::new(42);
        extract_csv_strings(&path, FileFormat::Csv, &mut mapper, &config).unwrap();

        assert!(mapper.contains("Alice"));
        assert!(mapper.contains("Bob"));
        // With tokenize_numbers, numeric values should be registered
        assert!(mapper.contains("42.5"));
        assert!(mapper.contains("-100"));
    }

    #[test]
    fn test_extract_json_with_tokenize_numbers() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("data.json");
        std::fs::write(&path, r#"{"name": "Alice", "score": 95}"#).unwrap();

        let config = TokenizeConfig {
            tokenize_numbers: true,
            ..Default::default()
        };
        let mut mapper = TokenMapper::new(42);
        extract_json_strings(&path, FileKind::Data, &mut mapper, &config).unwrap();

        assert!(mapper.contains("Alice"));
        // JSON numeric scalar "95" should be registered as string
        assert!(mapper.contains("95"));
    }

    #[test]
    fn test_is_date_string_iso_date() {
        let info = is_date_string("2024-01-15").unwrap();
        assert_eq!(info.format, DateFormat::IsoDate);
        assert_eq!(info.datetime.date(), NaiveDate::from_ymd_opt(2024, 1, 15).unwrap());
    }

    #[test]
    fn test_is_date_string_iso_datetime() {
        let info = is_date_string("2024-01-15T10:30:00").unwrap();
        assert_eq!(info.format, DateFormat::IsoDateTimeT);

        let info_z = is_date_string("2024-01-15T10:30:00Z").unwrap();
        assert_eq!(info_z.format, DateFormat::IsoDateTimeZ);
        assert_eq!(info_z.tz_suffix, "Z");

        let info_off = is_date_string("2024-01-15T10:30:00+05:30").unwrap();
        assert_eq!(info_off.format, DateFormat::IsoDateTimeOffset);
        assert_eq!(info_off.tz_suffix, "+05:30");
    }

    #[test]
    fn test_is_date_string_space_separator() {
        let info = is_date_string("2024-01-15 10:30:00").unwrap();
        assert_eq!(info.format, DateFormat::IsoDateTimeSpace);
    }

    #[test]
    fn test_is_date_string_compact() {
        let info = is_date_string("20240115").unwrap();
        assert_eq!(info.format, DateFormat::Compact);
    }

    #[test]
    fn test_is_date_string_rejects_non_dates() {
        assert!(is_date_string("hello").is_none());
        assert!(is_date_string("12345678").is_none()); // doesn't start with 19/20
        assert!(is_date_string("42").is_none());
        assert!(is_date_string("").is_none());
        assert!(is_date_string("2024-13-01").is_none()); // invalid month
    }

    #[test]
    fn test_date_shift_deterministic() {
        let shift1 = compute_date_shift(42);
        let shift2 = compute_date_shift(42);
        assert_eq!(shift1, shift2);
        assert_ne!(shift1, 0); // never zero
    }

    #[test]
    fn test_date_shifting_preserves_format() {
        let mut mapper = TokenMapper::new(42);
        let shift = 100; // +100 days

        try_register_shifted_date("2024-01-15", &mut mapper, shift);
        let token = mapper.get("2024-01-15").unwrap();
        // Should be a valid ISO date, 100 days later
        assert_eq!(token, "2024-04-24");

        try_register_shifted_date("20240115", &mut mapper, shift);
        let token = mapper.get("20240115").unwrap();
        assert_eq!(token, "20240424");
    }

    #[test]
    fn test_dates_skipped_by_default() {
        // Date strings should NOT be tokenized when tokenize_dates is false
        assert!(!should_tokenize_value("2024-01-15"));
        assert!(!should_tokenize_value("2024-01-15T10:30:00Z"));
        assert!(!should_tokenize_value("20240115"));
    }

    #[test]
    fn test_extract_csv_with_tokenize_dates() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("data.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "name,date").unwrap();
        writeln!(f, "Alice,2024-01-15").unwrap();
        writeln!(f, "Bob,2024-03-20").unwrap();

        let config = TokenizeConfig {
            tokenize_dates: true,
            ..Default::default()
        };
        let mut mapper = TokenMapper::new(42);
        extract_csv_strings(&path, FileFormat::Csv, &mut mapper, &config).unwrap();

        // Names registered normally
        assert!(mapper.contains("Alice"));
        assert!(mapper.contains("Bob"));
        // Dates registered with shifted values
        assert!(mapper.contains("2024-01-15"));
        assert!(mapper.contains("2024-03-20"));
        // Both shifted by the same offset (relative order preserved)
        let d1 = mapper.get("2024-01-15").unwrap();
        let d2 = mapper.get("2024-03-20").unwrap();
        assert_ne!(d1, "2024-01-15");
        assert_ne!(d2, "2024-03-20");
        // The difference between the two should be preserved (65 days)
        let orig_diff = NaiveDate::parse_from_str("2024-03-20", "%Y-%m-%d").unwrap()
            - NaiveDate::parse_from_str("2024-01-15", "%Y-%m-%d").unwrap();
        let shifted_diff = NaiveDate::parse_from_str(d2, "%Y-%m-%d").unwrap()
            - NaiveDate::parse_from_str(d1, "%Y-%m-%d").unwrap();
        assert_eq!(orig_diff, shifted_diff);
    }

    #[test]
    fn test_extract_csv_tokenize_columns_whitelist() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("data.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "name,city,country").unwrap();
        writeln!(f, "Alice,Seattle,USA").unwrap();
        writeln!(f, "Bob,Portland,Canada").unwrap();

        let config = TokenizeConfig {
            tokenize_columns: Some(["name".to_string()].into_iter().collect()),
            ..Default::default()
        };
        let mut mapper = TokenMapper::new(42);
        extract_csv_strings(&path, FileFormat::Csv, &mut mapper, &config).unwrap();

        // Only "name" column values should be registered
        assert!(mapper.contains("Alice"));
        assert!(mapper.contains("Bob"));
        // "city" and "country" values should NOT be registered
        assert!(!mapper.contains("Seattle"));
        assert!(!mapper.contains("Portland"));
        assert!(!mapper.contains("USA"));
        assert!(!mapper.contains("Canada"));
    }

    #[test]
    fn test_extract_csv_preserve_columns_blacklist() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("data.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "name,city,country").unwrap();
        writeln!(f, "Alice,Seattle,USA").unwrap();

        let config = TokenizeConfig {
            preserve_columns: Some(["country".to_string()].into_iter().collect()),
            ..Default::default()
        };
        let mut mapper = TokenMapper::new(42);
        extract_csv_strings(&path, FileFormat::Csv, &mut mapper, &config).unwrap();

        // "name" and "city" values should be registered
        assert!(mapper.contains("Alice"));
        assert!(mapper.contains("Seattle"));
        // "country" values should NOT be registered
        assert!(!mapper.contains("USA"));
    }

    #[test]
    fn test_extract_csv_column_filter_case_insensitive() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("data.csv");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "Name,City").unwrap();
        writeln!(f, "Alice,Seattle").unwrap();

        // Filter uses lowercase "name" but CSV header is "Name"
        let config = TokenizeConfig {
            tokenize_columns: Some(["name".to_string()].into_iter().collect()),
            ..Default::default()
        };
        let mut mapper = TokenMapper::new(42);
        extract_csv_strings(&path, FileFormat::Csv, &mut mapper, &config).unwrap();

        assert!(mapper.contains("Alice"));
        assert!(!mapper.contains("Seattle"));
    }

    #[test]
    fn test_config_should_tokenize_column() {
        // No filter — all columns tokenized
        let config = TokenizeConfig::default();
        assert!(config.should_tokenize_column("name"));
        assert!(config.should_tokenize_column("anything"));

        // Whitelist — only listed columns tokenized
        let config = TokenizeConfig {
            tokenize_columns: Some(["name".to_string(), "email".to_string()].into_iter().collect()),
            ..Default::default()
        };
        assert!(config.should_tokenize_column("name"));
        assert!(config.should_tokenize_column("Name")); // case-insensitive
        assert!(config.should_tokenize_column("email"));
        assert!(!config.should_tokenize_column("city"));

        // Blacklist — listed columns preserved
        let config = TokenizeConfig {
            preserve_columns: Some(["country".to_string()].into_iter().collect()),
            ..Default::default()
        };
        assert!(config.should_tokenize_column("name"));
        assert!(!config.should_tokenize_column("country"));
        assert!(!config.should_tokenize_column("Country")); // case-insensitive
    }
}