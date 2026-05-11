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
) -> Result<()> {
    let input_path = Path::new(input);
    let output_path = Path::new(output);

    if !input_path.exists() {
        bail!("input path does not exist: {}", input);
    }

    // Column filter flags are not valid in restore or verify mode
    if restore_mode && (tokenize_columns.is_some() || preserve_columns.is_some()) {
        bail!("--tokenize-columns and --preserve-columns cannot be used with --restore; \
               the column filter is read from the token dictionary automatically");
    }
    if verify_path.is_some() && (tokenize_columns.is_some() || preserve_columns.is_some()) {
        bail!("--tokenize-columns and --preserve-columns cannot be used with --verify");
    }

    if restore_mode {
        return run_restore(input_path, output_path, dictionary);
    }

    if let Some(verify) = verify_path {
        return run_verify(input_path, Path::new(verify));
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
    };

    let result = tokenize::tokenize(input, output, &dict_path, &config)?;
    print_result(&result, &dict_path, &config);
    Ok(())
}

fn run_restore(input: &Path, output: &Path, dictionary: Option<&str>) -> Result<()> {
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

    println!("═══ Restore Complete ═══");
    println!("  data files:       {}", result.data_files);
    println!("  schema files:     {}", result.schema_files);
    println!("  dictionary files: {}", result.dictionary_files);
    println!("  companion files:  {}", result.companion_files);
    println!("  tokens applied:   {}", result.unique_tokens);
    println!();
    println!("  Restored dataset: {}", output.display());
    Ok(())
}

fn run_verify(original: &Path, tokenized: &Path) -> Result<()> {
    use crate::tokenize::scanner::{scan_directory, FileKind};

    if !tokenized.exists() {
        bail!("tokenized path does not exist: {}", tokenized.display());
    }

    let orig_entries = scan_directory(original)?;
    let tok_entries = scan_directory(tokenized)?;

    println!("═══ Verification ═══");

    // Check file count match (excluding .knit-tokens.json)
    let orig_data: Vec<_> = orig_entries.iter().filter(|e| e.kind == FileKind::Data).collect();
    let tok_data: Vec<_> = tok_entries
        .iter()
        .filter(|e| e.kind == FileKind::Data)
        .filter(|e| !e.rel_path.to_string_lossy().contains(".knit-tokens"))
        .collect();

    let file_match = orig_data.len() == tok_data.len();
    println!(
        "  file count:        {} (original: {}, tokenized: {})",
        if file_match { "✓" } else { "✗" },
        orig_data.len(),
        tok_data.len()
    );

    // Check row counts for CSV files
    let mut row_match = true;
    for orig in &orig_data {
        let orig_path = original.join(&orig.rel_path);
        let tok_path = tokenized.join(&orig.rel_path);
        if tok_path.exists() {
            if let (Ok(o_count), Ok(t_count)) = (count_lines(&orig_path), count_lines(&tok_path)) {
                if o_count != t_count {
                    row_match = false;
                    println!(
                        "    row mismatch: {} (orig={}, tok={})",
                        orig.rel_path.display(),
                        o_count,
                        t_count
                    );
                }
            }
        }
    }
    println!("  row counts:        {}", if row_match { "✓" } else { "✗" });

    // Check schema files preserved
    let orig_schemas: Vec<_> = orig_entries.iter().filter(|e| e.kind == FileKind::Schema).collect();
    let tok_schemas: Vec<_> = tok_entries.iter().filter(|e| e.kind == FileKind::Schema).collect();
    let schema_match = orig_schemas.len() == tok_schemas.len();
    println!("  schema files:      {}", if schema_match { "✓" } else { "✗" });

    if file_match && row_match && schema_match {
        println!("\n  Structure verification passed.");
    } else {
        println!("\n  Structure verification FAILED.");
    }
    Ok(())
}

fn count_lines(path: &Path) -> Result<usize> {
    use std::io::{BufRead, BufReader};
    let file = std::fs::File::open(path)?;
    Ok(BufReader::new(file).lines().count())
}

fn print_result(result: &TokenizeResult, dict_path: &Path, config: &TokenizeConfig) {
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