//! Online numeric statistics using Welford's algorithm.
//!
//! Computes running mean, variance, min, max, and count in a single pass
//! with O(1) memory. Supports merging two states for parallel/chunked
//! processing.

use serde::{Deserialize, Serialize};

/// Online numeric statistics accumulator.
///
/// Uses Welford's algorithm for numerically stable computation of mean and
/// variance. Tracks min, max, count, null count, and integer/decimal metadata.
///
/// # Example
///
/// ```
/// use knit::learn::streaming::NumericState;
///
/// let mut state = NumericState::new();
/// state.update(1.0);
/// state.update(2.0);
/// state.update(3.0);
/// assert_eq!(state.count(), 3);
/// assert!((state.mean() - 2.0).abs() < 1e-10);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NumericState {
    /// Number of non-null values observed.
    count: u64,
    /// Number of null values observed.
    null_count: u64,
    /// Minimum value observed.
    min: f64,
    /// Maximum value observed.
    max: f64,
    /// Welford's running mean.
    mean: f64,
    /// Welford's M2: sum of squared differences from mean.
    m2: f64,
    /// Whether all observed values are integer-valued.
    all_integer: bool,
    /// Maximum decimal places observed (for float precision detection).
    max_decimal_places: u8,
}

impl NumericState {
    /// Create a new empty numeric state.
    pub fn new() -> Self {
        Self {
            count: 0,
            null_count: 0,
            min: f64::INFINITY,
            max: f64::NEG_INFINITY,
            mean: 0.0,
            m2: 0.0,
            all_integer: true,
            max_decimal_places: 0,
        }
    }

    /// Update state with a new non-null value.
    pub fn update(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }

        self.count += 1;

        // Min/max
        if value < self.min {
            self.min = value;
        }
        if value > self.max {
            self.max = value;
        }

        // Welford's online algorithm
        let delta = value - self.mean;
        self.mean += delta / self.count as f64;
        let delta2 = value - self.mean;
        self.m2 += delta * delta2;

        // Integer detection
        if self.all_integer && value.fract() != 0.0 {
            self.all_integer = false;
        }

        // Decimal places
        if !self.all_integer {
            let places = count_decimal_places(value);
            if places > self.max_decimal_places {
                self.max_decimal_places = places;
            }
        }
    }

    /// Record a null observation.
    pub fn update_null(&mut self) {
        self.null_count += 1;
    }

    /// Merge another `NumericState` into this one.
    ///
    /// Uses the parallel form of Welford's algorithm to combine two
    /// independently computed states.
    pub fn merge(&mut self, other: &NumericState) {
        if other.count == 0 {
            self.null_count += other.null_count;
            return;
        }
        if self.count == 0 {
            *self = other.clone();
            return;
        }

        let combined_count = self.count + other.count;
        let delta = other.mean - self.mean;

        // Parallel Welford merge
        let new_mean = self.mean + delta * (other.count as f64 / combined_count as f64);
        let new_m2 = self.m2
            + other.m2
            + delta * delta * (self.count as f64 * other.count as f64) / combined_count as f64;

        self.mean = new_mean;
        self.m2 = new_m2;
        self.count = combined_count;
        self.null_count += other.null_count;

        if other.min < self.min {
            self.min = other.min;
        }
        if other.max > self.max {
            self.max = other.max;
        }

        self.all_integer = self.all_integer && other.all_integer;
        self.max_decimal_places = self.max_decimal_places.max(other.max_decimal_places);
    }

    /// Number of non-null values observed.
    pub fn count(&self) -> u64 {
        self.count
    }

    /// Number of null values observed.
    pub fn null_count(&self) -> u64 {
        self.null_count
    }

    /// Minimum value observed, or `f64::INFINITY` if empty.
    pub fn min(&self) -> f64 {
        self.min
    }

    /// Maximum value observed, or `f64::NEG_INFINITY` if empty.
    pub fn max(&self) -> f64 {
        self.max
    }

    /// Running mean, or `0.0` if empty.
    pub fn mean(&self) -> f64 {
        self.mean
    }

    /// Population variance, or `0.0` if fewer than 2 values.
    pub fn variance(&self) -> f64 {
        if self.count < 2 {
            0.0
        } else {
            self.m2 / self.count as f64
        }
    }

    /// Sample variance (Bessel-corrected), or `0.0` if fewer than 2 values.
    pub fn sample_variance(&self) -> f64 {
        if self.count < 2 {
            0.0
        } else {
            self.m2 / (self.count - 1) as f64
        }
    }

    /// Population standard deviation.
    pub fn std_dev(&self) -> f64 {
        self.variance().sqrt()
    }

    /// Whether all observed values are integer-valued.
    pub fn all_integer(&self) -> bool {
        self.all_integer
    }

    /// Maximum decimal places observed.
    pub fn max_decimal_places(&self) -> u8 {
        self.max_decimal_places
    }

    /// Null rate (0.0–1.0).
    pub fn null_rate(&self) -> f64 {
        let total = self.count + self.null_count;
        if total == 0 {
            0.0
        } else {
            self.null_count as f64 / total as f64
        }
    }
}

impl Default for NumericState {
    fn default() -> Self {
        Self::new()
    }
}

