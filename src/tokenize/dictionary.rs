//! Token dictionary I/O — reading and writing the .knit-tokens.json file.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::tokenize::mapper::TokenMapper;
use crate::tokenize::{TokenizeConfig, scanner};

/// The token dictionary file format.
#[derive(Debug, Serialize, Deserialize)]
pub struct TokenDictionary {
    /// Format version.
    pub version: u32,
    /// Seed used for token generation.
    pub seed: u64,
    /// Statistics about the tokenization.
    pub stats: DictionaryStats,
    /// Column filter policy used during tokenization.
    #[serde(default, skip_serializing_if = "is_default_column_filter")]
    pub column_filter: ColumnFilter,
    /// Date shift offset in days (set when --tokenize-dates was used).
    /// Used during restore to reverse native Parquet temporal column shifts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub date_shift_days: Option<i64>,
    /// Numeric shift offset for native Parquet numeric columns.
    /// Used during restore to reverse the shift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub numeric_shift: Option<i64>,
    /// Whether file/folder names were tokenized.
    /// Used during restore to reverse path tokenization.
    #[serde(default)]
    pub tokenized_paths: bool,
    /// Whether partition folders were preserved during path tokenization.
    /// Used during restore to correctly reverse path renaming.
    #[serde(default = "default_true")]
    pub preserve_partitions: bool,
    /// The token mappings (original → token), sorted for deterministic output.
    pub tokens: BTreeMap<String, String>,
}

fn is_default_column_filter(f: &ColumnFilter) -> bool {
    f.tokenize_columns.is_none() && f.preserve_columns.is_none()
}

fn default_true() -> bool {
    true
}

/// Summary statistics stored in the dictionary.
#[derive(Debug, Serialize, Deserialize)]
pub struct DictionaryStats {
    /// Number of unique tokens generated.
    pub unique_tokens: usize,
}

/// Column filter policy stored in the dictionary for safe restore.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ColumnFilter {
    /// If set, only these columns were tokenized (lowercase).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokenize_columns: Option<Vec<String>>,
    /// If set, these columns were preserved (lowercase).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preserve_columns: Option<Vec<String>>,
}

impl TokenDictionary {
    /// Build a dictionary from a completed token mapper.
    pub fn from_mapper(mapper: &TokenMapper, config: &TokenizeConfig) -> Self {
        let tokens: BTreeMap<String, String> = mapper
            .mappings()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        let column_filter = ColumnFilter {
            tokenize_columns: config.tokenize_columns.as_ref().map(|s| {
                let mut v: Vec<String> = s.iter().cloned().collect();
                v.sort();
                v
            }),
            preserve_columns: config.preserve_columns.as_ref().map(|s| {
                let mut v: Vec<String> = s.iter().cloned().collect();
                v.sort();
                v
            }),
        };

        let date_shift_days = if config.tokenize_dates {
            Some(scanner::compute_date_shift(config.seed))
        } else {
            None
        };

        let numeric_shift = if config.tokenize_numbers {
            Some(scanner::compute_numeric_shift(config.seed))
        } else {
            None
        };

        Self {
            version: 1,
            seed: config.seed,
            stats: DictionaryStats {
                unique_tokens: tokens.len(),
            },
            column_filter,
            date_shift_days,
            numeric_shift,
            tokenized_paths: config.tokenize_paths,
            preserve_partitions: config.preserve_partitions,
            tokens,
        }
    }

    /// Write the dictionary to a JSON file.
    pub fn write(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self).context("serializing token dictionary")?;
        std::fs::write(path, json)
            .with_context(|| format!("writing dictionary to {}", path.display()))?;
        Ok(())
    }

    /// Read a dictionary from a JSON file.
    pub fn read(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("reading dictionary from {}", path.display()))?;
        let dict: Self = serde_json::from_str(&content)
            .with_context(|| format!("parsing dictionary {}", path.display()))?;
        Ok(dict)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_roundtrip_dictionary() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tokens.json");

        let mut tokens = BTreeMap::new();
        tokens.insert("Hello".to_string(), "Xkmpq".to_string());
        tokens.insert("World".to_string(), "Bvrlw".to_string());

        let dict = TokenDictionary {
            version: 1,
            seed: 42,
            stats: DictionaryStats { unique_tokens: 2 },
            column_filter: ColumnFilter::default(),
            date_shift_days: None,
            numeric_shift: None,
            tokenized_paths: false,
            preserve_partitions: true,
            tokens,
        };

        dict.write(&path).unwrap();
        let loaded = TokenDictionary::read(&path).unwrap();

        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.seed, 42);
        assert_eq!(loaded.tokens.len(), 2);
        assert_eq!(loaded.tokens.get("Hello").unwrap(), "Xkmpq");
    }

    #[test]
    fn test_roundtrip_dictionary_with_column_filter() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tokens.json");

        let dict = TokenDictionary {
            version: 1,
            seed: 42,
            stats: DictionaryStats { unique_tokens: 0 },
            column_filter: ColumnFilter {
                tokenize_columns: Some(vec!["name".to_string(), "email".to_string()]),
                preserve_columns: None,
            },
            date_shift_days: None,
            numeric_shift: None,
            tokenized_paths: false,
            preserve_partitions: true,
            tokens: BTreeMap::new(),
        };

        dict.write(&path).unwrap();
        let loaded = TokenDictionary::read(&path).unwrap();

        assert_eq!(
            loaded.column_filter.tokenize_columns.unwrap(),
            vec!["name", "email"]
        );
        assert!(loaded.column_filter.preserve_columns.is_none());
    }

    #[test]
    fn test_dictionary_no_column_filter_omitted() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("tokens.json");

        let dict = TokenDictionary {
            version: 1,
            seed: 42,
            stats: DictionaryStats { unique_tokens: 0 },
            column_filter: ColumnFilter::default(),
            date_shift_days: None,
            numeric_shift: None,
            tokenized_paths: false,
            preserve_partitions: true,
            tokens: BTreeMap::new(),
        };

        dict.write(&path).unwrap();

        // Verify JSON does not contain column_filter when empty
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(!content.contains("column_filter"));

        // But still loads fine (default)
        let loaded = TokenDictionary::read(&path).unwrap();
        assert!(loaded.column_filter.tokenize_columns.is_none());
        assert!(loaded.column_filter.preserve_columns.is_none());
    }
}
