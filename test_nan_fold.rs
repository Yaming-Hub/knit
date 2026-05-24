// Quick test to verify NaN propagation in fold with f64::min/max
fn main() {
    // Test case 1: Normal values
    let values1 = vec![10.0, 20.0, 30.0, 40.0];
    let min1 = values1.iter().cloned().fold(f64::INFINITY, f64::min);
    let max1 = values1.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!("Test 1 (normal): min={}, max={}", min1, max1);
    
    // Test case 2: Values with NaN
    let values2 = vec![10.0, f64::NAN, 30.0, 40.0];
    let min2 = values2.iter().cloned().fold(f64::INFINITY, f64::min);
    let max2 = values2.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!("Test 2 (with NaN): min={}, max={}", min2, max2);
    println!("  min2.is_nan() = {}", min2.is_nan());
    println!("  max2.is_nan() = {}", max2.is_nan());
    
    // Test case 3: Latitude range check with NaN
    let lat_values = vec![45.0, f64::NAN, 60.0, 70.0];
    let min_val = lat_values.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_val = lat_values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!("\nTest 3 (latitude with NaN):");
    println!("  min={}, max={}", min_val, max_val);
    let would_detect = min_val >= -90.0 && max_val <= 90.0;
    println!("  Would detect as latitude: {}", would_detect);
    
    // Test case 4: Alternative approach - filter out NaN first
    let filtered: Vec<f64> = lat_values.iter().cloned().filter(|v| !v.is_nan()).collect();
    let min_filtered = filtered.iter().cloned().fold(f64::INFINITY, f64::min);
    let max_filtered = filtered.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    println!("\nTest 4 (filtered approach):");
    println!("  min={}, max={}", min_filtered, max_filtered);
    let would_detect_filtered = min_filtered >= -90.0 && max_filtered <= 90.0;
    println!("  Would detect as latitude: {}", would_detect_filtered);
}
