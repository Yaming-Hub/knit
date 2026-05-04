//! Thread-safe in-memory key store for foreign-key sampling.
//!
//! This module provides [`InMemoryKeyStore`], the default [`KeyStore`]
//! implementation used during generation. Primary-key generators insert keys
//! as rows are produced; foreign-key generators in downstream entities sample
//! from this store to maintain referential integrity.

use rand::RngCore;
use std::sync::RwLock;

use crate::traits::KeyStore;

/// In-memory key store backed by a `Vec<i64>` behind a [`RwLock`].
///
/// Suitable for entities with up to ~10 M rows. Larger entities may benefit
/// from memory-mapped or sampled stores.
///
/// # Thread Safety
///
/// Uses [`RwLock`] to allow concurrent readers (FK samplers) with exclusive
/// writers (PK inserters). The generation engine ensures inserts complete
/// before downstream FK generators begin sampling.
///
/// # Sampling Fairness
///
/// Uses rejection-based uniform sampling to avoid modulo bias.
pub struct InMemoryKeyStore {
    keys: RwLock<Vec<i64>>,
}

impl InMemoryKeyStore {
    /// Create a new, empty key store.
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

impl Default for InMemoryKeyStore {
    fn default() -> Self {
        Self::new()
    }
}

impl KeyStore for InMemoryKeyStore {
    fn insert(&self, key: i64) {
        self.keys.write().expect("keystore lock poisoned").push(key);
    }

    fn sample(&self, rng: &mut dyn RngCore) -> Option<i64> {
        let keys = self.keys.read().expect("keystore lock poisoned");
        if keys.is_empty() {
            return None;
        }
        // Unbiased sampling via rejection method: find the largest multiple of len
        // that fits in u64, reject samples above it, then take modulo.
        let len = keys.len() as u64;
        let threshold = u64::MAX - (u64::MAX % len);
        let idx = loop {
            let r = rng.next_u64();
            if r < threshold {
                break (r % len) as usize;
            }
        };
        Some(keys[idx])
    }

    fn len(&self) -> usize {
        self.keys.read().expect("keystore lock poisoned").len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_chacha::ChaCha8Rng;
    use rand::SeedableRng;

    #[test]
    fn empty_store_returns_none() {
        let store = InMemoryKeyStore::new();
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        assert!(store.sample(&mut rng).is_none());
        assert!(store.is_empty());
        assert_eq!(store.len(), 0);
    }

    #[test]
    fn single_key_always_returned() {
        let store = InMemoryKeyStore::new();
        store.insert(7);
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        for _ in 0..100 {
            assert_eq!(store.sample(&mut rng), Some(7));
        }
    }

    #[test]
    fn all_inserted_keys_sampled() {
        let store = InMemoryKeyStore::with_capacity(10);
        for k in 0..10 {
            store.insert(k);
        }
        assert_eq!(store.len(), 10);

        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let mut seen = std::collections::HashSet::new();
        // With 10 keys and 1000 samples, we expect to see all keys
        for _ in 0..1000 {
            seen.insert(store.sample(&mut rng).unwrap());
        }
        assert_eq!(seen.len(), 10, "expected all 10 keys to be sampled");
    }

    #[test]
    fn deterministic_sampling() {
        let store = InMemoryKeyStore::new();
        for k in 1..=50 {
            store.insert(k);
        }
        let mut rng1 = ChaCha8Rng::seed_from_u64(99);
        let mut rng2 = ChaCha8Rng::seed_from_u64(99);
        for _ in 0..100 {
            assert_eq!(store.sample(&mut rng1), store.sample(&mut rng2));
        }
    }
}
