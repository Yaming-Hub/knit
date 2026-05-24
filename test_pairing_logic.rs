// Test the lat/lon pairing logic
fn main() {
    // Scenario: lat at index 5, lon at index 8
    let lat_idx: usize = 5;
    let lon_candidates = vec![2, 8, 12];
    
    // Find nearest lon
    let best_lon = lon_candidates
        .iter()
        .min_by_key(|&&l| (l as isize - lat_idx as isize).unsigned_abs());
    
    println!("lat_idx = {}", lat_idx);
    println!("lon_candidates = {:?}", lon_candidates);
    println!("best_lon = {:?}", best_lon);
    
    if let Some(&lon_idx) = best_lon {
        let distance = (lon_idx as isize - lat_idx as isize).unsigned_abs();
        println!("lon_idx = {}, distance = {}", lon_idx, distance);
        
        // Comment says "within 1 position" but code checks <= 2
        if distance <= 2 {
            println!("PAIRED (distance <= 2)");
        } else {
            println!("NOT PAIRED");
        }
    }
    
    println!("\nTest adjacency logic:");
    // Comment on line 1535: "Find nearest lon that's adjacent (within 1 position)"
    // But line 1542 checks: if distance <= 2
    // This means columns 3 positions apart can be paired!
    // Example: lat at 5, lon at 7 has distance 2, but they're not adjacent
    let test_cases = vec![
        (5, 5, "same column"),
        (5, 6, "adjacent (distance 1)"),
        (5, 7, "distance 2"),
        (5, 8, "distance 3"),
    ];
    
    for (lat, lon, desc) in test_cases {
        let dist = (lon as isize - lat as isize).unsigned_abs();
        let paired = dist <= 2;
        println!("{}: lat={}, lon={}, dist={}, paired={}", desc, lat, lon, dist, paired);
    }
}
