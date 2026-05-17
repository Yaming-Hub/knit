//! Shared test utilities for integration tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use arrow::record_batch::RecordBatch;
use knit::blueprint::{parse_toml, parse_toml_file, validate};
use knit::r#gen::{ActorPool, GenerationEngine};
use knit::plan::compile;

/// Resolve the path to the workspace `examples/` directory.
#[allow(dead_code)] // Shared helper for selectively running integration fixtures.
pub fn examples_dir() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest.join("examples")
}

/// Collect all `.knit.toml` files from the examples directory (recursive).
///
/// Fragment files (those without a `[model]` section) are excluded since they
/// cannot be parsed or validated standalone.
#[allow(dead_code)] // Shared helper for example-driven integration tests.
pub fn example_schemas() -> Vec<PathBuf> {
    let dir = examples_dir();
    let mut paths: Vec<PathBuf> = Vec::new();
    collect_knit_toml_files(&dir, &mut paths);
    // Exclude fragment files that lack a [model] table
    paths.retain(|p| {
        let content = std::fs::read_to_string(p)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", p.display()));
        let table: toml::Table = toml::from_str(&content).unwrap_or_default();
        table.contains_key("model")
    });
    paths.sort();
    paths
}

/// Recursively collect `.knit.toml` files from a directory.
///
/// Uses `symlink_metadata` to avoid following symlinks into potential loops.
fn collect_knit_toml_files(dir: &Path, paths: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
    {
        let path = entry
            .unwrap_or_else(|e| panic!("cannot read entry in {}: {e}", dir.display()))
            .path();
        let is_dir = path
            .symlink_metadata()
            .map(|m| m.is_dir())
            .unwrap_or(false);
        if is_dir {
            collect_knit_toml_files(&path, paths);
        } else if path.extension().and_then(|s| s.to_str()) == Some("toml")
            && path
                .file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.ends_with(".knit.toml"))
        {
            paths.push(path);
        }
    }
}

/// Parse, validate, compile, and generate all batches for a TOML schema string.
#[allow(dead_code)] // Shared helper for string-based integration tests.
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
#[allow(dead_code)] // Shared helper for file-based integration tests.
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
#[allow(dead_code)] // Shared helper for integration assertions.
pub fn total_rows(batches: &[RecordBatch]) -> usize {
    batches.iter().map(|b| b.num_rows()).sum()
}