/// Count the number of decimal places in a float value.
fn count_decimal_places(value: f64) -> u8 {
    if !value.is_finite() {
        return 0;
    }
    let s = format!("{}", value);
    match s.find('.') {
        Some(dot_pos) => {
            let decimals = &s[dot_pos + 1..];
            let trimmed = decimals.trim_end_matches('0');
            trimmed.len() as u8
        }
        None => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_state() {
        let s = NumericState::new();
        assert_eq!(s.count(), 0);
        assert_eq!(s.mean(), 0.0);
        assert_eq!(s.variance(), 0.0);
        assert!(s.all_integer());
    }

    #[test]
    fn single_value() {
        let mut s = NumericState::new();
        s.update(42.0);
        assert_eq!(s.count(), 1);
        assert!((s.mean() - 42.0).abs() < 1e-10);
        assert_eq!(s.min(), 42.0);
        assert_eq!(s.max(), 42.0);
        assert_eq!(s.variance(), 0.0);
    }

    #[test]
    fn welford_correctness() {
        let mut s = NumericState::new();
        let values = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        for &v in &values {
            s.update(v);
        }
        assert_eq!(s.count(), 8);
        assert!((s.mean() - 5.0).abs() < 1e-10);
        // Population variance = 4.0
        assert!((s.variance() - 4.0).abs() < 1e-10);
        // Sample variance = 32/7 ≈ 4.571
        assert!((s.sample_variance() - 32.0 / 7.0).abs() < 1e-10);
        assert_eq!(s.min(), 2.0);
        assert_eq!(s.max(), 9.0);
    }

    #[test]
    fn merge_two_states() {
        let mut a = NumericState::new();
        let mut b = NumericState::new();
        let values = [2.0, 4.0, 4.0, 4.0, 5.0, 5.0, 7.0, 9.0];
        for &v in &values[..4] {
            a.update(v);
        }
        for &v in &values[4..] {
            b.update(v);
        }
        a.merge(&b);
        assert_eq!(a.count(), 8);
        assert!((a.mean() - 5.0).abs() < 1e-10);
        assert!((a.variance() - 4.0).abs() < 1e-10);
        assert_eq!(a.min(), 2.0);
        assert_eq!(a.max(), 9.0);
    }

    #[test]
    fn merge_empty_into_populated() {
        let mut a = NumericState::new();
        a.update(10.0);
        a.update(20.0);
        let b = NumericState::new();
        a.merge(&b);
        assert_eq!(a.count(), 2);
        assert!((a.mean() - 15.0).abs() < 1e-10);
    }

    #[test]
    fn merge_populated_into_empty() {
        let mut a = NumericState::new();
        let mut b = NumericState::new();
        b.update(10.0);
        b.update(20.0);
        a.merge(&b);
        assert_eq!(a.count(), 2);
        assert!((a.mean() - 15.0).abs() < 1e-10);
    }

    #[test]
    fn null_tracking() {
        let mut s = NumericState::new();
        s.update(1.0);
        s.update_null();
        s.update(3.0);
        s.update_null();
        assert_eq!(s.count(), 2);
        assert_eq!(s.null_count(), 2);
        assert!((s.null_rate() - 0.5).abs() < 1e-10);
    }

    #[test]
    fn integer_detection() {
        let mut s = NumericState::new();
        s.update(1.0);
        s.update(2.0);
        s.update(3.0);
        assert!(s.all_integer());
        s.update(3.5);
        assert!(!s.all_integer());
    }

    #[test]
    fn decimal_places() {
        let mut s = NumericState::new();
        s.update(1.5);
        s.update(2.33);
        s.update(3.141);
        assert!(!s.all_integer());
        assert_eq!(s.max_decimal_places(), 3);
    }

    #[test]
    fn nan_and_infinity_ignored() {
        let mut s = NumericState::new();
        s.update(1.0);
        s.update(f64::NAN);
        s.update(f64::INFINITY);
        s.update(f64::NEG_INFINITY);
        s.update(2.0);
        assert_eq!(s.count(), 2);
        assert!((s.mean() - 1.5).abs() < 1e-10);
    }

    #[test]
    fn large_values_numerical_stability() {
        // Test with values that would cause catastrophic cancellation
        // with naive sum-of-squares approach
        let mut s = NumericState::new();
        let base = 1e9;
        for i in 0..1000 {
            s.update(base + i as f64);
        }
        // Mean should be base + 499.5
        assert!((s.mean() - (base + 499.5)).abs() < 1e-6);
        // Variance of 0..999 = (999*1000) / (12*1000) ≈ 83250.0
        let expected_var = (999.0 * 1001.0) / 12.0; // population variance
        assert!(
            (s.variance() - expected_var).abs() / expected_var < 1e-6,
            "variance {} vs expected {}",
            s.variance(),
            expected_var
        );
    }

    #[test]
    fn serialization_roundtrip() {
        let mut s = NumericState::new();
        s.update(1.5);
        s.update(2.5);
        s.update_null();

        let json = serde_json::to_string(&s).unwrap();
        let deserialized: NumericState = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.count(), s.count());
        assert!((deserialized.mean() - s.mean()).abs() < 1e-10);
        assert!((deserialized.variance() - s.variance()).abs() < 1e-10);
        assert_eq!(deserialized.null_count(), s.null_count());
    }
}
