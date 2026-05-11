//! Structured model directory format — read and write knit models as directories.
//!
//! The structured format splits a monolithic schema into focused files:
//! - `knit.toml` — root manifest (identity, seed, config)
//! - `layout.toml` — physical output structure (folders, partitions)
//! - `tables/*.toml` — one file per table/entity
//! - `relationships.toml` — foreign keys, associations
//! - `correlations.toml` — cross-field correlations
//! - `shared.toml` — custom types, mixins, personas

pub mod reader;
pub mod writer;

use std::path::Path;

/// Detect whether a path points to a structured model directory.
///
/// Returns `true` if the path is a directory containing `knit.toml`,
/// or if it directly points to a `knit.toml` file.
pub fn is_structured_model(path: &Path) -> bool {
    if path.is_dir() {
        path.join("knit.toml").is_file()
    } else {
        path.file_name()
            .and_then(|n| n.to_str())
            .map(|n| n == "knit.toml")
            .unwrap_or(false)
    }
}

/// Get the model root directory from a path that might point to knit.toml or the directory itself.
pub fn model_root(path: &Path) -> &Path {
    if path.is_file() {
        path.parent().unwrap_or(path)
    } else {
        path
    }
}