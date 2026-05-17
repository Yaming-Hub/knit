//! Output-format integration tests covering write-and-read round-trips.

use std::fs;
use std::fs::File;
use std::path::Path;

use arrow::array::{BooleanArray, Float64Array, Int64Array, StringArray};
use arrow::compute::concat_batches;
use arrow::record_batch::RecordBatch;
use knit::bind::{OutputFormat, SinkConfig, create_sink};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde_json::Value;
use tempfile::TempDir;

mod common;
use common::{generate_from_toml, total_rows};

/// Simple schema used to verify bind output preserves rows and columns.
const OUTPUT_FORMAT_SCHEMA: &str = r#"
blueprint_version = "1.0"

[model]
name = "output_formats"
seed = 77

[[entities]]
name = "items"
count = 100

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
kind = "uniform"
[entities.fields.generator.params]
min = 10.0
max = 99.0

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "derived"
expr = "concat(\"item-\", cast_string(${id}))"
depends_on = ["id"]

[[entities.fields]]
name = "active"
data_type = "bool"
[entities.fields.generator]
type = "one_of"
choices = [
    { value = true, weight = 0.65 },
    { value = false, weight = 0.35 },
]
"#;

#[derive(Debug, Clone, PartialEq)]
struct ExpectedRow {
    id: i64,
    score: f64,
    label: String,
    active: bool,
}

fn combined_items_batch() -> RecordBatch {
    let data = generate_from_toml(OUTPUT_FORMAT_SCHEMA);
    let batches = data.get("items").expect("items entity should exist");
    concat_batches(&batches[0].schema(), batches).expect("items batches should concatenate")
}

fn expected_rows(batch: &RecordBatch) -> Vec<ExpectedRow> {
    let id = batch
        .column(batch.schema().index_of("id").unwrap())
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("id should be Int64");
    let score = batch
        .column(batch.schema().index_of("score").unwrap())
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("score should be Float64");
    let label = batch
        .column(batch.schema().index_of("label").unwrap())
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("label should be Utf8");
    let active = batch
        .column(batch.schema().index_of("active").unwrap())
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("active should be Boolean");

    (0..batch.num_rows())
        .map(|row| ExpectedRow {
            id: id.value(row),
            score: score.value(row),
            label: label.value(row).to_string(),
            active: active.value(row),
        })
        .collect()
}

fn assert_column_names(batch: &RecordBatch) {
    let schema = batch.schema();
    let names: Vec<&str> = schema
        .fields()
        .iter()
        .map(|field| field.name().as_str())
        .collect();
    assert_eq!(names, vec!["id", "score", "label", "active"]);
}

fn assert_all_rows(expected: &[ExpectedRow], actual: &[ExpectedRow]) {
    assert_eq!(
        expected.len(),
        actual.len(),
        "row count mismatch: expected {}, got {}",
        expected.len(),
        actual.len()
    );
    for index in 0..expected.len() {
        let expected_row = &expected[index];
        let actual_row = &actual[index];
        assert_eq!(actual_row.id, expected_row.id, "id mismatch at row {index}");
        assert!(
            (actual_row.score - expected_row.score).abs() < 1e-12,
            "score mismatch at row {index}: expected {}, got {}",
            expected_row.score,
            actual_row.score
        );
        assert_eq!(
            actual_row.label, expected_row.label,
            "label mismatch at row {index}"
        );
        assert_eq!(
            actual_row.active, expected_row.active,
            "active mismatch at row {index}"
        );
    }
}

fn write_batches(path: &Path, format: OutputFormat, batches: &[RecordBatch]) {
    let file =
        File::create(path).unwrap_or_else(|e| panic!("failed to create {}: {e}", path.display()));
    let config = SinkConfig {
        format,
        ..Default::default()
    };
    let mut sink =
        create_sink(Box::new(file), batches[0].schema(), &config).expect("sink should be created");
    for batch in batches {
        sink.write_batch(batch).expect("write_batch should succeed");
    }
    let stats = sink.finish().expect("finish should succeed");
    assert_eq!(stats.rows_written as usize, total_rows(batches));
    assert!(stats.bytes_written > 0);
}

