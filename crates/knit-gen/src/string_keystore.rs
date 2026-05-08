//! Thread-safe in-memory key store for string/UUID foreign-key sampling.
//!
//! This module provides [`InMemoryStringKeyStore`], the string-typed counterpart
//! to [`InMemoryKeyStore`](crate::InMemoryKeyStore). It stores UUID or string
//! primary-key values and supports uniform random sampling for FK resolution.

use rand::RngCore;
use std::sync::RwLock;

use crate::traits::StringKeyStore;

/// In-memory key store for string/UUID keys backed by a `Vec<String>` behind a [`RwLock`].
///
/// Suitable for entities with up to ~10 M rows. Uses the same unbiased
/// rejection-based sampling as [`InMemoryKeyStore`](crate::InMemoryKeyStore).
pub struct InMemoryStringKeyStore {
    keys: RwLock<Vec<String>>,
}

impl InMemoryStringKeyStore {
    /// Create a new, empty string key store.
    pub fn new() -> Self {
        Self {
            keys: RwLock::new(Vec::new()),
        }
    }

    /// Create a key store pre-allocated for `capacity` keys.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            keys: RwLock::new(Vec::with_capacity(capacity)),
        }
    }
}

impl Default for InMemoryStringKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

impl StringKeyStore for InMemoryStringKeyStore {
    fn insert(&self, key: String) {
        self.keys
            .write()
            .expect("string keystore lock poisoned")
            .push(key);
    }

    fn sample(&self, rng: &mut dyn RngCore) -> Option<String> {
        let keys = self.keys.read().expect("string keystore lock poisoned");
        if keys.is_empty() {
            return None;
        }
        // Unbiased sampling via rejection method.
        let len = keys.len() as u64;
        let threshold = u64::MAX - (u64::MAX % len);
        let idx = loop {
            let r = rng.next_u64();
            if r < threshold {
                break (r % len) as usize;
            }
        };
        Some(keys[idx].clone())
    }

    fn len(&self) -> usize {
        self.keys
            .read()
            .expect("string keystore lock poisoned")
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn empty_store_returns_none() {
        let store = InMemoryStringKeyStore::new();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        assert_eq!(store.sample(&mut rng), None);
    }

    #[test]
    fn insert_and_sample() {
        let store = InMemoryStringKeyStore::new();
        store.insert("abc-123".to_string());
        store.insert("def-456".to_string());
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let val = store.sample(&mut rng).unwrap();
        assert!(val == "abc-123" || val == "def-456");
    }

    #[test]
    fn len_tracks_inserts() {
        let store = InMemoryStringKeyStore::with_capacity(10);
        assert_eq!(store.len(), 0);
        assert!(store.is_empty());
        store.insert("x".to_string());
        assert_eq!(store.len(), 1);
        assert!(!store.is_empty());
    }

    #[test]
    fn sampling_is_uniform() {
        let store = InMemoryStringKeyStore::new();
        store.insert("a".to_string());
        store.insert("b".to_string());
        let mut rng = ChaCha8Rng::seed_from_u64(99);
        let mut counts = std::collections::HashMap::new();
        for _ in 0..1000 {
            let v = store.sample(&mut rng).unwrap();
            *counts.entry(v).or_insert(0) += 1;
        }
        // Both should get roughly 500 (within 100 of 500).
        let a_count = counts.get("a").copied().unwrap_or(0);
        let b_count = counts.get("b").copied().unwrap_or(0);
        assert!((400..=600).contains(&a_count), "a_count={a_count}");
        assert!((400..=600).contains(&b_count), "b_count={b_count}");
    }

    #[test]
    fn concurrent_insert_and_sample() {
        use std::sync::Arc;
        use std::thread;

        let store = Arc::new(InMemoryStringKeyStore::new());
        // Insert from one thread.
        let store_w = Arc::clone(&store);
        let writer = thread::spawn(move || {
            for i in 0..100 {
                store_w.insert(format!("key-{i}"));
            }
        });
        writer.join().unwrap();
        assert_eq!(store.len(), 100);

        // Sample from another thread.
        let store_r = Arc::clone(&store);
        let reader = thread::spawn(move || {
            let mut rng = ChaCha8Rng::seed_from_u64(7);
            store_r.sample(&mut rng).unwrap()
        });
        let val = reader.join().unwrap();
        assert!(val.starts_with("key-"));
    }
}
