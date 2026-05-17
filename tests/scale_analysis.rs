//! Integration tests for scale analysis, planning, and model rewriting.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::record_batch::RecordBatch;
use knit::blueprint::{parse_toml, validate};
use knit::core::types::{CountSpec, DataModel};
use knit::r#gen::{ActorPool, GenerationEngine};
use knit::plan::compile;
use knit::scale::analyze::analyze_or_from_annotations;
use knit::scale::{ScaleTargets, compute_plan, rewrite};

mod common;
use common::total_rows;

/// A simple actor-driven schema used to exercise scaling behavior.
const SCALE_SCHEMA: &str = r#"
blueprint_version = "1.0"

[model]
name = "scale_analysis"
seed = 123

[[entities]]
name = "user"
actor = true
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
name = "email"
data_type = "string"
[entities.fields.generator]
type = "pattern"
pattern = "user###@example.com"

[[entities]]
name = "order"
count = 500

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "user_id"
data_type = "int"

[[entities.fields]]
name = "amount"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 10.0
max = 100.0

[[relationships]]
name = "order_user"
from = "order"
to = "user"
kind = "many_to_one"
foreign_key = "user_id"
"#;

fn parse_and_validate_model(toml: &str) -> DataModel {
    let model = parse_toml(toml).expect("schema should parse");
    let errors = validate(&model);
    assert!(errors.is_empty(), "validation errors: {errors:?}");
    model
}

fn entity_count(model: &DataModel, entity_name: &str) -> u64 {
    let entity = model
        .entities
        .iter()
        .find(|entity| entity.name == entity_name)
        .unwrap_or_else(|| panic!("entity '{entity_name}' should exist"));

    match entity.count {
        CountSpec::Fixed(count) => count,
        ref other => panic!("expected fixed count for '{entity_name}', got {other:?}"),
    }
}

fn generate_from_model(model: &DataModel) -> HashMap<String, Vec<RecordBatch>> {
    let plan = compile(model).expect("scaled model should compile");
    let mut batches: HashMap<String, Vec<RecordBatch>> = HashMap::new();
    let mut engine = GenerationEngine::new();

    if !plan.actor_pool.pools.is_empty() {
        let actor_pool = ActorPool::from_plan(&plan.actor_pool, model.seed);
        engine = engine.with_actor_pool(Arc::new(actor_pool));
        engine.build_graphs(&plan);
    }

    engine
        .execute(&plan, |entity, batch| {
            batches.entry(entity.to_string()).or_default().push(batch);
            Ok(())
        })
        .expect("generation should succeed");

    batches
}

#[test]
fn test_actor_scaling() {
    let mut model = parse_and_validate_model(SCALE_SCHEMA);
    let (analysis, from_annotations) = analyze_or_from_annotations(&model);
    assert!(
        !from_annotations,
        "test schema should use heuristic analysis"
    );

    let actor = analysis
        .actor
        .as_ref()
        .expect("actor dimension should be detected");
    assert_eq!(actor.entity_name, "user");
    assert_eq!(actor.current_count, 100);
    assert_eq!(actor.dependents, vec![("order".to_string(), 5.0)]);

    let plan = compute_plan(
        &analysis,
        &ScaleTargets {
            actors: Some(200),
            ..ScaleTargets::default()
        },
    )
    .expect("actor scaling plan should compute");

    assert_eq!(plan.entity_overrides.get("user"), Some(&200));
    assert_eq!(plan.entity_overrides.get("order"), Some(&1000));

    rewrite(&mut model, &plan);
    assert_eq!(entity_count(&model, "user"), 200);
    assert_eq!(entity_count(&model, "order"), 1000);
}

#[test]
fn test_uniform_count_scaling() {
    let model = parse_and_validate_model(SCALE_SCHEMA);
    let (analysis, _) = analyze_or_from_annotations(&model);

    let plan = compute_plan(
        &analysis,
        &ScaleTargets {
            count: Some(3.0),
            ..ScaleTargets::default()
        },
    )
    .expect("uniform count scaling should compute");

    assert_eq!(plan.entity_overrides.get("user"), Some(&300));
    assert_eq!(plan.entity_overrides.get("order"), Some(&1500));
}

#[test]
fn test_density_scaling() {
    let mut model = parse_and_validate_model(SCALE_SCHEMA);
    let (analysis, _) = analyze_or_from_annotations(&model);

    let plan = compute_plan(
        &analysis,
        &ScaleTargets {
            density: vec![("order".to_string(), 1.5)],
            ..ScaleTargets::default()
        },
    )
    .expect("density scaling should compute for child entity");

    assert_eq!(plan.entity_overrides.get("order"), Some(&750));
    assert!(
        !plan.entity_overrides.contains_key("user"),
        "density scaling should leave the actor entity unchanged"
    );

    rewrite(&mut model, &plan);
    assert_eq!(entity_count(&model, "user"), 100);
    assert_eq!(entity_count(&model, "order"), 750);

    let err = compute_plan(
        &analysis,
        &ScaleTargets {
            density: vec![("user".to_string(), 1.5)],
            ..ScaleTargets::default()
        },
    )
    .expect_err("density scaling should reject the actor entity");
    let message = err.to_string();
    assert!(
        message.contains("actor entity"),
        "unexpected error: {message}"
    );
}

#[test]
fn test_scaled_model_generates_correctly() {
    let mut model = parse_and_validate_model(SCALE_SCHEMA);
    let (analysis, _) = analyze_or_from_annotations(&model);
    let plan = compute_plan(
        &analysis,
        &ScaleTargets {
            actors: Some(200),
            ..ScaleTargets::default()
        },
    )
    .expect("actor scaling plan should compute");

    rewrite(&mut model, &plan);

    let errors = validate(&model);
    assert!(
        errors.is_empty(),
        "scaled model should validate: {errors:?}"
    );

    let batches = generate_from_model(&model);
    assert_eq!(total_rows(&batches["user"]), 200);
    assert_eq!(total_rows(&batches["order"]), 1000);
}
