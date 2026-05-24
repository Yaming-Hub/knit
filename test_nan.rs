fn main() {
    let t: f64 = 100.0;
    let expected: f64 = f64::NAN;
    let denom = t.abs().max(expected.abs()).max(1.0);
    println!("denom = {}", denom);
    let diff = (t - expected).abs() / denom;
    println!("diff = {}", diff);
    println!("diff > 0.01 = {}", diff > 0.01);
    
    let t2: f64 = f64::INFINITY;
    let exp2: f64 = f64::INFINITY;
    let denom2 = t2.abs().max(exp2.abs()).max(1.0);
    let diff2 = (t2 - exp2).abs() / denom2;
    println!("inf - inf: diff = {}, > 0.01 = {}", diff2, diff2 > 0.01);
    
    let t3: f64 = 0.0;
    let exp3: f64 = 0.0 / 0.0;
    let denom3 = t3.abs().max(exp3.abs()).max(1.0);
    let diff3 = (t3 - exp3).abs() / denom3;
    println!("0.0 vs NaN: diff = {}, > 0.01 = {}", diff3, diff3 > 0.01);
}
