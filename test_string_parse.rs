fn main() {
    let test_cases = vec!["1.5", "NaN", "inf", "-inf", "3.14159"];
    for s in test_cases {
        if let Ok(v) = s.parse::<f64>() {
            println!("'{}' -> {} (is_nan: {}, is_finite: {})", s, v, v.is_nan(), v.is_finite());
        } else {
            println!("'{}' -> parse error", s);
        }
    }
}
