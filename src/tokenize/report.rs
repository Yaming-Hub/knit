//! Tokenization report generation.
//!
//! Produces a detailed summary of what was tokenized, including per-file
//! breakdown, token statistics, and coverage metrics.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use crate::tokenize::dictionary::TokenDictionary;
use crate::tokenize::scanner::{scan_directory, FileKind};

/// Summary report of a tokenization run.
#[derive(Debug)]
pub struct TokenizeReport {
    /// Per-file statistics.
    pub files: Vec<FileReport>,
    /// Global token statistics.
    pub token_stats: TokenStats,
    /// Whether native date shifting was applied.
    pub date_shift_applied: bool,
    /// Whether native numeric shifting was applied.
    pub numeric_shift_applied: bool,
    /// Date shift offset in days (if applied).
    pub date_shift_days: Option<i64>,
    /// Numeric shift offset (if applied).
    pub numeric_shift: Option<i64>,
}

/// Per-file statistics.
#[derive(Debug)]
pub struct FileReport {
    /// Relative path of the file.
    pub path: String,
    /// File classification (Data, Schema, etc.).
    pub kind: String,
    /// File format (csv, json, parquet, etc.).
    pub format: String,
    /// Number of rows (data files only).
    pub rows: Option<usize>,
    /// Number of columns (data files only).
    pub columns: Option<usize>,
}

/// Global token statistics.
#[derive(Debug)]
pub struct TokenStats {
    /// Total unique tokens in the dictionary.
    pub unique_tokens: usize,
    /// Distribution of original value lengths (length → count).
    pub original_length_distribution: BTreeMap<usize, usize>,
    /// Distribution of token value lengths (length → count).
    pub token_length_distribution: BTreeMap<usize, usize>,
    /// Number of tokens that preserved the original length.
    pub length_preserved_count: usize,
    /// Seed used for tokenization.
    pub seed: u64,
}

/// Generate a tokenization report from the output directory and dictionary.
pub fn generate_report(
    output_dir: &Path,
    dict_path: &Path,
) -> Result<TokenizeReport> {
    let dict = TokenDictionary::read(dict_path)
        .with_context(|| format!("reading dictionary from {}", dict_path.display()))?;

    let entries = scan_directory(output_dir)?;

    let mut files = Vec::new();
    for entry in &entries {
        if entry.rel_path.to_string_lossy().contains(".knit-tokens") {
            continue;
        }

        let kind = match entry.kind {
            FileKind::Data => "data",
            FileKind::Schema => "schema",
            FileKind::Dictionary => "dictionary",
            FileKind::Companion => "companion",
        };

        let (rows, columns) = if entry.kind == FileKind::Data {
            count_rows_cols(&output_dir.join(&entry.rel_path), entry.format.as_str())
                .unwrap_or((None, None))
        } else {
            (None, None)
        };

        files.push(FileReport {
            path: entry.rel_path.to_string_lossy().to_string(),
            kind: kind.to_string(),
            format: entry.format.as_str().to_string(),
            rows,
            columns,
        });
    }

    // Compute token statistics
    let mut orig_len_dist: BTreeMap<usize, usize> = BTreeMap::new();
    let mut tok_len_dist: BTreeMap<usize, usize> = BTreeMap::new();
    let mut length_preserved = 0usize;

    for (orig, tok) in &dict.tokens {
        *orig_len_dist.entry(orig.len()).or_insert(0) += 1;
        *tok_len_dist.entry(tok.len()).or_insert(0) += 1;
        if orig.len() == tok.len() {
            length_preserved += 1;
        }
    }

    let token_stats = TokenStats {
        unique_tokens: dict.tokens.len(),
        original_length_distribution: orig_len_dist,
        token_length_distribution: tok_len_dist,
        length_preserved_count: length_preserved,
        seed: dict.seed,
    };

    Ok(TokenizeReport {
        files,
        token_stats,
        date_shift_applied: dict.date_shift_days.is_some(),
        numeric_shift_applied: dict.numeric_shift.is_some(),
        date_shift_days: dict.date_shift_days,
        numeric_shift: dict.numeric_shift,
    })
}

