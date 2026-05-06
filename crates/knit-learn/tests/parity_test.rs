//! Parity and correctness tests for the incremental learning pipeline.
//!
//! These tests verify that the incremental pipeline:
//! - Produces deterministic results (same data + seed → same output)
//! - Is chunk-order independent (single vs multiple chunks → equivalent results)
//! - Correctly detects types, categories, and null rates
//! - Handles edge cases (empty data, single row, wide schemas)
//! - Accurately maintains statistics across incremental updates

use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Float64Array, Int32Array, Int64Array, StringArray,
    TimestampMillisecondArray,
};
use arrow::datatypes::{DataType, Field, Schema, TimeUnit};
use arrow::record_batch::RecordBatch;

use knit_learn::incremental::{finalize_state, ingest_batches_to_state, update_relationship_evidence};
use knit_learn::streaming::LearnState;
use knit_learn::type_inference::InferredType;

/// Build a realistic multi-column test batch.
fn make_realistic_batch(n_rows: usize, offset: usize) -> RecordBatch {
    let ids: Vec<i64> = (offset..offset + n_rows).map(|i| i as i64 + 1).collect();
    let amounts: Vec<f64> = (offset..offset + n_rows)
        .map(|i| 10.5 + (i as f64) * 0.7)
        .collect();
    let categories: Vec<&str> = (offset..offset + n_rows)
        .map(|i| match i % 4 {
            0 => "active",
            1 => "pending",
            2 => "active",
            _ => "closed",
        })
        .collect();
    let flags: Vec<bool> = (offset..offset + n_rows).map(|i| i % 3 != 0).collect();
    let timestamps: Vec<i64> = (offset..offset + n_rows)
        .map(|i| 1_700_000_000_000 + (i as i64) * 86_400_000)
        .collect();

    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
        Field::new("status", DataType::Utf8, false),
        Field::new("is_active", DataType::Boolean, false),
        Field::new(
            "created_at",
            DataType::Timestamp(TimeUnit::Millisecond, None),
            false,
        ),
    ]));

    RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int64Array::from(ids)) as ArrayRef,
            Arc::new(Float64Array::from(amounts)) as ArrayRef,
            Arc::new(StringArray::from(categories)) as ArrayRef,
            Arc::new(BooleanArray::from(flags)) as ArrayRef,
            Arc::new(TimestampMillisecondArray::from(timestamps)) as ArrayRef,
        ],
    )
    .unwrap()
}

/// Run incremental-mode analysis on a set of RecordBatches (single chunk).
fn run_incremental_single(
    entity: &str,
    batches: &[RecordBatch],
) -> knit_learn::schema_assembly::TableAnalysis {
    let mut state = LearnState::new(42);
    ingest_batches_to_state(&mut state, entity, batches, "single.csv");
    let (analyses, _rels) = finalize_state(&state);
    analyses.into_iter().find(|a| a.name == entity).unwrap()
}

/// Run incremental-mode with data split across multiple ingest calls.
fn run_incremental_chunked(
    entity: &str,
    batches: &[RecordBatch],
) -> knit_learn::schema_assembly::TableAnalysis {
    let mut state = LearnState::new(42);
    for (i, batch) in batches.iter().enumerate() {
        ingest_batches_to_state(
            &mut state,
            entity,
            &[batch.clone()],
            &format!("chunk_{i}.csv"),
        );
    }
    let (analyses, _rels) = finalize_state(&state);
    analyses.into_iter().find(|a| a.name == entity).unwrap()
}

// ─── Determinism Tests ──────────────────────────────────────────────────────

#[test]
fn determinism_same_seed_same_result() {
    let batch = make_realistic_batch(100, 0);

    let result1 = run_incremental_single("orders", &[batch.clone()]);
    let result2 = run_incremental_single("orders", &[batch]);

    for (c1, c2) in result1.columns.iter().zip(result2.columns.iter()) {
        assert_eq!(c1.inferred_type, c2.inferred_type, "type differs for '{}'", c1.name);
        assert_eq!(c1.null_rate, c2.null_rate, "null_rate differs for '{}'", c1.name);
        assert_eq!(c1.is_integer_valued, c2.is_integer_valued);
        assert_eq!(c1.distribution.is_some(), c2.distribution.is_some());
    }
}

