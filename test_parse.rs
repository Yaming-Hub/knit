fn main() {
    println!("NaN parse: {:?}", "NaN".parse::<f64>());
    println!("nan parse: {:?}", "nan".parse::<f64>());
    println!("inf parse: {:?}", "inf".parse::<f64>());
    println!("Infinity parse: {:?}", "Infinity".parse::<f64>());
}
