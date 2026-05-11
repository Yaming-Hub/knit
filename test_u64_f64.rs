fn main() {
    // Test uint64 precision loss with f64
    let large_u64 = u64::MAX;
    let as_f64 = large_u64 as f64;
    let back_to_u64 = as_f64 as u64;
    println!("Original: {}", large_u64);
    println!("As f64: {}", as_f64);
    println!("Back to u64: {}", back_to_u64);
    println!("Loss: {}", large_u64 - back_to_u64);
    
    // Test at what point we lose precision
    let test_val = (1u64 << 53) + 1; // 2^53 + 1
    let as_f64 = test_val as f64;
    let back = as_f64 as u64;
    println!("\nTest 2^53+1: orig={}, f64={}, back={}, equal={}", test_val, as_f64, back, test_val == back);
}
