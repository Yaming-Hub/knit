//! Shape-preserving token generation.
//!
//! Generates random tokens that preserve the structural shape of original values:
//! same length, same separator positions, same character class (upper/lower/digit).

use std::collections::{HashMap, HashSet};

use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

/// Maps original string values to generated tokens.
///
/// The mapper builds tokens incrementally as values are registered.
/// Each original value gets exactly one token, and no two originals
/// share the same token (collision-free).
pub struct TokenMapper {
    /// Forward map: original → token.
    map: HashMap<String, String>,
    /// Set of generated tokens (for collision detection).
    used_tokens: HashSet<String>,
    /// Seeded RNG for deterministic generation.
    rng: StdRng,
}

impl TokenMapper {
    /// Create a new mapper with the given seed.
    pub fn new(seed: u64) -> Self {
        Self {
            map: HashMap::new(),
            used_tokens: HashSet::new(),
            rng: StdRng::seed_from_u64(seed),
        }
    }

    /// Create a mapper from a pre-built reverse map (for restore mode).
    /// In restore mode, the map stores token→original so that `get(token)` returns the original.
    pub fn from_reverse_map(reverse: HashMap<String, String>) -> Self {
        let used_tokens: HashSet<String> = reverse.values().cloned().collect();
        Self {
            map: reverse,
            used_tokens,
            rng: StdRng::seed_from_u64(0),
        }
    }

    /// Register a value to be tokenized. Generates and caches its token.
    pub fn register(&mut self, original: &str) {
        if self.map.contains_key(original) {
            return;
        }
        let token = self.generate_token(original);
        self.used_tokens.insert(token.clone());
        self.map.insert(original.to_string(), token);
    }

    /// Register a value with a specific replacement (e.g., shifted dates).
    pub fn register_with_value(&mut self, original: &str, replacement: &str) {
        if self.map.contains_key(original) {
            return;
        }
        self.used_tokens.insert(replacement.to_string());
        self.map
            .insert(original.to_string(), replacement.to_string());
    }

    /// Look up the token for an original value.
    pub fn get(&self, original: &str) -> Option<&str> {
        self.map.get(original).map(|s| s.as_str())
    }

    /// Check if a value is registered.
    pub fn contains(&self, original: &str) -> bool {
        self.map.contains_key(original)
    }

    /// Number of registered tokens.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Return whether no tokens have been registered.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Get all mappings (for dictionary serialization).
    pub fn mappings(&self) -> &HashMap<String, String> {
        &self.map
    }

    /// Generate a shape-preserving token for the given value.
    fn generate_token(&mut self, original: &str) -> String {
        // Try up to 1000 times to avoid collisions
        for _ in 0..1000 {
            let token = self.generate_shape_token(original);
            if !self.used_tokens.contains(&token) && !self.map.contains_key(&token) {
                return token;
            }
        }
        // Fallback for exhausted token space: vary length slightly rather than
        // appending a hex suffix that would destroy shape.
        // Add one extra char of the dominant class in the original.
        let dominant_class = if original.chars().all(|c| c.is_ascii_uppercase()) {
            'A'
        } else if original.chars().all(|c| c.is_ascii_lowercase()) {
            'a'
        } else {
            'x'
        };
        for extra in 1..=5 {
            let padded = format!(
                "{}{}",
                original,
                std::iter::repeat_n(dominant_class, extra).collect::<String>()
            );
            let token = self.generate_shape_token(&padded);
            if !self.used_tokens.contains(&token) && !self.map.contains_key(&token) {
                return token;
            }
        }
        // Ultimate fallback: append unique suffix
        let base = self.generate_shape_token(original);
        let suffix: u32 = self.rng.random();
        format!("{}_{:08x}", base, suffix)
    }

    /// Generate one shape-preserving token attempt.
    fn generate_shape_token(&mut self, original: &str) -> String {
        let segments = split_by_separators(original);
        let mut result = String::with_capacity(original.len());

        for segment in segments {
            match segment {
                Segment::Separator(ch) => result.push(ch),
                Segment::Text(text) => {
                    for ch in text.chars() {
                        result.push(self.random_char_matching(ch));
                    }
                }
            }
        }
        result
    }

