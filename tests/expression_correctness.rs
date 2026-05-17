//! Expression integration tests that verify derived fields evaluate exactly.

use arrow::array::{BooleanArray, Float64Array, Int64Array, StringArray};
use arrow::compute::concat_batches;
use arrow::record_batch::RecordBatch;

mod common;
use common::generate_from_toml;

/// Schema exercising numeric, string, and boolean derived expressions.
const EXPRESSION_SCHEMA: &str = r#"
blueprint_version = "1.0"

[model]
name = "expression_correctness"
seed = 123

[[entities]]
name = "items"
count = 1000

[[entities.fields]]
name = "quantity"
data_type = "int"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 1.0
max = 10.0

[[entities.fields]]
name = "unit_price"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 5.0
max = 200.0

[[entities.fields]]
name = "tax_rate"
data_type = "float"
[entities.fields.generator]
type = "constant"
value = 0.1

[[entities.fields]]
name = "subtotal"
data_type = "float"
[entities.fields.generator]
type = "derived"
expr = "${quantity} * ${unit_price}"
depends_on = ["quantity", "unit_price"]

[[entities.fields]]
name = "total"
data_type = "float"
[entities.fields.generator]
type = "derived"
expr = "${subtotal} * (1.0 + ${tax_rate})"
depends_on = ["subtotal", "tax_rate"]

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "derived"
expr = "concat(\"ITEM-\", cast_string(${quantity}))"
depends_on = ["quantity"]

[[entities.fields]]
name = "is_expensive"
data_type = "bool"
[entities.fields.generator]
type = "derived"
expr = "${unit_price} > 100.0"
depends_on = ["unit_price"]
"#;

fn combined_items_batch() -> RecordBatch {
    let data = generate_from_toml(EXPRESSION_SCHEMA);
    let batches = data.get("items").expect("items entity should exist");
    concat_batches(&batches[0].schema(), batches).expect("items batches should concatenate")
}

#[test]
fn derived_fields_match_recomputed_values_for_every_row() {
    let batch = combined_items_batch();

    let quantity = batch
        .column(batch.schema().index_of("quantity").unwrap())
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("quantity should be Int64");
    let unit_price = batch
        .column(batch.schema().index_of("unit_price").unwrap())
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("unit_price should be Float64");
    let tax_rate = batch
        .column(batch.schema().index_of("tax_rate").unwrap())
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("tax_rate should be Float64");
    let subtotal = batch
        .column(batch.schema().index_of("subtotal").unwrap())
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("subtotal should be Float64");
    let total = batch
        .column(batch.schema().index_of("total").unwrap())
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("total should be Float64");
    let label = batch
        .column(batch.schema().index_of("label").unwrap())
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("label should be Utf8");
    let is_expensive = batch
        .column(batch.schema().index_of("is_expensive").unwrap())
        .as_any()
        .downcast_ref::<BooleanArray>()
        .expect("is_expensive should be Boolean");

    assert_eq!(batch.num_rows(), 1000);

    for row in 0..batch.num_rows() {
        let quantity_value = quantity.value(row);
        let unit_price_value = unit_price.value(row);
        let tax_rate_value = tax_rate.value(row);

        let expected_subtotal = quantity_value as f64 * unit_price_value;
        let expected_total = expected_subtotal * (1.0 + tax_rate_value);
        let expected_label = format!("ITEM-{quantity_value}");
        let expected_is_expensive = unit_price_value > 100.0;

        assert!(
            (subtotal.value(row) - expected_subtotal).abs() < 1e-9,
            "subtotal mismatch at row {row}: expected {expected_subtotal}, got {}",
            subtotal.value(row)
        );
        assert!(
            (total.value(row) - expected_total).abs() < 1e-9,
            "total mismatch at row {row}: expected {expected_total}, got {}",
            total.value(row)
        );
        assert_eq!(
            label.value(row),
            expected_label,
            "label mismatch at row {row}"
        );
        assert_eq!(
            is_expensive.value(row),
            expected_is_expensive,
            "is_expensive mismatch at row {row}"
        );
    }
}
