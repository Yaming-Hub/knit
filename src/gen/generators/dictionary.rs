//! Dictionary-based string generator.
//!
//! Samples values from an external word list loaded at plan resolution time.
//! Supports three expansion strategies when more unique values are needed
//! than the dictionary contains:
//!
//! - **Sample**: sample with replacement (duplicates allowed)
//! - **Combinatorial**: tokenize entries into positional word pools, recombine
//! - **Suffix**: append numeric suffixes (-001, -002, etc.)

use std::sync::Arc;

use arrow::array::{ArrayRef, StringArray};
use rand::RngCore;

use crate::r#gen::context::GenContext;
use crate::r#gen::traits::FieldGenerator;

/// Expansion strategy when dictionary entries are exhausted.
#[derive(Debug, Clone, PartialEq)]
pub enum ExpansionStrategy {
    /// Sample with replacement — duplicates allowed.
    Sample,
    /// Split entries into positional word tokens, recombine randomly.
    Combinatorial,
    /// Append numeric suffixes: "value-001", "value-002", etc.
    Suffix,
}

impl ExpansionStrategy {
    /// Parse expansion strategy from string (case-insensitive).
    pub fn parse(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "combinatorial" => Self::Combinatorial,
            "suffix" => Self::Suffix,
            _ => Self::Sample,
        }
    }
}

/// Generate string values by sampling from a dictionary word list.
///
/// The dictionary is a flat list of strings loaded from an external file.
/// When the generation count exceeds dictionary size, the expansion strategy
/// determines how new values are produced.
pub struct DictionaryGenerator {
    /// The loaded dictionary entries.
    entries: Vec<String>,
    /// Expansion strategy.
    expansion: ExpansionStrategy,
    /// For combinatorial: tokenized word lists per position.
    token_pools: Option<Vec<Vec<String>>>,
}

impl DictionaryGenerator {
    /// Create a new dictionary generator from loaded entries and expansion mode.
    pub fn new(entries: Vec<String>, expansion: String) -> Self {
        let strategy = ExpansionStrategy::parse(&expansion);
        let token_pools = if strategy == ExpansionStrategy::Combinatorial {
            Some(build_token_pools(&entries))
        } else {
            None
        };
        Self {
            entries,
            expansion: strategy,
            token_pools,
        }
    }

    /// Generate a single value using the configured strategy.
    fn generate_one(&self, rng: &mut dyn RngCore, index: usize) -> String {
        if self.entries.is_empty() {
            return String::new();
        }

        match self.expansion {
            ExpansionStrategy::Sample => {
                let idx = rng.next_u32() as usize % self.entries.len();
                self.entries[idx].clone()
            }
            ExpansionStrategy::Combinatorial => {
                if let Some(pools) = &self.token_pools {
                    if pools.is_empty() {
                        // Fallback to sample if no tokens
                        let idx = rng.next_u32() as usize % self.entries.len();
                        return self.entries[idx].clone();
                    }
                    let mut parts = Vec::with_capacity(pools.len());
                    for pool in pools {
                        if pool.is_empty() {
                            continue;
                        }
                        let idx = rng.next_u32() as usize % pool.len();
                        parts.push(pool[idx].as_str());
                    }
                    parts.join(" ")
                } else {
                    let idx = rng.next_u32() as usize % self.entries.len();
                    self.entries[idx].clone()
                }
            }
            ExpansionStrategy::Suffix => {
                let idx = rng.next_u32() as usize % self.entries.len();
                let base = &self.entries[idx];
                // Append suffix if we need more values than dict size
                // Use the row index to create unique suffixes
                if index < self.entries.len() {
                    base.clone()
                } else {
                    format!("{}-{:03}", base, index / self.entries.len())
                }
            }
        }
    }
}

impl FieldGenerator for DictionaryGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, _ctx: &GenContext) -> ArrayRef {
        let values: Vec<String> = (0..count).map(|i| self.generate_one(rng, i)).collect();
        Arc::new(StringArray::from(
            values.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        ))
    }

    fn output_type(&self) -> arrow::datatypes::DataType {
        arrow::datatypes::DataType::Utf8
    }
}

