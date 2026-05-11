//! Token dictionary I/O — reading and writing the .knit-tokens.json file.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::tokenize::mapper::TokenMapper;
use crate::tokenize::TokenizeConfig;

/// The token dictionary file format.
#[derive(Debug, Serialize, Deserialize)]
pub struct TokenDictionary {
    /// Format version.
    pub version: u32,
    /// Seed used for token generation.
    pub seed: u64,
    /// Statistics about the tokenization.
    pub stats: DictionaryStats,
    /// The token mappings (original → token), sorted for deterministic output.
    pub tokens: BTreeMap<String, String>,
}

/// Summary statistics stored in the dictionary.
#[derive(Debug, Serialize, Deserialize)]
pub struct DictionaryStats {
    /// Number of unique tokens generated.
    pub unique_tokens: usize,
}

impl TokenDictionary {
    /// Build a dictionary from a completed token mapper.
    pub fn from_mapper(mapper: &TokenMapper, config: &TokenizeConfig) -> Self {
        let tokens: BTreeMap<String, String> = mapper
            .mappings()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();

        Self {
            version: 1,
            seed: config.seed,
            stats: DictionaryStats {
                unique_tokens: tokens.len(),
            },
            tokens,
        }
    }

    /// Write the dictionary to a JSON file.
    pub fn write(&self, path: &Path) -> Result<()> {
        let json = serde_json::to_string_pretty(self)
            .context("serializing token dictionary")?;
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
            tokens,
        };

        dict.write(&path).unwrap();
        let loaded = TokenDictionary::read(&path).unwrap();

        assert_eq!(loaded.version, 1);
        assert_eq!(loaded.seed, 42);
        assert_eq!(loaded.tokens.len(), 2);
        assert_eq!(loaded.tokens.get("Hello").unwrap(), "Xkmpq");
    }
}