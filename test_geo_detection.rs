// Test geographic detection logic for European dataset
fn main() {
    // European cities dataset:
    // - Latitudes: 35°N to 70°N (e.g., Sicily to Norway)
    // - Longitudes: -10°W to 40°E (e.g., Portugal to Turkey)
    
    let lat_values = vec![48.8566, 51.5074, 52.5200, 41.9028];  // Paris, London, Berlin, Rome
    let lon_values = vec![2.3522, -0.1278, 13.4050, 12.4964];    // All in [-90, 90]!
    
    let lat_min = lat_values.iter().cloned().fold(f64::INFINITY, f64::min);
    let lat_max = lat_values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let lon_min = lon_values.iter().cloned().fold(f64::INFINITY, f64::min);
    let lon_max = lon_values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    
    println!("European Cities Dataset:");
    println!("Latitudes:  min={:.4}, max={:.4}", lat_min, lat_max);
    println!("Longitudes: min={:.4}, max={:.4}", lon_min, lon_max);
    println!();
    
    // Current logic from PR:
    // Latitude: all values in [-90, 90]
    let is_lat_candidate = lat_min >= -90.0 && lat_max <= 90.0;
    println!("Latitude as lat_candidate: {}", is_lat_candidate);
    
    // Longitude: all values in [-180, 180] with some outside [-90, 90]
    let is_lon_candidate_lat = lat_min >= -180.0 && lat_max <= 180.0 
        && (lat_min < -90.0 || lat_max > 90.0);
    let is_lon_candidate_lon = lon_min >= -180.0 && lon_max <= 180.0 
        && (lon_min < -90.0 || lon_max > 90.0);
    
    println!("Latitude as lon_candidate: {}", is_lon_candidate_lat);
    println!("Longitude as lon_candidate: {}", is_lon_candidate_lon);
    println!();
    
    if !is_lon_candidate_lon {
        println!("🐛 BUG: Longitude column NOT detected because all values are in [-90, 90]!");
        println!("   Both columns would be classified as lat_candidates.");
        println!("   No geographic tuple would be created!");
    }
    
    println!("\nAnother example - US West Coast:");
    let us_lat = vec![32.7157, 34.0522, 37.7749, 47.6062];  // San Diego, LA, SF, Seattle
    let us_lon = vec![-117.1611, -118.2437, -122.4194, -122.3321];  // All negative, all > -180
    
    let us_lat_min = us_lat.iter().cloned().fold(f64::INFINITY, f64::min);
    let us_lat_max = us_lat.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    let us_lon_min = us_lon.iter().cloned().fold(f64::INFINITY, f64::min);
    let us_lon_max = us_lon.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
    
    println!("Latitudes:  min={:.4}, max={:.4}", us_lat_min, us_lat_max);
    println!("Longitudes: min={:.4}, max={:.4}", us_lon_min, us_lon_max);
    
    let us_lon_detected = us_lon_min >= -180.0 && us_lon_max <= 180.0 
        && (us_lon_min < -90.0 || us_lon_max > 90.0);
    println!("Longitude detected: {} (min < -90, so YES)", us_lon_detected);
}