/// Build positional token pools from dictionary entries.
///
/// Splits each entry by whitespace and groups tokens by position.
/// For example, entries ["Ultra Steel Watch", "Pro Cotton Shoes"]
/// produce pools: [["Ultra", "Pro"], ["Steel", "Cotton"], ["Watch", "Shoes"]].
fn build_token_pools(entries: &[String]) -> Vec<Vec<String>> {
    if entries.is_empty() {
        return vec![];
    }

    // Find the max number of tokens across all entries
    let max_tokens = entries
        .iter()
        .map(|e| e.split_whitespace().count())
        .max()
        .unwrap_or(0);

    if max_tokens == 0 {
        return vec![];
    }

    let mut pools: Vec<Vec<String>> = vec![vec![]; max_tokens];

    for entry in entries {
        for (i, token) in entry.split_whitespace().enumerate() {
            if i < max_tokens {
                // Deduplicate within each pool
                let pool = &mut pools[i];
                let token_str = token.to_string();
                if !pool.contains(&token_str) {
                    pool.push(token_str);
                }
            }
        }
    }

    // Remove empty trailing pools
    while pools.last().is_some_and(|p| p.is_empty()) {
        pools.pop();
    }

    pools
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Array;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;
    use std::collections::HashMap;

    fn test_ctx() -> GenContext<'static> {
        static COLS: std::sync::LazyLock<HashMap<String, ArrayRef>> =
            std::sync::LazyLock::new(HashMap::new);
        GenContext::new(&COLS, 0, 0, 1, "test")
    }

    #[test]
    fn sample_strategy_picks_from_entries() {
        let entries = vec!["alpha".into(), "beta".into(), "gamma".into()];
        let r#gen = DictionaryGenerator::new(entries.clone(), "sample".into());
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 10, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(str_arr.len(), 10);
        for i in 0..10 {
            assert!(entries.contains(&str_arr.value(i).to_string()));
        }
    }

    #[test]
    fn combinatorial_strategy_recombines_tokens() {
        let entries = vec![
            "Ultra Steel Watch".into(),
            "Pro Cotton Shoes".into(),
            "Slim Rubber Lamp".into(),
        ];
        let r#gen = DictionaryGenerator::new(entries, "combinatorial".into());
        assert!(r#gen.token_pools.is_some());
        let pools = r#gen.token_pools.as_ref().unwrap();
        assert_eq!(pools.len(), 3);
        assert!(pools[0].contains(&"Ultra".to_string()));
        assert!(pools[1].contains(&"Cotton".to_string()));
        assert!(pools[2].contains(&"Lamp".to_string()));

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 5, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(str_arr.len(), 5);
        // Each value should have 3 words
        for i in 0..5 {
            let val = str_arr.value(i);
            assert_eq!(val.split_whitespace().count(), 3, "got: {}", val);
        }
    }

    #[test]
    fn suffix_strategy_appends_number() {
        let entries = vec!["Widget".into(), "Gadget".into()];
        let r#gen = DictionaryGenerator::new(entries, "suffix".into());
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 5, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        assert_eq!(str_arr.len(), 5);
        // First 2 should be plain (index < entries.len())
        for i in 0..2 {
            let val = str_arr.value(i);
            assert!(val == "Widget" || val == "Gadget", "unexpected: {}", val);
        }
        // Indices >= 2 should have suffix
        for i in 2..5 {
            let val = str_arr.value(i);
            assert!(val.contains('-'), "expected suffix in: {}", val);
        }
    }

    #[test]
    fn empty_dictionary_produces_empty_strings() {
        let r#gen = DictionaryGenerator::new(vec![], "sample".into());
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let ctx = test_ctx();
        let arr = r#gen.generate(&mut rng, 3, &ctx);
        let str_arr = arr.as_any().downcast_ref::<StringArray>().unwrap();
        for i in 0..3 {
            assert_eq!(str_arr.value(i), "");
        }
    }

    #[test]
    fn build_token_pools_deduplicates() {
        let entries = vec!["Ultra Steel Watch".into(), "Ultra Cotton Shoes".into()];
        let pools = build_token_pools(&entries);
        assert_eq!(pools.len(), 3);
        // "Ultra" appears once in pool 0
        assert_eq!(pools[0].iter().filter(|t| *t == "Ultra").count(), 1);
        assert_eq!(pools[0].len(), 1); // only "Ultra"
        assert_eq!(pools[1].len(), 2); // "Steel", "Cotton"
    }
}
