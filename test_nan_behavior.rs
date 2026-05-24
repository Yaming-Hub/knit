// More comprehensive test of f64::min/max behavior with NaN
fn main() {
    println!("Testing f64::min and f64::max with NaN:\n");
    
    // Direct calls
    println!("f64::min(10.0, f64::NAN) = {}", f64::min(10.0, f64::NAN));
    println!("f64::min(f64::NAN, 10.0) = {}", f64::min(f64::NAN, 10.0));
    println!("f64::max(10.0, f64::NAN) = {}", f64::max(10.0, f64::NAN));
    println!("f64::max(f64::NAN, 10.0) = {}", f64::max(f64::NAN, 10.0));
    
    println!("\nCompare with methods:");
    println!("10.0_f64.min(f64::NAN) = {}", 10.0_f64.min(f64::NAN));
    println!("10.0_f64.max(f64::NAN) = {}", 10.0_f64.max(f64::NAN));
    
    // Fold with NaN at different positions
    println!("\nFold tests:");
    
    let v1 = vec![f64::NAN, 20.0, 30.0];
    let min1 = v1.iter().cloned().fold(f64::INFINITY, f64::min);
    println!("NaN first: {:?} -> min = {}", v1, min1);
    
    let v2 = vec![20.0, f64::NAN, 30.0];
    let min2 = v2.iter().cloned().fold(f64::INFINITY, f64::min);
    println!("NaN middle: {:?} -> min = {}", v2, min2);
    
    let v3 = vec![20.0, 30.0, f64::NAN];
    let min3 = v3.iter().cloned().fold(f64::INFINITY, f64::min);
    println!("NaN last: {:?} -> min = {}", v3, min3);
    
    let v4 = vec![f64::NAN, f64::NAN, f64::NAN];
    let min4 = v4.iter().cloned().fold(f64::INFINITY, f64::min);
    println!("All NaN: {:?} -> min = {} (is_nan={})", v4, min4, min4.is_nan());
}
