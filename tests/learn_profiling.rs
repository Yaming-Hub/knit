//! Integration tests for learn profiling, type inference, and correlation detection.

use std::sync::Arc;

use arrow::array::{Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use knit::learn::correlation::{CorrelationMethod, detect_correlations};
use knit::learn::profile::compute_profiles;
use knit::learn::type_inference::{InferredType, infer_type};

fn profile_for(batch: RecordBatch, column_name: &str) -> knit::learn::profile::ColumnProfile {
    compute_profiles(&[batch])
        .expect("profiling should succeed")
        .into_iter()
        .find(|profile| profile.name == column_name)
        .unwrap_or_else(|| panic!("profile '{column_name}' should exist"))
}

#[test]
fn test_numeric_profiling() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("int_values", DataType::Int64, false),
        Field::new("float_values", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(vec![1, 2, 3, 4, 5])),
            Arc::new(Float64Array::from(vec![10.0, 20.0, 30.0, 40.0, 50.0])),
        ],
    )
    .expect("record batch should build");

    let profiles = compute_profiles(&[batch]).expect("profiling should succeed");
    let int_profile = profiles
        .iter()
        .find(|profile| profile.name == "int_values")
        .expect("int profile should exist");
    let float_profile = profiles
        .iter()
        .find(|profile| profile.name == "float_values")
        .expect("float profile should exist");

    assert_eq!(int_profile.count, 5);
    assert_eq!(int_profile.null_count, 0);
    let int_numeric = int_profile
        .numeric
        .as_ref()
        .expect("int stats should exist");
    assert_eq!(int_numeric.min, 1.0);
    assert_eq!(int_numeric.max, 5.0);
    assert!((int_numeric.mean - 3.0).abs() < 1e-9);

    assert_eq!(float_profile.count, 5);
    assert_eq!(float_profile.null_count, 0);
    let float_numeric = float_profile
        .numeric
        .as_ref()
        .expect("float stats should exist");
    assert_eq!(float_numeric.min, 10.0);
    assert_eq!(float_numeric.max, 50.0);
    assert!((float_numeric.mean - 30.0).abs() < 1e-9);
}

#[test]
fn test_string_profiling() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "category",
        DataType::Utf8,
        false,
    )]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(StringArray::from(vec![
            "red", "red", "blue", "red", "blue",
        ]))],
    )
    .expect("record batch should build");

    let profile = profile_for(batch, "category");
    assert_eq!(profile.distinct_count, Some(2));
    assert_eq!(profile.cardinality_ratio, Some(0.4));
}

#[test]
fn test_null_profiling() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "maybe_value",
        DataType::Int64,
        true,
    )]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(Int64Array::from(vec![
            Some(1),
            None,
            Some(3),
            None,
        ]))],
    )
    .expect("record batch should build");

    let profile = profile_for(batch, "maybe_value");
    assert_eq!(profile.count, 4);
    assert_eq!(profile.null_count, 2);
    assert!((profile.null_rate - 0.5).abs() < 1e-9);
}

#[test]
fn test_type_inference() {
    let integer_values = vec![Some("1"), Some("2"), Some("3"), Some("4"), Some("5")];
    let integer = infer_type(&integer_values, 0.2);
    assert_eq!(integer.inferred_type, InferredType::Integer);

    let boolean_values = vec![Some("true"), Some("false"), Some("true")];
    let boolean = infer_type(&boolean_values, 0.2);
    assert_eq!(boolean.inferred_type, InferredType::Boolean);

    let uuid_values = vec![
        Some("550e8400-e29b-41d4-a716-446655440000"),
        Some("123e4567-e89b-12d3-a456-426614174000"),
        Some("f47ac10b-58cc-4372-a567-0e02b2c3d479"),
    ];
    let uuid = infer_type(&uuid_values, 0.2);
    assert_eq!(uuid.inferred_type, InferredType::Uuid);
}

#[test]
fn test_correlation_detection() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("x", DataType::Float64, false),
        Field::new("double_x", DataType::Float64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Float64Array::from(vec![1.0, 2.0, 3.0, 4.0, 5.0])),
            Arc::new(Float64Array::from(vec![2.0, 4.0, 6.0, 8.0, 10.0])),
        ],
    )
    .expect("record batch should build");

    let profiles =
        compute_profiles(std::slice::from_ref(&batch)).expect("profiling should succeed");
    let correlations = detect_correlations(&profiles, &[batch]);
    let pearson = correlations
        .iter()
        .find(|correlation| {
            correlation.method == CorrelationMethod::Pearson
                && correlation.column_a == "double_x"
                && correlation.column_b == "x"
                || correlation.method == CorrelationMethod::Pearson
                    && correlation.column_a == "x"
                    && correlation.column_b == "double_x"
        })
        .expect("pearson correlation should be detected");

    assert!((pearson.coefficient - 1.0).abs() < 1e-9);
}
