//! Foreign-key integrity tests: every FK value must exist in the referenced
//! parent entity's primary-key column.

use std::collections::HashSet;

use arrow::array::{Array, Int64Array};
use knit_integration_tests::{example_schemas, generate_from_file};

/// Collect all values of an `Int64` (or castable) column across batches.
fn collect_i64_column(
    batches: &[arrow::record_batch::RecordBatch],
    column: &str,
) -> Vec<Option<i64>> {
    let mut values = Vec::new();
    for batch in batches {
        let idx = batch
            .schema()
            .index_of(column)
            .unwrap_or_else(|_| panic!("column '{column}' not found"));
        let arr = batch.column(idx);
        let i64_arr = arr
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap_or_else(|| panic!("column '{column}' is not Int64"));
        for i in 0..i64_arr.len() {
            if i64_arr.is_null(i) {
                values.push(None);
            } else {
                values.push(Some(i64_arr.value(i)));
            }
        }
    }
    values
}

#[test]
fn ecommerce_order_user_fk_integrity() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |n| n.starts_with("ecommerce"))
        })
        .expect("ecommerce.weave.toml not found");

    let batches = generate_from_file(&path);

    let user_ids: HashSet<i64> = collect_i64_column(
        batches.get("users").expect("no users batches"),
        "id",
    )
    .into_iter()
    .flatten()
    .collect();

    assert!(!user_ids.is_empty(), "users.id should not be empty");

    let order_user_ids = collect_i64_column(
        batches.get("orders").expect("no orders batches"),
        "user_id",
    );

    for (i, val) in order_user_ids.iter().enumerate() {
        if let Some(uid) = val {
            assert!(
                user_ids.contains(uid),
                "orders row {i}: user_id={uid} not found in users.id"
            );
        }
    }
}

#[test]
fn ecommerce_order_product_fk_integrity() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |n| n.starts_with("ecommerce"))
        })
        .expect("ecommerce.weave.toml not found");

    let batches = generate_from_file(&path);

    let product_ids: HashSet<i64> = collect_i64_column(
        batches.get("products").expect("no products batches"),
        "id",
    )
    .into_iter()
    .flatten()
    .collect();

    assert!(
        !product_ids.is_empty(),
        "products.id should not be empty"
    );

    let order_product_ids = collect_i64_column(
        batches.get("orders").expect("no orders batches"),
        "product_id",
    );

    for (i, val) in order_product_ids.iter().enumerate() {
        if let Some(pid) = val {
            assert!(
                product_ids.contains(pid),
                "orders row {i}: product_id={pid} not found in products.id"
            );
        }
    }
}

#[test]
fn ecommerce_review_fk_integrity() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |n| n.starts_with("ecommerce"))
        })
        .expect("ecommerce.weave.toml not found");

    let batches = generate_from_file(&path);

    let user_ids: HashSet<i64> = collect_i64_column(
        batches.get("users").expect("no users batches"),
        "id",
    )
    .into_iter()
    .flatten()
    .collect();

    let product_ids: HashSet<i64> = collect_i64_column(
        batches.get("products").expect("no products batches"),
        "id",
    )
    .into_iter()
    .flatten()
    .collect();

    let review_user_ids = collect_i64_column(
        batches.get("reviews").expect("no reviews batches"),
        "user_id",
    );
    for (i, val) in review_user_ids.iter().enumerate() {
        if let Some(uid) = val {
            assert!(
                user_ids.contains(uid),
                "reviews row {i}: user_id={uid} not found in users.id"
            );
        }
    }

    let review_product_ids = collect_i64_column(
        batches.get("reviews").expect("no reviews batches"),
        "product_id",
    );
    for (i, val) in review_product_ids.iter().enumerate() {
        if let Some(pid) = val {
            assert!(
                product_ids.contains(pid),
                "reviews row {i}: product_id={pid} not found in products.id"
            );
        }
    }
}

#[test]
fn iot_reading_device_fk_integrity() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |n| n.starts_with("iot"))
        })
        .expect("iot_sensors.weave.toml not found");

    let batches = generate_from_file(&path);

    let device_ids: HashSet<i64> = collect_i64_column(
        batches.get("devices").expect("no devices batches"),
        "id",
    )
    .into_iter()
    .flatten()
    .collect();

    let reading_device_ids = collect_i64_column(
        batches.get("readings").expect("no readings batches"),
        "device_id",
    );

    for (i, val) in reading_device_ids.iter().enumerate() {
        if let Some(did) = val {
            assert!(
                device_ids.contains(did),
                "readings row {i}: device_id={did} not found in devices.id"
            );
        }
    }
}

#[test]
fn financial_transaction_account_fk_integrity() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |n| n.starts_with("financial"))
        })
        .expect("financial.weave.toml not found");

    let batches = generate_from_file(&path);

    let account_ids: HashSet<i64> = collect_i64_column(
        batches.get("accounts").expect("no accounts batches"),
        "id",
    )
    .into_iter()
    .flatten()
    .collect();

    let tx_account_ids = collect_i64_column(
        batches.get("transactions").expect("no transactions batches"),
        "account_id",
    );

    for (i, val) in tx_account_ids.iter().enumerate() {
        if let Some(aid) = val {
            assert!(
                account_ids.contains(aid),
                "transactions row {i}: account_id={aid} not found in accounts.id"
            );
        }
    }
}
