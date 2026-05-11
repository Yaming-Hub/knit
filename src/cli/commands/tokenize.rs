//! CLI handler for `knit tokenize`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::{bail, Result};

use crate::tokenize::{self, TokenizeConfig, TokenizeResult};

/// Run the tokenize command.
pub fn run(
    input: &str,
    output: &str,
    dictionary: Option<&str>,
    restore_mode: bool,
    verify_path: Option<&str>,
    seed: Option<u64>,
    tokenize_numbers: bool,
    tokenize_dates: bool,
    tokenize_headers: bool,
    preserve_partitions: bool,
    tokenize_columns: Option<Vec<String>>,
    preserve_columns: Option<Vec<String>>,
    report: bool,
    output_format: Option<&str>,
    json_output: bool,
) -> Result<()> {
    let input_path = Path::new(input);
    let output_path = Path::new(output);

    if !input_path.exists() {
        bail!("input path does not exist: {}", input);
    }

    // Validate --output-format value
    let fmt = if let Some(fmt_str) = output_format {
        let parsed = crate::tokenize::scanner::FileFormat::parse(fmt_str)
            .ok_or_else(|| anyhow::anyhow!(
                "invalid output format '{}': expected csv, tsv, json, jsonl, or parquet",
                fmt_str
            ))?;
        Some(parsed)
    } else {
        None
    };

    // Column filter flags are not valid in restore or verify mode
    if restore_mode && (tokenize_columns.is_some() || preserve_columns.is_some()) {
        bail!("--tokenize-columns and --preserve-columns cannot be used with --restore; \
               the column filter is read from the token dictionary automatically");
    }
    if verify_path.is_some() && (tokenize_columns.is_some() || preserve_columns.is_some()) {
        bail!("--tokenize-columns and --preserve-columns cannot be used with --verify");
    }

    if restore_mode {
        return run_restore(input_path, output_path, dictionary, json_output);
    }

    if let Some(verify) = verify_path {
        return run_verify(input_path, Path::new(verify), json_output);
    }

    // Normalize column filter into HashSets (lowercase, trimmed, deduped)
    let tokenize_cols = tokenize_columns.map(|v| normalize_column_names(&v));
    let preserve_cols = preserve_columns.map(|v| normalize_column_names(&v));

    run_tokenize(
        input_path,
        output_path,
        dictionary,
        seed,
        tokenize_numbers,
        tokenize_dates,
        tokenize_headers,
        preserve_partitions,
        tokenize_cols,
        preserve_cols,
        report,
        fmt,
        json_output,
    )
}

