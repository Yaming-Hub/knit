//! Memory-efficient key store using reservoir sampling.
//!
//! For very large parent entities (>100 M rows), keeping every primary key in
//! memory is impractical. [`SampledKeyStore`] maintains a fixed-capacity
//! reservoir sample using Algorithm R, giving each key an equal probability
//! of being retained regardless of insertion order.
//!
//! Implements the [`KeyStore`] trait so it can be used as a drop-in
//! replacement for [`InMemoryKeyStore`](crate::InMemoryKeyStore) in the
//! generation engine when [`KeyStoreKind::SampledSubset`] is selected.

use std::sync::RwLock;

use rand::RngCore;
use rand_chacha::ChaCha8Rng;
use rand::SeedableRng;

use crate::traits::KeyStore;

/// A memory-efficient key store that maintains a reservoir sample of keys.
///
/// Uses [Algorithm R](https://en.wikipedia.org/wiki/Reservoir_sampling#Simple_algorithm)
/// to keep a uniformly random subset of at most `capacity` keys. This enables
/// foreign-key sampling against entities with hundreds of millions of rows
/// without storing every key.
///
/// # Guarantees
///
/// - At most `capacity` keys are held in memory at any time.
/// - After `n` insertions (where `n > capacity`), each key has a
///   `capacity / n` probability of being in the reservoir.
/// - [`sample`](KeyStore::sample) returns a uniformly random element from the reservoir.
///
/// # Thread Safety
///
/// Uses [`RwLock`] to allow concurrent readers (FK samplers) with exclusive
/// writers (PK inserters), matching [`InMemoryKeyStore`](crate::InMemoryKeyStore).
pub struct SampledKeyStore {
    inner: RwLock<SampledInner>,
}

struct SampledInner {
    keys: Vec<i64>,
    capacity: usize,
    total_seen: u64,
    /// Dedicated RNG for reservoir replacement decisions.
    rng: ChaCha8Rng,
}

impl SampledKeyStore {
    /// Create a new sampled key store with the given maximum capacity.
    ///
    /// The `seed` is used for the internal RNG that drives reservoir
    /// replacement decisions (separate from the per-field generation RNG).
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    pub fn new(capacity: usize, seed: u64) -> Self {
        assert!(capacity > 0, "SampledKeyStore capacity must be > 0");
        Self {
            inner: RwLock::new(SampledInner {
                keys: Vec::with_capacity(capacity),
                capacity,
                total_seen: 0,
                rng: ChaCha8Rng::seed_from_u64(seed),
            }),
        }
    }

    /// Return the total number of keys that have been offered for insertion.
    pub fn total_seen(&self) -> u64 {
        self.inner.read().expect("sampled keystore lock poisoned").total_seen
    }

    /// Return the maximum capacity of the reservoir.
    pub fn capacity(&self) -> usize {
        self.inner.read().expect("sampled keystore lock poisoned").capacity
    }
}

impl KeyStore for SampledKeyStore {
    fn insert(&self, key: i64) {
        let mut inner = self.inner.write().expect("sampled keystore lock poisoned");
        inner.total_seen += 1;
        if inner.keys.len() < inner.capacity {
            inner.keys.push(key);
        } else {
            // Algorithm R: replace element at random index with probability capacity/total_seen.
            let j = inner.rng.next_u64() % inner.total_seen;
            if (j as usize) < inner.capacity {
                inner.keys[j as usize] = key;
            }
        }
    }

    fn sample(&self, rng: &mut dyn RngCore) -> Option<i64> {
        let keys = &self.inner.read().expect("sampled keystore lock poisoned").keys;
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
        Some(keys[idx])
    }

    fn len(&self) -> usize {
        self.inner.read().expect("sampled keystore lock poisoned").keys.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    #[test]
    fn reservoir_stays_within_capacity() {
        let capacity = 100;
        let store = SampledKeyStore::new(capacity, 42);

        for i in 0..10_000i64 {
            store.insert(i);
        }

        assert_eq!(store.len(), capacity);
        assert_eq!(store.total_seen(), 10_000);
    }

    #[test]
    fn sample_returns_none_when_empty() {
        let store = SampledKeyStore::new(10, 1);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        assert!(store.sample(&mut rng).is_none());
    }

    #[test]
    fn under_capacity_all_keys_retained() {
        let store = SampledKeyStore::new(100, 7);

        for i in 0..50 {
            store.insert(i);
        }

        assert_eq!(store.len(), 50);
        assert_eq!(store.total_seen(), 50);
    }

    #[test]
    fn sample_returns_valid_key() {
        let store = SampledKeyStore::new(10, 99);

        for i in 0..5 {
            store.insert(i);
        }

        let mut rng = ChaCha8Rng::seed_from_u64(99);
        let val = store.sample(&mut rng).expect("should have keys");
        assert!((0..5).contains(&val));
    }

    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn zero_capacity_panics() {
        SampledKeyStore::new(0, 0);
    }

    #[test]
    fn implements_keystore_trait() {
        // Verify it can be used as Arc<dyn KeyStore>
        let store: std::sync::Arc<dyn KeyStore> =
            std::sync::Arc::new(SampledKeyStore::new(100, 42));
        store.insert(1);
        store.insert(2);
        store.insert(3);
        assert_eq!(store.len(), 3);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        let val = store.sample(&mut rng).unwrap();
        assert!((1..=3).contains(&val));
    }
}
