//! Data ingestion readers for CSV, Parquet, and JSON/JSONL files.
//!
//! Each reader returns `Vec<RecordBatch>` for simplicity.
//! Multi-file ingestion maps a directory of files to entity-level batches.

use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::Schema;
use arrow::record_batch::RecordBatch;
use arrow_csv::ReaderBuilder as CsvReaderBuilder;
use arrow_json::ReaderBuilder as JsonReaderBuilder;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use tracing::{debug, info};

use crate::error::{LearnError, LearnResult};

/// Options for CSV ingestion.
#[derive(Debug, Clone)]
pub struct CsvOptions {
    /// Field delimiter byte (default: `b','`).
    pub delimiter: u8,
    /// Whether the first row is a header (default: `true`).
    pub has_header: bool,
    /// Maximum number of records to infer schema from (default: 100).
    pub schema_infer_rows: usize,
    /// Optional batch size for reading.
    pub batch_size: usize,
}

impl Default for CsvOptions {
    fn default() -> Self {
        Self {
            delimiter: b',',
            has_header: true,
            schema_infer_rows: 100,
            batch_size: 8192,
        }
    }
}

/// Read a CSV file into record batches.
///
/// Uses Arrow's CSV reader with type sniffing. The schema is inferred
/// from the first `schema_infer_rows` rows.
///
/// # Errors
///
/// Returns `LearnError` if the file cannot be opened or parsed.
pub fn read_csv(path: &Path, opts: &CsvOptions) -> LearnResult<Vec<RecordBatch>> {
    info!(path = %path.display(), "Reading CSV file");

    // Infer schema
    let mut schema_file = File::open(path)?;
    let format = arrow_csv::reader::Format::default()
        .with_delimiter(opts.delimiter)
        .with_header(opts.has_header);
    let (schema, _) = format.infer_schema(&mut schema_file, Some(opts.schema_infer_rows))?;
    let schema = Arc::new(schema);

    debug!(fields = schema.fields().len(), "Inferred CSV schema");

    let file = File::open(path)?;
    let reader = CsvReaderBuilder::new(schema)
        .with_delimiter(opts.delimiter)
        .with_header(opts.has_header)
        .with_batch_size(opts.batch_size)
        .build(file)?;
    let batches: Vec<RecordBatch> = reader.collect::<Result<Vec<_>, _>>()?;

    info!(batches = batches.len(), "CSV ingestion complete");
    Ok(batches)
}

/// Read a Parquet file into record batches.
///
/// Schema is extracted from Parquet file metadata.
///
/// # Errors
///
/// Returns `LearnError` if the file cannot be opened or parsed.
pub fn read_parquet(path: &Path) -> LearnResult<Vec<RecordBatch>> {
    info!(path = %path.display(), "Reading Parquet file");

    let file = File::open(path)?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)?;

    debug!(
        rows = builder.metadata().file_metadata().num_rows(),
        "Parquet metadata loaded"
    );

    let reader = builder.build()?;
    let batches: Vec<RecordBatch> = reader.collect::<Result<Vec<_>, _>>()?;

    info!(batches = batches.len(), "Parquet ingestion complete");
    Ok(batches)
}

/// Read a JSON (newline-delimited) file into record batches.
///
/// Supports both JSON arrays and JSONL (one object per line).
/// Nested objects are handled by Arrow's JSON reader (produces struct columns).
///
/// # Errors
///
/// Returns `LearnError` if the file cannot be opened or parsed.
pub fn read_json(path: &Path, batch_size: usize) -> LearnResult<Vec<RecordBatch>> {
    info!(path = %path.display(), "Reading JSON file");

    let file = File::open(path)?;
    let buf = BufReader::new(file);

    // Infer schema from reading the file
    let (inferred_schema, _) =
        arrow_json::reader::infer_json_schema(BufReader::new(File::open(path)?), None)?;
    let schema = Arc::new(inferred_schema);

    debug!(fields = schema.fields().len(), "Inferred JSON schema");

    let reader = JsonReaderBuilder::new(schema)
        .with_batch_size(batch_size)
        .build(buf)?;
    let batches: Vec<RecordBatch> = reader.collect::<Result<Vec<_>, _>>()?;

    info!(batches = batches.len(), "JSON ingestion complete");
    Ok(batches)
}