/// Normalize a list of column names: trim whitespace, lowercase, remove empties, deduplicate.
fn normalize_column_names(names: &[String]) -> HashSet<String> {
    names
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

fn run_tokenize(
    input: &Path,
    output: &Path,
    dictionary: Option<&str>,
    seed: Option<u64>,
    tokenize_numbers: bool,
    tokenize_dates: bool,
    tokenize_headers: bool,
    preserve_partitions: bool,
    tokenize_columns: Option<HashSet<String>>,
    preserve_columns: Option<HashSet<String>>,
    report: bool,
    output_format: Option<crate::tokenize::scanner::FileFormat>,
    json_output: bool,
) -> Result<()> {
    let dict_path = dictionary
        .map(PathBuf::from)
        .unwrap_or_else(|| output.join(".knit-tokens.json"));

    let config = TokenizeConfig {
        seed: seed.unwrap_or(42),
        tokenize_numbers,
        tokenize_dates,
        tokenize_headers,
        preserve_partitions,
        tokenize_columns,
        preserve_columns,
        native_date_shift: None,
        native_numeric_shift: None,
        output_format,
    };

    let result = tokenize::tokenize(input, output, &dict_path, &config)?;

    let report_data = if report {
        Some(crate::tokenize::report::generate_report(output, &dict_path)?)
    } else {
        None
    };

    print_result(&result, &dict_path, &config, report_data.as_ref(), json_output);

    // Print text-format report separately (JSON report is merged into main output)
    if let Some(ref rpt) = report_data {
        if !json_output {
            print!("{}", crate::tokenize::report::format_text(rpt));
        }
    }

    Ok(())
}

fn run_restore(input: &Path, output: &Path, dictionary: Option<&str>, json_output: bool) -> Result<()> {
    let dict_path = dictionary
        .map(PathBuf::from)
        .unwrap_or_else(|| input.join(".knit-tokens.json"));

    if !dict_path.exists() {
        bail!(
            "token dictionary not found at {}. Use --dictionary to specify its location.",
            dict_path.display()
        );
    }

    let result = tokenize::restore(input, output, &dict_path)?;

    if json_output {
        println!("{}", serde_json::json!({
            "event": "restore_complete",
            "data_files": result.data_files,
            "schema_files": result.schema_files,
            "dictionary_files": result.dictionary_files,
            "companion_files": result.companion_files,
            "unique_tokens": result.unique_tokens,
            "output": output.display().to_string(),
        }));
    } else {
        println!("═══ Restore Complete ═══");
        println!("  data files:       {}", result.data_files);
        println!("  schema files:     {}", result.schema_files);
        println!("  dictionary files: {}", result.dictionary_files);
        println!("  companion files:  {}", result.companion_files);
        println!("  tokens applied:   {}", result.unique_tokens);
        println!();
        println!("  Restored dataset: {}", output.display());
    }
    Ok(())
}

fn run_verify(original: &Path, tokenized: &Path, json_output: bool) -> Result<()> {
    use crate::tokenize::scanner::{scan_directory, FileKind};

    if !tokenized.exists() {
        bail!("tokenized path does not exist: {}", tokenized.display());
    }

    let orig_entries = scan_directory(original)?;
    let tok_entries = scan_directory(tokenized)?;

    // Compare data file sets by relative path (not just counts)
    let orig_data: Vec<_> = orig_entries.iter().filter(|e| e.kind == FileKind::Data).collect();
    let tok_data: Vec<_> = tok_entries
        .iter()
        .filter(|e| e.kind == FileKind::Data)
        .filter(|e| !e.rel_path.to_string_lossy().contains(".knit-tokens"))
        .collect();

    let orig_paths: std::collections::HashSet<String> = orig_data
        .iter()
        .map(|e| e.rel_path.to_string_lossy().to_string())
        .collect();
    let tok_paths: std::collections::HashSet<String> = tok_data
        .iter()
        .map(|e| e.rel_path.to_string_lossy().to_string())
        .collect();
    let file_match = orig_paths == tok_paths;
    let mut missing_files: Vec<_> = orig_paths.difference(&tok_paths).cloned().collect();
    let mut extra_files: Vec<_> = tok_paths.difference(&orig_paths).cloned().collect();
    missing_files.sort();
    extra_files.sort();

    // Check row counts for data files that exist in both trees
    let mut row_mismatches: Vec<serde_json::Value> = Vec::new();
    let mut row_match = true;
    for orig in &orig_data {
        let orig_path = original.join(&orig.rel_path);
        let tok_path = tokenized.join(&orig.rel_path);
        if tok_path.exists() {
            if let (Ok(o_count), Ok(t_count)) = (count_lines(&orig_path), count_lines(&tok_path)) {
                if o_count != t_count {
                    row_match = false;
                    row_mismatches.push(serde_json::json!({
                        "file": orig.rel_path.display().to_string(),
                        "original": o_count,
                        "tokenized": t_count,
                    }));
                }
            }
        }
    }

    // Compare schema file sets by relative path
    let orig_schemas: Vec<_> = orig_entries.iter().filter(|e| e.kind == FileKind::Schema).collect();
    let tok_schemas: Vec<_> = tok_entries.iter().filter(|e| e.kind == FileKind::Schema).collect();
    let orig_schema_paths: std::collections::HashSet<String> = orig_schemas
        .iter()
        .map(|e| e.rel_path.to_string_lossy().to_string())
        .collect();
    let tok_schema_paths: std::collections::HashSet<String> = tok_schemas
        .iter()
        .map(|e| e.rel_path.to_string_lossy().to_string())
        .collect();
    let schema_match = orig_schema_paths == tok_schema_paths;

    let passed = file_match && row_match && schema_match;

    if json_output {
        let mut json = serde_json::json!({
            "event": "verify",
            "passed": passed,
            "file_count": { "match": file_match, "original": orig_data.len(), "tokenized": tok_data.len() },
            "row_counts": { "match": row_match },
            "schema_files": { "match": schema_match, "original": orig_schemas.len(), "tokenized": tok_schemas.len() },
        });
        if !missing_files.is_empty() {
            json["file_count"]["missing"] = serde_json::json!(missing_files);
        }
        if !extra_files.is_empty() {
            json["file_count"]["extra"] = serde_json::json!(extra_files);
        }
        if !row_mismatches.is_empty() {
            json["row_counts"]["mismatches"] = serde_json::Value::Array(row_mismatches);
        }
        println!("{}", json);
    } else {
        println!("═══ Verification ═══");
        println!(
            "  file count:        {} (original: {}, tokenized: {})",
            if file_match { "✓" } else { "✗" },
            orig_data.len(),
            tok_data.len()
        );
        for f in &missing_files {
            println!("    missing: {}", f);
        }
        for f in &extra_files {
            println!("    extra:   {}", f);
        }

        for m in &row_mismatches {
            println!(
                "    row mismatch: {} (orig={}, tok={})",
                m["file"].as_str().unwrap_or("?"),
                m["original"],
                m["tokenized"]
            );
        }
        println!("  row counts:        {}", if row_match { "✓" } else { "✗" });
        println!("  schema files:      {}", if schema_match { "✓" } else { "✗" });

        if passed {
            println!("\n  Structure verification passed.");
        } else {
            println!("\n  Structure verification FAILED.");
        }
    }

    if !passed {
        bail!("structure verification failed");
    }
    Ok(())
}

fn count_lines(path: &Path) -> Result<usize> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path)?;
    Ok(BufReader::new(file).lines().count())
}

