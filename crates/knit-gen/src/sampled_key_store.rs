//! Memory-efficient key store using hash-based reservoir sampling.
//!
//! For very large parent entities (>100 M rows), keeping every primary key in
//! memory is impractical. [`SampledKeyStore`] maintains a fixed-capacity
//! sample using order-independent hash-based selection, giving deterministic
//! results regardless of insertion order (important for parallel partitions).
//!
//! Implements the [`KeyStore`] trait so it can be used as a drop-in
//! replacement for [`InMemoryKeyStore`](crate::InMemoryKeyStore) in the
//! generation engine when [`KeyStoreKind::SampledSubset`] is selected.

use std::collections::BinaryHeap;
use std::sync::RwLock;

use rand::RngCore;

use crate::traits::KeyStore;

/// Fast bijective hash (splitmix64) for deterministic key priorities.
fn key_hash(seed: u64, key: i64) -> u64 {
    let mut x = seed.wrapping_add(key as u64).wrapping_mul(0x9E3779B97F4A7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D049BB133111EB);
    x ^ (x >> 31)
}

/// A memory-efficient key store that keeps a deterministic sample of keys.
///
/// Uses hash-based min-sampling: each key gets a deterministic priority via
/// `key_hash(seed, key)`, and the reservoir keeps the `capacity` keys with
/// the smallest hash values. This is **order-independent** — the same set of
/// inserted keys always produces the same reservoir, regardless of insertion
/// order or thread scheduling.
///
/// Internally uses a max-heap (by hash) of size `capacity` for O(log k)
/// replacement during inserts. On first sample, the heap is flattened to a
/// Vec for O(1) random access.
///
/// # Guarantees
///
/// - At most `capacity` keys are held in memory at any time.
/// - The stored sample is deterministic for a given `(seed, key set)` pair.
/// - [`sample`](KeyStore::sample) returns a uniformly random element from the reservoir.
///
/// # Thread Safety
///
/// Uses [`RwLock`] for concurrent access. The engine guarantees that inserts
/// (parent PK generation) complete before samples (child FK generation) begin.
pub struct SampledKeyStore {
    inner: RwLock<SampledInner>,
}

struct SampledInner {
    /// Max-heap of (hash, key). Keeps the `capacity` keys with smallest hashes.
    heap: BinaryHeap<(u64, i64)>,
    /// Flattened keys for sampling — populated lazily on first `sample()` call.
    keys_cache: Option<Vec<i64>>,
    capacity: usize,
    total_seen: u64,
    seed: u64,
}

impl SampledKeyStore {
    /// Create a new sampled key store with the given maximum capacity.
    ///
    /// The `seed` mixes with each key's value to produce deterministic,
    /// order-independent priority scores.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    pub fn new(capacity: usize, seed: u64) -> Self {
        assert!(capacity > 0, "SampledKeyStore capacity must be > 0");
        Self {
            inner: RwLock::new(SampledInner {
                heap: BinaryHeap::with_capacity(capacity + 1),
                keys_cache: None,
                capacity,
                total_seen: 0,
                seed,
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
        inner.keys_cache = None; // invalidate sampling cache

        let hash = key_hash(inner.seed, key);

        if inner.heap.len() < inner.capacity {
            inner.heap.push((hash, key));
        } else if let Some(&(max_hash, _)) = inner.heap.peek() {
            if hash < max_hash {
                inner.heap.pop();
                inner.heap.push((hash, key));
            }
        }
    }

    fn sample(&self, rng: &mut dyn RngCore) -> Option<i64> {
        let mut inner = self.inner.write().expect("sampled keystore lock poisoned");

        // Lazily flatten heap to vec on first sample call.
        if inner.keys_cache.is_none() {
            inner.keys_cache = Some(inner.heap.iter().map(|&(_, k)| k).collect());
        }
        let keys = inner.keys_cache.as_ref().expect("just populated");

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
        self.inner.read().expect("sampled keystore lock poisoned").heap.len()
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

    #[test]
    fn insertion_order_independent() {
        // Same keys inserted in different orders must produce the same reservoir.
        let forward = SampledKeyStore::new(50, 123);
        for i in 0..1000i64 {
            forward.insert(i);
        }

        let reverse = SampledKeyStore::new(50, 123);
        for i in (0..1000i64).rev() {
            reverse.insert(i);
        }

        // Sample enough times to observe all reservoir keys with high probability.
        let mut rng1 = ChaCha8Rng::seed_from_u64(0);
        let mut rng2 = ChaCha8Rng::seed_from_u64(0);
        let fwd_set: std::collections::HashSet<i64> = (0..5000)
            .filter_map(|_| forward.sample(&mut rng1))
            .collect();
        let rev_set: std::collections::HashSet<i64> = (0..5000)
            .filter_map(|_| reverse.sample(&mut rng2))
            .collect();

        assert_eq!(fwd_set.len(), 50, "should see all 50 reservoir keys");
        assert_eq!(fwd_set, rev_set, "reservoir must be order-independent");
    }
}
