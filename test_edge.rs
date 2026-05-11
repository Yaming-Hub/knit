#[cfg(test)]
mod edge_case_tests {
    use super::parse_cadence_days;

    #[test]
    fn test_single_char_d() {
        // "d" should fail because num_str is empty
        assert!(parse_cadence_days("d").is_err());
    }

    #[test]
    fn test_single_char_w() {
        // "w" should fail because num_str is empty
        assert!(parse_cadence_days("w").is_err());
    }
    
    #[test]
    fn test_overflow_weeks() {
        // u32::MAX / 7 = 613566756, so 613566757w should overflow
        assert!(parse_cadence_days("613566757w").is_err());
    }
}