fn print_result(
    result: &TokenizeResult,
    dict_path: &Path,
    config: &TokenizeConfig,
    report: Option<&crate::tokenize::report::TokenizeReport>,
    json_output: bool,
) {
    if json_output {
        let mut json = serde_json::json!({
            "event": "tokenize_complete",
            "data_files": result.data_files,
            "schema_files": result.schema_files,
            "dictionary_files": result.dictionary_files,
            "companion_files": result.companion_files,
            "unique_tokens": result.unique_tokens,
            "dictionary": dict_path.display().to_string(),
        });
        if let Some(ref cols) = config.tokenize_columns {
            let mut sorted: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
            sorted.sort();
            json["tokenize_columns"] = serde_json::json!(sorted);
        }
        if let Some(ref cols) = config.preserve_columns {
            let mut sorted: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
            sorted.sort();
            json["preserve_columns"] = serde_json::json!(sorted);
        }
        if let Some(rpt) = report {
            let report_json = crate::tokenize::report::format_json(rpt);
            json["report"] = report_json;
        }
        println!("{}", json);
    } else {
        println!("═══ Tokenization Complete ═══");
        println!(
            "  files:       {} data, {} schema, {} dictionary, {} companion",
            result.data_files, result.schema_files, result.dictionary_files, result.companion_files
        );
        println!("  tokens:      {} unique string values → tokenized", result.unique_tokens);

        if let Some(ref cols) = config.tokenize_columns {
            let mut sorted: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
            sorted.sort();
            println!("  columns:     only [{}]", sorted.join(", "));
        } else if let Some(ref cols) = config.preserve_columns {
            let mut sorted: Vec<&str> = cols.iter().map(|s| s.as_str()).collect();
            sorted.sort();
            println!("  preserved:   [{}]", sorted.join(", "));
        }

        println!("  dictionary:  {}", dict_path.display());
        println!();
        println!("The tokenized dataset is safe to share.");
        println!("Keep the dictionary private — it can restore the original data.");
    }
}