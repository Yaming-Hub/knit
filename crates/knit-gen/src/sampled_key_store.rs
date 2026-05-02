//! Memory-efficient key store using reservoir sampling.
//!
//! For very large parent entities (>100 M rows), keeping every primary key in
//! memory is impractical. [`SampledKeyStore`] maintains a fixed-capacity
//! reservoir sample using Algorithm R, giving each key an equal probability
//! of being retained regardless of insertion order.

use knit_core::Value;
use rand::RngCore;

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
/// - [`sample`](Self::sample) returns a uniformly random element from the reservoir.
pub struct SampledKeyStore {
    keys: Vec<Value>,
    capacity: usize,
    total_seen: u64,
}

impl SampledKeyStore {
    /// Create a new sampled key store with the given maximum capacity.
    ///
    /// # Panics
    ///
    /// Panics if `capacity` is zero.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0, "SampledKeyStore capacity must be > 0");
        Self {
            keys: Vec::with_capacity(capacity),
            capacity,
            total_seen: 0,
        }
    }

    /// Insert a key into the reservoir.
    ///
    /// If the reservoir is not yet full, the key is appended directly.
    /// Otherwise, it replaces a random existing key with probability
    /// `capacity / total_seen`, implementing Algorithm R.
    pub fn insert(&mut self, key: Value, rng: &mut dyn RngCore) {
        self.total_seen += 1;
        if self.keys.len() < self.capacity {
            self.keys.push(key);
        } else {
            // Algorithm R: replace element at random index with probability capacity/total_seen.
            let j = rng.next_u64() % self.total_seen;
            if (j as usize) < self.capacity {
                self.keys[j as usize] = key;
            }
        }
    }

    /// Sample a random key from the reservoir.
    ///
    /// Returns `None` if no keys have been inserted.
    pub fn sample(&self, rng: &mut dyn RngCore) -> Option<&Value> {
        if self.keys.is_empty() {
            return None;
        }
        let idx = (rng.next_u64() % self.keys.len() as u64) as usize;
        Some(&self.keys[idx])
    }

    /// Return the number of keys currently in the reservoir.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    /// Returns `true` if the reservoir is empty.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Return the total number of keys that have been offered for insertion.
    pub fn total_seen(&self) -> u64 {
        self.total_seen
    }

    /// Return the maximum capacity of the reservoir.
    pub fn capacity(&self) -> usize {
        self.capacity
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
        let mut store = SampledKeyStore::new(capacity);
        let mut rng = ChaCha8Rng::seed_from_u64(42);

        for i in 0..10_000u64 {
            store.insert(Value::Int(i as i64), &mut rng);
        }

        assert_eq!(store.len(), capacity);
        assert_eq!(store.total_seen(), 10_000);
    }

    #[test]
    fn sample_returns_none_when_empty() {
        let store = SampledKeyStore::new(10);
        let mut rng = ChaCha8Rng::seed_from_u64(1);
        assert!(store.sample(&mut rng).is_none());
    }

    #[test]
    fn under_capacity_all_keys_retained() {
        let mut store = SampledKeyStore::new(100);
        let mut rng = ChaCha8Rng::seed_from_u64(7);

        for i in 0..50 {
            store.insert(Value::Int(i), &mut rng);
        }

        assert_eq!(store.len(), 50);
        assert_eq!(store.total_seen(), 50);
    }

    #[test]
    fn sample_returns_valid_key() {
        let mut store = SampledKeyStore::new(10);
        let mut rng = ChaCha8Rng::seed_from_u64(99);

        for i in 0..5 {
            store.insert(Value::Int(i), &mut rng);
        }

        let val = store.sample(&mut rng).expect("should have keys");
        match val {
            Value::Int(v) => assert!((0..5).contains(v)),
            _ => panic!("expected Int value"),
        }
    }

    #[test]
    #[should_panic(expected = "capacity must be > 0")]
    fn zero_capacity_panics() {
        SampledKeyStore::new(0);
    }
}
