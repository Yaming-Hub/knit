//! End-to-end CLI subprocess tests.
//!
//! These tests invoke the `knit` binary as a subprocess, verifying that
//! commands produce expected outputs and exit codes.

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json;
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
                "--batch-size",
                "25",
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
fn generate_seed_override_differs_from_schema_default() {
    // The test schema has seed=12345. Using --seed with a different value
    // should produce different output, proving the override works.
    let dir_default = TempDir::new().unwrap();
    let dir_override = TempDir::new().unwrap();

    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            dir_default.path().to_str().unwrap(),
            "--format",
            "csv",
            "--quiet",
        ])
        .assert()
        .success();

    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            dir_override.path().to_str().unwrap(),
            "--seed",
            "99999",
            "--format",
            "csv",
            "--quiet",
        ])
        .assert()
        .success();

    let c1 = fs::read(dir_default.path().join("items.csv")).unwrap();
    let c2 = fs::read(dir_override.path().join("items.csv")).unwrap();
    assert_ne!(c1, c2, "--seed override should produce different output than schema default seed");
}

#[test]
fn generate_parallel_flag_works() {
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            dir.path().to_str().unwrap(),
            "--parallel",
            "2",
            "--format",
            "csv",
            "--quiet",
        ])
        .assert()
        .success();

    let csv = fs::read_to_string(dir.path().join("items.csv")).unwrap();
    let row_count = csv.lines().count() - 1; // subtract header
    assert_eq!(row_count, 100, "should generate 100 rows with --parallel 2");
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
    // Use a schema with noise profiles to test that --no-noise actually skips them
    let tmp = TempDir::new().unwrap();
    let schema_path = tmp.path().join("noisy.weave.toml");
    fs::write(
        &schema_path,
        r#"
schema_version = "1.0"
[model]
name = "noisy_test"
seed = 99

[[entities]]
name = "data"
count = 200

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
min = 0.0
max = 100.0

[[noise]]
name = "inject_nulls"
entity = "data"
null_rate = 1.0
"#,
    )
    .unwrap();

    // Generate WITH noise (nulls injected at 100% rate)
    let noisy_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            schema_path.to_str().unwrap(),
            "-o",
            noisy_dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--quiet",
        ])
        .assert()
        .success();

    // Generate WITHOUT noise (--no-noise flag)
    let clean_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            schema_path.to_str().unwrap(),
            "-o",
            clean_dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--no-noise",
            "--quiet",
        ])
        .assert()
        .success();

    // Clean output should differ from noisy output
    let noisy_csv = fs::read_to_string(noisy_dir.path().join("data.csv")).unwrap();
    let clean_csv = fs::read_to_string(clean_dir.path().join("data.csv")).unwrap();
    assert_ne!(
        noisy_csv, clean_csv,
        "--no-noise should produce different output than noisy generation"
    );
    // Clean output should have no empty score cells (no nulls injected)
    let empty_scores = clean_csv
        .lines()
        .skip(1) // header
        .filter(|line| line.ends_with(',') || line.contains(",,"))
        .count();
    assert_eq!(
        empty_scores, 0,
        "clean output should have no null values, found {empty_scores}"
    );
}

#[test]
fn generate_count_absolute_override() {
    // --count 5 should produce exactly 5 rows per entity
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--count",
            "5",
            "--quiet",
        ])
        .assert()
        .success();

    // cli_test.weave.toml has 1 entity "items" with count=100
    // With --count 5 it should have exactly 5 data rows + 1 header
    let csv_files: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "csv"))
        .collect();
    assert!(!csv_files.is_empty());
    for entry in &csv_files {
        let content = fs::read_to_string(entry.path()).unwrap();
        let line_count = content.lines().count();
        assert_eq!(line_count, 6, "should have 1 header + 5 data rows");
    }
}

#[test]
fn generate_count_multiplier_override() {
    // --count 0.5x should halve the row counts
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--count",
            "0.5x",
            "--quiet",
        ])
        .assert()
        .success();

    // cli_test.weave.toml has count=100, so 0.5x → 50 rows
    let csv_files: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "csv"))
        .collect();
    assert!(!csv_files.is_empty());
    for entry in &csv_files {
        let content = fs::read_to_string(entry.path()).unwrap();
        let line_count = content.lines().count();
        assert_eq!(line_count, 51, "should have 1 header + 50 data rows");
    }
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

    // Parse each line as JSON and verify we get progress + complete events
    let mut has_progress = false;
    let mut has_complete = false;
    for line in stdout.lines() {
        if line.contains("\"progress\"") {
            has_progress = true;
        }
        if line.contains("\"complete\"") {
            has_complete = true;
        }
    }
    assert!(has_progress, "expected at least one progress event in JSON output");
    assert!(has_complete, "expected a complete event in JSON output");
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

