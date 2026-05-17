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
        path.is_file()
            && path
                .file_name()
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn is_structured_model_detects_directory_and_manifest_file() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let manifest = root.join("knit.toml");
        fs::write(&manifest, "[model]\nname = \"demo\"\n").unwrap();

        assert!(is_structured_model(root));
        assert!(is_structured_model(&manifest));
    }

    #[test]
    fn is_structured_model_rejects_non_manifest_paths() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let other = root.join("other.toml");
        fs::write(&other, "[model]\nname = \"demo\"\n").unwrap();

        assert!(!is_structured_model(root));
        assert!(!is_structured_model(&other));
    }

    #[test]
    fn is_structured_model_rejects_nonexistent_knit_toml() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("knit.toml");
        assert!(!is_structured_model(&missing));
    }

    #[test]
    fn model_root_returns_expected_path() {
        let dir = TempDir::new().unwrap();
        let root = dir.path();
        let manifest = root.join("knit.toml");
        fs::write(&manifest, "[model]\nname = \"demo\"\n").unwrap();

        assert_eq!(model_root(root), root);
        assert_eq!(model_root(&manifest), root);
    }
}
