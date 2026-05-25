// Test edge cases in arithmetic constraint detection
#[cfg(test)]
mod tests {
    #[test]
    fn test_compute_relation_error_nan_handling() {
        // Simulate what happens when arithmetic produces NaN or infinity
        let target = vec![Some(1.0), Some(2.0), Some(3.0), Some(f64::NAN)];
        let a = vec![Some(1.0), Some(1.0), Some(1.0), Some(1.0)];
        let b = vec![Some(0.0), Some(1.0), Some(2.0), Some(f64::INFINITY)];
        
        // Test: target = a + b
        // Row 0: 1.0 vs 1.0 + 0.0 = 1.0 (match)
        // Row 1: 2.0 vs 1.0 + 1.0 = 2.0 (match)
        // Row 2: 3.0 vs 1.0 + 2.0 = 3.0 (match)
        // Row 3: NaN vs 1.0 + inf = inf (NaN != inf, but is this handled correctly?)
        
        // If expected is infinity and target is NaN:
        // denom = NaN.abs().max(inf.abs()).max(1.0) = inf
        // (NaN - inf).abs() / inf = NaN / inf = NaN
        // NaN > 0.01 evaluates to false in Rust, so it would NOT be counted as mismatch!
        // This is a silent bug - NaN values would be treated as matches.
    }
    
    #[test]
    fn test_division_by_near_zero() {
        // What if vb is very close to zero but not exactly 0.0?
        let target = vec![Some(1000000.0)];
        let a = vec![Some(1.0)];
        let b = vec![Some(0.000001)];
        
        // expected = 1.0 / 0.000001 = 1000000.0
        // This would match. But what if floating point errors accumulate?
        // The tolerance check uses relative error, which should handle this.
    }
    
    #[test]
    fn test_negative_zero() {
        // Rust's f64 has -0.0 and +0.0
        // Is -0.0 == 0.0? Yes in Rust.
        // So division by -0.0 would be skipped (good).
    }
    
    #[test]
    fn test_overflow_to_infinity() {
        let target = vec![Some(f64::INFINITY)];
        let a = vec![Some(f64::MAX)];
        let b = vec![Some(f64::MAX)];
        
        // expected = f64::MAX * f64::MAX = inf (overflow)
        // denom = inf.abs().max(inf.abs()).max(1.0) = inf
        // (inf - inf).abs() / inf = NaN / inf = NaN
        // NaN > 0.01 evaluates to false, so it would be counted as a MATCH.
        // This is a bug: inf - inf produces NaN, which silently passes the check!
    }
}
