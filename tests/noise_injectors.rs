//! Integration tests covering individual noise injectors and combined pipelines.

use arrow::array::{Array, Float64Array, Int64Array, StringArray, TimestampMillisecondArray};
use arrow::compute::concat_batches;
use arrow::datatypes::{DataType, TimeUnit};
use arrow::record_batch::RecordBatch;
use rand::SeedableRng;
use rand::rngs::ChaCha8Rng;
use regex::Regex;

use knit::noise::{
    DuplicateInjector, FormatCorruptor, GaussianNoise, OutlierInjector, PerturbConfig,
    PerturbOverrides, Perturbator, Pipeline, SwapInjector, TemporalSpikeInjector, TruncateInjector,
    TypoInjector, ValueDrifter,
};

mod common;
use common::generate_from_toml;

/// Schema with stable clean data used to verify injector behavior.
const NOISE_INJECTOR_SCHEMA: &str = r#"
blueprint_version = "1.0"

[model]
name = "noise_injectors"
seed = 42

[[entities]]
name = "items"
count = 16

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
max = 20.0

[[entities.fields]]
name = "name"
data_type = "string"
[entities.fields.generator]
type = "derived"
expr = "concat(\"User\", cast_string(${id}), \"Alpha\")"
depends_on = ["id"]

[[entities.fields]]
name = "email"
data_type = "string"
[entities.fields.generator]
type = "derived"
expr = "concat(\"user\", cast_string(${id}), \"@example.com\")"
depends_on = ["id"]

[[entities.fields]]
name = "event_time"
data_type = "datetime"
[entities.fields.generator]
type = "sequence"
start = "2024-01-01T00:00:00Z"
step = "1h"
"#;

fn combined_items_batch() -> RecordBatch {
    let data = generate_from_toml(NOISE_INJECTOR_SCHEMA);
    let batches = data.get("items").expect("items entity should exist");
    concat_batches(&batches[0].schema(), batches).expect("items batches should concatenate")
}

fn int_values(batch: &RecordBatch, column: &str) -> Vec<i64> {
    let array = batch
        .column(
            batch
                .schema()
                .index_of(column)
                .expect("column should exist"),
        )
        .as_any()
        .downcast_ref::<Int64Array>()
        .expect("column should be Int64");
    (0..array.len()).map(|index| array.value(index)).collect()
}

fn float_values(batch: &RecordBatch, column: &str) -> Vec<f64> {
    let array = batch
        .column(
            batch
                .schema()
                .index_of(column)
                .expect("column should exist"),
        )
        .as_any()
        .downcast_ref::<Float64Array>()
        .expect("column should be Float64");
    (0..array.len()).map(|index| array.value(index)).collect()
}

fn string_values(batch: &RecordBatch, column: &str) -> Vec<String> {
    let array = batch
        .column(
            batch
                .schema()
                .index_of(column)
                .expect("column should exist"),
        )
        .as_any()
        .downcast_ref::<StringArray>()
        .expect("column should be Utf8");
    (0..array.len())
        .map(|index| array.value(index).to_string())
        .collect()
}

fn timestamp_values(batch: &RecordBatch, column: &str) -> Vec<i64> {
    let array = batch.column(
        batch
            .schema()
            .index_of(column)
            .expect("column should exist"),
    );
    match array.data_type() {
        DataType::Timestamp(TimeUnit::Millisecond, _) => array
            .as_any()
            .downcast_ref::<TimestampMillisecondArray>()
            .expect("timestamp column should be millisecond precision")
            .values()
            .iter()
            .copied()
            .collect(),
        other => panic!("unsupported timestamp type in test: {other:?}"),
    }
}

/// Verify `DuplicateInjector` appends duplicate rows to the end of the batch.
#[test]
fn duplicate_injector_appends_duplicate_rows() {
    let source = combined_items_batch();
    let source_ids = int_values(&source, "id");
    let mut rng = ChaCha8Rng::seed_from_u64(7);
    let config = PerturbConfig::default().with_probability(1.0);

    let noisy = DuplicateInjector::new()
        .perturb(source.clone(), &mut rng, &config)
        .expect("duplicate injection should succeed");
    let noisy_ids = int_values(&noisy, "id");

    assert_eq!(noisy.num_rows(), source.num_rows() * 2);
    assert_eq!(&noisy_ids[..source_ids.len()], source_ids.as_slice());
    assert_eq!(&noisy_ids[source_ids.len()..], source_ids.as_slice());
}

/// Verify `SwapInjector` only reorders the targeted column while preserving its values.
#[test]
fn swap_injector_reorders_targeted_values_without_changing_their_multiset() {
    let source = combined_items_batch();
    let original_ids = int_values(&source, "id");
    let original_names = string_values(&source, "name");
    let mut rng = ChaCha8Rng::seed_from_u64(11);
    let config = PerturbConfig::default()
        .with_probability(1.0)
        .with_columns(vec!["name".to_string()]);

    let noisy = SwapInjector::new()
        .perturb(source.clone(), &mut rng, &config)
        .expect("swap injection should succeed");
    let noisy_ids = int_values(&noisy, "id");
    let noisy_names = string_values(&noisy, "name");
    let mut sorted_original_names = original_names.clone();
    sorted_original_names.sort();
    let mut sorted_noisy_names = noisy_names.clone();
    sorted_noisy_names.sort();

    assert_eq!(
        noisy_ids, original_ids,
        "non-targeted id column should be unchanged"
    );
    assert_ne!(
        noisy_names, original_names,
        "targeted names should be reordered"
    );
    assert_eq!(sorted_noisy_names, sorted_original_names);
}

