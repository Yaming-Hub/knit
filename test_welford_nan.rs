fn main() {
    let mut mean = 0.0;
    let mut m2 = 0.0;
    
    // Simulate Welford with NaN
    for (i, &val) in [1.0, 2.0, f64::NAN, 4.0, 5.0].iter().enumerate() {
        let n = (i + 1) as f64;
        let delta = val - mean;
        mean += delta / n;
        let delta2 = val - mean;
        m2 += delta * delta2;
    }
    
    println!("Final mean: {}", mean);
    println!("Final m2: {}", m2);
    println!("Variance: {}", m2 / 4.0);
}