/// Count rows and columns for a data file.
fn count_rows_cols(path: &Path, format: &str) -> Result<(Option<usize>, Option<usize>)> {
    match format {
        "csv" | "tsv" => count_csv_rows_cols(path),
        "json" | "jsonl" => count_jsonl_rows(path),
        "parquet" => count_parquet_rows_cols(path),
        _ => Ok((None, None)),
    }
}

fn count_csv_rows_cols(path: &Path) -> Result<(Option<usize>, Option<usize>)> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut lines = reader.lines();
    let columns = lines
        .next()
        .and_then(|l| l.ok())
        .map(|header| header.split(',').count());
    let rows = lines.count(); // remaining lines after header
    Ok((Some(rows), columns))
}

fn count_jsonl_rows(path: &Path) -> Result<(Option<usize>, Option<usize>)> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path)?;
    let reader = BufReader::new(file);
    let mut rows = 0;
    let mut columns = None;
    for line in reader.lines() {
        let line = line?;
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed == "[" || trimmed == "]" {
            continue;
        }
        rows += 1;
        if columns.is_none() {
            // Try to parse first data line to count fields
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed.trim_end_matches(',')) {
                if let Some(obj) = val.as_object() {
                    columns = Some(obj.len());
                }
            }
        }
    }
    Ok((Some(rows), columns))
}

fn count_parquet_rows_cols(path: &Path) -> Result<(Option<usize>, Option<usize>)> {
    use parquet::file::reader::{FileReader, SerializedFileReader};
    let file = std::fs::File::open(path)?;
    let reader = SerializedFileReader::new(file)?;
    let metadata = reader.metadata();
    let rows: i64 = (0..metadata.num_row_groups())
        .map(|i| metadata.row_group(i).num_rows())
        .sum();
    let columns = metadata.file_metadata().schema_descr().num_columns();
    Ok((Some(rows as usize), Some(columns)))
}

/// Format the report as human-readable text.
pub fn format_text(report: &TokenizeReport) -> String {
    let mut out = String::new();
    out.push_str("═══ Tokenization Report ═══\n\n");

    // File summary
    let data_count = report.files.iter().filter(|f| f.kind == "data").count();
    let schema_count = report.files.iter().filter(|f| f.kind == "schema").count();
    let companion_count = report.files.iter().filter(|f| f.kind == "companion").count();
    out.push_str(&format!(
        "  Files: {} data, {} schema, {} companion\n\n",
        data_count, schema_count, companion_count
    ));

    // Per-file detail
    if !report.files.iter().any(|f| f.kind == "data") {
        out.push_str("  No data files found.\n\n");
    } else {
        out.push_str("  File Details:\n");
        out.push_str(&format!(
            "    {:<40} {:>8} {:>8} {:>8}\n",
            "Path", "Format", "Rows", "Columns"
        ));
        out.push_str(&format!("    {}\n", "─".repeat(68)));
        for f in &report.files {
            if f.kind != "data" {
                continue;
            }
            let rows = f.rows.map(|r| r.to_string()).unwrap_or_else(|| "—".to_string());
            let cols = f.columns.map(|c| c.to_string()).unwrap_or_else(|| "—".to_string());
            out.push_str(&format!(
                "    {:<40} {:>8} {:>8} {:>8}\n",
                truncate_path(&f.path, 40),
                f.format,
                rows,
                cols
            ));
        }
        out.push('\n');
    }

    // Token statistics
    out.push_str("  Token Statistics:\n");
    out.push_str(&format!("    unique tokens:       {}\n", report.token_stats.unique_tokens));
    out.push_str(&format!("    length preserved:    {} ({:.0}%)\n",
        report.token_stats.length_preserved_count,
        if report.token_stats.unique_tokens > 0 {
            report.token_stats.length_preserved_count as f64 / report.token_stats.unique_tokens as f64 * 100.0
        } else {
            0.0
        }
    ));
    out.push_str(&format!("    seed:                {}\n", report.token_stats.seed));

    // Length distribution summary (top 5 most common)
    if !report.token_stats.original_length_distribution.is_empty() {
        let mut sorted: Vec<_> = report.token_stats.original_length_distribution.iter().collect();
        sorted.sort_by(|a, b| b.1.cmp(a.1));
        out.push_str("    value length distribution (top 5):\n");
        for (len, count) in sorted.iter().take(5) {
            out.push_str(&format!("      {:>3} chars: {} values\n", len, count));
        }
    }
    out.push('\n');

    // Native column handling
    out.push_str("  Native Column Handling:\n");
    if report.date_shift_applied {
        out.push_str(&format!("    temporal shift:  {} days\n",
            report.date_shift_days.unwrap_or(0)));
    } else {
        out.push_str("    temporal shift:  not applied\n");
    }
    if report.numeric_shift_applied {
        out.push_str(&format!("    numeric shift:   offset {}\n",
            report.numeric_shift.unwrap_or(0)));
    } else {
        out.push_str("    numeric shift:   not applied\n");
    }
    out.push('\n');

    out
}

