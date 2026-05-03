//! End-to-end CLI subprocess tests.
//!
//! These tests invoke the `knit` binary as a subprocess, verifying that
//! commands produce expected outputs and exit codes.

use assert_cmd::Command;
use predicates::prelude::*;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Helper: get a Command for the `knit` binary, with CWD set to workspace root.
fn knit() -> Command {
    let mut cmd = Command::cargo_bin("knit").expect("knit binary not found");
    cmd.current_dir(workspace_root());
    cmd
}

/// Resolve the workspace root (parent of `crates/`).
fn workspace_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent() // crates/
        .and_then(|p| p.parent()) // workspace root
        .expect("could not resolve workspace root")
        .to_path_buf()
}

// ── Version & Help ──────────────────────────────────────────────────

#[test]
fn version_flag() {
    knit()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("knit"));
}

#[test]
fn help_flag() {
    knit()
        .arg("--help")
        .assert()
        .success()
        .stdout(predicate::str::contains("generate"))
        .stdout(predicate::str::contains("validate"))
        .stdout(predicate::str::contains("learn"));
}

// ── Validate ────────────────────────────────────────────────────────

#[test]
fn validate_valid_schema() {
    knit()
        .args(["validate", "examples/ecommerce.weave.toml"])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid"));
}

#[test]
fn validate_nonexistent_file() {
    knit()
        .args(["validate", "does_not_exist.toml"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

#[test]
fn validate_invalid_schema() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join("bad.weave.toml");
    fs::write(&path, "this is not valid toml [[[").unwrap();

    knit()
        .args(["validate", path.to_str().unwrap()])
        .assert()
        .failure();
}

// ── Plan ────────────────────────────────────────────────────────────

#[test]
fn plan_shows_entities() {
    knit()
        .args(["plan", "examples/ecommerce.weave.toml"])
        .assert()
        .success()
        .stdout(predicate::str::contains("users").or(predicate::str::contains("products")));
}

#[test]
fn plan_json_mode() {
    knit()
        .args(["plan", "examples/ecommerce.weave.toml", "--json"])
        .assert()
        .success()
        .stdout(predicate::str::contains("{"));
}

// ── Generate ────────────────────────────────────────────────────────

/// Schema used for generate tests — minimal, fast, and stable.
const TEST_SCHEMA: &str = "examples/cli_test.weave.toml";

#[test]
fn generate_parquet_output() {
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            dir.path().to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    // Should have created parquet files
    let files: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "parquet"))
        .collect();
    assert!(!files.is_empty(), "expected parquet files in output dir");
}

#[test]
fn generate_csv_output() {
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--quiet",
        ])
        .assert()
        .success();

    let files: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "csv"))
        .collect();
    assert!(!files.is_empty(), "expected csv files in output dir");
}

#[test]
fn generate_json_output() {
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            dir.path().to_str().unwrap(),
            "--format",
            "json",
            "--quiet",
        ])
        .assert()
        .success();

    let files: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    assert!(!files.is_empty(), "expected json files in output dir");
}

#[test]
fn generate_seed_determinism() {
    let dir1 = TempDir::new().unwrap();
    let dir2 = TempDir::new().unwrap();

    for dir in [&dir1, &dir2] {
        knit()
            .args([
                "generate",
                TEST_SCHEMA,
                "-o",
                dir.path().to_str().unwrap(),
                "--seed",
                "42",
                "--format",
                "csv",
                "--quiet",
            ])
            .assert()
            .success();
    }

    // Compare file contents — same seed should produce identical output
    let files1 = sorted_file_names(dir1.path());
    let files2 = sorted_file_names(dir2.path());
    assert_eq!(files1, files2, "same files should be created");

    for name in &files1 {
        let c1 = fs::read(dir1.path().join(name)).unwrap();
        let c2 = fs::read(dir2.path().join(name)).unwrap();
        assert_eq!(c1, c2, "file {} should be identical with same seed", name);
    }
}

#[test]
fn generate_dry_run_no_output() {
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            dir.path().to_str().unwrap(),
            "--dry-run",
            "--quiet",
        ])
        .assert()
        .success();

    let files: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(files.is_empty(), "dry-run should not create files");
}

#[test]
fn generate_no_noise_flag() {
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            dir.path().to_str().unwrap(),
            "--no-noise",
            "--quiet",
        ])
        .assert()
        .success();

    // Should still produce output files (noise skipped silently if no profiles)
    let files: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(!files.is_empty(), "should produce output even with --no-noise");
}

#[test]
fn generate_json_progress_events() {
    let dir = TempDir::new().unwrap();
    let output = knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            dir.path().to_str().unwrap(),
            "--json",
            "--quiet",
        ])
        .output()
        .expect("failed to run knit");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    // JSON mode should emit structured events
    assert!(
        stdout.contains("entity") || stdout.contains("complete"),
        "expected JSON progress events, got: {}",
        &stdout[..stdout.len().min(200)]
    );
}

// ── Init ────────────────────────────────────────────────────────────

#[test]
fn init_creates_schema_file() {
    let dir = TempDir::new().unwrap();
    let out_path = dir.path().join("test.weave.toml");
    knit()
        .args(["init", "-o", out_path.to_str().unwrap()])
        .assert()
        .success();

    assert!(out_path.exists(), "init should create schema file");
    let content = fs::read_to_string(&out_path).unwrap();
    assert!(
        content.contains("schema_version"),
        "generated file should contain schema_version"
    );
}

// ── Schema subcommands ──────────────────────────────────────────────

#[test]
fn schema_expand() {
    knit()
        .args(["schema", "expand", "examples/ecommerce.weave.toml"])
        .assert()
        .success()
        .stdout(predicate::str::contains("entities").or(predicate::str::contains("name")));
}

// ── Error cases ─────────────────────────────────────────────────────

#[test]
fn unknown_command_fails() {
    knit()
        .arg("nonexistent")
        .assert()
        .failure();
}

#[test]
fn generate_missing_schema_fails() {
    knit()
        .args(["generate", "missing.toml", "-o", "out"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("error"));
}

// ── Helpers ─────────────────────────────────────────────────────────

fn sorted_file_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
