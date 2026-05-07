//! # knit-integration-tests — End-to-end test suite
//!
//! This crate contains integration tests that exercise the full knit pipeline:
//! schema parsing → validation → planning → generation.
//!
//! It also houses shared test utilities used across multiple test files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use arrow::record_batch::RecordBatch;
use knit_gen::GenerationEngine;
use knit_plan::compile;
use knit_schema::{parse_toml, parse_toml_file, validate};

/// Resolve the path to the workspace `examples/` directory.
pub fn examples_dir() -> PathBuf {
    // The crate root is `crates/knit-integration-tests/`.
    // Walk up two levels to reach the workspace root.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(|p| p.parent())
        .expect("cannot resolve workspace root")
        .join("examples")
}

/// Collect all `.weave.toml` files from the examples directory.
pub fn example_schemas() -> Vec<PathBuf> {
    let dir = examples_dir();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.extension().and_then(|s| s.to_str()) == Some("toml")
                && path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .is_some_and(|n| n.ends_with(".weave.toml"))
            {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    paths.sort();
    paths
}

/// Parse, validate, compile, and generate all batches for a TOML schema string.
///
/// Returns a map from entity name to a `Vec` of [`RecordBatch`]es.
pub fn generate_from_toml(toml_input: &str) -> HashMap<String, Vec<RecordBatch>> {
    let model = parse_toml(toml_input).expect("parse failed");
    let errors = validate(&model);
    assert!(errors.is_empty(), "validation errors: {errors:?}");
    let plan = compile(&model).expect("compile failed");

    let mut batches: HashMap<String, Vec<RecordBatch>> = HashMap::new();
    let mut engine = GenerationEngine::new();

    // Build actor pool if the plan has actor pools defined
    if !plan.actor_pool.pools.is_empty() {
        let actor_pool = knit_gen::ActorPool::from_plan(&plan.actor_pool, model.seed);
        engine = engine.with_actor_pool(std::sync::Arc::new(actor_pool));
    }

    engine
        .execute(&plan, |entity, batch| {
            batches.entry(entity.to_string()).or_default().push(batch);
            Ok(())
        })
        .expect("generation failed");
    batches
}

/// Parse, validate, compile, and generate all batches for a `.weave.toml` file.
///
/// Returns a map from entity name to a `Vec` of [`RecordBatch`]es.
pub fn generate_from_file(path: &Path) -> HashMap<String, Vec<RecordBatch>> {
    let model = parse_toml_file(path).expect("parse failed");
    let errors = validate(&model);
    assert!(errors.is_empty(), "validation errors: {errors:?}");
    let plan = compile(&model).expect("compile failed");

    let mut batches: HashMap<String, Vec<RecordBatch>> = HashMap::new();
    let mut engine = GenerationEngine::new();

    // Build actor pool if the plan has actor pools defined
    if !plan.actor_pool.pools.is_empty() {
        let actor_pool = knit_gen::ActorPool::from_plan(&plan.actor_pool, model.seed);
        engine = engine.with_actor_pool(std::sync::Arc::new(actor_pool));
    }

    engine
        .execute(&plan, |entity, batch| {
            batches.entry(entity.to_string()).or_default().push(batch);
            Ok(())
        })
        .expect("generation failed");
    batches
}

/// Count total rows across all batches for a given entity.
pub fn total_rows(batches: &[RecordBatch]) -> usize {
    batches.iter().map(|b| b.num_rows()).sum()
}
