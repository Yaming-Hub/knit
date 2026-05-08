//! Thread/conversation generator — creates self-referential conversation trees.
//!
//! For each row, decides whether to start a new thread (emits NULL) or reply to
//! a recent message (emits that message's PK). Uses a recency-weighted ring
//! buffer to select reply targets, creating realistic conversation structures
//! where recent messages are more likely to receive replies.

use std::sync::Mutex;

use arrow::array::{ArrayRef, Int64Array};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::context::GenContext;
use crate::traits::FieldGenerator;

/// Entry in the thread ring buffer tracking a generated PK and its thread depth.
#[derive(Clone, Copy)]
struct ThreadEntry {
    pk: i64,
    depth: u32,
}

/// Mutable state for the thread generator, protected by a Mutex for thread safety.
struct ThreadState {
    /// Ring buffer of recent messages (PK + depth).
    buffer: Vec<ThreadEntry>,
    /// Current write position in the ring buffer.
    write_pos: usize,
    /// Number of entries actually filled (≤ buffer capacity).
    count: usize,
}

impl ThreadState {
    fn new(capacity: usize) -> Self {
        Self {
            buffer: Vec::with_capacity(capacity),
            write_pos: 0,
            count: 0,
        }
    }

    fn push(&mut self, entry: ThreadEntry) {
        if self.buffer.len() < self.buffer.capacity() {
            self.buffer.push(entry);
        } else {
            self.buffer[self.write_pos] = entry;
        }
        self.write_pos = (self.write_pos + 1) % self.buffer.capacity();
        self.count = (self.count + 1).min(self.buffer.capacity());
    }

    /// Select a reply target weighted by recency (exponential decay).
    /// Returns the PK and depth of the selected entry, or None if buffer is empty
    /// or all entries exceed max_depth.
    fn select_reply(&self, rng: &mut dyn RngCore, max_depth: u32) -> Option<ThreadEntry> {
        if self.count == 0 {
            return None;
        }

        // Filter to entries whose reply would not exceed max depth.
        // A reply to an entry at depth D creates a child at depth D+1,
        // so we only allow parents with depth < max_depth - 1.
        let depth_limit = if max_depth > 0 { max_depth - 1 } else { 0 };
        let capacity = self.buffer.capacity().max(1);
        let mut eligible: Vec<(ThreadEntry, f64)> = Vec::new();

        for i in 0..self.count {
            let entry = &self.buffer[i];
            if entry.depth > depth_limit {
                continue;
            }
            // Age: how many entries ago this was written
            let age = if self.write_pos > i {
                self.write_pos - i - 1
            } else {
                capacity - i - 1 + self.write_pos
            };
            let weight = (-0.05 * age as f64).exp();
            eligible.push((*entry, weight));
        }

        if eligible.is_empty() {
            return None;
        }

        let total: f64 = eligible.iter().map(|(_, w)| w).sum();
        if total <= 0.0 {
            return Some(eligible[0].0);
        }

        // Sample using CDF
        let u = gen_f64(rng) * total;
        let mut cumulative = 0.0;
        for (entry, w) in &eligible {
            cumulative += w;
            if u <= cumulative {
                return Some(*entry);
            }
        }
        Some(eligible.last().unwrap().0)
    }
}

/// Generate a uniform f64 in [0, 1) from an RNG.
fn gen_f64(rng: &mut dyn RngCore) -> f64 {
    let bits = rng.next_u64() >> 11;
    bits as f64 / (1u64 << 53) as f64
}

/// Generates self-referential thread/conversation structure.
///
/// Produces nullable Int64 arrays where NULL = thread starter, non-null = reply
/// pointing to a previous row's PK in the same entity. Uses a recency-weighted
/// ring buffer so recent messages are more likely to receive replies.
pub struct ThreadRefGenerator {
    /// Probability that a row is a reply (vs. starting a new thread).
    reply_probability: f64,
    /// Maximum thread depth before forcing a new thread.
    max_depth: u32,
    /// Name of the PK field to read from batch_columns.
    pk_field: String,
    /// Per-generator mutable state (ring buffer of recent PKs + depths).
    state: Mutex<ThreadState>,
}

impl ThreadRefGenerator {
    /// Create a new thread-ref generator.
    ///
    /// * `reply_probability` — chance each row is a reply (vs. thread starter)
    /// * `max_depth` — maximum reply chain depth
    /// * `reply_window` — ring buffer capacity for recent PKs
    /// * `pk_field` — name of the primary key field to read from batch columns
    pub fn new(reply_probability: f64, max_depth: u32, reply_window: usize, pk_field: String) -> Self {
        Self {
            reply_probability,
            max_depth,
            pk_field,
            state: Mutex::new(ThreadState::new(reply_window.max(1))),
        }
    }
}