    /// Generate a random character matching the class of the input character.
    fn random_char_matching(&mut self, ch: char) -> char {
        if ch.is_ascii_uppercase() {
            (b'A' + self.rng.random_range(0..26u8)) as char
        } else if ch.is_ascii_lowercase() {
            (b'a' + self.rng.random_range(0..26u8)) as char
        } else if ch.is_ascii_digit() {
            (b'0' + self.rng.random_range(0..10u8)) as char
        } else if ch.is_alphabetic() {
            // Non-ASCII letters: replace with random ASCII letter of same case
            if ch.is_uppercase() {
                (b'A' + self.rng.random_range(0..26u8)) as char
            } else {
                (b'a' + self.rng.random_range(0..26u8)) as char
            }
        } else if ch.is_numeric() {
            // Non-ASCII digits
            (b'0' + self.rng.random_range(0..10u8)) as char
        } else {
            // Punctuation and other: preserve as-is (structural separators)
            ch
        }
    }
}

/// Separators that define string structure.
const SEPARATORS: &[char] = &['@', '.', '-', '_', '/', ' ', ',', ':', ';', '\\'];

#[derive(Debug)]
enum Segment {
    Separator(char),
    Text(String),
}

/// Split a string into text segments and separators.
fn split_by_separators(s: &str) -> Vec<Segment> {
    let mut segments = Vec::new();
    let mut current = String::new();

    for ch in s.chars() {
        if SEPARATORS.contains(&ch) {
            if !current.is_empty() {
                segments.push(Segment::Text(std::mem::take(&mut current)));
            }
            segments.push(Segment::Separator(ch));
        } else {
            current.push(ch);
        }
    }
    if !current.is_empty() {
        segments.push(Segment::Text(current));
    }
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shape_preserves_length() {
        let mut mapper = TokenMapper::new(42);
        mapper.register("Hello");
        let token = mapper.get("Hello").unwrap();
        assert_eq!(token.len(), 5);
    }

    #[test]
    fn test_shape_preserves_case_pattern() {
        let mut mapper = TokenMapper::new(42);
        mapper.register("HelloWorld");
        let token = mapper.get("HelloWorld").unwrap();

        let original_pattern: Vec<bool> = "HelloWorld".chars().map(|c| c.is_uppercase()).collect();
        let token_pattern: Vec<bool> = token.chars().map(|c| c.is_uppercase()).collect();
        assert_eq!(original_pattern, token_pattern);
    }

    #[test]
    fn test_shape_preserves_separators() {
        let mut mapper = TokenMapper::new(42);
        mapper.register("john.smith@example.com");
        let token = mapper.get("john.smith@example.com").unwrap();

        // Should have dots at same positions, @ at same position
        let orig_seps: Vec<(usize, char)> = "john.smith@example.com"
            .char_indices()
            .filter(|(_, c)| SEPARATORS.contains(c))
            .collect();
        let tok_seps: Vec<(usize, char)> = token
            .char_indices()
            .filter(|(_, c)| SEPARATORS.contains(c))
            .collect();
        assert_eq!(orig_seps, tok_seps);
    }

    #[test]
    fn test_deterministic_with_seed() {
        let mut m1 = TokenMapper::new(42);
        let mut m2 = TokenMapper::new(42);
        m1.register("test_value");
        m2.register("test_value");
        assert_eq!(m1.get("test_value"), m2.get("test_value"));
    }

    #[test]
    fn test_same_value_same_token() {
        let mut mapper = TokenMapper::new(42);
        mapper.register("US");
        mapper.register("US"); // re-register
        assert_eq!(mapper.len(), 1);
    }

    #[test]
    fn test_different_values_different_tokens() {
        let mut mapper = TokenMapper::new(42);
        mapper.register("AB");
        mapper.register("CD");
        let t1 = mapper.get("AB").unwrap();
        let t2 = mapper.get("CD").unwrap();
        assert_ne!(t1, t2);
    }

    #[test]
    fn test_digits_preserved_as_digits() {
        let mut mapper = TokenMapper::new(42);
        mapper.register("ABC123");
        let token = mapper.get("ABC123").unwrap();
        assert!(token[..3].chars().all(|c| c.is_ascii_uppercase()));
        assert!(token[3..].chars().all(|c| c.is_ascii_digit()));
    }

    #[test]
    fn test_collision_avoidance() {
        // Register many 2-char values; should all get unique tokens
        let mut mapper = TokenMapper::new(42);
        for a in b'A'..=b'Z' {
            for b in b'A'..=b'Z' {
                let s = format!("{}{}", a as char, b as char);
                mapper.register(&s);
            }
        }
        // 676 values, all unique tokens
        let tokens: HashSet<&str> = mapper.map.values().map(|s| s.as_str()).collect();
        assert_eq!(tokens.len(), 676);
    }
}
