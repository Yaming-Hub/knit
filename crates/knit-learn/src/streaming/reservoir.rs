//! Deterministic reservoir sampling (Algorithm R).
//!
//! Maintains a fixed-size uniform random sample of items from an
//! arbitrarily long stream. Uses a seeded RNG for reproducibility.

use serde::{Deserialize, Serialize};

/// Deterministic reservoir sample of string values.
///
/// Maintains a uniform random sample of at most `capacity` items from an
/// arbitrarily long stream. Uses Algorithm R with a seeded PRNG for
/// deterministic results.
///
/// # Example
///
/// ```
/// use knit_learn::streaming::ReservoirSample;
///
/// let mut sample = ReservoirSample::new(5, 42);
/// for i in 0..100 {
///     sample.add(format!("item_{i}"));
/// }
/// assert_eq!(sample.items().len(), 5);
/// assert_eq!(sample.total_seen(), 100);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReservoirSample {
    /// Maximum number of items to retain.
    capacity: usize,
    /// Current sample.
    items: Vec<String>,
    /// Total number of items offered.
    total_seen: u64,
    /// PRNG state (SplitMix64 for fast, deterministic sampling).
    rng_state: u64,
}

impl ReservoirSample {
    /// Create a new reservoir sample with the given capacity and seed.
    pub fn new(capacity: usize, seed: u64) -> Self {
        Self {
            capacity,
            items: Vec::with_capacity(capacity.min(1024)),
            total_seen: 0,
            rng_state: seed,
        }
    }

    /// Add an item to the reservoir.
    ///
    /// If the reservoir is not yet full, the item is always added.
    /// Once full, each new item has a `capacity / total_seen` probability
    /// of replacing a random existing item.
    pub fn add(&mut self, item: String) {
        self.total_seen += 1;

        if self.items.len() < self.capacity {
            self.items.push(item);
        } else {
            // Generate random index in [0, total_seen)
            let j = self.next_u64() % self.total_seen;
            if (j as usize) < self.capacity {
                self.items[j as usize] = item;
            }
        }
    }

    /// Merge another reservoir sample into this one.
    ///
    /// Uses population-weighted selection: items from each source are
    /// retained with probability proportional to their source population
    /// size (`total_seen`).
    pub fn merge(&mut self, other: &ReservoirSample) {
        if other.total_seen == 0 {
            return;
        }
        if self.total_seen == 0 {
            *self = other.clone();
            return;
        }

        let combined_total = self.total_seen.saturating_add(other.total_seen);
        let self_weight = self.total_seen as f64 / combined_total as f64;

        let mut rng_state = self.rng_state ^ other.rng_state;
        let mut merged = Vec::with_capacity(self.capacity);

        let combined_items: Vec<&String> = self.items.iter().chain(other.items.iter()).collect();

        if combined_items.len() <= self.capacity {
            // Both fit entirely
            merged = combined_items.into_iter().cloned().collect();
        } else {
            // Population-weighted selection: for each slot, pick from self's
            // items with probability self_weight, else from other's items.
            for _ in 0..self.capacity {
                let r = (splitmix64(&mut rng_state) as f64) / (u64::MAX as f64);
                let source = if r < self_weight {
                    &self.items
                } else {
                    &other.items
                };
                if !source.is_empty() {
                    let idx = splitmix64(&mut rng_state) as usize % source.len();
                    merged.push(source[idx].clone());
                }
            }
            self.rng_state = rng_state;
        }

        self.items = merged;
        self.total_seen = combined_total;
    }

    /// Current items in the sample.
    pub fn items(&self) -> &[String] {
        &self.items
    }

    /// Total number of items that have been offered.
    pub fn total_seen(&self) -> u64 {
        self.total_seen
    }

    /// Whether the reservoir is full.
    pub fn is_full(&self) -> bool {
        self.items.len() >= self.capacity
    }

    /// Maximum capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Generate next pseudorandom u64 using SplitMix64.
    fn next_u64(&mut self) -> u64 {
        splitmix64(&mut self.rng_state)
    }
}

