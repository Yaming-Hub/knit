fn main() {
    // Test 1: fold with NaN
    let values = vec![1.0, f64::NAN, 3.0, 2.0];
    let min = values.iter().copied().fold(f64::INFINITY, f64::min);
    let max = values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    println!("Test 1 - fold with NaN:");
    println!("  values: {:?}", values);
    println!("  min: {} (is_nan: {})", min, min.is_nan());
    println!("  max: {} (is_nan: {})", max, max.is_nan());
    println!("  min.is_finite(): {}", min.is_finite());
    println!();

    // Test 2: fold without NaN  
    let values2 = vec![1.0, 3.0, 2.0];
    let min2 = values2.iter().copied().fold(f64::INFINITY, f64::min);
    let max2 = values2.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    println!("Test 2 - fold without NaN:");
    println!("  values: {:?}", values2);
    println!("  min: {}", min2);
    println!("  max: {}", max2);
    println!();

    // Test 3: Empty vec
    let values3: Vec<f64> = vec![];
    let min3 = values3.iter().copied().fold(f64::INFINITY, f64::min);
    let max3 = values3.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    println!("Test 3 - Empty vec:");
    println!("  min: {} (expected: INFINITY)", min3);
    println!("  max: {} (expected: NEG_INFINITY)", max3);
    println!("  min.is_finite(): {}", min3.is_finite());
    println!("  max.is_finite(): {}", max3.is_finite());
}
