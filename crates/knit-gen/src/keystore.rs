//! Thread-safe in-memory key store for foreign-key sampling.

use rand::RngCore;
use std::sync::RwLock;

use crate::traits::KeyStore;

/// In-memory key store backed by a `Vec<i64>` behind a `RwLock`.
///
/// Suitable for entities with up to ~10 M rows. Larger entities should
/// use memory-mapped or sampled stores (future PRs).
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
