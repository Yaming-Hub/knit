//! HyperLogLog cardinality estimation.
//!
//! Estimates the number of distinct elements in a data stream using
//! O(1) memory (fixed-size register array). Uses the HyperLogLog++
//! algorithm with bias correction for small cardinalities.

use serde::{Deserialize, Serialize};

/// HyperLogLog cardinality estimator.
///
/// Uses `2^precision` registers (each a u8) to estimate distinct element
/// counts. With precision=14 (default), uses 16 KB of memory and achieves
/// ~0.8% standard error.
///
/// # Example
///
/// ```
/// use knit::learn::streaming::HyperLogLog;
///
/// let mut hll = HyperLogLog::new(14);
/// for i in 0..10_000 {
///     hll.add(&format!("item_{i}"));
/// }
/// let estimate = hll.cardinality();
/// // Within ~2% of 10,000
/// assert!((9500.0..10500.0).contains(&estimate));
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HyperLogLog {
    /// Precision parameter (number of bits for register index).
    precision: u8,
    /// Register array: each stores the maximum number of leading zeros + 1.
    registers: Vec<u8>,
}

impl HyperLogLog {
    /// Create a new HyperLogLog with the given precision.
    ///
    /// Precision must be between 4 and 18. Higher precision uses more memory
    /// but gives more accurate estimates.
    ///
    /// | Precision | Registers | Memory | Std Error |
    /// |-----------|-----------|--------|-----------|
    /// | 10 | 1,024 | 1 KB | ~3.25% |
    /// | 12 | 4,096 | 4 KB | ~1.625% |
    /// | 14 | 16,384 | 16 KB | ~0.8% |
    /// | 16 | 65,536 | 64 KB | ~0.4% |
    pub fn new(precision: u8) -> Self {
        let precision = precision.clamp(4, 18);
        let m = 1usize << precision;
        Self {
            precision,
            registers: vec![0; m],
        }
    }

    /// Add a string value to the estimator.
    pub fn add(&mut self, value: &str) {
        let hash = self.hash(value);
        self.add_hash(hash);
    }

    /// Add a pre-computed hash to the estimator.
    pub fn add_hash(&mut self, hash: u64) {
        let m = self.registers.len();
        let idx = (hash as usize) & (m - 1);
        let remaining = hash >> self.precision;
        // Count leading zeros in the remaining bits + 1 (ρ function).
        // For a k-bit word of all zeros, ρ = k + 1.
        let bits = 64 - self.precision;
        let rho = if remaining == 0 {
            bits + 1
        } else {
            (remaining.leading_zeros() as u8 - self.precision + 1).min(bits + 1)
        };
        if rho > self.registers[idx] {
            self.registers[idx] = rho;
        }
    }

    /// Estimate the number of distinct elements observed.
    pub fn cardinality(&self) -> f64 {
        let m = self.registers.len() as f64;
        let alpha_m = self.alpha();

        // Raw harmonic mean estimate
        let sum: f64 = self
            .registers
            .iter()
            .map(|&r| 2.0_f64.powi(-(r as i32)))
            .sum();
        let raw_estimate = alpha_m * m * m / sum;

        // Small range correction (linear counting)
        if raw_estimate <= 2.5 * m {
            let zeros = self.registers.iter().filter(|&&r| r == 0).count() as f64;
            if zeros > 0.0 {
                return m * (m / zeros).ln();
            }
        }

        // Large range correction (for 32-bit hash, not needed for 64-bit)
        // With 64-bit hash, no correction needed for practical cardinalities

        raw_estimate
    }

    /// Merge another HyperLogLog into this one.
    ///
    /// Both must have the same precision. After merging, this estimator
    /// reflects the union of both input sets.
    ///
    /// # Panics
    ///
    /// Panics if precisions don't match.
    pub fn merge(&mut self, other: &HyperLogLog) {
        assert_eq!(
            self.precision, other.precision,
            "cannot merge HyperLogLogs with different precisions ({} vs {})",
            self.precision, other.precision
        );
        for (i, &other_val) in other.registers.iter().enumerate() {
            if other_val > self.registers[i] {
                self.registers[i] = other_val;
            }
        }
    }

    /// Estimate the intersection cardinality with another HLL.
    ///
    /// Uses inclusion-exclusion: |A ∩ B| = |A| + |B| - |A ∪ B|
    ///
    /// Note: This can return negative values for nearly disjoint sets
    /// (due to estimation error). Returns 0.0 in that case.
    pub fn intersection_cardinality(&self, other: &HyperLogLog) -> f64 {
        let a = self.cardinality();
        let b = other.cardinality();
        let mut union = self.clone();
        union.merge(other);
        let ab = union.cardinality();
        (a + b - ab).max(0.0)
    }

    /// Precision parameter.
    pub fn precision(&self) -> u8 {
        self.precision
    }

    /// Number of registers.
    pub fn num_registers(&self) -> usize {
        self.registers.len()
    }

    /// Compute alpha_m constant for bias correction.
    fn alpha(&self) -> f64 {
        let m = self.registers.len() as f64;
        match self.registers.len() {
            16 => 0.673,
            32 => 0.697,
            64 => 0.709,
            _ => 0.7213 / (1.0 + 1.079 / m),
        }
    }

