// Test case to verify NULL handling in time-series trend detection
//
// Problem: extract_f64_column() skips NULLs, compressing the array.
// The regression then fits against compressed indices (0, 1, 2, ...)
// instead of original row positions (e.g., 5, 6, 7, ...).
//
// Example:
// Original batch: [NULL, NULL, NULL, NULL, NULL, 10.0, 20.0, 30.0, 40.0, 50.0]
// Extracted values: [10.0, 20.0, 30.0, 40.0, 50.0]
//
// Current behavior: Fits y = baseline + slope * [0, 1, 2, 3, 4]
//   - mean_x = 2.0
//   - mean_y = 30.0
//   - slope = 10.0 (correct relative to compressed indices)
//   - baseline = mean_y - slope * mean_x = 30.0 - 10.0 * 2.0 = 10.0
//
// Expected behavior: Should fit y = baseline + slope * [5, 6, 7, 8, 9]
//   - mean_x = 7.0
//   - mean_y = 30.0
//   - slope = 10.0 (correct relative to original indices)
//   - baseline = mean_y - slope * mean_x = 30.0 - 10.0 * 7.0 = -40.0
//
// Impact: When generating, the TimeSeries generator uses row index as 't'.
// If the learned baseline is 10.0 (from compressed indices), but generation
// uses original row indices (0-9), the generated values will be:
//   - Row 0: 10.0 + 10.0 * 0 = 10.0 (should be -40.0 + 10.0 * 0 = -40.0)
//   - Row 5: 10.0 + 10.0 * 5 = 60.0 (should be -40.0 + 10.0 * 5 = 10.0)
//
// This causes a systematic offset error in generated values.

use arrow::array::{Float64Array, RecordBatch};
use arrow::datatypes::{DataType, Field, Schema};
use std::sync::Arc;

fn main() {
    // Create a batch with NULLs at the start
    let schema = Schema::new(vec![
        Field::new("date", DataType::Utf8, false),
        Field::new("value", DataType::Float64, true),
    ]);

    let dates = arrow::array::StringArray::from(vec![
        "2020-01-01", "2020-01-02", "2020-01-03", "2020-01-04", "2020-01-05",
        "2020-01-06", "2020-01-07", "2020-01-08", "2020-01-09", "2020-01-10",
    ]);

    let values = Float64Array::from(vec![
        None, None, None, None, None,
        Some(10.0), Some(20.0), Some(30.0), Some(40.0), Some(50.0),
    ]);

    let batch = RecordBatch::try_new(
        Arc::new(schema),
        vec![Arc::new(dates), Arc::new(values)],
    ).unwrap();

    println!("Batch has {} rows", batch.num_rows());
    println!("Value column:");
    for i in 0..batch.num_rows() {
        let val_arr = batch.column(1).as_any().downcast_ref::<Float64Array>().unwrap();
        if val_arr.is_null(i) {
            println!("  Row {}: NULL", i);
        } else {
            println!("  Row {}: {}", i, val_arr.value(i));
        }
    }

    // Simulate extract_f64_column
    let extracted: Vec<f64> = (0..batch.num_rows())
        .filter_map(|i| {
            let val_arr = batch.column(1).as_any().downcast_ref::<Float64Array>().unwrap();
            if !val_arr.is_null(i) {
                Some(val_arr.value(i))
            } else {
                None
            }
        })
        .collect();

    println!("\nExtracted values (NULLs removed): {:?}", extracted);
    println!("Length: {} (original was {})", extracted.len(), batch.num_rows());

    // Current regression (using compressed indices)
    let n_f = extracted.len() as f64;
    let mean_x = (n_f - 1.0) / 2.0;
    let mean_y: f64 = extracted.iter().sum::<f64>() / n_f;

    let mut ss_xy = 0.0;
    let mut ss_xx = 0.0;
    for (i, &y) in extracted.iter().enumerate() {
        let x = i as f64;
        let dx = x - mean_x;
        let dy = y - mean_y;
        ss_xy += dx * dy;
        ss_xx += dx * dx;
    }

    let slope = ss_xy / ss_xx;
    let baseline = mean_y - slope * mean_x;

    println!("\nCurrent implementation (compressed indices):");
    println!("  mean_x (compressed): {}", mean_x);
    println!("  mean_y: {}", mean_y);
    println!("  slope: {}", slope);
    println!("  baseline: {}", baseline);

    // Generated values using current baseline
    println!("\nGenerated values with current baseline (for original row indices):");
    for i in 0..batch.num_rows() {
        let generated = baseline + slope * (i as f64);
        println!("  Row {}: {:.1}", i, generated);
    }

    // What it should be (using original row indices where non-NULL values exist)
    println!("\n--- Expected behavior (preserving original indices) ---");
    
    // Collect (original_index, value) pairs
    let indexed_values: Vec<(usize, f64)> = (0..batch.num_rows())
        .filter_map(|i| {
            let val_arr = batch.column(1).as_any().downcast_ref::<Float64Array>().unwrap();
            if !val_arr.is_null(i) {
                Some((i, val_arr.value(i)))
            } else {
                None
            }
        })
        .collect();

    println!("Indexed values: {:?}", indexed_values);

    let n_f_correct = indexed_values.len() as f64;
    let mean_x_correct: f64 = indexed_values.iter().map(|(i, _)| *i as f64).sum::<f64>() / n_f_correct;
    let mean_y_correct: f64 = indexed_values.iter().map(|(_, y)| y).sum::<f64>() / n_f_correct;

    let mut ss_xy_correct = 0.0;
    let mut ss_xx_correct = 0.0;
    for (i, y) in &indexed_values {
        let x = *i as f64;
        let dx = x - mean_x_correct;
        let dy = y - mean_y_correct;
        ss_xy_correct += dx * dy;
        ss_xx_correct += dx * dx;
    }

    let slope_correct = ss_xy_correct / ss_xx_correct;
    let baseline_correct = mean_y_correct - slope_correct * mean_x_correct;

    println!("\nCorrect implementation (original indices):");
    println!("  mean_x (original): {}", mean_x_correct);
    println!("  mean_y: {}", mean_y_correct);
    println!("  slope: {}", slope_correct);
    println!("  baseline: {}", baseline_correct);

    println!("\nGenerated values with correct baseline:");
    for i in 0..batch.num_rows() {
        let generated = baseline_correct + slope_correct * (i as f64);
        println!("  Row {}: {:.1}", i, generated);
    }

    println!("\n=== COMPARISON ===");
    println!("Baseline error: {} - {} = {}", baseline, baseline_correct, baseline - baseline_correct);
    println!("\nFor the non-NULL rows (5-9), generated values should match original:");
    println!("Row | Original | Current | Correct | Current Error");
    println!("----|----------|---------|---------|-------------");
    for (idx, val) in &indexed_values {
        let current_gen = baseline + slope * (*idx as f64);
        let correct_gen = baseline_correct + slope_correct * (*idx as f64);
        println!("{:4} | {:8.1} | {:7.1} | {:7.1} | {:7.1}",
                 idx, val, current_gen, correct_gen, current_gen - val);
    }
}