#[test]
fn determinism_different_seeds_same_types() {
    let batch = make_realistic_batch(100, 0);

    let mut state1 = LearnState::new(42);
    let mut state2 = LearnState::new(99);
    ingest_batches_to_state(&mut state1, "t", &[batch.clone()], "a.csv");
    ingest_batches_to_state(&mut state2, "t", &[batch], "a.csv");

    let (r1, _) = finalize_state(&state1);
    let (r2, _) = finalize_state(&state2);

    for (c1, c2) in r1[0].columns.iter().zip(r2[0].columns.iter()) {
        assert_eq!(c1.inferred_type, c2.inferred_type, "type differs for '{}'", c1.name);
    }
}

// ─── Chunking Equivalence Tests ─────────────────────────────────────────────

#[test]
fn chunking_types_equivalent() {
    let batches: Vec<RecordBatch> = (0..5)
        .map(|i| make_realistic_batch(50, i * 50))
        .collect();

    let single = run_incremental_single("orders", &batches);
    let chunked = run_incremental_chunked("orders", &batches);

    assert_eq!(single.columns.len(), chunked.columns.len());
    for (sc, cc) in single.columns.iter().zip(chunked.columns.iter()) {
        assert_eq!(
            sc.inferred_type, cc.inferred_type,
            "type mismatch for '{}': single={:?}, chunked={:?}",
            sc.name, sc.inferred_type, cc.inferred_type
        );
    }
}

#[test]
fn chunking_row_count_equivalent() {
    let batches: Vec<RecordBatch> = (0..4)
        .map(|i| make_realistic_batch(25, i * 25))
        .collect();

    let single = run_incremental_single("orders", &batches);
    let chunked = run_incremental_chunked("orders", &batches);

    assert_eq!(single.row_count, chunked.row_count);
}

#[test]
fn chunking_null_rates_equivalent() {
    let schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int32, false),
        Field::new("name", DataType::Utf8, true),
    ]));

    let batch1 = RecordBatch::try_new(
        schema.clone(),
        vec![
            Arc::new(Int32Array::from(vec![1, 2, 3, 4, 5])) as ArrayRef,
            Arc::new(StringArray::from(vec![
                Some("a"),
                None,
                Some("c"),
                None,
                Some("e"),
            ])) as ArrayRef,
        ],
    )
    .unwrap();

    let batch2 = RecordBatch::try_new(
        schema,
        vec![
            Arc::new(Int32Array::from(vec![6, 7, 8, 9, 10])) as ArrayRef,
            Arc::new(StringArray::from(vec![
                None,
                Some("g"),
                Some("h"),
                None,
                Some("j"),
            ])) as ArrayRef,
        ],
    )
    .unwrap();

    let single = run_incremental_single("t", &[batch1.clone(), batch2.clone()]);
    let chunked = run_incremental_chunked("t", &[batch1, batch2]);

    for (sc, cc) in single.columns.iter().zip(chunked.columns.iter()) {
        let diff = (sc.null_rate - cc.null_rate).abs();
        assert!(
            diff < 0.001,
            "null rate differs for '{}': {} vs {}",
            sc.name, sc.null_rate, cc.null_rate
        );
    }
}

// ─── Type Detection Tests ───────────────────────────────────────────────────

#[test]
fn type_detection_integer() {
    // Need enough unique values to avoid categorical detection
    let schema = Arc::new(Schema::new(vec![Field::new("count", DataType::Int32, false)]));
    let values: Vec<i32> = (1..=100).collect();
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(Int32Array::from(values)) as ArrayRef],
    )
    .unwrap();

    let result = run_incremental_single("t", &[batch]);
    let col = &result.columns[0];
    assert_ne!(col.inferred_type, Some(InferredType::Categorical));
    assert!(col.is_integer_valued);
}

