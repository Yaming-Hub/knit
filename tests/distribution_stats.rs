//! Distribution statistics tests: verify that generated data approximately
//! matches the configured statistical distributions.

use arrow::array::{Array, Float64Array};
mod common;
use common::generate_from_toml;

/// Collect all `Float64` values from the named column across batches.
fn collect_f64(batches: &[arrow::record_batch::RecordBatch], column: &str) -> Vec<f64> {
    let mut values = Vec::new();
    for batch in batches {
        let idx = batch
            .schema()
            .index_of(column)
            .unwrap_or_else(|_| panic!("column '{column}' not found"));
        let arr = batch
            .column(idx)
            .as_any()
            .downcast_ref::<Float64Array>()
            .unwrap_or_else(|| panic!("column '{column}' is not Float64"));
        for i in 0..arr.len() {
            if !arr.is_null(i) {
                values.push(arr.value(i));
            }
        }
    }
    values
}

fn mean(vals: &[f64]) -> f64 {
    vals.iter().sum::<f64>() / vals.len() as f64
}

fn std_dev(vals: &[f64]) -> f64 {
    let m = mean(vals);
    let variance = vals.iter().map(|v| (v - m).powi(2)).sum::<f64>() / vals.len() as f64;
    variance.sqrt()
}

#[test]
fn normal_distribution_mean_and_stddev() {
    let schema = r#"
schema_version = "1.0"

[model]
name = "stats_test_normal"
seed = 42

[[entities]]
name = "data"
count = 10000

[[entities.fields]]
name = "value"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "normal"
[entities.fields.generator.params]
mean = 100.0
std_dev = 10.0
"#;

    let batches = generate_from_toml(schema);
    let values = collect_f64(batches.get("data").expect("no data batches"), "value");

    assert_eq!(values.len(), 10_000, "expected 10 000 values");

    let m = mean(&values);
    let s = std_dev(&values);

    // Mean within 5% of target.
    assert!(
        (m - 100.0).abs() / 100.0 < 0.05,
        "mean {m:.2} is not within 5% of 100.0"
    );
    // Std dev within 15% of target.
    assert!(
        (s - 10.0).abs() / 10.0 < 0.15,
        "std_dev {s:.2} is not within 15% of 10.0"
    );
}

#[test]
fn uniform_distribution_bounds() {
    let schema = r#"
schema_version = "1.0"

[model]
name = "stats_test_uniform"
seed = 42

[[entities]]
name = "data"
count = 5000

[[entities.fields]]
name = "value"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 10.0
max = 50.0
"#;

    let batches = generate_from_toml(schema);
    let values = collect_f64(batches.get("data").expect("no data batches"), "value");

    assert_eq!(values.len(), 5_000);

    // All values must be within [low, high].
    for (i, v) in values.iter().enumerate() {
        assert!(
            *v >= 10.0 && *v <= 50.0,
            "row {i}: value {v} outside [10, 50]"
        );
    }

    // Mean should be roughly (low + high) / 2 = 30.
    let m = mean(&values);
    assert!(
        (m - 30.0).abs() / 30.0 < 0.05,
        "uniform mean {m:.2} too far from 30.0"
    );
}

#[test]
fn bernoulli_distribution_proportion() {
    let schema = r#"
schema_version = "1.0"

[model]
name = "stats_test_bernoulli"
seed = 42

[[entities]]
name = "data"
count = 10000

[[entities.fields]]
name = "flag"
data_type = "int"
[entities.fields.generator]
type = "distribution"
kind = "bernoulli"
[entities.fields.generator.params]
p = 0.3
"#;

    let batches = generate_from_toml(schema);
    let data_batches = batches.get("data").expect("no data batches");

    let mut true_count = 0u64;
    let mut total = 0u64;
    for batch in data_batches {
        let idx = batch.schema().index_of("flag").unwrap();
        let arr = batch
            .column(idx)
            .as_any()
            .downcast_ref::<arrow::array::Int64Array>()
            .expect("flag column is not Int64Array");
        for i in 0..arr.len() {
            if !arr.is_null(i) {
                total += 1;
                if arr.value(i) == 1 {
                    true_count += 1;
                }
            }
        }
    }

    let proportion = true_count as f64 / total as f64;
    // Proportion should be within 5 percentage points of p=0.3.
    assert!(
        (proportion - 0.3).abs() < 0.05,
        "bernoulli proportion {proportion:.3} too far from 0.3"
    );
}

#[test]
fn exponential_distribution_is_positive() {
    let schema = r#"
schema_version = "1.0"

[model]
name = "stats_test_exp"
seed = 42

[[entities]]
name = "data"
count = 5000

[[entities.fields]]
name = "value"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "exponential"
[entities.fields.generator.params]
lambda = 2.0
"#;

    let batches = generate_from_toml(schema);
    let values = collect_f64(batches.get("data").expect("no data batches"), "value");

    assert_eq!(values.len(), 5_000);

    // Exponential values should be non-negative.
    for (i, v) in values.iter().enumerate() {
        assert!(*v >= 0.0, "row {i}: exponential value {v} is negative");
    }

    // Mean should be close to 1/lambda = 0.5.
    let m = mean(&values);
    assert!(
        (m - 0.5).abs() / 0.5 < 0.10,
        "exponential mean {m:.3} too far from 0.5"
    );
}

#[test]
fn one_of_distribution_covers_all_choices() {
    let schema = r#"
schema_version = "1.0"

[model]
name = "stats_test_oneof"
seed = 42

[[entities]]
name = "data"
count = 5000

[[entities.fields]]
name = "category"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = [
    { value = "a", weight = 0.4 },
    { value = "b", weight = 0.3 },
    { value = "c", weight = 0.2 },
    { value = "d", weight = 0.1 },
]
"#;

    let batches = generate_from_toml(schema);
    let data_batches = batches.get("data").expect("no data batches");

    let mut counts = std::collections::HashMap::<String, usize>::new();
    for batch in data_batches {
        let idx = batch.schema().index_of("category").unwrap();
        let arr = batch
            .column(idx)
            .as_any()
            .downcast_ref::<arrow::array::StringArray>()
            .expect("category column is not StringArray");
        for i in 0..arr.len() {
            if !arr.is_null(i) {
                *counts.entry(arr.value(i).to_string()).or_default() += 1;
            }
        }
    }

    // All four choices should appear.
    for expected in &["a", "b", "c", "d"] {
        assert!(
            counts.contains_key(*expected),
            "choice '{expected}' never generated"
        );
    }
}
