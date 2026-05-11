//! Shared test utilities for integration tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use arrow::record_batch::RecordBatch;
use knit::gen::{ActorPool, GenerationEngine};
use knit::plan::compile;
use knit::blueprint::{parse_toml, parse_toml_file, validate};

/// Resolve the path to the workspace `examples/` directory.
pub fn examples_dir() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.join("examples")
}

/// Collect all `.knit.toml` files from the examples directory.
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
                    .is_some_and(|n| n.ends_with(".knit.toml"))
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
pub fn generate_from_toml(toml_input: &str) -> HashMap<String, Vec<RecordBatch>> {
    let model = parse_toml(toml_input).expect("parse failed");
    let errors = validate(&model);
    assert!(errors.is_empty(), "validation errors: {errors:?}");
    let plan = compile(&model).expect("compile failed");

    let mut batches: HashMap<String, Vec<RecordBatch>> = HashMap::new();
    let mut engine = GenerationEngine::new();

    if !plan.actor_pool.pools.is_empty() {
        let actor_pool = ActorPool::from_plan(&plan.actor_pool, model.seed);
        engine = engine.with_actor_pool(std::sync::Arc::new(actor_pool));
        engine.build_graphs(&plan);
    }

    engine
        .execute(&plan, |entity, batch| {
            batches.entry(entity.to_string()).or_default().push(batch);
            Ok(())
        })
        .expect("generation failed");
    batches
}

/// Parse, validate, compile, and generate all batches for a `.knit.toml` file.
pub fn generate_from_file(path: &Path) -> HashMap<String, Vec<RecordBatch>> {
    let model = parse_toml_file(path).expect("parse failed");
    let errors = validate(&model);
    assert!(errors.is_empty(), "validation errors: {errors:?}");
    let plan = compile(&model).expect("compile failed");

    let mut batches: HashMap<String, Vec<RecordBatch>> = HashMap::new();
    let mut engine = GenerationEngine::new();

    if !plan.actor_pool.pools.is_empty() {
        let actor_pool = ActorPool::from_plan(&plan.actor_pool, model.seed);
        engine = engine.with_actor_pool(std::sync::Arc::new(actor_pool));
        engine.build_graphs(&plan);
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
