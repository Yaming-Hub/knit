//! Data ingestion readers for CSV, Parquet, and JSON/JSONL files.
//!
//! Each reader returns `Vec<RecordBatch>` for simplicity.
//! Multi-file ingestion maps a directory of files to entity-level batches.
//!
//! ## Companion Schema Support
//!
//! When ingesting a directory, the module detects structured dataset layouts
//! that include companion `Schema/schema.json` files alongside data files.
//! This metadata provides richer type information, dictionary encodings,
//! and row-type discriminator patterns that improve learned schema quality.

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::datatypes::Schema;
use arrow::record_batch::RecordBatch;
use arrow_csv::ReaderBuilder as CsvReaderBuilder;
use arrow_json::ReaderBuilder as JsonReaderBuilder;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::{Deserialize, Serialize};
use serde_json;
use tracing::{debug, info, info_span, warn};

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
        if let Some(limit) = max_rows
            && rows_read >= limit {
                break;
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

// ── Companion Schema types ──────────────────────────────────────────

/// Parsed companion schema from a `schema.json` file alongside data files.
///
/// Provides richer metadata than Arrow schema inference alone: dictionary
/// encodings, row-type discriminators, and semantic data types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionSchema {
    /// Schema format version (expected: 1).
    #[serde(default)]
    pub schema_format_version: u32,
    /// Table / entity name from the schema.
    pub table_name: String,
    /// Whether the CSV has headers.
    #[serde(default = "default_true_fn")]
    pub has_headers: bool,
    /// Index of the row-type discriminator column (if any).
    pub row_type_column_index: Option<usize>,
    /// Column metadata.
    #[serde(default)]
    pub columns: Vec<CompanionColumn>,
    /// Dictionary definitions.
    #[serde(default)]
    pub dictionaries: Vec<CompanionDictionary>,
    /// Additional row types present in the dataset.
    #[serde(default)]
    pub additional_row_types: Vec<u32>,
    /// Relative path to dictionary files from the schema.json location.
    #[serde(default)]
    pub dictionary_path: Option<String>,
}

fn default_true_fn() -> bool {
    true
}

/// Column metadata from a companion schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionColumn {
    /// Column name.
    pub name: String,
    /// Column index in the CSV.
    pub col_number: usize,
    /// Semantic data type (e.g., "Int64", "UnixDateTime", "OptionalInt64", "Boolean").
    pub data_type: String,
    /// Dictionary ID if this column uses dictionary encoding.
    pub dictionary_id: Option<u32>,
    /// Row type value: this column is only populated when the discriminator equals this value.
    pub row_type: Option<u32>,
}

/// Dictionary definition from a companion schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompanionDictionary {
    /// Dictionary name (e.g., "InternalPersonId").
    pub name: String,
    /// Dictionary ID (referenced by columns).
    pub id: u32,
    /// Relative path to the dictionary CSV file (from `dictionary_path`).
    pub path: String,
    /// Maximum encoded ID value.
    pub max_encoded_id: u64,
}