impl FieldGenerator for ThreadRefGenerator {
    fn generate(&self, rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        // Read the PK column (already generated for this batch)
        let pk_col = ctx.batch_columns.get(&self.pk_field)
            .expect("thread_ref: PK field must be generated before thread_ref field");
        let pks: Vec<i64> = pk_col.as_any().downcast_ref::<Int64Array>()
            .expect("thread_ref: PK field must be Int64")
            .values()
            .iter()
            .copied()
            .collect();

        let mut state = self.state.lock().unwrap();
        let mut values: Vec<Option<i64>> = Vec::with_capacity(count);

        for i in 0..count {
            let pk = pks[i];
            let is_reply = gen_f64(rng) < self.reply_probability;

            let parent = if is_reply {
                state.select_reply(rng, self.max_depth)
            } else {
                None
            };

            let (value, depth) = if let Some(parent_entry) = parent {
                (Some(parent_entry.pk), parent_entry.depth + 1)
            } else {
                (None, 0) // Thread starter
            };

            values.push(value);

            // Add this message to the ring buffer
            state.push(ThreadEntry { pk, depth });
        }

        let array = Int64Array::from(values);
        std::sync::Arc::new(array) as ArrayRef
    }

    fn output_type(&self) -> DataType {
        DataType::Int64
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use arrow::array::{Array, Int64Array};
    use rand::SeedableRng;
    use rand_chacha::ChaCha8Rng;

    fn run_with_pks(gen: &ThreadRefGenerator, rng: &mut ChaCha8Rng, pks: &[i64]) -> ArrayRef {
        let pk_array: ArrayRef = std::sync::Arc::new(Int64Array::from(pks.to_vec()));
        let mut columns = HashMap::new();
        columns.insert("id".to_string(), pk_array);
        let ctx = GenContext::new(&columns, 0, 0, 1, "messages");
        gen.generate(rng, pks.len(), &ctx)
    }

    #[test]
    fn thread_starter_always_null() {
        let gen = ThreadRefGenerator::new(0.0, 10, 100, "id".to_string());
        let mut rng = ChaCha8Rng::seed_from_u64(42);
        let pks: Vec<i64> = (1..=50).collect();
        let result = run_with_pks(&gen, &mut rng, &pks);
        let arr = result.as_any().downcast_ref::<Int64Array>().unwrap();
        for i in 0..50 {
            assert!(arr.is_null(i), "row {i} should be null (thread starter)");
        }
    }

    #[test]
    fn all_replies_reference_valid_pks() {
        let gen = ThreadRefGenerator::new(1.0, 100, 50, "id".to_string());
        let mut rng = ChaCha8Rng::seed_from_u64(123);
        let pks: Vec<i64> = (100..=199).collect();
        let result = run_with_pks(&gen, &mut rng, &pks);
        let arr = result.as_any().downcast_ref::<Int64Array>().unwrap();

        // First row must be null (no prior messages to reply to)
        assert!(arr.is_null(0), "first row should be null (no prior messages)");

        // Subsequent rows should reference PKs that appeared before them
        let mut seen_pks = vec![pks[0]];
        for i in 1..100 {
            if !arr.is_null(i) {
                let parent_pk = arr.value(i);
                assert!(
                    seen_pks.contains(&parent_pk),
                    "row {i} references PK {parent_pk} which hasn't been generated yet"
                );
            }
            seen_pks.push(pks[i]);
        }
    }

    #[test]
    fn max_depth_respected() {
        let gen = ThreadRefGenerator::new(1.0, 2, 50, "id".to_string());
        let mut rng = ChaCha8Rng::seed_from_u64(456);
        let pks: Vec<i64> = (1..=200).collect();
        let result = run_with_pks(&gen, &mut rng, &pks);
        let arr = result.as_any().downcast_ref::<Int64Array>().unwrap();

        // Build depth map
        let mut depths: HashMap<i64, u32> = HashMap::new();
        for i in 0..200 {
            let pk = pks[i];
            let depth = if arr.is_null(i) {
                0
            } else {
                let parent_pk = arr.value(i);
                depths.get(&parent_pk).copied().unwrap_or(0) + 1
            };
            assert!(depth <= 2, "row {i} (pk={pk}) has depth {depth} which exceeds max_depth=2");
            depths.insert(pk, depth);
        }
    }

    #[test]
    fn deterministic_output() {
        let gen1 = ThreadRefGenerator::new(0.7, 5, 50, "id".to_string());
        let gen2 = ThreadRefGenerator::new(0.7, 5, 50, "id".to_string());
        let pks: Vec<i64> = (1..=30).collect();

        let mut rng1 = ChaCha8Rng::seed_from_u64(999);
        let mut rng2 = ChaCha8Rng::seed_from_u64(999);

        let r1 = run_with_pks(&gen1, &mut rng1, &pks);
        let r2 = run_with_pks(&gen2, &mut rng2, &pks);

        let a1 = r1.as_any().downcast_ref::<Int64Array>().unwrap();
        let a2 = r2.as_any().downcast_ref::<Int64Array>().unwrap();

        for i in 0..30 {
            assert_eq!(a1.is_null(i), a2.is_null(i), "row {i} null mismatch");
            if !a1.is_null(i) {
                assert_eq!(a1.value(i), a2.value(i), "row {i} value mismatch");
            }
        }
    }
}