#[test]
fn init_output_validates_successfully() {
    let dir = TempDir::new().unwrap();
    let schema = dir.path().join("new.weave.toml");
    knit()
        .args(["init", "-o", schema.to_str().unwrap()])
        .assert()
        .success();

    // The generated schema must pass validation
    knit()
        .args(["validate", schema.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn init_output_generates_data() {
    let dir = TempDir::new().unwrap();
    let schema = dir.path().join("new.weave.toml");
    let out_dir = dir.path().join("data");
    knit()
        .args(["init", "-o", schema.to_str().unwrap()])
        .assert()
        .success();

    // The generated schema must produce output data
    knit()
        .args([
            "generate",
            schema.to_str().unwrap(),
            "-o",
            out_dir.to_str().unwrap(),
            "--format",
            "csv",
        ])
        .assert()
        .success();

    assert!(out_dir.exists(), "output directory should be created");
}

#[test]
fn init_scaffold_references_only_valid_generator_types() {
    let dir = TempDir::new().unwrap();
    let schema = dir.path().join("new.weave.toml");
    knit()
        .args(["init", "-o", schema.to_str().unwrap()])
        .assert()
        .success();

    let content = fs::read_to_string(&schema).unwrap();
    let valid_types = [
        "sequence",
        "uuid",
        "pattern",
        "distribution",
        "one_of",
        "lookup",
        "relative",
        "business_hours",
        "derived",
        "conditional",
        "composite",
        "constant",
        "unique",
        "faker",
    ];
    let invalid_types = ["temporal", "foreign_key", "auto_increment"];
    for invalid in &invalid_types {
        assert!(
            !content.contains(invalid),
            "scaffold should not reference invalid generator type '{invalid}'"
        );
    }
    // Verify the reference list mentions valid types
    for valid in &valid_types {
        assert!(
            content.contains(valid),
            "scaffold should document generator type '{valid}'"
        );
    }
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

#[test]
fn schema_normalize() {
    knit()
        .args(["schema", "normalize", "examples/ecommerce.weave.toml"])
        .assert()
        .success()
        .stdout(predicate::str::contains("entities").or(predicate::str::contains("name")));
}

#[test]
fn schema_diff_identical() {
    // Diffing a schema against itself should produce no differences
    knit()
        .args([
            "schema",
            "diff",
            "examples/ecommerce.weave.toml",
            "examples/ecommerce.weave.toml",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("identical"));
}

#[test]
fn schema_diff_different_schemas() {
    // Diffing two different schemas should produce differences
    knit()
        .args([
            "schema",
            "diff",
            "examples/ecommerce.weave.toml",
            "examples/financial.weave.toml",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("change(s) found"));
}

#[test]
fn schema_diff_learned_vs_original() {
    // Generate → learn → diff learned against original
    let dir = TempDir::new().unwrap();
    let data_dir = dir.path().join("data");
    let learned = dir.path().join("learned.weave.toml");

    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            data_dir.to_str().unwrap(),
            "--format",
            "csv",
        ])
        .assert()
        .success();

    knit()
        .args([
            "learn",
            data_dir.to_str().unwrap(),
            "-o",
            learned.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    // Diff should succeed and report differences (learned schema differs from original)
    knit()
        .args(["schema", "diff", TEST_SCHEMA, learned.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("change(s) found"));
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

// ── Learn ───────────────────────────────────────────────────────────

#[test]
fn learn_from_csv_produces_valid_schema() {
    // Step 1: Generate CSV data from a known schema
    let data_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            data_dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--seed",
            "42",
            "--quiet",
        ])
        .assert()
        .success();

    // Step 2: Learn schema from the generated CSV
    let learned_schema = data_dir.path().join("learned.weave.toml");
    knit()
        .args([
            "learn",
            data_dir.path().to_str().unwrap(),
            "-o",
            learned_schema.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    assert!(learned_schema.exists(), "learned schema should be created");

    // Step 3: Validate the learned schema
    knit()
        .args(["validate", learned_schema.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn learn_from_parquet_produces_valid_schema() {
    // Generate Parquet data
    let data_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            data_dir.path().to_str().unwrap(),
            "--format",
            "parquet",
            "--seed",
            "42",
            "--quiet",
        ])
        .assert()
        .success();

    // Learn from Parquet
    let learned_schema = data_dir.path().join("learned.weave.toml");
    knit()
        .args([
            "learn",
            data_dir.path().to_str().unwrap(),
            "-o",
            learned_schema.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    assert!(learned_schema.exists());

    // Validate
    knit()
        .args(["validate", learned_schema.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn learn_from_jsonl_produces_valid_schema() {
    // Verifies JSONL (newline-delimited JSON) ingestion.
    let data_dir = TempDir::new().unwrap();
    let jsonl_path = data_dir.path().join("items.jsonl");
    fs::write(
        &jsonl_path,
        r#"{"id":1,"value":42.5,"label":"alpha"}
{"id":2,"value":88.1,"label":"beta"}
{"id":3,"value":15.3,"label":"gamma"}
{"id":4,"value":67.9,"label":"alpha"}
{"id":5,"value":23.4,"label":"beta"}
"#,
    )
    .unwrap();

    // Learn from JSONL
    let learned_schema = data_dir.path().join("learned.weave.toml");
    knit()
        .args([
            "learn",
            data_dir.path().to_str().unwrap(),
            "-o",
            learned_schema.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    assert!(learned_schema.exists());

    // Validate
    knit()
        .args(["validate", learned_schema.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn learn_from_json_array_produces_valid_schema() {
    // JSON arrays (as produced by `knit generate --format json`) should work with learn.
    let data_dir = TempDir::new().unwrap();
    let json_path = data_dir.path().join("items.json");
    fs::write(
        &json_path,
        r#"[{"id":1,"value":42.5,"label":"alpha"},{"id":2,"value":88.1,"label":"beta"},{"id":3,"value":15.3,"label":"gamma"},{"id":4,"value":67.9,"label":"alpha"},{"id":5,"value":23.4,"label":"beta"}]"#,
    )
    .unwrap();

    let learned_schema = data_dir.path().join("learned.weave.toml");
    knit()
        .args([
            "learn",
            data_dir.path().to_str().unwrap(),
            "-o",
            learned_schema.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    assert!(learned_schema.exists());

    // Validate the inferred schema
    knit()
        .args(["validate", learned_schema.to_str().unwrap()])
        .assert()
        .success();
}

#[test]
fn learn_round_trip_generates_data() {
    // Full round-trip: generate → learn → generate from learned schema
    let gen1_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            gen1_dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--seed",
            "100",
            "--quiet",
        ])
        .assert()
        .success();

    // Learn
    let learned = gen1_dir.path().join("learned.weave.toml");
    knit()
        .args([
            "learn",
            gen1_dir.path().to_str().unwrap(),
            "-o",
            learned.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    // Generate from the learned schema
    let gen2_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            learned.to_str().unwrap(),
            "-o",
            gen2_dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--quiet",
        ])
        .assert()
        .success();

    // Verify the second generation produced data
    let files: Vec<_> = fs::read_dir(gen2_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "csv"))
        .collect();
    assert!(!files.is_empty(), "round-trip generation should produce CSV files");

    // Verify the generated file has rows
    for entry in &files {
        let content = fs::read_to_string(entry.path()).unwrap();
        let line_count = content.lines().count();
        assert!(line_count > 1, "generated file should have header + data rows");
    }
}

#[test]
fn learn_json_round_trip_generates_data() {
    // Full round-trip using JSON format: generate (JSON array) → learn → generate
    let gen1_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            gen1_dir.path().to_str().unwrap(),
            "--format",
            "json",
            "--seed",
            "200",
            "--quiet",
        ])
        .assert()
        .success();

    // Learn from JSON array files (previously this would fail)
    let learned = gen1_dir.path().join("learned.weave.toml");
    knit()
        .args([
            "learn",
            gen1_dir.path().to_str().unwrap(),
            "-o",
            learned.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    // Generate from the learned schema
    let gen2_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            learned.to_str().unwrap(),
            "-o",
            gen2_dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--quiet",
        ])
        .assert()
        .success();

    let files: Vec<_> = fs::read_dir(gen2_dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "csv"))
        .collect();
    assert!(
        !files.is_empty(),
        "JSON round-trip should produce output files"
    );

    // Verify generated files have actual data rows
    for entry in &files {
        let content = fs::read_to_string(entry.path()).unwrap();
        let line_count = content.lines().count();
        assert!(line_count > 1, "generated file should have header + data rows");
    }
}

#[test]
fn learn_missing_source_fails() {
    knit()
        .args(["learn", "nonexistent_dir", "-o", "out.weave.toml"])
        .assert()
        .failure();
}

#[test]
fn learn_quiet_suppresses_output() {
    let data_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            data_dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--seed",
            "42",
            "--quiet",
        ])
        .assert()
        .success();

    let learned = data_dir.path().join("learned.weave.toml");
    let output = knit()
        .args([
            "learn",
            data_dir.path().to_str().unwrap(),
            "-o",
            learned.to_str().unwrap(),
            "--quiet",
        ])
        .output()
        .expect("failed to run knit learn");

    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Filter out Windows incremental compilation notes
    let meaningful_stderr: String = stderr
        .lines()
        .filter(|l| !l.contains("error finalizing incremental"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        meaningful_stderr.trim().is_empty(),
        "--quiet should suppress all learn output on stderr, got: {}",
        meaningful_stderr
    );
}

#[test]
fn learn_json_mode_outputs_summary() {
    let data_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            TEST_SCHEMA,
            "-o",
            data_dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--seed",
            "42",
            "--quiet",
        ])
        .assert()
        .success();

    let learned = data_dir.path().join("learned.weave.toml");
    let output = knit()
        .args([
            "learn",
            data_dir.path().to_str().unwrap(),
            "-o",
            learned.to_str().unwrap(),
            "--json",
            "--quiet",
        ])
        .output()
        .expect("failed to run knit learn");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout)
        .expect("--json should output valid JSON");
    assert_eq!(json["event"], "complete");
    assert_eq!(json["tables"], 1);
    assert!(json["columns"].as_u64().unwrap() > 0);
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

#[test]
fn generate_param_substitution_in_derived() {
    let tmp = TempDir::new().unwrap();
    let schema_path = tmp.path().join("param_test.weave.toml");
    fs::write(
        &schema_path,
        r#"
schema_version = "1.0"
seed = 1

[[entities]]
name = "items"
count = 5

[[entities.fields]]
name = "id"
data_type = "int"
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "derived"
expr = "${param.prefix}-item-${id}"
depends_on = ["id"]
"#,
    )
    .unwrap();

    let out_dir = tmp.path().join("output");
    fs::create_dir_all(&out_dir).unwrap();

    // Generate with --param prefix=ACME
    knit()
        .args([
            "generate",
            schema_path.to_str().unwrap(),
            "-o",
            out_dir.to_str().unwrap(),
            "--format",
            "csv",
            "--param",
            "prefix=ACME",
            "--quiet",
        ])
        .assert()
        .success();

    // Verify the CSV contains param-substituted values
    let csv_file = out_dir.join("items.csv");
    let content = fs::read_to_string(&csv_file).unwrap();
    assert!(
        content.contains("ACME-item-1"),
        "expected param substitution in derived field, got:\n{content}"
    );
    assert!(content.contains("ACME-item-5"));
}

#[test]
fn generate_param_without_flag_leaves_placeholder() {
    let tmp = TempDir::new().unwrap();
    let schema_path = tmp.path().join("param_test.weave.toml");
    fs::write(
        &schema_path,
        r#"
schema_version = "1.0"
seed = 1

[[entities]]
name = "items"
count = 3

[[entities.fields]]
name = "id"
data_type = "int"
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "derived"
expr = "${param.env}-${id}"
depends_on = ["id"]
"#,
    )
    .unwrap();

    let out_dir = tmp.path().join("output");
    fs::create_dir_all(&out_dir).unwrap();

    // Generate WITHOUT --param env=... → placeholder stays literal
    knit()
        .args([
            "generate",
            schema_path.to_str().unwrap(),
            "-o",
            out_dir.to_str().unwrap(),
            "--format",
            "csv",
            "--quiet",
        ])
        .assert()
        .success();

    let csv_file = out_dir.join("items.csv");
    let content = fs::read_to_string(&csv_file).unwrap();
    assert!(
        content.contains("${param.env}-1"),
        "unresolved param should stay as literal placeholder, got:\n{content}"
    );
}