/// Verify `TruncateInjector` shortens targeted strings while keeping them as prefixes.
#[test]
fn truncate_injector_shortens_strings() {
    let source = combined_items_batch();
    let original_names = string_values(&source, "name");
    let mut rng = ChaCha8Rng::seed_from_u64(13);
    let config = PerturbConfig::default()
        .with_probability(1.0)
        .with_columns(vec!["name".to_string()]);

    let noisy = TruncateInjector::new()
        .perturb(source.clone(), &mut rng, &config)
        .expect("truncate injection should succeed");
    let noisy_names = string_values(&noisy, "name");

    assert!(
        noisy_names
            .iter()
            .zip(&original_names)
            .any(|(noisy, original)| noisy.len() < original.len()),
        "at least one string should be truncated"
    );
    let truncated_count = noisy_names
        .iter()
        .zip(&original_names)
        .filter(|(noisy, original)| noisy.len() < original.len())
        .count();
    assert!(
        truncated_count >= original_names.len() * 3 / 4,
        "with probability 1.0, at least 75% of strings should be truncated, \
         but only {truncated_count}/{} were",
        original_names.len()
    );
    for (noisy, original) in noisy_names.iter().zip(&original_names) {
        assert!(original.starts_with(noisy));
    }
}

/// Verify `TypoInjector` changes targeted strings while preserving batch shape.
#[test]
fn typo_injector_modifies_string_values() {
    let source = combined_items_batch();
    let original_names = string_values(&source, "name");
    let mut rng = ChaCha8Rng::seed_from_u64(17);
    let config = PerturbConfig::default()
        .with_probability(1.0)
        .with_columns(vec!["name".to_string()]);

    let noisy = TypoInjector::new()
        .perturb(source.clone(), &mut rng, &config)
        .expect("typo injection should succeed");
    let noisy_names = string_values(&noisy, "name");

    assert_eq!(noisy.num_rows(), source.num_rows());
    let changed_count = noisy_names
        .iter()
        .zip(&original_names)
        .filter(|(noisy, original)| noisy != original)
        .count();
    assert!(
        changed_count >= original_names.len() * 3 / 4,
        "with probability 1.0, at least 75% of strings should contain a typo, \
         but only {changed_count}/{} changed",
        original_names.len()
    );
}

/// Verify `GaussianNoise` perturbs targeted numeric values.
#[test]
fn gaussian_noise_changes_numeric_values() {
    let source = combined_items_batch();
    let original_scores = float_values(&source, "score");
    let mut rng = ChaCha8Rng::seed_from_u64(19);
    let config = PerturbConfig::default()
        .with_probability(1.0)
        .with_columns(vec!["score".to_string()]);

    let noisy = GaussianNoise::absolute(2.5)
        .perturb(source.clone(), &mut rng, &config)
        .expect("gaussian noise should succeed");
    let noisy_scores = float_values(&noisy, "score");

    let changed_count = noisy_scores
        .iter()
        .zip(&original_scores)
        .filter(|(noisy, original)| (**noisy - **original).abs() > 1e-9)
        .count();
    assert!(
        changed_count >= original_scores.len() * 3 / 4,
        "with probability 1.0, at least 75% of scores should change, \
         but only {changed_count}/{} changed",
        original_scores.len()
    );
}

/// Verify `OutlierInjector` pushes targeted numeric values outside the original range.
#[test]
fn outlier_injector_creates_values_outside_the_original_range() {
    let source = combined_items_batch();
    let original_scores = float_values(&source, "score");
    let original_min = original_scores
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let original_max = original_scores
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let mut rng = ChaCha8Rng::seed_from_u64(23);
    let config = PerturbConfig::default()
        .with_probability(1.0)
        .with_columns(vec!["score".to_string()]);

    let noisy = OutlierInjector::new(25.0)
        .perturb(source.clone(), &mut rng, &config)
        .expect("outlier injection should succeed");
    let noisy_scores = float_values(&noisy, "score");

    let outlier_count = noisy_scores
        .iter()
        .filter(|value| **value < original_min || **value > original_max)
        .count();
    assert!(
        outlier_count >= noisy_scores.len() * 3 / 4,
        "with probability 1.0, at least 75% of scores should become outliers, \
         but only {outlier_count}/{} did",
        noisy_scores.len()
    );
}

