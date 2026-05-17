//! Property-based integration tests for core generation invariants.

use std::collections::HashSet;

use arrow::array::{Array, Int64Array};
use arrow::datatypes::DataType as ArrowDataType;
use arrow::record_batch::RecordBatch;
use knit::noise::{DuplicateInjector, NullInjector, PerturbConfig, Pipeline};
use proptest::prelude::*;
use rand::Rng;
use rand::SeedableRng;
use rand::rngs::ChaCha8Rng;

mod common;
use common::generate_from_toml;

fn batches_equal(a: &[RecordBatch], b: &[RecordBatch]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).all(|(left, right)| left == right)
}

fn collect_i64_column(batches: &[RecordBatch], column: &str) -> Vec<i64> {
    let mut values = Vec::new();
    for batch in batches {
        let column_index = batch
            .schema()
            .index_of(column)
            .unwrap_or_else(|_| panic!("column '{column}' should exist"));
        let array = batch.column(column_index);
        let array = array
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap_or_else(|| panic!("column '{column}' should be Int64"));
        for index in 0..array.len() {
            if !array.is_null(index) {
                values.push(array.value(index));
            }
        }
    }
    values
}

fn schema_fields(batch: &RecordBatch) -> Vec<(String, ArrowDataType, bool)> {
    batch
        .schema()
        .fields()
        .iter()
        .map(|field| {
            (
                field.name().clone(),
                field.data_type().clone(),
                field.is_nullable(),
            )
        })
        .collect()
}