#[test]
fn parquet_round_trip_preserves_rows_columns_and_values() {
    let source = combined_items_batch();
    let expected = expected_rows(&source);
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("items.parquet");
    write_batches(&path, OutputFormat::Parquet, std::slice::from_ref(&source));

    let file = File::open(&path).unwrap();
    let reader = ParquetRecordBatchReaderBuilder::try_new(file)
        .expect("parquet metadata should load")
        .build()
        .expect("parquet reader should build");
    let read_batches: Vec<RecordBatch> = reader
        .map(|batch| batch.expect("parquet batch should read"))
        .collect();
    let round_trip = concat_batches(&read_batches[0].schema(), &read_batches)
        .expect("parquet batches should concatenate");

    assert_eq!(round_trip.num_rows(), source.num_rows());
    assert_column_names(&round_trip);
    assert_all_rows(&expected, &expected_rows(&round_trip));
}

#[test]
fn csv_round_trip_preserves_header_row_count_and_values() {
    let source = combined_items_batch();
    let expected = expected_rows(&source);
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("items.csv");
    write_batches(&path, OutputFormat::Csv, std::slice::from_ref(&source));

    let content = fs::read_to_string(&path).unwrap();
    let mut reader = csv::Reader::from_reader(content.as_bytes());
    let headers = reader.headers().unwrap().clone();
    assert_eq!(
        headers.iter().collect::<Vec<_>>(),
        vec!["id", "score", "label", "active"]
    );

    let rows = reader
        .records()
        .collect::<Result<Vec<_>, _>>()
        .expect("CSV rows should parse");
    assert_eq!(rows.len(), expected.len());

    for index in 0..expected.len() {
        let row = &rows[index];
        assert_eq!(
            row.get(0).unwrap().parse::<i64>().unwrap(),
            expected[index].id,
            "CSV id mismatch at row {index}"
        );
        assert!(
            (row.get(1).unwrap().parse::<f64>().unwrap() - expected[index].score).abs() < 1e-12,
            "CSV score mismatch at row {index}"
        );
        assert_eq!(
            row.get(2).unwrap(),
            expected[index].label,
            "CSV label mismatch at row {index}"
        );
        assert_eq!(
            row.get(3).unwrap().parse::<bool>().unwrap(),
            expected[index].active,
            "CSV active mismatch at row {index}"
        );
    }
}

#[test]
fn json_round_trip_produces_valid_json_with_expected_records() {
    let source = combined_items_batch();
    let expected = expected_rows(&source);
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("items.json");
    write_batches(&path, OutputFormat::Json, std::slice::from_ref(&source));

    let parsed: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap())
        .expect("JSON output should parse");
    let rows = parsed.as_array().expect("JSON output should be an array");
    assert_eq!(rows.len(), expected.len());

    for index in 0..expected.len() {
        let row = rows[index]
            .as_object()
            .expect("JSON row should be an object");
        assert_eq!(
            row.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["id", "score", "label", "active"]
        );
        assert_eq!(
            row["id"].as_i64().unwrap(),
            expected[index].id,
            "JSON id mismatch at row {index}"
        );
        assert!(
            (row["score"].as_f64().unwrap() - expected[index].score).abs() < 1e-12,
            "JSON score mismatch at row {index}"
        );
        assert_eq!(
            row["label"].as_str().unwrap(),
            expected[index].label,
            "JSON label mismatch at row {index}"
        );
        assert_eq!(
            row["active"].as_bool().unwrap(),
            expected[index].active,
            "JSON active mismatch at row {index}"
        );
    }
}

#[test]
fn jsonl_round_trip_produces_valid_json_lines_with_expected_records() {
    let source = combined_items_batch();
    let expected = expected_rows(&source);
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("items.jsonl");
    write_batches(&path, OutputFormat::Jsonl, std::slice::from_ref(&source));

    let lines: Vec<_> = fs::read_to_string(&path)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("JSONL line should parse"))
        .collect();
    assert_eq!(lines.len(), expected.len());

    for index in 0..expected.len() {
        let row = lines[index]
            .as_object()
            .expect("JSONL row should be an object");
        assert_eq!(
            row["id"].as_i64().unwrap(),
            expected[index].id,
            "JSONL id mismatch at row {index}"
        );
        assert!(
            (row["score"].as_f64().unwrap() - expected[index].score).abs() < 1e-12,
            "JSONL score mismatch at row {index}"
        );
        assert_eq!(
            row["label"].as_str().unwrap(),
            expected[index].label,
            "JSONL label mismatch at row {index}"
        );
        assert_eq!(
            row["active"].as_bool().unwrap(),
            expected[index].active,
            "JSONL active mismatch at row {index}"
        );
    }
}