impl CompanionSchema {
    /// Parse a companion schema from a JSON file.
    pub fn from_file(path: &Path) -> LearnResult<Self> {
        let file = File::open(path)?;
        let schema: CompanionSchema = serde_json::from_reader(file).map_err(|e| {
            LearnError::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("failed to parse companion schema {}: {e}", path.display()),
            ))
        })?;
        debug!(
            table = %schema.table_name,
            columns = schema.columns.len(),
            dictionaries = schema.dictionaries.len(),
            row_type_col = ?schema.row_type_column_index,
            "parsed companion schema"
        );
        Ok(schema)
    }

    /// Get dictionary info for a column by name.
    pub fn dictionary_for_column(&self, col_name: &str) -> Option<&CompanionDictionary> {
        let col = self.columns.iter().find(|c| c.name == col_name)?;
        let dict_id = col.dictionary_id?;
        self.dictionaries.iter().find(|d| d.id == dict_id)
    }

    /// Get the row-type value for a column, if any.
    pub fn row_type_for_column(&self, col_name: &str) -> Option<u32> {
        self.columns
            .iter()
            .find(|c| c.name == col_name)
            .and_then(|c| c.row_type)
    }

    /// Get the discriminator column name (if `row_type_column_index` is set).
    pub fn discriminator_column(&self) -> Option<&str> {
        let idx = self.row_type_column_index?;
        self.columns
            .iter()
            .find(|c| c.col_number == idx)
            .map(|c| c.name.as_str())
    }

    /// Resolve the absolute path to dictionary files.
    pub fn resolve_dictionary_dir(&self, schema_json_path: &Path) -> Option<PathBuf> {
        let dict_rel = self.dictionary_path.as_deref()?;
        let schema_dir = schema_json_path.parent()?;
        let resolved = schema_dir.join(dict_rel);
        if resolved.is_dir() {
            Some(resolved)
        } else {
            None
        }
    }

    /// Build a lookup from column name to CompanionColumn.
    pub fn column_map(&self) -> HashMap<String, &CompanionColumn> {
        self.columns.iter().map(|c| (c.name.clone(), c)).collect()
    }
}

/// Result of multi-file ingestion: maps entity name → batches.
#[derive(Debug)]
pub struct IngestionResult {
    /// Entity name derived from the file stem or companion schema.
    pub entity: String,
    /// Schema of the ingested data.
    pub schema: Arc<Schema>,
    /// Record batches.
    pub batches: Vec<RecordBatch>,
    /// Companion schema metadata (if a `Schema/schema.json` was found).
    pub companion: Option<CompanionSchema>,
    /// Path to the companion schema.json file (for resolving relative paths).
    pub companion_path: Option<PathBuf>,
    /// Relative path from the dataset root to the entity's data directory
    /// (e.g. `"Collab/Results"`). Used to reproduce folder hierarchy in output.
    pub source_layout: Option<String>,
    /// Hive-style partition key name (e.g. `"PartitionDate"`) detected from
    /// directory names like `PartitionDate=2024-10-13`.
    pub partition_by: Option<String>,
    /// Observed partition values with row proportions.
    pub partition_values: Vec<crate::core::PartitionValue>,
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
                if let Ok(canonical) = path.canonicalize()
                    && visited.insert(canonical) {
                        dirs.push(path);
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
    let _span = info_span!("ingest", dir = %dir.display()).entered();
    info!(?max_rows, "ingesting directory");

    // Try structured dataset discovery first (companion schema.json layout)
    if let Some(results) = try_structured_ingest(dir, max_rows)? {
        info!(
            entities = results.len(),
            "Structured dataset ingestion complete"
        );
        return Ok(results);
    }

    // Fall back to flat file discovery
    ingest_directory_flat(dir, max_rows)
}

/// Try structured dataset discovery: detect `EntityDir/Schema/schema.json` layout.
///
/// Returns `Some(results)` if at least one companion schema was found, `None` otherwise.
fn try_structured_ingest(
    dir: &Path,
    max_rows: Option<usize>,
) -> LearnResult<Option<Vec<IngestionResult>>> {
    // Scan immediate subdirectories for Schema/schema.json
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(None),
    };

    let mut discovered: Vec<(String, PathBuf, Vec<PathBuf>, CompanionSchema)> = Vec::new();

    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let schema_json = path.join("Schema").join("schema.json");
        if !schema_json.exists() {
            // Also try lowercase
            let schema_json_lc = path.join("schema").join("schema.json");
            if !schema_json_lc.exists() {
                continue;
            }
        }

        let schema_path = if path.join("Schema").join("schema.json").exists() {
            path.join("Schema").join("schema.json")
        } else {
            path.join("schema").join("schema.json")
        };

