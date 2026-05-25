// Test MIN_STRING_COLUMN_RATIO threshold
fn main() {
    // Test case 1: 5 columns, 1 string (20% - exactly at threshold)
    let total_cols_1 = 5;
    let string_cols_1 = 1;
    let ratio_1 = string_cols_1 as f64 / total_cols_1 as f64;
    println!("Test 1: {} string / {} total = {:.2} = {:.0}%", 
             string_cols_1, total_cols_1, ratio_1, ratio_1 * 100.0);
    println!("  Would trigger full-row-dict: {}", ratio_1 >= 0.2);
    println!("  Example: [id, amount, quantity, price, date] - NOT a categorical table!\n");
    
    // Test case 2: 10 columns, 2 string (20% - at threshold)
    let total_cols_2 = 10;
    let string_cols_2 = 2;
    let ratio_2 = string_cols_2 as f64 / total_cols_2 as f64;
    println!("Test 2: {} string / {} total = {:.2} = {:.0}%",
             string_cols_2, total_cols_2, ratio_2, ratio_2 * 100.0);
    println!("  Would trigger full-row-dict: {}", ratio_2 >= 0.2);
    println!("  Example: [country, region, pop, gdp, area, density, growth, ...] - mostly numeric!\n");
    
    // Test case 3: 5 columns, 3 string (60% - clearly categorical)
    let total_cols_3 = 5;
    let string_cols_3 = 3;
    let ratio_3 = string_cols_3 as f64 / total_cols_3 as f64;
    println!("Test 3: {} string / {} total = {:.2} = {:.0}%", 
             string_cols_3, total_cols_3, ratio_3, ratio_3 * 100.0);
    println!("  Would trigger full-row-dict: {}", ratio_3 >= 0.2);
    println!("  Example: [country, city, region, population, area] - reasonable!\n");
    
    println!("Conclusion: 0.2 (20%) seems too low for full-row dictionary.");
    println!("Tables with 80% numeric columns are not good candidates for full-row dict.");
    println!("Previous threshold of 0.5 (50%) was more appropriate.");
}