proptest! {
    /// Verify generated row counts always match the declared entity count
    #[test]
    fn row_count_matches_declared(count in 1u32..=500u32) {
        let schema = format!(r#"
blueprint_version = "1.0"

[model]
name = "row_count"
seed = 42

[[entities]]
name = "items"
count = {count}

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1
"#);

        let data = generate_from_toml(&schema);
        let batches = data.get("items").expect("items entity should exist");
        let total: usize = batches.iter().map(|batch| batch.num_rows()).sum();

        prop_assert_eq!(total, count as usize);
    }

    /// Verify row count invariant holds for counts that span multiple batches (>8192)
    #[test]
    fn row_count_matches_declared_multi_batch(count in 8193u32..=10000u32) {
        let schema = format!(r#"
blueprint_version = "1.0"

[model]
name = "row_count_multi"
seed = 42

[[entities]]
name = "items"
count = {count}

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1
"#);

        let data = generate_from_toml(&schema);
        let batches = data.get("items").expect("items entity should exist");
        prop_assert!(batches.len() > 1, "should produce multiple batches for count > 8192");
        let total: usize = batches.iter().map(|batch| batch.num_rows()).sum();
        prop_assert_eq!(total, count as usize);
    }

    /// Verify identical schemas and seeds always produce byte-identical batches
    #[test]
    fn generation_is_deterministic_for_same_seed(seed in any::<u64>()) {
        let schema = format!(r#"
blueprint_version = "1.0"

[model]
name = "determinism"
seed = {seed}

[[entities]]
name = "items"
count = 64

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
type = "one_of"
choices = [
    {{ value = "alpha", weight = 0.5 }},
    {{ value = "beta", weight = 0.3 }},
    {{ value = "gamma", weight = 0.2 }},
]

[[entities.fields]]
name = "active"
data_type = "bool"
[entities.fields.generator]
type = "one_of"
choices = [
    {{ value = true, weight = 0.7 }},
    {{ value = false, weight = 0.3 }},
]
"#);

        let run_one = generate_from_toml(&schema);
        let run_two = generate_from_toml(&schema);
        let batches_one = run_one.get("items").expect("first run should contain items");
        let batches_two = run_two.get("items").expect("second run should contain items");

        prop_assert!(batches_equal(batches_one, batches_two));
    }

    /// Verify generated Arrow schemas preserve declared column names and types
    #[test]
    fn generated_schema_matches_declared_fields(field_count in 1usize..=4usize) {
        let mut fields = String::from(r#"
[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1
"#);
        let mut expected = vec![("id".to_string(), ArrowDataType::Int64, true)];

        if field_count >= 2 {
            fields.push_str(r#"

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "derived"
expr = "concat(\"item-\", cast_string(${id}))"
depends_on = ["id"]
"#);
            expected.push(("label".to_string(), ArrowDataType::Utf8, true));
        }

        if field_count >= 3 {
            fields.push_str(r#"

[[entities.fields]]
name = "score"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 0.0
max = 1.0
"#);
            expected.push(("score".to_string(), ArrowDataType::Float64, true));
        }

        if field_count >= 4 {
            fields.push_str(r#"

[[entities.fields]]
name = "active"
data_type = "bool"
[entities.fields.generator]
type = "one_of"
choices = [
    { value = true, weight = 0.6 },
    { value = false, weight = 0.4 },
]
"#);
            expected.push(("active".to_string(), ArrowDataType::Boolean, true));
        }

        let schema = format!(r#"
blueprint_version = "1.0"

[model]
name = "schema_preservation"
seed = 7

[[entities]]
name = "items"
count = 16
{fields}
"#);

        let data = generate_from_toml(&schema);
        let batches = data.get("items").expect("items entity should exist");
        let actual = schema_fields(&batches[0]);

        prop_assert_eq!(actual, expected);
    }

    /// Verify child foreign keys only reference values present in the parent primary key column
    #[test]
    fn foreign_keys_reference_existing_parent_rows(
        parent_count in 1u32..=250u32,
        child_count in 1u32..=500u32,
        seed in any::<u64>(),
    ) {
        let schema = format!(r#"
blueprint_version = "1.0"

[model]
name = "fk_integrity"
seed = {seed}

[[entities]]
name = "parents"
count = {parent_count}

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities]]
name = "children"
count = {child_count}

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "parent_id"
data_type = "int"

[[relationships]]
name = "child_parent"
from = "children"
to = "parents"
kind = "many_to_one"
foreign_key = "parent_id"
"#);

        let data = generate_from_toml(&schema);
        let parent_ids: HashSet<i64> = collect_i64_column(
            data.get("parents").expect("parents entity should exist"),
            "id",
        )
        .into_iter()
        .collect();
        let child_parent_ids = collect_i64_column(
            data.get("children").expect("children entity should exist"),
            "parent_id",
        );

        prop_assert_eq!(parent_ids.len(), parent_count as usize);
        prop_assert_eq!(child_parent_ids.len(), child_count as usize);
        prop_assert!(!parent_ids.is_empty());
        prop_assert!(child_parent_ids
            .iter()
            .all(|parent_id| parent_ids.contains(parent_id)));
    }

    /// Verify null-injection noise preserves Arrow field names, types, and row counts
    #[test]
    fn null_injection_preserves_schema(
        count in 1u32..=200u32,
        seed in any::<u64>(),
    ) {
        let schema = format!(r#"
blueprint_version = "1.0"

[model]
name = "noise_nulls"
seed = {seed}

[[entities]]
name = "items"
count = {count}

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
expr = "concat(\"item-\", cast_string(${{id}}))"
depends_on = ["id"]

[[entities.fields]]
name = "score"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 0.0
max = 100.0
"#);

        let data = generate_from_toml(&schema);
        let batches = data.get("items").expect("items entity should exist");
        let mut seed_rng = ChaCha8Rng::seed_from_u64(seed);
        let pipeline_seed = seed_rng.next_u64();
        let mut pipeline = Pipeline::new(PerturbConfig::default().with_probability(1.0).with_seed(pipeline_seed));
        pipeline.add(Box::new(NullInjector::new()));

        for batch in batches {
            let noisy = pipeline.run(batch.clone()).expect("null injection should succeed");
            prop_assert_eq!(schema_fields(&noisy), schema_fields(batch));
            prop_assert_eq!(noisy.num_rows(), batch.num_rows());

            // Verify nulls were actually injected in at least one nullable column
            if batch.num_rows() > 1 {
                let mut total_nulls = 0u64;
                for col_idx in 0..noisy.num_columns() {
                    total_nulls += noisy.column(col_idx).null_count() as u64;
                }
                prop_assert!(
                    total_nulls > 0,
                    "with probability 1.0, null injection should produce at least one null"
                );
            }
        }
    }

    /// Verify duplicate-injection noise preserves Arrow field names and types while only changing row count
    #[test]
    fn duplicate_injection_preserves_schema(
        count in 1u32..=200u32,
        seed in any::<u64>(),
    ) {
        let schema = format!(r#"
blueprint_version = "1.0"

[model]
name = "noise_duplicates"
seed = {seed}

[[entities]]
name = "items"
count = {count}

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "active"
data_type = "bool"
[entities.fields.generator]
type = "one_of"
choices = [
    {{ value = true, weight = 0.5 }},
    {{ value = false, weight = 0.5 }},
]
"#);

        let data = generate_from_toml(&schema);
        let batches = data.get("items").expect("items entity should exist");
        let mut seed_rng = ChaCha8Rng::seed_from_u64(seed);
        let pipeline_seed = seed_rng.next_u64();
        let mut pipeline = Pipeline::new(PerturbConfig::default().with_probability(1.0).with_seed(pipeline_seed));
        pipeline.add(Box::new(DuplicateInjector::new()));

        for batch in batches {
            let noisy = pipeline.run(batch.clone()).expect("duplicate injection should succeed");
            prop_assert_eq!(schema_fields(&noisy), schema_fields(batch));
            prop_assert_eq!(noisy.num_rows(), batch.num_rows() * 2);
        }
    }
}
