//! Integration tests covering bind output formats that were previously untested.

use std::fs;
use std::fs::File;
use std::path::Path;

use apache_avro::Reader as AvroReader;
use apache_avro::types::Value as AvroValue;
use arrow::array::{BooleanArray, Int64Array, StringArray};
use arrow::compute::concat_batches;
use arrow::ipc::reader::FileReader as ArrowIpcFileReader;
use arrow::record_batch::RecordBatch;
use knit::bind::{OutputFormat, SinkConfig, TemplateMode, create_sink};
use tempfile::TempDir;

mod common;
use common::generate_from_toml;

/// Simple schema used to verify bind output preserves rows and columns.
const BIND_FORMAT_SCHEMA: &str = r#"
blueprint_version = "1.0"

[model]
name = "bind_formats"
seed = 77

[[entities]]
name = "items"
count = 8

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpectedRow {
    id: i64,
    label: String,
    active: bool,
}

fn combined_items_batch() -> RecordBatch {
    let data = generate_from_toml(BIND_FORMAT_SCHEMA);
    let batches = data.get("items").expect("items entity should exist");
    concat_batches(&batches[0].schema(), batches).expect("items batches should concatenate")
}

fn expected_rows(batch: &RecordBatch) -> Vec<ExpectedRow> {
    let ids = batch
        .column(
            batch
                .schema()
                .index_of("id")
                .expect("id column should exist"),
        )
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("id should be Int64");
    let labels = batch
        .column(
            batch
                .schema()
                .index_of("label")
                .expect("label column should exist"),
        )
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("label should be Utf8");
    let actives = batch
        .column(
            batch
                .schema()
                .index_of("active")
                .expect("active column should exist"),
        )
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("active should be Boolean");

    (0..batch.num_rows())
        .map(|row| ExpectedRow {
            id: ids.value(row),
            label: labels.value(row).to_string(),
            active: actives.value(row),
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
    assert_eq!(names, vec!["id", "label", "active"]);
}

fn write_batches(path: &Path, config: SinkConfig, batches: &[RecordBatch]) {
    let file = File::create(path)
        .unwrap_or_else(|error| panic!("failed to create {}: {error}", path.display()));
    let mut sink =
        create_sink(Box::new(file), batches[0].schema(), &config).expect("sink should be created");
    let expected_rows: usize = batches.iter().map(|batch| batch.num_rows()).sum();

    for batch in batches {
        sink.write_batch(batch).expect("write_batch should succeed");
    }

    let stats = sink.finish().expect("finish should succeed");
    assert_eq!(stats.rows_written as usize, expected_rows);
    assert!(stats.bytes_written > 0);
}

fn avro_record_field<'a>(record: &'a [(String, AvroValue)], name: &str) -> &'a AvroValue {
    record
        .iter()
        .find_map(|(field_name, value)| (field_name == name).then_some(value))
        .unwrap_or_else(|| panic!("missing Avro field {name}"))
}

fn unwrap_avro_union(value: &AvroValue) -> &AvroValue {
    match value {
        AvroValue::Union(_, inner) => inner.as_ref(),
        other => other,
    }
}

/// Verify Avro output round-trips rows, columns, and values through the Avro reader.
#[test]
fn avro_round_trip_preserves_rows_columns_and_values() {
    let source = combined_items_batch();
    let expected = expected_rows(&source);
    let dir = TempDir::new().expect("temp dir should be created");
    let path = dir.path().join("items.avro");

    write_batches(
        &path,
        SinkConfig {
            format: OutputFormat::Avro,
            record_name: "items".to_string(),
            ..Default::default()
        },
        std::slice::from_ref(&source),
    );

    let file = File::open(&path).expect("Avro file should open");
    let records: Vec<AvroValue> = AvroReader::new(file)
        .expect("Avro reader should build")
        .map(|record| record.expect("Avro record should decode"))
        .collect();

    assert_eq!(records.len(), expected.len());
    for (record, expected_row) in records.iter().zip(&expected) {
        let AvroValue::Record(fields) = record else {
            panic!("Avro row should decode as a record: {record:?}");
        };
        assert_eq!(
            unwrap_avro_union(avro_record_field(fields, "id")),
            &AvroValue::Long(expected_row.id)
        );
        assert_eq!(
            unwrap_avro_union(avro_record_field(fields, "label")),
            &AvroValue::String(expected_row.label.clone())
        );
        assert_eq!(
            unwrap_avro_union(avro_record_field(fields, "active")),
            &AvroValue::Boolean(expected_row.active)
        );
    }
}

