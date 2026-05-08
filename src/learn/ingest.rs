//! Data ingestion readers for CSV, Parquet, and JSON/JSONL files.
//!
//! Each reader returns `Vec<RecordBatch>` for simplicity.
//! Multi-file ingestion maps a directory of files to entity-level batches.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::Schema;
use arrow::record_batch::RecordBatch;
use arrow_csv::ReaderBuilder as CsvReaderBuilder;
use arrow_json::ReaderBuilder as JsonReaderBuilder;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json;
use tracing::{debug, info, warn};

use crate::learn::error::{LearnError, LearnResult};

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
    read_csv_with_limit(path, opts, None)
}

/// Read a CSV file with an optional row limit.
///
/// Stops reading batches from disk once `max_rows` is reached, avoiding
/// full-file I/O for large CSVs.
pub fn read_csv_with_limit(
    path: &Path,
    opts: &CsvOptions,
    max_rows: Option<usize>,
) -> LearnResult<Vec<RecordBatch>> {
    info!(path = %path.display(), ?max_rows, "Reading CSV file");

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

    let mut batches = Vec::new();
    let mut rows_read: usize = 0;
    for batch_result in reader {
        let batch = batch_result?;
        rows_read += batch.num_rows();
        batches.push(batch);
        if let Some(limit) = max_rows {
            if rows_read >= limit {
                break;
            }
        }
    }

    // Final truncation to exact limit (last batch may overshoot)
    let batches = match max_rows {
        Some(limit) => truncate_batches(batches, limit),
        None => batches,
    };

    info!(
        batches = batches.len(),
        rows = rows_read,
        "CSV ingestion complete"
    );
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

/// Read a JSON file into record batches.
///
/// Supports both JSON arrays (`[{...}, {...}]`) and JSONL (one object per line).
/// If the file starts with `[`, it is treated as a JSON array and each element
/// is converted to a newline-delimited object before passing to Arrow's reader.
/// JSONL files are streamed directly from disk without buffering the entire file.
///
/// # Errors
///
/// Returns `LearnError` if the file cannot be opened or parsed.
pub fn read_json(path: &Path, batch_size: usize) -> LearnResult<Vec<RecordBatch>> {
    use std::io::{BufReader, Read};

    info!(path = %path.display(), "Reading JSON file");

    // Peek at the first non-whitespace byte to detect format
    let mut peek_buf = [0u8; 64];
    let mut file = File::open(path)?;
    let n = file.read(&mut peek_buf)?;
    let first_char = peek_buf[..n]
        .iter()
        .find(|b| !b.is_ascii_whitespace())
        .copied();

    if first_char == Some(b'[') {
        // JSON array: must buffer to convert to JSONL for Arrow
        debug!("Detected JSON array format, converting to JSONL");
        let raw = std::fs::read_to_string(path)?;
        let trimmed = raw.trim_start();
        let arr: Vec<serde_json::Value> = serde_json::from_str(trimmed).map_err(|e| {
            LearnError::Arrow(arrow::error::ArrowError::JsonError(format!(
                "Failed to parse JSON array: {e}"
            )))
        })?;
        let mut buf = Vec::new();
        for obj in &arr {
            serde_json::to_writer(&mut buf, obj).map_err(|e| {
                LearnError::Arrow(arrow::error::ArrowError::JsonError(format!(
                    "Failed to serialize JSON object: {e}"
                )))
            })?;
            buf.push(b'\n');
        }

        let (inferred_schema, _) =
            arrow_json::reader::infer_json_schema(std::io::Cursor::new(&buf), None)?;
        let schema = Arc::new(inferred_schema);
        debug!(fields = schema.fields().len(), "Inferred JSON schema");

        let reader = JsonReaderBuilder::new(schema)
            .with_batch_size(batch_size)
            .build(std::io::Cursor::new(buf))?;
        let batches: Vec<RecordBatch> = reader.collect::<Result<Vec<_>, _>>()?;
        info!(batches = batches.len(), "JSON array ingestion complete");
        Ok(batches)
    } else {
        // JSONL: stream directly from disk
        let infer_file = File::open(path)?;
        let (inferred_schema, _) =
            arrow_json::reader::infer_json_schema(BufReader::new(infer_file), None)?;
        let schema = Arc::new(inferred_schema);
        debug!(fields = schema.fields().len(), "Inferred JSON schema");

        let file = File::open(path)?;
        let reader = JsonReaderBuilder::new(schema)
            .with_batch_size(batch_size)
            .build(BufReader::new(file))?;
        let batches: Vec<RecordBatch> = reader.collect::<Result<Vec<_>, _>>()?;
        info!(batches = batches.len(), "JSON ingestion complete");
        Ok(batches)
    }
}

/// Detect file format from extension and read into record batches.
///
/// Supports `.csv`, `.tsv`, `.parquet`, `.json`, and `.jsonl`.
///
/// # Errors
///
/// Returns `LearnError::UnsupportedFormat` for unknown extensions.
pub fn read_auto(path: &Path) -> LearnResult<Vec<RecordBatch>> {
    read_auto_with_limit(path, None)
}

/// Read with an optional row limit. For CSV/TSV, stops I/O early.
/// For Parquet/JSON, reads fully then truncates (documented limitation).
pub fn read_auto_with_limit(path: &Path, max_rows: Option<usize>) -> LearnResult<Vec<RecordBatch>> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();

    match ext.as_str() {
        "csv" => read_csv_with_limit(path, &CsvOptions::default(), max_rows),
        "tsv" => {
            let opts = CsvOptions {
                delimiter: b'\t',
                ..Default::default()
            };
            read_csv_with_limit(path, &opts, max_rows)
        }
        "parquet" => {
            let batches = read_parquet(path)?;
            Ok(match max_rows {
                Some(limit) => truncate_batches(batches, limit),
                None => batches,
            })
        }
        "json" | "jsonl" => {
            let batches = read_json(path, 8192)?;
            Ok(match max_rows {
                Some(limit) => truncate_batches(batches, limit),
                None => batches,
            })
        }
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

/// Truncate a list of record batches to at most `max_rows` total rows.
///
/// Returns early once the row budget is exhausted, slicing the last batch
/// if needed.
pub fn truncate_batches(batches: Vec<RecordBatch>, max_rows: usize) -> Vec<RecordBatch> {
    let mut result = Vec::new();
    let mut remaining = max_rows;
    for batch in batches {
        if remaining == 0 {
            break;
        }
        let n = batch.num_rows();
        if n <= remaining {
            remaining -= n;
            result.push(batch);
        } else {
            result.push(batch.slice(0, remaining));
            break;
        }
    }
    result
}

/// Ingest all supported files from a directory.
///
/// Each file becomes an entity whose name is the file stem (e.g.,
/// `customers.csv` → entity `"customers"`). Unsupported files are skipped.
///
/// If `max_rows` is `Some(n)`, each entity is limited to at most `n` rows.
///
/// # Errors
///
/// Returns `LearnError` if the directory cannot be read or a supported file
/// fails to parse.
pub fn ingest_directory(dir: &Path) -> LearnResult<Vec<IngestionResult>> {
    ingest_directory_with_limit(dir, None)
}

/// Recursively collect all files from a directory tree.
///
/// Propagates the root directory read error. Unreadable subdirectories
/// are logged and skipped. Directory symlinks are skipped to avoid cycles.
fn collect_files_recursive(dir: &Path) -> LearnResult<Vec<PathBuf>> {
    use std::collections::HashSet;

    let mut files = Vec::new();
    let mut dirs = vec![dir.to_path_buf()];
    let mut visited: HashSet<PathBuf> = HashSet::new();

    // Canonicalize and visit the root; propagate error if unreadable
    let root_canonical = dir.canonicalize().map_err(|e| {
        LearnError::Io(std::io::Error::new(
            e.kind(),
            format!("cannot read directory {}: {e}", dir.display()),
        ))
    })?;
    visited.insert(root_canonical);

    let mut first = true;
    while let Some(d) = dirs.pop() {
        let entries = match std::fs::read_dir(&d) {
            Ok(e) => e,
            Err(e) => {
                if first {
                    return Err(LearnError::Io(e));
                }
                warn!(dir = %d.display(), error = %e, "skipping unreadable subdirectory");
                continue;
            }
        };
        first = false;

        for entry in entries.filter_map(|e| e.ok()) {
            let path = entry.path();
            let ft = match entry.file_type() {
                Ok(ft) => ft,
                Err(_) => continue,
            };
            if ft.is_dir() && !ft.is_symlink() {
                if let Ok(canonical) = path.canonicalize() {
                    if visited.insert(canonical) {
                        dirs.push(path);
                    }
                }
            } else if ft.is_file() {
                files.push(path);
            }
        }
    }
    Ok(files)
}

/// Ingest with an optional per-entity row limit.
///
/// Recursively discovers all supported data files in subdirectories.
pub fn ingest_directory_with_limit(
    dir: &Path,
    max_rows: Option<usize>,
) -> LearnResult<Vec<IngestionResult>> {
    info!(dir = %dir.display(), ?max_rows, "Ingesting directory");

    let mut results = Vec::new();
    let mut entries: Vec<PathBuf> = collect_files_recursive(dir)?;
    entries.sort();

    // Derive unique entity names: use file_stem, but prefix with parent dir
    // name when duplicates would occur.
    let stems: Vec<String> = entries
        .iter()
        .filter(|p| {
            let ext = p
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();
            ["csv", "tsv", "parquet", "json", "jsonl"].contains(&ext.as_str())
        })
        .map(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown")
                .to_string()
        })
        .collect();
    let mut stem_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for s in &stems {
        *stem_counts.entry(s.clone()).or_insert(0) += 1;
    }
    let duplicated_stems: std::collections::HashSet<&str> = stem_counts
        .iter()
        .filter(|(_, c)| **c > 1)
        .map(|(s, _)| s.as_str())
        .collect();

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

        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("unknown")
            .to_string();

        let entity = if duplicated_stems.contains(stem.as_str()) {
            // Prefix with parent directory name to disambiguate
            let parent = path
                .parent()
                .and_then(|p| p.file_name())
                .and_then(|s| s.to_str())
                .unwrap_or("unknown");
            format!("{parent}_{stem}")
        } else {
            stem
        };

        let batches = read_auto_with_limit(&path, max_rows)?;
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
        let tsv = write_csv(dir.path(), "data.tsv", "col_a\tcol_b\n1\thello\n2\tworld\n");
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
    fn json_array_read() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("data.json");
        std::fs::write(&p, r#"[{"x":1,"y":"a"},{"x":2,"y":"b"},{"x":3,"y":"c"}]"#).unwrap();
        let batches = read_json(&p, 1024).unwrap();
        assert!(!batches.is_empty());
        let total_rows: usize = batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total_rows, 3);
        assert_eq!(batches[0].num_columns(), 2);
    }

    #[test]
    fn directory_ingestion() {
        let dir = tempfile::tempdir().unwrap();
        write_csv(dir.path(), "users.csv", "id,name\n1,Alice\n2,Bob\n");
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

    #[test]
    fn directory_ingestion_recursive() {
        let dir = tempfile::tempdir().unwrap();
        // Create subdirectories with data files
        let sub1 = dir.path().join("subdir1");
        let sub2 = dir.path().join("subdir2");
        std::fs::create_dir(&sub1).unwrap();
        std::fs::create_dir(&sub2).unwrap();

        write_csv(&sub1, "customers.csv", "id,name\n1,Alice\n2,Bob\n");
        write_csv(&sub2, "products.csv", "pid,price\n10,5.99\n11,12.50\n");
        // Also a file at the top level
        write_csv(dir.path(), "meta.csv", "key,val\na,1\nb,2\n");

        let results = ingest_directory(dir.path()).unwrap();
        assert_eq!(results.len(), 3);
        let names: Vec<&str> = results.iter().map(|r| r.entity.as_str()).collect();
        assert!(names.contains(&"customers"));
        assert!(names.contains(&"products"));
        assert!(names.contains(&"meta"));
    }

    #[test]
    fn directory_ingestion_disambiguates_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let sub1 = dir.path().join("alpha");
        let sub2 = dir.path().join("beta");
        std::fs::create_dir(&sub1).unwrap();
        std::fs::create_dir(&sub2).unwrap();

        // Both subdirs have a file named "test.csv"
        write_csv(&sub1, "test.csv", "id,name\n1,Alice\n2,Bob\n");
        write_csv(&sub2, "test.csv", "id,val\n10,x\n11,y\n");

        let results = ingest_directory(dir.path()).unwrap();
        assert_eq!(results.len(), 2);
        let names: Vec<&str> = results.iter().map(|r| r.entity.as_str()).collect();
        // Should be disambiguated with parent dir prefix
        assert!(
            names.contains(&"alpha_test"),
            "expected alpha_test, got {names:?}"
        );
        assert!(
            names.contains(&"beta_test"),
            "expected beta_test, got {names:?}"
        );
    }

    #[test]
    fn truncate_batches_limits_rows() {
        use arrow::array::Int32Array;
        use arrow::datatypes::{Field as ArrowField, Schema as ArrowSchema};

        let schema = Arc::new(ArrowSchema::new(vec![ArrowField::new(
            "x",
            arrow::datatypes::DataType::Int32,
            false,
        )]));

        // Create 3 batches of 10 rows each (30 total)
        let batches: Vec<RecordBatch> = (0..3)
            .map(|i| {
                let arr = Int32Array::from((i * 10..(i + 1) * 10).collect::<Vec<i32>>());
                RecordBatch::try_new(schema.clone(), vec![Arc::new(arr)]).unwrap()
            })
            .collect();

        // Truncate to 15 rows: should get first batch (10) + half of second (5)
        let result = truncate_batches(batches.clone(), 15);
        let total: usize = result.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 15);
        assert_eq!(result.len(), 2);

        // Truncate to 0: should get nothing
        let result = truncate_batches(batches.clone(), 0);
        assert!(result.is_empty());

        // Truncate to more than available: should get all
        let result = truncate_batches(batches, 100);
        let total: usize = result.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 30);
    }

    #[test]
    fn ingest_directory_with_limit_truncates() {
        let dir = tempfile::tempdir().unwrap();
        // 5 rows of data
        write_csv(dir.path(), "data.csv", "id,val\n1,a\n2,b\n3,c\n4,d\n5,e\n");

        let results = ingest_directory_with_limit(dir.path(), Some(3)).unwrap();
        assert_eq!(results.len(), 1);
        let total: usize = results[0].batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 3);
    }
}
