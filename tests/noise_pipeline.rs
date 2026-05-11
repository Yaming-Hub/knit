//! Noise pipeline integration test: verify that noise profiles in the schema
//! parse correctly and that the noise pipeline produces measurable effects
//! when applied to generated data.

use arrow::array::Array;

mod common;
use common::generate_from_toml;
use knit::noise::{NullInjector, PerturbConfig, Pipeline};

/// Schema with a noise profile that injects nulls into the `value` column.
const NOISY_SCHEMA: &str = r#"
blueprint_version = "1.0"

[model]
name = "noise_test"
seed = 42

[[entities]]
name = "items"
count = 5000

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "value"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "normal"
[entities.fields.generator.params]
mean = 100.0
std_dev = 10.0

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = [
    { value = "alpha", weight = 0.5 },
    { value = "beta", weight = 0.3 },
    { value = "gamma", weight = 0.2 },
]

[[noise]]
name = "inject_nulls"
entity = "items"
null_rate = 0.10
"#;

/// Same schema without noise for comparison.
const CLEAN_SCHEMA: &str = r#"
blueprint_version = "1.0"

[model]
name = "noise_test_clean"
seed = 42

[[entities]]
name = "items"
count = 5000

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "value"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "normal"
[entities.fields.generator.params]
mean = 100.0
std_dev = 10.0

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = [
    { value = "alpha", weight = 0.5 },
    { value = "beta", weight = 0.3 },
    { value = "gamma", weight = 0.2 },
]
"#;

#[test]
fn clean_schema_produces_no_nulls_in_value_column() {
    let data = generate_from_toml(CLEAN_SCHEMA);
    let batches = data.get("items").expect("items entity");

    let mut null_count = 0u64;
    let mut total = 0u64;
    for batch in batches {
        let idx = batch.schema().index_of("value").unwrap();
        let col = batch.column(idx);
        null_count += col.null_count() as u64;
        total += col.len() as u64;
    }

    assert_eq!(
        null_count, 0,
        "clean schema should have no nulls in value column"
    );
    assert_eq!(total, 5000);
}

#[test]
fn noise_profile_parsed_correctly() {
    let model = knit::blueprint::parse_toml(NOISY_SCHEMA).expect("parse failed");
    let errors = knit::blueprint::validate(&model);
    assert!(errors.is_empty(), "validation errors: {errors:?}");

    assert_eq!(model.noise_profiles.len(), 1);
    assert_eq!(model.noise_profiles[0].name, "inject_nulls");
    assert_eq!(model.noise_profiles[0].entity, "items");
    assert!((model.noise_profiles[0].null_rate - 0.10).abs() < f64::EPSILON);
}

#[test]
fn noise_pipeline_injects_nulls_into_generated_data() {
    // Generate clean data
    let data = generate_from_toml(CLEAN_SCHEMA);
    let batches = data.get("items").expect("items entity");

    // Apply noise pipeline to each batch
    let cfg = PerturbConfig::default()
        .with_probability(0.10)
        .with_seed(42);
    let mut pipeline = Pipeline::new(cfg);
    pipeline.add(Box::new(NullInjector::new()));

    let mut total_nulls = 0u64;
    let mut total_cells = 0u64;
    for batch in batches {
        let noisy = pipeline.run(batch.clone()).expect("noise pipeline failed");
        // Count nulls across all columns
        for col_idx in 0..noisy.num_columns() {
            total_nulls += noisy.column(col_idx).null_count() as u64;
            total_cells += noisy.column(col_idx).len() as u64;
        }
    }

    // With 10% probability across all columns, we expect significant nulls
    let null_rate = total_nulls as f64 / total_cells as f64;
    assert!(
        null_rate > 0.05,
        "expected null rate > 5% from 10% injection, got {null_rate:.3}"
    );
    assert!(
        null_rate < 0.20,
        "null rate {null_rate:.3} unreasonably high for 10% injection"
    );
}