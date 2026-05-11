//! Round-trip tests: generate data → profile/fit → verify inferred params
//! match the original schema specification.

use arrow::array::{Array, Float64Array, StringArray};

mod common;
use common::generate_from_toml;
use knit::learn::fitting::{fit_categorical, fit_distribution, Distribution};

/// Schema with a normal distribution (mean=100, std_dev=15) and categorical column.
const ROUNDTRIP_SCHEMA: &str = r#"
blueprint_version = "1.0"

[model]
name = "roundtrip_test"
seed = 42

[[entities]]
name = "samples"
count = 10000

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "score"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "normal"
[entities.fields.generator.params]
mean = 100.0
std_dev = 15.0

[[entities.fields]]
name = "category"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = [
    { value = "red", weight = 0.5 },
    { value = "green", weight = 0.3 },
    { value = "blue", weight = 0.2 },
]

[[entities.fields]]
name = "uniform_val"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 10.0
max = 50.0
"#;

/// Helper: collect all f64 values from a named column across batches.
fn collect_f64(batches: &[arrow::record_batch::RecordBatch], col_name: &str) -> Vec<f64> {
    let mut vals = Vec::new();
    for batch in batches {
        let idx = batch
            .schema()
            .index_of(col_name)
            .unwrap_or_else(|_| panic!("column {col_name} not found"));
        let arr = batch.column(idx);
        if let Some(f64_arr) = arr.as_any().downcast_ref::<Float64Array>() {
            for i in 0..f64_arr.len() {
                if !arr.is_null(i) {
                    vals.push(f64_arr.value(i));
                }
            }
        }
    }
    vals
}

/// Helper: collect all string values from a named column across batches.
fn collect_strings(batches: &[arrow::record_batch::RecordBatch], col_name: &str) -> Vec<String> {
    let mut vals = Vec::new();
    for batch in batches {
        let idx = batch
            .schema()
            .index_of(col_name)
            .unwrap_or_else(|_| panic!("column {col_name} not found"));
        let arr = batch.column(idx);
        if let Some(str_arr) = arr.as_any().downcast_ref::<StringArray>() {
            for i in 0..str_arr.len() {
                if !arr.is_null(i) {
                    vals.push(str_arr.value(i).to_string());
                }
            }
        }
    }
    vals
}

#[test]
fn normal_distribution_recovers_params() {
    let data = generate_from_toml(ROUNDTRIP_SCHEMA);
    let batches = data.get("samples").expect("samples entity");

    let scores = collect_f64(batches, "score");
    assert!(scores.len() >= 9000, "expected ~10K values");

    let fit = fit_distribution(&scores).expect("fit should succeed");

    // The best fit should be Normal
    assert_eq!(
        fit.best.distribution.name(),
        "normal",
        "expected normal as best fit, got {}",
        fit.best.distribution.name()
    );

    // Extract recovered params
    if let Distribution::Normal(mean, std_dev) = fit.best.distribution {
        let mean_err = (mean - 100.0).abs() / 100.0;
        let std_err = (std_dev - 15.0).abs() / 15.0;
        assert!(
            mean_err < 0.05,
            "mean recovery error {mean_err:.3} exceeds 5% (got {mean})"
        );
        assert!(
            std_err < 0.10,
            "std_dev recovery error {std_err:.3} exceeds 10% (got {std_dev})"
        );
    } else {
        panic!("expected Normal distribution variant");
    }
}

#[test]
fn uniform_distribution_recovers_params() {
    let data = generate_from_toml(ROUNDTRIP_SCHEMA);
    let batches = data.get("samples").expect("samples entity");

    let vals = collect_f64(batches, "uniform_val");
    assert!(vals.len() >= 9000);

    let fit = fit_distribution(&vals).expect("fit should succeed");

    // The best fit should be Uniform
    assert_eq!(
        fit.best.distribution.name(),
        "uniform",
        "expected uniform as best fit, got {}",
        fit.best.distribution.name()
    );

    if let Distribution::Uniform(min, max) = fit.best.distribution {
        assert!(
            (min - 10.0).abs() < 1.0,
            "min recovery off by more than 1.0 (got {min})"
        );
        assert!(
            (max - 50.0).abs() < 1.0,
            "max recovery off by more than 1.0 (got {max})"
        );
    } else {
        panic!("expected Uniform distribution variant");
    }
}