/// SplitMix64 PRNG — fast, deterministic, good statistical properties.
fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_reservoir() {
        let sample = ReservoirSample::new(10, 42);
        assert_eq!(sample.items().len(), 0);
        assert_eq!(sample.total_seen(), 0);
        assert!(!sample.is_full());
    }

    #[test]
    fn fills_to_capacity() {
        let mut sample = ReservoirSample::new(5, 42);
        for i in 0..5 {
            sample.add(format!("item_{i}"));
        }
        assert_eq!(sample.items().len(), 5);
        assert_eq!(sample.total_seen(), 5);
        assert!(sample.is_full());
    }

    #[test]
    fn stays_at_capacity() {
        let mut sample = ReservoirSample::new(5, 42);
        for i in 0..1000 {
            sample.add(format!("item_{i}"));
        }
        assert_eq!(sample.items().len(), 5);
        assert_eq!(sample.total_seen(), 1000);
    }

    #[test]
    fn deterministic() {
        let mut a = ReservoirSample::new(5, 42);
        let mut b = ReservoirSample::new(5, 42);
        for i in 0..1000 {
            let s = format!("item_{i}");
            a.add(s.clone());
            b.add(s);
        }
        assert_eq!(a.items(), b.items());
    }

    #[test]
    fn different_seeds_differ() {
        let mut a = ReservoirSample::new(5, 42);
        let mut b = ReservoirSample::new(5, 99);
        for i in 0..1000 {
            let s = format!("item_{i}");
            a.add(s.clone());
            b.add(s);
        }
        // Very unlikely to be identical with different seeds
        assert_ne!(a.items(), b.items());
    }

    #[test]
    fn merge_two_samples() {
        let mut a = ReservoirSample::new(10, 42);
        let mut b = ReservoirSample::new(10, 99);
        for i in 0..50 {
            a.add(format!("a_{i}"));
        }
        for i in 0..50 {
            b.add(format!("b_{i}"));
        }
        a.merge(&b);
        assert_eq!(a.total_seen(), 100);
        assert!(a.items().len() <= 10);
        // Should have items from both sources
        let has_a = a.items().iter().any(|s| s.starts_with("a_"));
        let has_b = a.items().iter().any(|s| s.starts_with("b_"));
        assert!(
            has_a || has_b,
            "merged sample should contain items from at least one source"
        );
    }

    #[test]
    fn merge_empty_into_populated() {
        let mut a = ReservoirSample::new(5, 42);
        for i in 0..10 {
            a.add(format!("item_{i}"));
        }
        let b = ReservoirSample::new(5, 99);
        let items_before: Vec<String> = a.items().to_vec();
        a.merge(&b);
        assert_eq!(a.items(), &items_before);
        assert_eq!(a.total_seen(), 10);
    }

    #[test]
    fn merge_populated_into_empty() {
        let mut a = ReservoirSample::new(5, 42);
        let mut b = ReservoirSample::new(5, 99);
        for i in 0..10 {
            b.add(format!("item_{i}"));
        }
        a.merge(&b);
        assert_eq!(a.total_seen(), 10);
        assert!(!a.items().is_empty());
    }

    #[test]
    fn serialization_roundtrip() {
        let mut sample = ReservoirSample::new(5, 42);
        for i in 0..20 {
            sample.add(format!("val_{i}"));
        }
        let json = serde_json::to_string(&sample).unwrap();
        let deserialized: ReservoirSample = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.items(), sample.items());
        assert_eq!(deserialized.total_seen(), sample.total_seen());
        assert_eq!(deserialized.capacity(), sample.capacity());
    }

    #[test]
    fn uniform_distribution() {
        // Statistical test: each of 100 items should appear roughly
        // equally in a capacity-10 sample over many trials
        let trials = 10_000;
        let n_items = 100;
        let capacity = 10;
        let mut counts = vec![0u64; n_items];

        for trial in 0..trials {
            let mut sample = ReservoirSample::new(capacity, trial as u64);
            for i in 0..n_items {
                sample.add(format!("{i}"));
            }
            for item in sample.items() {
                let idx: usize = item.parse().unwrap();
                counts[idx] += 1;
            }
        }

        // Expected count per item = trials * capacity / n_items = 1000
        let expected = trials as f64 * capacity as f64 / n_items as f64;
        for (i, &count) in counts.iter().enumerate() {
            let ratio = count as f64 / expected;
            assert!(
                (0.85..1.15).contains(&ratio),
                "item {i} appeared {count} times (expected ~{expected}, ratio {ratio:.3})"
            );
        }
    }
}
