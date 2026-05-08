//! Space-Saving algorithm for approximate top-K frequent items.
//!
//! Tracks the K most frequent items in a stream using bounded memory.
//! Items with frequency > N/K are guaranteed to be tracked. The algorithm
//! may also track items with lower frequency, but their counts may be
//! over-estimated.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Approximate top-K frequent item tracker using the Space-Saving algorithm.
///
/// Maintains at most `capacity` items with their estimated frequencies.
/// When a new item arrives and the tracker is full, it replaces the
/// least-frequent item.
///
/// # Guarantees
///
/// - Any item with true frequency > N/K is guaranteed to be tracked
/// - Tracked counts may be over-estimated by at most the count of the
///   evicted item
///
/// # Example
///
/// ```
/// use knit_learn::streaming::TopKTracker;
///
/// let mut tracker = TopKTracker::new(3);
/// for _ in 0..10 { tracker.add("apple"); }
/// for _ in 0..5 { tracker.add("banana"); }
/// for _ in 0..1 { tracker.add("cherry"); }
///
/// let top = tracker.top_items();
/// assert_eq!(top[0].0, "apple");
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TopKTracker {
    /// Maximum number of items to track.
    capacity: usize,
    /// Items and their estimated counts.
    items: HashMap<String, u64>,
    /// Total items observed.
    total: u64,
}

impl TopKTracker {
    /// Create a new tracker with the given capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            items: HashMap::with_capacity(capacity),
            total: 0,
        }
    }

    /// Add an observation of the given item.
    pub fn add(&mut self, item: &str) {
        self.total += 1;

        if self.items.contains_key(item) {
            *self.items.get_mut(item).unwrap() += 1;
            return;
        }

        if self.items.len() < self.capacity {
            self.items.insert(item.to_string(), 1);
        } else {
            // Find the minimum-count item (deterministic tie-breaking by key)
            let min_entry = self
                .items
                .iter()
                .min_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)))
                .map(|(k, &v)| (k.clone(), v));

            if let Some((min_key, min_count)) = min_entry {
                self.items.remove(&min_key);
                // New item gets min_count + 1 (over-estimation)
                self.items.insert(item.to_string(), min_count + 1);
            }
        }
    }

    /// Merge another tracker into this one.
    ///
    /// Combines counts for shared items and keeps the top-K by count.
    pub fn merge(&mut self, other: &TopKTracker) {
        self.total = self.total.saturating_add(other.total);

        // Add all counts from other
        for (item, &count) in &other.items {
            *self.items.entry(item.clone()).or_insert(0) += count;
        }

        // Trim to capacity by removing lowest-count items
        // (deterministic tie-breaking by lexicographic key order)
        while self.items.len() > self.capacity {
            let min_key = self
                .items
                .iter()
                .min_by(|a, b| a.1.cmp(b.1).then(a.0.cmp(b.0)))
                .map(|(k, _)| k.clone());
            if let Some(key) = min_key {
                self.items.remove(&key);
            }
        }
    }

    /// Return items sorted by frequency (descending).
    pub fn top_items(&self) -> Vec<(String, u64)> {
        let mut items: Vec<(String, u64)> =
            self.items.iter().map(|(k, &v)| (k.clone(), v)).collect();
        items.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
        items
    }

    /// Get the estimated count for a specific item, or `None` if not tracked.
    pub fn get_count(&self, item: &str) -> Option<u64> {
        self.items.get(item).copied()
    }

    /// Total number of observations.
    pub fn total(&self) -> u64 {
        self.total
    }

    /// Number of distinct items currently tracked.
    pub fn tracked_count(&self) -> usize {
        self.items.len()
    }

    /// Maximum capacity.
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_tracker() {
        let tracker = TopKTracker::new(10);
        assert_eq!(tracker.total(), 0);
        assert_eq!(tracker.tracked_count(), 0);
        assert!(tracker.top_items().is_empty());
    }

    #[test]
    fn tracks_within_capacity() {
        let mut tracker = TopKTracker::new(5);
        tracker.add("a");
        tracker.add("b");
        tracker.add("c");
        assert_eq!(tracker.tracked_count(), 3);
        assert_eq!(tracker.get_count("a"), Some(1));
        assert_eq!(tracker.get_count("b"), Some(1));
    }

    #[test]
    fn frequency_ordering() {
        let mut tracker = TopKTracker::new(10);
        for _ in 0..10 {
            tracker.add("apple");
        }
        for _ in 0..5 {
            tracker.add("banana");
        }
        for _ in 0..1 {
            tracker.add("cherry");
        }
        let top = tracker.top_items();
        assert_eq!(top[0].0, "apple");
        assert_eq!(top[0].1, 10);
        assert_eq!(top[1].0, "banana");
        assert_eq!(top[1].1, 5);
        assert_eq!(top[2].0, "cherry");
        assert_eq!(top[2].1, 1);
    }

    #[test]
    fn eviction_at_capacity() {
        let mut tracker = TopKTracker::new(3);
        // Add 3 items
        tracker.add("a");
        tracker.add("b");
        tracker.add("c");
        assert_eq!(tracker.tracked_count(), 3);

        // Add a 4th — should evict one
        tracker.add("d");
        assert_eq!(tracker.tracked_count(), 3);
        assert_eq!(tracker.total(), 4);
    }

    #[test]
    fn frequent_items_survive_eviction() {
        let mut tracker = TopKTracker::new(3);
        // Make "apple" very frequent
        for _ in 0..100 {
            tracker.add("apple");
        }
        // Add many unique items
        for i in 0..50 {
            tracker.add(&format!("rare_{i}"));
        }
        // "apple" should still be tracked
        assert!(tracker.get_count("apple").is_some());
        assert_eq!(tracker.get_count("apple").unwrap(), 100);
    }

    #[test]
    fn merge_two_trackers() {
        let mut a = TopKTracker::new(5);
        let mut b = TopKTracker::new(5);
        for _ in 0..10 {
            a.add("shared");
            b.add("shared");
        }
        for _ in 0..5 {
            a.add("only_a");
        }
        for _ in 0..5 {
            b.add("only_b");
        }
        a.merge(&b);
        assert_eq!(a.total(), 30);
        assert_eq!(a.get_count("shared"), Some(20));
    }

    #[test]
    fn serialization_roundtrip() {
        let mut tracker = TopKTracker::new(5);
        for _ in 0..10 {
            tracker.add("foo");
        }
        tracker.add("bar");
        let json = serde_json::to_string(&tracker).unwrap();
        let deserialized: TopKTracker = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.total(), tracker.total());
        assert_eq!(deserialized.get_count("foo"), tracker.get_count("foo"));
    }
}
