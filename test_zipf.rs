use rand_distr::{Zipf, Distribution};
use rand::SeedableRng;
use rand_chacha::ChaCha8Rng;

fn main() {
    let mut rng = ChaCha8Rng::seed_from_u64(42);
    let dist = Zipf::new(100, 1.0).unwrap();
    let sample = dist.sample(&mut rng);
    println!("Sample type: {}", std::any::type_name_of_val(&sample));
    println!("Sample value: {}", sample);
    
    // Try to assign to f64
    let as_f64: f64 = sample as f64;
    println!("As f64: {}", as_f64);
}