#[test]
fn type_detection_categorical_string() {
    let schema = Arc::new(Schema::new(vec![Field::new("color", DataType::Utf8, false)]));
    let values: Vec<&str> = (0..100)
        .map(|i| match i % 3 {
            0 => "red",
            1 => "green",
            _ => "blue",
        })
        .collect();
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(StringArray::from(values)) as ArrayRef],
    )
    .unwrap();

    let result = run_incremental_single("t", &[batch]);
    let col = &result.columns[0];
    assert_eq!(col.inferred_type, Some(InferredType::Categorical));
    assert!(col.categorical_weights.is_some());
}

#[test]
fn type_detection_boolean() {
    let schema = Arc::new(Schema::new(vec![Field::new("flag", DataType::Boolean, false)]));
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(BooleanArray::from(vec![true, false, true, true, false])) as ArrayRef],
    )
    .unwrap();

    let result = run_incremental_single("t", &[batch]);
    assert_eq!(result.columns[0].inferred_type, Some(InferredType::Boolean));
}

#[test]
fn type_detection_temporal() {
    let schema = Arc::new(Schema::new(vec![Field::new(
        "ts",
        DataType::Timestamp(TimeUnit::Millisecond, None),
        false,
    )]));
    let timestamps: Vec<i64> = (0..50)
        .map(|i| 1_700_000_000_000i64 + i * 86_400_000)
        .collect();
    let batch = RecordBatch::try_new(
        schema,
        vec![Arc::new(TimestampMillisecondArray::from(timestamps)) as ArrayRef],
    )
    .unwrap();

    let result = run_incremental_single("t", &[batch]);
    let col = &result.columns[0];
    // Temporal columns get time component detection from Arrow hint
    assert!(col.has_time_component, "should detect time component from Timestamp type");
}

// ─── Relationship Detection Tests ───────────────────────────────────────────

#[test]
fn relationship_detection_fk_candidate() {
    let users_schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("name", DataType::Utf8, false),
    ]));
    let orders_schema = Arc::new(Schema::new(vec![
        Field::new("id", DataType::Int64, false),
        Field::new("user_id", DataType::Int64, false),
        Field::new("amount", DataType::Float64, false),
    ]));

    let user_ids: Vec<i64> = (1..=100).collect();
    let user_names: Vec<String> = (1..=100).map(|i| format!("user_{i}")).collect();
    let user_names_ref: Vec<&str> = user_names.iter().map(|s| s.as_str()).collect();

    let order_ids: Vec<i64> = (1..=200).collect();
    let order_user_ids: Vec<i64> = (0..200).map(|i| (i % 100) + 1).collect();
    let order_amounts: Vec<f64> = (0..200).map(|i| 10.0 + i as f64).collect();

    let users_batch = RecordBatch::try_new(
        users_schema,
        vec![
            Arc::new(Int64Array::from(user_ids)) as ArrayRef,
            Arc::new(StringArray::from(user_names_ref)) as ArrayRef,
        ],
    )
    .unwrap();

    let orders_batch = RecordBatch::try_new(
        orders_schema,
        vec![
            Arc::new(Int64Array::from(order_ids)) as ArrayRef,
            Arc::new(Int64Array::from(order_user_ids)) as ArrayRef,
            Arc::new(Float64Array::from(order_amounts)) as ArrayRef,
        ],
    )
    .unwrap();

    let mut state = LearnState::new(42);
    ingest_batches_to_state(&mut state, "user", &[users_batch], "users.csv");
    ingest_batches_to_state(&mut state, "order", &[orders_batch], "orders.csv");
    update_relationship_evidence(&mut state);

    assert!(
        !state.relationship_evidence.is_empty(),
        "should detect user_id FK candidate"
    );

    let has_user_fk = state.relationship_evidence.iter().any(|e| {
        e.from_table == "order" && e.from_column == "user_id" && e.to_table == "user"
    });
    assert!(has_user_fk, "should detect order.user_id → user.id");
}

