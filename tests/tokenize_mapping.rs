//! Integration tests for tokenize config filtering, mapping, and round trips.

use std::collections::HashSet;
use std::fs::{self, File};
use std::sync::Arc;

use arrow::array::{AsArray, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use knit::tokenize::dictionary::TokenDictionary;
use knit::tokenize::mapper::TokenMapper;
use knit::tokenize::{TokenizeConfig, tokenize};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use tempfile::TempDir;

fn lowercase_set(values: &[&str]) -> HashSet<String> {
    values.iter().map(|value| value.to_string()).collect()
}

#[test]
fn test_column_filter_whitelist() {
    let config = TokenizeConfig {
        tokenize_columns: Some(lowercase_set(&["email", "ssn"])),
        ..TokenizeConfig::default()
    };

    assert!(config.should_tokenize_column("email"));
    assert!(config.should_tokenize_column("Email"));
    assert!(!config.should_tokenize_column("name"));
}

#[test]
fn test_column_filter_blacklist() {
    let config = TokenizeConfig {
        preserve_columns: Some(lowercase_set(&["id", "created_at"])),
        ..TokenizeConfig::default()
    };

    assert!(!config.should_tokenize_column("id"));
    assert!(!config.should_tokenize_column("Created_At"));
    assert!(config.should_tokenize_column("email"));
}

#[test]
fn test_header_tokenization_follows_column_filter() {
    let config = TokenizeConfig {
        tokenize_headers: true,
        tokenize_columns: Some(lowercase_set(&["email"])),
        ..TokenizeConfig::default()
    };

    assert!(config.should_tokenize_header("email"));
    assert!(!config.should_tokenize_header("name"));

    let disabled = TokenizeConfig {
        tokenize_headers: false,
        tokenize_columns: Some(lowercase_set(&["email"])),
        ..TokenizeConfig::default()
    };
    assert!(!disabled.should_tokenize_header("email"));
}

#[test]
fn test_token_mapper_determinism() {
    let mut first = TokenMapper::new(42);
    let mut second = TokenMapper::new(42);
    let mut different = TokenMapper::new(99);

    for value in ["Alice", "alice@example.com", "AB-123"] {
        first.register(value);
        second.register(value);
        different.register(value);
    }

    assert_eq!(first.get("Alice"), second.get("Alice"));
    assert_eq!(
        first.get("alice@example.com"),
        second.get("alice@example.com")
    );
    assert_ne!(first.get("Alice"), different.get("Alice"));
}

#[test]
fn test_full_tokenize_round_trip() {
    let input = TempDir::new().expect("input tempdir should be created");
    let output = TempDir::new().expect("output tempdir should be created");
    let input_dir = input.path().join("people");
    fs::create_dir_all(&input_dir).expect("input subdirectory should be created");

    let parquet_path = input_dir.join("data.parquet");
    let schema = Arc::new(Schema::new(vec![
        Field::new("name", DataType::Utf8, false),
        Field::new("city", DataType::Utf8, false),
        Field::new("age", DataType::Int64, false),
    ]));
    let batch = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(StringArray::from(vec!["Alice", "Bob"])),
            Arc::new(StringArray::from(vec!["Seattle", "Portland"])),
            Arc::new(Int64Array::from(vec![30, 41])),
        ],
    )
    .expect("record batch should build");

    let file = File::create(&parquet_path).expect("input parquet file should be created");
    let mut writer = ArrowWriter::try_new(file, schema, None).expect("parquet writer should build");
    writer.write(&batch).expect("batch should write");
    writer.close().expect("parquet writer should close");

    let dict_path = output.path().join(".knit-tokens.json");
    let result = tokenize(
        input.path(),
        output.path(),
        &dict_path,
        &TokenizeConfig::default(),
    )
    .expect("tokenization should succeed");

    assert_eq!(result.data_files, 1);
    assert!(
        result.unique_tokens >= 4,
        "expected string values to be tokenized"
    );

    let output_parquet = output.path().join("people").join("data.parquet");
    assert!(output_parquet.exists(), "tokenized parquet should exist");
    assert!(dict_path.exists(), "token dictionary should exist");

    let dictionary = TokenDictionary::read(&dict_path).expect("dictionary should round-trip");
    assert!(dictionary.tokens.contains_key("Alice"));
    assert!(dictionary.tokens.contains_key("Seattle"));

    let file = File::open(&output_parquet).expect("tokenized parquet should be readable");
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .expect("parquet reader should build")
        .build()
        .expect("parquet reader should open");
    let batches: Vec<_> = reader
        .map(|batch| batch.expect("parquet batch should read"))
        .collect();
    assert_eq!(batches.len(), 1);

    let names = batches[0].column(0).as_string::<i32>();
    let cities = batches[0].column(1).as_string::<i32>();
    assert_ne!(names.value(0), "Alice");
    assert_ne!(cities.value(0), "Seattle");
}