/// Verify SQL output emits DDL, transaction wrappers, and INSERT rows with expected values.
#[test]
fn sql_output_contains_expected_statements_and_values() {
    let source = combined_items_batch();
    let expected = expected_rows(&source);
    let dir = TempDir::new().expect("temp dir should be created");
    let path = dir.path().join("items.sql");

    write_batches(
        &path,
        SinkConfig {
            format: OutputFormat::Sql,
            record_name: "items".to_string(),
            sql_create_table: true,
            sql_transaction: true,
            sql_rows_per_insert: 3,
            ..Default::default()
        },
        std::slice::from_ref(&source),
    );

    let content = fs::read_to_string(&path).expect("SQL output should be readable");
    assert!(content.starts_with("CREATE TABLE \"items\" ("));
    assert!(content.contains("\"id\" BIGINT"));
    assert!(content.contains("\"label\" TEXT"));
    assert!(content.contains("\"active\" BOOLEAN"));
    assert!(content.contains("\nBEGIN;\n"));
    assert!(content.ends_with("\nCOMMIT;\n"));
    assert_eq!(content.matches("INSERT INTO \"items\"").count(), 3);

    // Verify every generated row appears as a VALUES tuple
    let mut found_count = 0;
    for row in &expected {
        let expected_tuple = format!(
            "({}, '{}', {})",
            row.id,
            row.label,
            if row.active { "TRUE" } else { "FALSE" }
        );
        assert!(
            content.contains(&expected_tuple),
            "missing SQL tuple {expected_tuple}"
        );
        found_count += 1;
    }
    assert_eq!(
        found_count,
        expected.len(),
        "every row should be present in the SQL output"
    );
}

/// Verify Arrow IPC output can be read back with the Arrow file reader unchanged.
#[test]
fn arrow_ipc_round_trip_preserves_rows_columns_and_values() {
    let source = combined_items_batch();
    let expected = expected_rows(&source);
    let dir = TempDir::new().expect("temp dir should be created");
    let path = dir.path().join("items.arrow");

    write_batches(
        &path,
        SinkConfig {
            format: OutputFormat::ArrowIpc,
            ..Default::default()
        },
        std::slice::from_ref(&source),
    );

    let file = File::open(&path).expect("IPC file should open");
    let reader = ArrowIpcFileReader::try_new(file, None).expect("IPC reader should build");
    let batches: Vec<RecordBatch> = reader
        .map(|batch| batch.expect("IPC batch should read"))
        .collect();
    let round_trip =
        concat_batches(&batches[0].schema(), &batches).expect("IPC batches should concatenate");

    assert_eq!(round_trip.num_rows(), source.num_rows());
    assert_column_names(&round_trip);
    assert_eq!(expected_rows(&round_trip), expected);
}

/// Verify custom MiniJinja templates render the expected per-row output.
#[test]
fn template_output_renders_custom_rows_with_expected_values() {
    let source = combined_items_batch();
    let expected = expected_rows(&source);
    let dir = TempDir::new().expect("temp dir should be created");
    let path = dir.path().join("items.txt");

    write_batches(
        &path,
        SinkConfig {
            format: OutputFormat::Template,
            template_source:
                "{% for row in rows %}{{ row.id }}|{{ row.label }}|{{ row.active }}\n{% endfor %}"
                    .to_string(),
            template_mode: Some(TemplateMode::PerBatch),
            ..Default::default()
        },
        std::slice::from_ref(&source),
    );

    let lines: Vec<String> = fs::read_to_string(&path)
        .expect("template output should be readable")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(ToOwned::to_owned)
        .collect();

    let expected_lines: Vec<String> = expected
        .iter()
        .map(|row| format!("{}|{}|{}", row.id, row.label, row.active))
        .collect();
    assert_eq!(lines, expected_lines);
}