/// Format the report as JSON.
pub fn format_json(report: &TokenizeReport) -> serde_json::Value {
    let files: Vec<serde_json::Value> = report
        .files
        .iter()
        .map(|f| {
            let mut obj = serde_json::json!({
                "path": f.path,
                "kind": f.kind,
                "format": f.format,
            });
            if let Some(rows) = f.rows {
                obj["rows"] = serde_json::json!(rows);
            }
            if let Some(cols) = f.columns {
                obj["columns"] = serde_json::json!(cols);
            }
            obj
        })
        .collect();

    let mut json = serde_json::json!({
        "event": "tokenize_report",
        "files": files,
        "token_stats": {
            "unique_tokens": report.token_stats.unique_tokens,
            "length_preserved": report.token_stats.length_preserved_count,
            "seed": report.token_stats.seed,
        },
        "native_shifts": {
            "date_shift_applied": report.date_shift_applied,
            "numeric_shift_applied": report.numeric_shift_applied,
        },
    });

    if let Some(d) = report.date_shift_days {
        json["native_shifts"]["date_shift_days"] = serde_json::json!(d);
    }
    if let Some(n) = report.numeric_shift {
        json["native_shifts"]["numeric_shift"] = serde_json::json!(n);
    }

    json
}

fn truncate_path(path: &str, max_len: usize) -> String {
    if path.len() <= max_len {
        path.to_string()
    } else {
        let suffix = &path[path.len() - (max_len - 1)..];
        format!("…{}", suffix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    #[test]
    fn test_truncate_path() {
        assert_eq!(truncate_path("short.csv", 40), "short.csv");
        let long = "a".repeat(50);
        let truncated = truncate_path(&long, 40);
        assert!(truncated.starts_with('…'));
        // Display width is 40 (1 for ellipsis + 39 ASCII chars)
        assert_eq!(truncated.chars().count(), 40);
    }

    #[test]
    fn test_format_text_empty() {
        let report = TokenizeReport {
            files: vec![],
            token_stats: TokenStats {
                unique_tokens: 0,
                original_length_distribution: BTreeMap::new(),
                token_length_distribution: BTreeMap::new(),
                length_preserved_count: 0,
                seed: 42,
            },
            date_shift_applied: false,
            numeric_shift_applied: false,
            date_shift_days: None,
            numeric_shift: None,
        };
        let text = format_text(&report);
        assert!(text.contains("Tokenization Report"));
        assert!(text.contains("unique tokens:       0"));
    }

    #[test]
    fn test_format_text_with_files() {
        let report = TokenizeReport {
            files: vec![
                FileReport {
                    path: "users.csv".to_string(),
                    kind: "data".to_string(),
                    format: "csv".to_string(),
                    rows: Some(100),
                    columns: Some(5),
                },
                FileReport {
                    path: "schema.json".to_string(),
                    kind: "schema".to_string(),
                    format: "json".to_string(),
                    rows: None,
                    columns: None,
                },
            ],
            token_stats: TokenStats {
                unique_tokens: 50,
                original_length_distribution: BTreeMap::from([(5, 30), (10, 20)]),
                token_length_distribution: BTreeMap::from([(5, 30), (10, 20)]),
                length_preserved_count: 50,
                seed: 42,
            },
            date_shift_applied: true,
            numeric_shift_applied: false,
            date_shift_days: Some(365),
            numeric_shift: None,
        };
        let text = format_text(&report);
        assert!(text.contains("users.csv"));
        assert!(text.contains("100"));
        assert!(text.contains("unique tokens:       50"));
        assert!(text.contains("length preserved:    50 (100%)"));
        assert!(text.contains("365 days"));
    }

    #[test]
    fn test_format_json() {
        let report = TokenizeReport {
            files: vec![FileReport {
                path: "data.parquet".to_string(),
                kind: "data".to_string(),
                format: "parquet".to_string(),
                rows: Some(1000),
                columns: Some(10),
            }],
            token_stats: TokenStats {
                unique_tokens: 200,
                original_length_distribution: BTreeMap::new(),
                token_length_distribution: BTreeMap::new(),
                length_preserved_count: 180,
                seed: 99,
            },
            date_shift_applied: false,
            numeric_shift_applied: true,
            date_shift_days: None,
            numeric_shift: Some(-5000),
        };
        let json = format_json(&report);
        assert_eq!(json["token_stats"]["unique_tokens"], 200);
        assert_eq!(json["native_shifts"]["numeric_shift"], -5000);
        assert_eq!(json["files"][0]["rows"], 1000);
    }

    #[test]
    fn test_generate_report_csv() {
        let dir = TempDir::new().unwrap();
        let data_dir = dir.path().join("output");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Create a CSV file
        std::fs::write(
            data_dir.join("users.csv"),
            "name,age,city\nAlice,30,NYC\nBob,25,LA\n",
        ).unwrap();

        // Create a minimal dictionary
        let dict = TokenDictionary {
            version: 1,
            seed: 42,
            stats: crate::tokenize::dictionary::DictionaryStats { unique_tokens: 2 },
            column_filter: Default::default(),
            date_shift_days: None,
            numeric_shift: None,
            tokens: BTreeMap::from([
                ("Alice".to_string(), "Xkmpq".to_string()),
                ("Bob".to_string(), "Bvr".to_string()),
            ]),
        };
        let dict_path = data_dir.join(".knit-tokens.json");
        dict.write(&dict_path).unwrap();

        let report = generate_report(&data_dir, &dict_path).unwrap();
        assert_eq!(report.files.len(), 1); // dictionary excluded
        assert_eq!(report.files[0].path, "users.csv");
        assert_eq!(report.files[0].rows, Some(2));
        assert_eq!(report.files[0].columns, Some(3));
        assert_eq!(report.token_stats.unique_tokens, 2);
        assert_eq!(report.token_stats.length_preserved_count, 2); // "Alice"(5)->"Xkmpq"(5), "Bob"(3)->"Bvr"(3)
    }

    #[test]
    fn test_generate_report_parquet() {
        use arrow::array::StringArray;
        use arrow::datatypes::{Field, Schema, DataType};
        use arrow::record_batch::RecordBatch;
        use parquet::arrow::ArrowWriter;
        use std::sync::Arc;

        let dir = TempDir::new().unwrap();
        let data_dir = dir.path().join("output");
        std::fs::create_dir_all(&data_dir).unwrap();

        // Create a Parquet file
        let schema = Arc::new(Schema::new(vec![
            Field::new("name", DataType::Utf8, false),
            Field::new("city", DataType::Utf8, false),
        ]));
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec!["Alice", "Bob", "Carol"])),
                Arc::new(StringArray::from(vec!["NYC", "LA", "SF"])),
            ],
        ).unwrap();
        let pq_path = data_dir.join("users.parquet");
        let file = std::fs::File::create(&pq_path).unwrap();
        let mut writer = ArrowWriter::try_new(file, schema, None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        let dict = TokenDictionary {
            version: 1,
            seed: 42,
            stats: crate::tokenize::dictionary::DictionaryStats { unique_tokens: 3 },
            column_filter: Default::default(),
            date_shift_days: Some(100),
            numeric_shift: Some(5000),
            tokens: BTreeMap::from([
                ("Alice".to_string(), "Xkmpq".to_string()),
                ("Bob".to_string(), "Bvr".to_string()),
                ("NYC".to_string(), "XYZ".to_string()),
            ]),
        };
        let dict_path = data_dir.join(".knit-tokens.json");
        dict.write(&dict_path).unwrap();

        let report = generate_report(&data_dir, &dict_path).unwrap();
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].rows, Some(3));
        assert_eq!(report.files[0].columns, Some(2));
        assert!(report.date_shift_applied);
        assert!(report.numeric_shift_applied);
        assert_eq!(report.date_shift_days, Some(100));
        assert_eq!(report.numeric_shift, Some(5000));
    }
}