// ─── Edge Cases ─────────────────────────────────────────────────────────────

#[test]
fn edge_case_single_row() {
    let batch = make_realistic_batch(1, 0);
    let result = run_incremental_single("t", &[batch]);
    assert_eq!(result.row_count, 1);
    assert_eq!(result.columns.len(), 5);
}

#[test]
fn edge_case_empty_state_finalize() {
    let state = LearnState::new(42);
    let (analyses, rels) = finalize_state(&state);
    assert!(analyses.is_empty());
    assert!(rels.is_empty());
}

#[test]
fn edge_case_wide_schema() {
    let fields: Vec<Field> = (0..50)
        .map(|i| Field::new(format!("col_{i}"), DataType::Int32, false))
        .collect();
    let schema = Arc::new(Schema::new(fields));
    let arrays: Vec<ArrayRef> = (0..50)
        .map(|i| Arc::new(Int32Array::from(vec![i; 10])) as ArrayRef)
        .collect();
    let batch = RecordBatch::try_new(schema, arrays).unwrap();

    let result = run_incremental_single("wide", &[batch]);
    assert_eq!(result.columns.len(), 50);
}

#[test]
fn stress_test_10k_rows() {
    let batch = make_realistic_batch(10_000, 0);
    let result = run_incremental_single("big", &[batch]);

    assert_eq!(result.row_count, 10_000);

    let id_col = result.columns.iter().find(|c| c.name == "id").unwrap();
    assert!(id_col.is_integer_valued);

    let status_col = result.columns.iter().find(|c| c.name == "status").unwrap();
    assert_eq!(status_col.inferred_type, Some(InferredType::Categorical));

    let bool_col = result.columns.iter().find(|c| c.name == "is_active").unwrap();
    assert_eq!(bool_col.inferred_type, Some(InferredType::Boolean));
}

#[test]
fn stress_test_chunked_10k() {
    let batches: Vec<RecordBatch> = (0..20)
        .map(|i| make_realistic_batch(500, i * 500))
        .collect();

    let single = run_incremental_single("big", &batches);
    let chunked = run_incremental_chunked("big", &batches);

    assert_eq!(single.row_count, chunked.row_count);
    assert_eq!(single.row_count, 10_000);

    for (sc, cc) in single.columns.iter().zip(chunked.columns.iter()) {
        assert_eq!(
            sc.inferred_type, cc.inferred_type,
            "type mismatch for '{}' in 10K stress test",
            sc.name
        );
    }
}

// ─── State Persistence Round-trip ───────────────────────────────────────────

#[test]
fn state_save_load_round_trip() {
    let batch = make_realistic_batch(100, 0);
    let mut state = LearnState::new(42);
    ingest_batches_to_state(&mut state, "orders", &[batch], "test.csv");
    update_relationship_evidence(&mut state);

    let tmp = std::env::temp_dir().join("knit_parity_test_state.json");
    state.save(&tmp).unwrap();

    let loaded = LearnState::load(&tmp).unwrap().expect("state file should exist");

    let (orig_analyses, orig_rels) = finalize_state(&state);
    let (loaded_analyses, loaded_rels) = finalize_state(&loaded);

    assert_eq!(orig_analyses.len(), loaded_analyses.len());
    assert_eq!(orig_rels.len(), loaded_rels.len());

    for (oa, la) in orig_analyses.iter().zip(loaded_analyses.iter()) {
        assert_eq!(oa.row_count, la.row_count);
        for (oc, lc) in oa.columns.iter().zip(la.columns.iter()) {
            assert_eq!(
                oc.inferred_type, lc.inferred_type,
                "type differs for '{}'",
                oc.name
            );
            assert_eq!(oc.null_rate, lc.null_rate);
        }
    }

    let _ = std::fs::remove_file(&tmp);
}