        match CompanionSchema::from_file(&schema_path) {
            Ok(companion) => {
                let entity_name = companion.table_name.clone();

                // Find data files in Results/ subdirectory or entity directory itself
                let data_dir = if path.join("Results").is_dir() {
                    path.join("Results")
                } else if path.join("results").is_dir() {
                    path.join("results")
                } else {
                    path.clone()
                };

                let mut data_files: Vec<PathBuf> = Vec::new();
                if let Ok(data_entries) = std::fs::read_dir(&data_dir) {
                    for de in data_entries.filter_map(|e| e.ok()) {
                        let dp = de.path();
                        if dp.is_file() {
                            let ext = dp
                                .extension()
                                .and_then(|e| e.to_str())
                                .unwrap_or("")
                                .to_lowercase();
                            if ["csv", "tsv", "parquet", "json", "jsonl"].contains(&ext.as_str()) {
                                data_files.push(dp);
                            }
                        } else if dp.is_dir() {
                            // Check partition subdirectories (e.g., PartitionDate=...)
                            if let Ok(sub_entries) = std::fs::read_dir(&dp) {
                                for se in sub_entries.filter_map(|e| e.ok()) {
                                    let sp = se.path();
                                    if sp.is_file() {
                                        let ext = sp
                                            .extension()
                                            .and_then(|e| e.to_str())
                                            .unwrap_or("")
                                            .to_lowercase();
                                        if ["csv", "tsv", "parquet", "json", "jsonl"]
                                            .contains(&ext.as_str())
                                        {
                                            data_files.push(sp);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }

                if data_files.is_empty() {
                    warn!(entity = %entity_name, "companion schema found but no data files");
                    continue;
                }

                data_files.sort();
                info!(
                    entity = %entity_name,
                    files = data_files.len(),
                    "discovered structured entity"
                );
                discovered.push((entity_name, schema_path, data_files, companion));
            }
            Err(e) => {
                warn!(path = %schema_path.display(), error = %e, "failed to parse companion schema");
            }
        }
    }

    if discovered.is_empty() {
        return Ok(None);
    }

    // Ingest discovered entities
    let mut results = Vec::new();
    for (entity_name, schema_path, data_files, companion) in discovered {
        let mut all_batches: Vec<RecordBatch> = Vec::new();
        let mut total_rows: usize = 0;
        let row_limit = max_rows.unwrap_or(usize::MAX);

        // Track partition key and per-partition row counts
        let mut partition_key: Option<String> = None;
        let mut partition_row_counts: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();

        for file in &data_files {
            if total_rows >= row_limit {
                break;
            }

            // Detect hive partition from parent directory name (e.g. PartitionDate=2024-10-13)
            if let Some(parent) = file.parent()
                && let Some(dir_name) = parent.file_name().and_then(|n| n.to_str())
                    && let Some(eq_pos) = dir_name.find('=') {
                        let key = &dir_name[..eq_pos];
                        let value = &dir_name[eq_pos + 1..];
                        if partition_key.is_none() {
                            partition_key = Some(key.to_string());
                        }
                        // Count rows per partition value (updated after reading)
                        let remaining = row_limit - total_rows;
                        let batches = read_auto_with_limit(file, Some(remaining))?;
                        let mut file_rows = 0;
                        for b in batches {
                            file_rows += b.num_rows();
                            total_rows += b.num_rows();
                            all_batches.push(b);
                        }
                        *partition_row_counts.entry(value.to_string()).or_insert(0) += file_rows;
                        continue;
                    }

            let remaining = row_limit - total_rows;
            let batches = read_auto_with_limit(file, Some(remaining))?;
            for b in batches {
                total_rows += b.num_rows();
                all_batches.push(b);
            }
        }

        if all_batches.is_empty() {
            continue;
        }

        // Compute a merged schema that picks the most specific type for each column
        // (e.g., Null → Utf8 if any partition has Utf8 for that column)
        let schema = merge_schemas(&all_batches);
        // Re-schema batches if needed (partitioned files may have slightly different schemas)
        let all_batches = unify_batch_schemas(all_batches, &schema);

        // Compute relative path from dataset root to data directory for output layout
        let source_layout = data_files.first().and_then(|f| {
            f.parent()
                .and_then(|p| p.strip_prefix(dir).ok())
                .map(|rel| {
                    // For partitioned dirs, go up one level (Results/PartitionDate=... → Results)
                    // Check if the immediate parent directory name contains '=' (partition key)
                    let parent_name = rel.file_name().and_then(|n| n.to_str()).unwrap_or("");
                    if parent_name.contains('=') {
                        // It's a partition subdir; use grandparent
                        rel.parent()
                            .map(|pp| pp.to_string_lossy().to_string())
                            .unwrap_or_else(|| rel.to_string_lossy().to_string())
                    } else {
                        rel.to_string_lossy().to_string()
                    }
                })
        });

        // Build partition values with weights from row counts
        let partition_values: Vec<crate::core::PartitionValue> = if !partition_row_counts.is_empty()
        {
            let total = partition_row_counts.values().sum::<usize>() as f64;
            let mut pvs: Vec<crate::core::PartitionValue> = partition_row_counts
                .iter()
                .map(|(v, &count)| crate::core::PartitionValue {
                    value: v.clone(),
                    weight: count as f64 / total,
                })
                .collect();
            pvs.sort_by(|a, b| a.value.cmp(&b.value));
            pvs
        } else {
            Vec::new()
        };

        results.push(IngestionResult {
            entity: entity_name,
            schema,
            batches: all_batches,
            companion: Some(companion),
            companion_path: Some(schema_path),
            source_layout,
            partition_by: partition_key,
            partition_values,
        });
    }

    Ok(Some(results))
}

/// Compute a unified schema from multiple batches, picking the most specific
/// type for each column (e.g., `Null` → `Utf8` when another batch has `Utf8`).
fn merge_schemas(batches: &[RecordBatch]) -> Arc<Schema> {
    use arrow::datatypes::{DataType as ArrowDT, Field};
    if batches.is_empty() {
        return Arc::new(Schema::empty());
    }
    let first = batches[0].schema();
    let merged_fields: Vec<Field> = first
        .fields()
        .iter()
        .enumerate()
        .map(|(i, f)| {
            let mut best_dt = f.data_type().clone();
            for b in batches.iter().skip(1) {
                if i < b.num_columns() {
                    let other_dt = b.schema().field(i).data_type().clone();
                    if best_dt == ArrowDT::Null && other_dt != ArrowDT::Null {
                        best_dt = other_dt;
                    }
                }
            }
            Field::new(f.name(), best_dt, true)
        })
        .collect();
    Arc::new(Schema::new(merged_fields))
}

/// Unify record batch schemas: re-cast columns to match the target schema.
/// Handles partitions with fewer columns (fills with nulls) and type mismatches
/// (casts Null → concrete type).
fn unify_batch_schemas(batches: Vec<RecordBatch>, target: &Arc<Schema>) -> Vec<RecordBatch> {
    use arrow::array::new_null_array;
    use arrow::compute::cast;
    batches
        .into_iter()
        .filter_map(|b| {
            if b.schema() == *target {
                Some(b)
            } else {
                let num_rows = b.num_rows();
                // Cast each column to the target type
                let columns: Vec<_> = (0..target.fields().len())
                    .map(|i| {
                        let target_dt = target.field(i).data_type();
                        if i >= b.num_columns() {
                            // Partition has fewer columns — fill with nulls
                            new_null_array(target_dt, num_rows)
                        } else {
                            let col = b.column(i);
                            if col.data_type() == target_dt {
                                col.clone()
                            } else if *col.data_type() == arrow::datatypes::DataType::Null {
                                new_null_array(target_dt, col.len())
                            } else {
                                cast(col, target_dt)
                                    .unwrap_or_else(|_| new_null_array(target_dt, col.len()))
                            }
                        }
                    })
                    .collect();
                RecordBatch::try_new(target.clone(), columns).ok()
            }
        })
        .collect()
}

/// Flat file discovery (original behavior, no companion schema).
fn ingest_directory_flat(dir: &Path, max_rows: Option<usize>) -> LearnResult<Vec<IngestionResult>> {
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
            companion: None,
            companion_path: None,
            source_layout: None,
            partition_by: None,
            partition_values: Vec::new(),
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

    #[test]
    fn structured_ingest_detects_hive_partitions() {
        // Build a mini structured dataset with hive-partitioned layout:
        //   Entity/Schema/schema.json
        //   Entity/Results/Date=2024-01-01/part.csv
        //   Entity/Results/Date=2024-01-02/part.csv
        let dir = tempfile::tempdir().unwrap();
        let entity_dir = dir.path().join("Items");
        let schema_dir = entity_dir.join("Schema");
        let results_dir = entity_dir.join("Results");
        let part1_dir = results_dir.join("Date=2024-01-01");
        let part2_dir = results_dir.join("Date=2024-01-02");

        std::fs::create_dir_all(&schema_dir).unwrap();
        std::fs::create_dir_all(&part1_dir).unwrap();
        std::fs::create_dir_all(&part2_dir).unwrap();

        // Companion schema with camelCase fields matching CompanionSchema struct
        let schema_json = r#"{
            "tableName": "Items",
            "schemaFormatVersion": 1,
            "columns": [
                {"name": "id", "colNumber": 0, "dataType": "Int64"},
                {"name": "value", "colNumber": 1, "dataType": "String"}
            ]
        }"#;
        std::fs::write(schema_dir.join("schema.json"), schema_json).unwrap();

        // Partition 1: 3 rows
        write_csv(&part1_dir, "part.csv", "id,value\n1,a\n2,b\n3,c\n");
        // Partition 2: 2 rows
        write_csv(&part2_dir, "part.csv", "id,value\n4,d\n5,e\n");

        let results = try_structured_ingest(dir.path(), None).unwrap().unwrap();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert_eq!(r.entity, "Items");

        // Should detect partition key
        assert_eq!(r.partition_by.as_deref(), Some("Date"));

        // Should have 2 partition values
        assert_eq!(r.partition_values.len(), 2);
        let pv_map: std::collections::HashMap<&str, f64> = r
            .partition_values
            .iter()
            .map(|pv| (pv.value.as_str(), pv.weight))
            .collect();
        // 3 out of 5 rows = 0.6, 2 out of 5 = 0.4
        assert!((pv_map["2024-01-01"] - 0.6).abs() < 0.01);
        assert!((pv_map["2024-01-02"] - 0.4).abs() < 0.01);

        // Total rows should be 5
        let total: usize = r.batches.iter().map(|b| b.num_rows()).sum();
        assert_eq!(total, 5);

        // source_layout should point above partition dirs
        assert_eq!(r.source_layout.as_deref(), Some("Items\\Results"));
    }

    #[test]
    fn structured_ingest_no_partitions_has_none() {
        // Non-partitioned structured layout
        let dir = tempfile::tempdir().unwrap();
        let entity_dir = dir.path().join("Simple");
        let schema_dir = entity_dir.join("Schema");
        let results_dir = entity_dir.join("Results");

        std::fs::create_dir_all(&schema_dir).unwrap();
        std::fs::create_dir_all(&results_dir).unwrap();

        let schema_json = r#"{
            "tableName": "Simple",
            "schemaFormatVersion": 1,
            "columns": [{"name": "x", "colNumber": 0, "dataType": "Int64"}]
        }"#;
        std::fs::write(schema_dir.join("schema.json"), schema_json).unwrap();
        write_csv(&results_dir, "data.csv", "x\n10\n20\n");

        let results = try_structured_ingest(dir.path(), None).unwrap().unwrap();
        assert_eq!(results.len(), 1);
        let r = &results[0];
        assert!(r.partition_by.is_none());
        assert!(r.partition_values.is_empty());
    }
}