/// Detect file format from extension and read into record batches.
///
/// Supports `.csv`, `.tsv`, `.parquet`, `.json`, and `.jsonl`.
///
/// # Errors
///
/// Returns `LearnError::UnsupportedFormat` for unknown extensions.
pub fn read_auto(path: &Path) -> LearnResult<Vec<RecordBatch>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "csv" => read_csv(path, &CsvOptions::default()),
        "tsv" => {
            let opts = CsvOptions {
                delimiter: b'\t',
                ..Default::default()
            };
            read_csv(path, &opts)
        }
        "parquet" => read_parquet(path),
        "json" | "jsonl" => read_json(path, 8192),
        other => Err(LearnError::UnsupportedFormat(other.to_string())),
    }
}

/// Result of multi-file ingestion: maps entity name → batches.
#[derive(Debug)]
pub struct IngestionResult {
    /// Entity name derived from the file stem.
    pub entity: String,
    /// Schema of the ingested data.
    pub schema: Arc<Schema>,
    /// Record batches.
    pub batches: Vec<RecordBatch>,
}

/// Ingest all supported files from a directory.
///
/// Each file becomes an entity whose name is the file stem (e.g.,
/// `customers.csv` → entity `"customers"`). Unsupported files are skipped.
///
/// # Errors
///
/// Returns `LearnError` if the directory cannot be read or a supported file
/// fails to parse.
pub fn ingest_directory(dir: &Path) -> LearnResult<Vec<IngestionResult>> {
    info!(dir = %dir.display(), "Ingesting directory");

    let mut results = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();

    for path in entries {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();

        if !["csv", "tsv", "parquet", "json", "jsonl"].contains(&ext.as_str()) {
            debug!(path = %path.display(), "Skipping unsupported file");
            continue;
        }

        let entity = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let batches = read_auto(&path)?;
        let schema = if let Some(b) = batches.first() {
            b.schema()
        } else {
            continue;
        };

        results.push(IngestionResult {
            entity,
            schema,
            batches,
        });
    }

    info!(entities = results.len(), "Directory ingestion complete");
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_csv(dir: &Path, name: &str, content: &str) -> PathBuf {
        let p = dir.join(name);
        let mut f = File::create(&p).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        p
    }

    #[test]
    fn csv_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let csv = write_csv(
            dir.path(),
            "test.csv",
            "id,name,age\n1,Alice,30\n2,Bob,25\n",
        );
        let batches = read_csv(&csv, &CsvOptions::default()).unwrap();
        assert!(!batches.is_empty());
        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 2);
        assert_eq!(batches[0].num_columns(), 3);
    }

    #[test]
    fn tsv_auto_detect() {
        let dir = tempfile::tempdir().unwrap();
        let tsv = write_csv(
            dir.path(),
            "data.tsv",
            "col_a\tcol_b\n1\thello\n2\tworld\n",
        );
        let batches = read_auto(&tsv).unwrap();
        assert!(!batches.is_empty());
    }

    #[test]
    fn unsupported_format() {
        let dir = tempfile::tempdir().unwrap();
        let p = write_csv(dir.path(), "data.xlsx", "junk");
        let result = read_auto(&p);
        assert!(matches!(result, Err(LearnError::UnsupportedFormat(_))));
    }

    #[test]
    fn json_read() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("data.json");
        let mut f = File::create(&p).unwrap();
        writeln!(f, r#"{{"x": 1, "y": "a"}}"#).unwrap();
        writeln!(f, r#"{{"x": 2, "y": "b"}}"#).unwrap();
        let batches = read_json(&p, 1024).unwrap();
        assert!(!batches.is_empty());
    }

    #[test]
    fn directory_ingestion() {
        let dir = tempfile::tempdir().unwrap();
        write_csv(
            dir.path(),
            "users.csv",
            "id,name\n1,Alice\n2,Bob\n",
        );
        write_csv(
            dir.path(),
            "orders.csv",
            "oid,amount\n100,9.99\n101,19.50\n",
        );
        // Also write an unsupported file that should be skipped
        write_csv(dir.path(), "readme.txt", "ignore me");

        let results = ingest_directory(dir.path()).unwrap();
        assert_eq!(results.len(), 2);
        let names: Vec<&str> = results.iter().map(|r| r.entity.as_str()).collect();
        assert!(names.contains(&"users"));
        assert!(names.contains(&"orders"));
    }
}
