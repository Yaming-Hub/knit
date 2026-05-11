//! Dataset tokenization for safe sharing.
//!
//! Replaces sensitive string content with opaque tokens while preserving
//! dataset structure, relationships, and statistical properties.

pub mod apply;
pub mod dictionary;
pub mod mapper;
pub mod scanner;

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use tracing::{debug, info};

use crate::tokenize::dictionary::TokenDictionary;
use crate::tokenize::mapper::TokenMapper;
use crate::tokenize::scanner::{scan_directory, FileEntry, FileKind};

/// Configuration for a tokenization run.
#[derive(Debug, Clone)]
pub struct TokenizeConfig {
    /// Random seed for deterministic token generation.
    pub seed: u64,
    /// Whether to also tokenize numeric values.
    pub tokenize_numbers: bool,
    /// Whether to also tokenize date/timestamp values.
    pub tokenize_dates: bool,
    /// Whether to tokenize column headers.
    pub tokenize_headers: bool,
    /// Whether to preserve partition folder names as-is.
    pub preserve_partitions: bool,
}

impl Default for TokenizeConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            tokenize_numbers: false,
            tokenize_dates: false,
            tokenize_headers: false,
            preserve_partitions: true,
        }
    }
}

/// Result of a tokenization run.
#[derive(Debug)]
pub struct TokenizeResult {
    /// Number of data files processed.
    pub data_files: usize,
    /// Number of schema files processed.
    pub schema_files: usize,
    /// Number of dictionary files processed.
    pub dictionary_files: usize,
    /// Number of companion files copied.
    pub companion_files: usize,
    /// Number of unique tokens generated.
    pub unique_tokens: usize,
}

/// Run the full tokenization pipeline: scan → build map → apply → emit dictionary.
pub fn tokenize(
    input_dir: &Path,
    output_dir: &Path,
    dict_path: &Path,
    config: &TokenizeConfig,
) -> Result<TokenizeResult> {
    info!(input = %input_dir.display(), output = %output_dir.display(), "starting tokenization");

    // Phase 1: Scan directory and classify files
    let entries = scan_directory(input_dir)?;
    info!(
        data = entries.iter().filter(|e| e.kind == FileKind::Data).count(),
        schema = entries.iter().filter(|e| e.kind == FileKind::Schema).count(),
        dictionary = entries.iter().filter(|e| e.kind == FileKind::Dictionary).count(),
        companion = entries.iter().filter(|e| e.kind == FileKind::Companion).count(),
        "scanned {} files", entries.len()
    );

    // Phase 2: Build token map by scanning all string values
    let mut mapper = TokenMapper::new(config.seed);
    for entry in &entries {
        if entry.kind == FileKind::Companion {
            continue;
        }
        debug!(file = %entry.rel_path.display(), kind = ?entry.kind, "scanning strings");
        scanner::extract_strings(entry, input_dir, &mut mapper, config)
            .with_context(|| format!("scanning {}", entry.rel_path.display()))?;
    }
    info!(unique_tokens = mapper.len(), "token map built");

    // Phase 3: Apply tokens and write output
    std::fs::create_dir_all(output_dir)
        .with_context(|| format!("creating output directory {}", output_dir.display()))?;

    let mut data_files = 0;
    let mut schema_files = 0;
    let mut dictionary_files = 0;
    let mut companion_files = 0;

    for entry in &entries {
        let out_path = output_dir.join(&entry.rel_path);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        match entry.kind {
            FileKind::Data => {
                apply::apply_data_file(entry, input_dir, &out_path, &mapper, config)?;
                data_files += 1;
            }
            FileKind::Schema => {
                apply::apply_schema_file(entry, input_dir, &out_path, &mapper)?;
                schema_files += 1;
            }
            FileKind::Dictionary => {
                apply::apply_data_file(entry, input_dir, &out_path, &mapper, config)?;
                dictionary_files += 1;
            }
            FileKind::Companion => {
                let src = input_dir.join(&entry.rel_path);
                std::fs::copy(&src, &out_path)?;
                companion_files += 1;
            }
        }
    }

    // Phase 4: Write token dictionary
    let dict = TokenDictionary::from_mapper(&mapper, config);
    dict.write(dict_path)?;
    info!(path = %dict_path.display(), "token dictionary written");

    Ok(TokenizeResult {
        data_files,
        schema_files,
        dictionary_files,
        companion_files,
        unique_tokens: mapper.len(),
    })
}