/// Verify `FormatCorruptor` breaks clean email formatting.
#[test]
fn format_corruptor_breaks_email_structure() {
    let source = combined_items_batch();
    let original_emails = string_values(&source, "email");
    let email_regex = Regex::new(r"^[^@]+@[^@]+\.[^@]+$").expect("regex should compile");
    let mut rng = ChaCha8Rng::seed_from_u64(29);
    let config = PerturbConfig::default()
        .with_probability(1.0)
        .with_columns(vec!["email".to_string()]);

    let noisy = FormatCorruptor::new()
        .perturb(source.clone(), &mut rng, &config)
        .expect("format corruption should succeed");
    let noisy_emails = string_values(&noisy, "email");

    assert!(
        noisy_emails
            .iter()
            .zip(&original_emails)
            .all(|(noisy, original)| noisy != original),
        "all targeted emails should be modified"
    );
    assert!(
        noisy_emails
            .iter()
            .all(|email| !email_regex.is_match(email)),
        "all corrupted emails should violate the normal email pattern"
    );
}

/// Verify `ValueDrifter` applies a predictable progressive offset by row index.
#[test]
fn value_drifter_applies_progressive_drift() {
    let source = combined_items_batch();
    let original_scores = float_values(&source, "score");
    let mut rng = ChaCha8Rng::seed_from_u64(31);
    let config = PerturbConfig::default()
        .with_probability(1.0)
        .with_columns(vec!["score".to_string()]);

    let noisy = ValueDrifter::new(0.5)
        .perturb(source.clone(), &mut rng, &config)
        .expect("value drift should succeed");
    let noisy_scores = float_values(&noisy, "score");

    for (index, (noisy, original)) in noisy_scores.iter().zip(&original_scores).enumerate() {
        assert!((noisy - (original + 0.5 * index as f64)).abs() < 1e-9);
    }
}

/// Verify `TemporalSpikeInjector` compresses timestamps around a small number of spike centers.
#[test]
fn temporal_spike_injector_clusters_timestamps() {
    let source = combined_items_batch();
    let original_times = timestamp_values(&source, "event_time");
    let original_span = original_times.last().expect("timestamps should exist")
        - original_times.first().expect("timestamps should exist");
    let mut rng = ChaCha8Rng::seed_from_u64(37);
    let config = PerturbConfig::default()
        .with_probability(1.0)
        .with_columns(vec!["event_time".to_string()]);

    let noisy = TemporalSpikeInjector::new()
        .with_spike_count(1)
        .with_spread_ms(1.0)
        .perturb(source.clone(), &mut rng, &config)
        .expect("temporal spikes should succeed");
    let noisy_times = timestamp_values(&noisy, "event_time");
    let noisy_min = noisy_times
        .iter()
        .copied()
        .min()
        .expect("timestamps should exist");
    let noisy_max = noisy_times
        .iter()
        .copied()
        .max()
        .expect("timestamps should exist");
    let noisy_span = noisy_max - noisy_min;

    assert_ne!(noisy_times, original_times, "timestamps should change");
    assert!(
        noisy_span < original_span / 2,
        "spike clustering should reduce timestamp spread"
    );
}

/// Verify a multi-injector pipeline combines numeric, text, and row-level effects.
#[test]
fn multi_injector_pipeline_combines_effects() {
    let source = combined_items_batch();
    let original_scores = float_values(&source, "score");
    let original_names = string_values(&source, "name");
    let mut pipeline = Pipeline::new(PerturbConfig::default().with_seed(41));
    pipeline.add_with_overrides(
        Box::new(GaussianNoise::absolute(2.0)),
        PerturbOverrides {
            probability: Some(1.0),
            columns: Some(knit::noise::ColumnFilter::ByName(vec!["score".to_string()])),
            scope_expr: None,
        },
    );
    pipeline.add_with_overrides(
        Box::new(TypoInjector::new()),
        PerturbOverrides {
            probability: Some(1.0),
            columns: Some(knit::noise::ColumnFilter::ByName(vec!["name".to_string()])),
            scope_expr: None,
        },
    );
    pipeline.add_with_overrides(
        Box::new(DuplicateInjector::new()),
        PerturbOverrides {
            probability: Some(1.0),
            columns: None,
            scope_expr: None,
        },
    );

    let noisy = pipeline
        .run(source.clone())
        .expect("noise pipeline should succeed");
    let noisy_scores = float_values(&noisy, "score");
    let noisy_names = string_values(&noisy, "name");

    assert_eq!(noisy.num_rows(), source.num_rows() * 2);
    let score_changed = noisy_scores
        .iter()
        .take(source.num_rows())
        .zip(&original_scores)
        .filter(|(noisy, original)| (**noisy - **original).abs() > 1e-9)
        .count();
    assert!(
        score_changed >= original_scores.len() * 3 / 4,
        "with probability 1.0, at least 75% of scores should change in pipeline, \
         but only {score_changed}/{} changed",
        original_scores.len()
    );
    let name_changed = noisy_names
        .iter()
        .take(source.num_rows())
        .zip(&original_names)
        .filter(|(noisy, original)| noisy != original)
        .count();
    assert!(
        name_changed >= original_names.len() * 3 / 4,
        "with probability 1.0, at least 75% of names should change in pipeline, \
         but only {name_changed}/{} changed",
        original_names.len()
    );
}