    /// Hash a string value using a variant of FNV-1a adapted for HLL.
    fn hash(&self, value: &str) -> u64 {
        // Use a good mixing function (MurmurHash3 finalizer)
        let mut h: u64 = 0xcbf29ce484222325;
        for byte in value.as_bytes() {
            h ^= *byte as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
        // Final mixing (MurmurHash3 64-bit finalizer)
        h ^= h >> 33;
        h = h.wrapping_mul(0xff51afd7ed558ccd);
        h ^= h >> 33;
        h = h.wrapping_mul(0xc4ceb9fe1a85ec53);
        h ^= h >> 33;
        h
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_hll() {
        let hll = HyperLogLog::new(14);
        assert_eq!(hll.cardinality(), 0.0);
        assert_eq!(hll.num_registers(), 16384);
    }

    #[test]
    fn small_cardinality() {
        let mut hll = HyperLogLog::new(14);
        hll.add("apple");
        hll.add("banana");
        hll.add("cherry");
        let est = hll.cardinality();
        // For very small sets, linear counting should be accurate
        assert!((2.0..5.0).contains(&est), "expected ~3, got {est}");
    }

    #[test]
    fn duplicates_dont_increase_count() {
        let mut hll = HyperLogLog::new(14);
        for _ in 0..1000 {
            hll.add("same_value");
        }
        let est = hll.cardinality();
        assert!(est < 2.0, "expected ~1, got {est}");
    }

    #[test]
    fn accuracy_at_10k() {
        let mut hll = HyperLogLog::new(14);
        for i in 0..10_000 {
            hll.add(&format!("item_{i}"));
        }
        let est = hll.cardinality();
        let error = (est - 10_000.0).abs() / 10_000.0;
        assert!(
            error < 0.05,
            "expected ~10000, got {est} (error {:.1}%)",
            error * 100.0
        );
    }

    #[test]
    fn accuracy_at_100k() {
        let mut hll = HyperLogLog::new(14);
        for i in 0..100_000 {
            hll.add(&format!("item_{i}"));
        }
        let est = hll.cardinality();
        let error = (est - 100_000.0).abs() / 100_000.0;
        assert!(
            error < 0.05,
            "expected ~100000, got {est} (error {:.1}%)",
            error * 100.0
        );
    }

    #[test]
    fn merge_disjoint() {
        let mut a = HyperLogLog::new(14);
        let mut b = HyperLogLog::new(14);
        for i in 0..5000 {
            a.add(&format!("a_{i}"));
        }
        for i in 0..5000 {
            b.add(&format!("b_{i}"));
        }
        a.merge(&b);
        let est = a.cardinality();
        let error = (est - 10_000.0).abs() / 10_000.0;
        assert!(
            error < 0.05,
            "expected ~10000 after merge, got {est} (error {:.1}%)",
            error * 100.0
        );
    }

    #[test]
    fn merge_overlapping() {
        let mut a = HyperLogLog::new(14);
        let mut b = HyperLogLog::new(14);
        // Both see items 0-4999
        for i in 0..5000 {
            a.add(&format!("item_{i}"));
            b.add(&format!("item_{i}"));
        }
        // A also sees 5000-7499
        for i in 5000..7500 {
            a.add(&format!("item_{i}"));
        }
        a.merge(&b);
        let est = a.cardinality();
        // Union should be ~7500
        let error = (est - 7_500.0).abs() / 7_500.0;
        assert!(
            error < 0.05,
            "expected ~7500 after merge, got {est} (error {:.1}%)",
            error * 100.0
        );
    }

    #[test]
    fn intersection_estimate() {
        let mut a = HyperLogLog::new(14);
        let mut b = HyperLogLog::new(14);
        // Shared: items 0-4999, A-only: 5000-7499, B-only: 5000-7499 (different prefix)
        for i in 0..5000 {
            let s = format!("shared_{i}");
            a.add(&s);
            b.add(&s);
        }
        for i in 5000..7500 {
            a.add(&format!("a_only_{i}"));
            b.add(&format!("b_only_{i}"));
        }
        let intersection = a.intersection_cardinality(&b);
        // Expected ~5000
        let error = (intersection - 5_000.0).abs() / 5_000.0;
        assert!(
            error < 0.15,
            "expected ~5000 intersection, got {intersection} (error {:.1}%)",
            error * 100.0
        );
    }

    #[test]
    #[should_panic(expected = "cannot merge")]
    fn merge_different_precisions_panics() {
        let mut a = HyperLogLog::new(10);
        let b = HyperLogLog::new(12);
        a.merge(&b);
    }

    #[test]
    fn serialization_roundtrip() {
        let mut hll = HyperLogLog::new(10);
        for i in 0..100 {
            hll.add(&format!("item_{i}"));
        }
        let json = serde_json::to_string(&hll).unwrap();
        let deserialized: HyperLogLog = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.precision(), hll.precision());
        assert!(
            (deserialized.cardinality() - hll.cardinality()).abs() < 1e-10,
            "cardinality should be identical after deserialization"
        );
    }

    #[test]
    fn precision_clamping() {
        let low = HyperLogLog::new(2);
        assert_eq!(low.precision(), 4); // clamped to min 4

        let high = HyperLogLog::new(20);
        assert_eq!(high.precision(), 18); // clamped to max 18
    }
}