/// Restore a tokenized dataset using a token dictionary.
pub fn restore(
    input_dir: &Path,
    output_dir: &Path,
    dict_path: &Path,
) -> Result<TokenizeResult> {
    info!(input = %input_dir.display(), dict = %dict_path.display(), "restoring from tokens");

    let dict = TokenDictionary::read(dict_path)?;
    let reverse_map: HashMap<String, String> = dict
        .tokens
        .iter()
        .map(|(orig, tok)| (tok.clone(), orig.clone()))
        .collect();

    let entries = scan_directory(input_dir)?;
    std::fs::create_dir_all(output_dir)?;

    // Build a mapper with the reverse map for apply functions
    let mapper = TokenMapper::from_reverse_map(reverse_map);

    let config = TokenizeConfig::default();
    let mut data_files = 0;
    let mut schema_files = 0;
    let mut dictionary_files = 0;
    let mut companion_files = 0;

    for entry in &entries {
        // Skip the dictionary file itself
        if entry.rel_path.to_string_lossy().contains(".knit-tokens") {
            continue;
        }

        let out_path = output_dir.join(&entry.rel_path);
        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        match entry.kind {
            FileKind::Data => {
                apply::apply_data_file(entry, input_dir, &out_path, &mapper, &config)?;
                data_files += 1;
            }
            FileKind::Schema => {
                apply::apply_schema_file(entry, input_dir, &out_path, &mapper)?;
                schema_files += 1;
            }
            FileKind::Dictionary => {
                apply::apply_data_file(entry, input_dir, &out_path, &mapper, &config)?;
                dictionary_files += 1;
            }
            FileKind::Companion => {
                let src = input_dir.join(&entry.rel_path);
                std::fs::copy(&src, &out_path)?;
                companion_files += 1;
            }
        }
    }

    Ok(TokenizeResult {
        data_files,
        schema_files,
        dictionary_files,
        companion_files,
        unique_tokens: dict.tokens.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_roundtrip_csv() {
        let input = TempDir::new().unwrap();
        let output = TempDir::new().unwrap();
        let restored = TempDir::new().unwrap();

        // Create a simple CSV
        let csv_path = input.path().join("users.csv");
        let mut f = std::fs::File::create(&csv_path).unwrap();
        writeln!(f, "id,name,email").unwrap();
        writeln!(f, "1,John Smith,john@example.com").unwrap();
        writeln!(f, "2,Jane Doe,jane@example.com").unwrap();

        let config = TokenizeConfig { seed: 123, ..Default::default() };
        let dict_path = output.path().join(".knit-tokens.json");

        // Tokenize
        let result = tokenize(input.path(), output.path(), &dict_path, &config).unwrap();
        assert_eq!(result.data_files, 1);
        assert!(result.unique_tokens > 0);

        // Verify tokenized content differs from original
        let tokenized = std::fs::read_to_string(output.path().join("users.csv")).unwrap();
        assert!(!tokenized.contains("John Smith"));
        assert!(!tokenized.contains("john@example.com"));
        // Headers should be preserved
        assert!(tokenized.contains("id,name,email"));

        // Restore
        let res2 = restore(output.path(), restored.path(), &dict_path).unwrap();
        assert_eq!(res2.data_files, 1);

        let restored_content = std::fs::read_to_string(restored.path().join("users.csv")).unwrap();
        assert!(restored_content.contains("John Smith"));
        assert!(restored_content.contains("john@example.com"));
    }

    #[test]
    fn test_global_consistency() {
        let input = TempDir::new().unwrap();
        let output = TempDir::new().unwrap();

        // Two CSVs sharing the same value "US"
        let csv1 = input.path().join("orders.csv");
        let mut f1 = std::fs::File::create(&csv1).unwrap();
        writeln!(f1, "id,region").unwrap();
        writeln!(f1, "1,US").unwrap();
        writeln!(f1, "2,EU").unwrap();

        let csv2 = input.path().join("users.csv");
        let mut f2 = std::fs::File::create(&csv2).unwrap();
        writeln!(f2, "id,country").unwrap();
        writeln!(f2, "1,US").unwrap();
        writeln!(f2, "2,JP").unwrap();

        let config = TokenizeConfig { seed: 42, ..Default::default() };
        let dict_path = output.path().join(".knit-tokens.json");

        tokenize(input.path(), output.path(), &dict_path, &config).unwrap();

        // Read tokenized files
        let t1 = std::fs::read_to_string(output.path().join("orders.csv")).unwrap();
        let t2 = std::fs::read_to_string(output.path().join("users.csv")).unwrap();

        // Extract the token for "US" from orders.csv (row 1, col 1)
        let lines1: Vec<&str> = t1.lines().collect();
        let us_token_1 = lines1[1].split(',').nth(1).unwrap();

        let lines2: Vec<&str> = t2.lines().collect();
        let us_token_2 = lines2[1].split(',').nth(1).unwrap();

        // Same original "US" → same token in both files
        assert_eq!(us_token_1, us_token_2);
        assert_ne!(us_token_1, "US");
    }
}