#[test]
fn categorical_recovers_weights() {
    let data = generate_from_toml(ROUNDTRIP_SCHEMA);
    let batches = data.get("samples").expect("samples entity");

    let cats = collect_strings(batches, "category");
    assert!(cats.len() >= 9000);

    let fit = fit_categorical(&cats);

    // Should have exactly 3 categories
    assert_eq!(fit.cardinality, 3);

    // Check that recovered weights are close to specified weights
    let red_weight = *fit.weights.get("red").unwrap_or(&0.0);
    let green_weight = *fit.weights.get("green").unwrap_or(&0.0);
    let blue_weight = *fit.weights.get("blue").unwrap_or(&0.0);

    assert!(
        (red_weight - 0.5).abs() < 0.05,
        "red weight {red_weight:.3} not close to 0.5"
    );
    assert!(
        (green_weight - 0.3).abs() < 0.05,
        "green weight {green_weight:.3} not close to 0.3"
    );
    assert!(
        (blue_weight - 0.2).abs() < 0.05,
        "blue weight {blue_weight:.3} not close to 0.2"
    );
}

#[test]
fn schema_assembly_produces_valid_model() {
    use knit::learn::fitting::fit_distribution;
    use knit::learn::schema_assembly::{assemble_data_model, ColumnAnalysis, TableAnalysis};
    use knit::blueprint::validate;

    let data = generate_from_toml(ROUNDTRIP_SCHEMA);
    let batches = data.get("samples").expect("samples entity");

    // Build a TableAnalysis from the generated data
    let scores = collect_f64(batches, "score");
    let uniform_vals = collect_f64(batches, "uniform_val");
    let cats = collect_strings(batches, "category");

    // Guard: ensure extraction actually collected data
    assert!(
        scores.len() >= 9000,
        "too few score values: {}",
        scores.len()
    );
    assert!(
        uniform_vals.len() >= 9000,
        "too few uniform_val values: {}",
        uniform_vals.len()
    );
    assert!(
        cats.len() >= 9000,
        "too few category values: {}",
        cats.len()
    );

    let score_fit = fit_distribution(&scores);
    let uniform_fit = fit_distribution(&uniform_vals);
    let cat_fit = fit_categorical(&cats);

    // Verify fits succeeded before assembling
    assert!(score_fit.is_some(), "score distribution fit failed");
    assert!(uniform_fit.is_some(), "uniform_val distribution fit failed");

    let mut id_col = ColumnAnalysis::new("id".to_string(), 0.0, 1.0);
    id_col.is_primary_key = true;

    let mut score_col = ColumnAnalysis::new("score".to_string(), 0.0, 0.95);
    score_col.distribution = score_fit;

    let mut cat_col = ColumnAnalysis::new("category".to_string(), 0.0, 0.9);
    cat_col.categorical_weights = Some(cat_fit.weights.into_iter().collect());

    let mut uniform_col = ColumnAnalysis::new("uniform_val".to_string(), 0.0, 0.95);
    uniform_col.distribution = uniform_fit;

    let columns = vec![id_col, score_col, cat_col, uniform_col];

    let analysis = TableAnalysis::new("samples".to_string(), columns, 1000);

    // Assemble a DataModel from the inferred analysis
    let model = assemble_data_model("roundtrip_inferred", &[analysis]);

    // The assembled model should pass validation
    let errors = validate(&model);
    assert!(
        errors.is_empty(),
        "assembled model has validation errors: {errors:?}"
    );

    // Check structural correctness
    assert_eq!(model.entities.len(), 1);
    let entity = &model.entities[0];
    assert_eq!(entity.name, "samples");
    assert_eq!(
        entity.fields.len(),
        4,
        "expected 4 fields (id, score, category, uniform_val)"
    );

    // Verify field names are present
    let field_names: Vec<&str> = entity.fields.iter().map(|f| f.name.as_str()).collect();
    assert!(field_names.contains(&"id"), "missing id field");
    assert!(field_names.contains(&"score"), "missing score field");
    assert!(field_names.contains(&"category"), "missing category field");
    assert!(
        field_names.contains(&"uniform_val"),
        "missing uniform_val field"
    );

    // Verify that distribution fields got generator specs (not just sequence/default)
    let score_field = entity.fields.iter().find(|f| f.name == "score").unwrap();
    assert!(
        score_field.generator.is_some(),
        "score field should have a generator"
    );
    let cat_field = entity.fields.iter().find(|f| f.name == "category").unwrap();
    assert!(
        cat_field.generator.is_some(),
        "category field should have a generator"
    );
}
