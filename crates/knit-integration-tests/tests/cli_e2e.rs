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

// ── Generators ──────────────────────────────────────────────────────

#[test]
fn generators_lists_all_types() {
    knit()
        .arg("generators")
        .assert()
        .success()
        .stdout(predicate::str::contains("distribution"))
        .stdout(predicate::str::contains("faker"))
        .stdout(predicate::str::contains("sequence"))
        .stdout(predicate::str::contains("dictionary"))
        .stdout(predicate::str::contains("business_hours"));
}

#[test]
fn generators_json_output() {
    let output = knit()
        .args(["--json", "generators"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: serde_json::Value = serde_json::from_slice(&output).unwrap();
    let generators = json["generators"].as_array().unwrap();
    assert!(generators.len() >= 15, "expected at least 15 generators");
    let distributions = json["distributions"].as_array().unwrap();
    assert!(distributions.len() >= 10, "expected at least 10 distributions");
    // Check structure of first generator
    let first = &generators[0];
    assert!(first["name"].is_string());
    assert!(first["description"].is_string());
    assert!(first["parameters"].is_string());
    assert!(first["example"].is_string());
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

#[test]
fn validate_missing_dictionary_file() {
    let dir = TempDir::new().unwrap();
    let schema = dir.path().join("dict_schema.weave.toml");
    fs::write(
        &schema,
        r#"schema_version = "1.0"

[model]
name = "dict_test"
seed = 42

[[entities]]
name = "items"
count = 10

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "dictionary"
file = "missing.dict.txt"
expansion = "sample"
"#,
    )
    .unwrap();

    knit()
        .args(["validate", schema.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("dictionary file"))
        .stderr(predicate::str::contains("not found"));
}

#[test]
fn validate_existing_dictionary_file_passes() {
    let dir = TempDir::new().unwrap();
    let dict_file = dir.path().join("words.dict.txt");
    fs::write(&dict_file, "apple\nbanana\ncherry\n").unwrap();

    let schema = dir.path().join("dict_schema.weave.toml");
    fs::write(
        &schema,
        r#"schema_version = "1.0"

[model]
name = "dict_test"
seed = 42

[[entities]]
name = "items"
count = 10

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "fruit"
data_type = "string"
[entities.fields.generator]
type = "dictionary"
file = "words.dict.txt"
expansion = "sample"
"#,
    )
    .unwrap();

    knit()
        .args(["validate", schema.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("valid"));
}

#[test]
fn validate_dictionary_absolute_path_rejected() {
    let dir = TempDir::new().unwrap();
    let schema = dir.path().join("abs.weave.toml");
    // Use a platform-appropriate absolute path
    let abs_path = if cfg!(windows) {
        "C:\\\\Windows\\\\System32\\\\drivers\\\\etc\\\\hosts"
    } else {
        "/etc/passwd"
    };
    fs::write(
        &schema,
        format!(
            r#"schema_version = "1.0"

[model]
name = "abs_test"
seed = 42

[[entities]]
name = "items"
count = 10

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "dictionary"
file = "{}"
expansion = "sample"
"#,
            abs_path
        ),
    )
    .unwrap();

    knit()
        .args(["validate", schema.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must be relative"));
}

#[test]
fn validate_dictionary_path_traversal_rejected() {
    let dir = TempDir::new().unwrap();
    let schema = dir.path().join("traversal.weave.toml");
    fs::write(
        &schema,
        r#"schema_version = "1.0"

[model]
name = "traversal_test"
seed = 42

[[entities]]
name = "items"
count = 10

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "dictionary"
file = "../../../etc/shadow"
expansion = "sample"
"#,
    )
    .unwrap();

    knit()
        .args(["validate", schema.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("must not contain '..'"));
}

#[test]
fn validate_dictionary_empty_content_rejected() {
    let dir = TempDir::new().unwrap();
    let dict_file = dir.path().join("empty.dict.txt");
    fs::write(&dict_file, "   \n  \n\n").unwrap();

    let schema = dir.path().join("empty_dict.weave.toml");
    fs::write(
        &schema,
        r#"schema_version = "1.0"

[model]
name = "empty_test"
seed = 42

[[entities]]
name = "items"
count = 10

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "dictionary"
file = "empty.dict.txt"
expansion = "sample"
"#,
    )
    .unwrap();

    knit()
        .args(["validate", schema.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("no usable entries"));
}

#[test]
fn validate_dictionary_json_mode_reports_file_errors() {
    let dir = TempDir::new().unwrap();
    let schema = dir.path().join("dict_json.weave.toml");
    fs::write(
        &schema,
        r#"schema_version = "1.0"

[model]
name = "json_test"
seed = 42

[[entities]]
name = "items"
count = 10

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "dictionary"
file = "missing.dict.txt"
expansion = "sample"
"#,
    )
    .unwrap();

    let output = knit()
        .args(["validate", schema.to_str().unwrap(), "--json"])
        .assert()
        .failure()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(parsed["valid"], false);
    let errors = parsed["errors"].as_array().unwrap();
    assert!(!errors.is_empty());
    assert_eq!(errors[0]["kind"], "file");
    assert!(errors[0]["message"].as_str().unwrap().contains("not found"));
}

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

#[test]
fn validate_fk_target_no_pk() {
    let dir = TempDir::new().unwrap();
    let schema = dir.path().join("test.weave.toml");
    fs::write(
        &schema,
        r#"
name = "test"
seed = 42

[[entities]]
name = "user"
count = 10
[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true
[[entities.fields]]
name = "email"
data_type = "string"

[[entities]]
name = "order"
count = 20
[[entities.fields]]
name = "amount"
data_type = "float"

[[relationships]]
name = "user_order"
from = "user"
to = "order"
kind = "one_to_many"
foreign_key = "id"
"#,
    )
    .unwrap();
    knit()
        .args(["validate", schema.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("has no primary key"));
}

#[test]
fn validate_fk_type_mismatch() {
    let dir = TempDir::new().unwrap();
    let schema = dir.path().join("test.weave.toml");
    fs::write(
        &schema,
        r#"
name = "test"
seed = 42

[[entities]]
name = "user"
count = 10
[[entities.fields]]
name = "id"
data_type = "uuid"
primary_key = true

[[entities]]
name = "order"
count = 20
[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[[entities.fields]]
name = "user_id"
data_type = "int"

[[relationships]]
name = "order_user"
from = "order"
to = "user"
kind = "many_to_one"
foreign_key = "user_id"
"#,
    )
    .unwrap();
    knit()
        .args(["validate", schema.to_str().unwrap()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("does not match target entity"));
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

#[test]
fn generate_entity_filter_single() {
    // --entity users should only produce users.csv
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            "examples/ecommerce.weave.toml",
            "-o",
            dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--entity",
            "users",
            "--quiet",
        ])
        .assert()
        .success();

    let files: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(files, vec!["users.csv"], "only users.csv should be generated");
}

#[test]
fn generate_entity_filter_multiple() {
    // --entity users --entity products should produce both
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            "examples/ecommerce.weave.toml",
            "-o",
            dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--entity",
            "users",
            "--entity",
            "products",
            "--quiet",
        ])
        .assert()
        .success();

    let mut files: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    files.sort();
    assert_eq!(files, vec!["products.csv", "users.csv"]);
}

#[test]
fn generate_entity_filter_unknown_entity_fails() {
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            "examples/ecommerce.weave.toml",
            "-o",
            dir.path().to_str().unwrap(),
            "--entity",
            "nonexistent",
        ])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown entity 'nonexistent'"))
        .stderr(predicate::str::contains("available:"));
}

#[test]
fn generate_entity_filter_fk_integrity() {
    // Generating orders (which has FK to users) should still work because
    // the engine generates users data for key stores even if not outputting it
    let dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            "examples/ecommerce.weave.toml",
            "-o",
            dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--entity",
            "orders",
            "--quiet",
        ])
        .assert()
        .success();

    let files: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .collect();
    assert_eq!(files, vec!["orders.csv"], "only orders.csv should be generated");

    // Verify the orders file has data (FK columns should be populated)
    let content = fs::read_to_string(dir.path().join("orders.csv")).unwrap();
    let row_count = content.lines().count() - 1; // minus header
    assert!(row_count > 0, "orders should have rows with FK values populated");
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
fn learn_sample_zero_rejected() {
    let tmp = TempDir::new().unwrap();
    let csv_path = tmp.path().join("data.csv");
    fs::write(&csv_path, "id,value\n1,10\n").unwrap();
    let out = tmp.path().join("out.weave.toml");

    knit()
        .args([
            "learn",
            csv_path.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
            "--sample",
            "0",
        ])
        .assert()
        .failure()
        .stderr(predicates::str::contains("--sample must be at least 1"));
}

#[test]
fn learn_sample_limits_rows() {
    let tmp = TempDir::new().unwrap();

    // Create a CSV with 100 rows
    let csv_path = tmp.path().join("big.csv");
    let mut csv_content = String::from("id,value\n");
    for i in 1..=100 {
        csv_content.push_str(&format!("{},{}\n", i, i * 10));
    }
    fs::write(&csv_path, &csv_content).unwrap();

    let learned = tmp.path().join("learned.weave.toml");

    // Learn with --sample 10 (only first 10 rows)
    knit()
        .args([
            "learn",
            csv_path.to_str().unwrap(),
            "-o",
            learned.to_str().unwrap(),
            "--sample",
            "10",
            "--quiet",
        ])
        .assert()
        .success();

    assert!(learned.exists(), "learned schema should be created");
    let content = fs::read_to_string(&learned).unwrap();
    // Should still produce a valid schema with entity "big"
    assert!(content.contains("big"), "entity name should come from file stem");
    assert!(content.contains("[[entities.fields]]"));
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

// ── Learn + Dictionary Extraction Round-Trip ────────────────────────

#[test]
fn learn_dictionary_extraction_round_trip() {
    // Create CSV source data with a high-cardinality string column (>50 unique values)
    // to trigger dictionary extraction during learn.
    let source_dir = TempDir::new().unwrap();
    let csv_path = source_dir.path().join("products.csv");

    let mut csv_content = String::from("id,product_name,price,category\n");
    for i in 1..=200 {
        // 200 unique product names → triggers dictionary extraction (threshold >50)
        csv_content.push_str(&format!(
            "{},Product-{}-Widget,{:.2},{}\n",
            i,
            i,
            10.0 + (i as f64) * 0.5,
            ["Electronics", "Clothing", "Food", "Books"][(i - 1) % 4]
        ));
    }
    fs::write(&csv_path, &csv_content).unwrap();

    // Learn from the CSV (should extract dictionary for product_name)
    let learned_schema = source_dir.path().join("learned.weave.toml");
    knit()
        .args([
            "learn",
            source_dir.path().to_str().unwrap(),
            "-o",
            learned_schema.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    assert!(learned_schema.exists(), "learned schema should be created");

    // Verify the specific dictionary file was created
    let expected_dict = source_dir.path().join("products_product_name.dict.txt");
    assert!(
        expected_dict.exists(),
        "expected dictionary file 'products_product_name.dict.txt' to be created"
    );

    // Verify dictionary file has correct content
    let dict_content = fs::read_to_string(&expected_dict).unwrap();
    let dict_lines: Vec<&str> = dict_content.lines().collect();
    assert!(
        dict_lines.len() >= 200,
        "dictionary should have ≥200 entries (one per unique value), got {}",
        dict_lines.len()
    );
    assert!(
        dict_lines.iter().all(|l| l.contains("Widget")),
        "all dictionary entries should contain 'Widget' from source data"
    );

    // Verify the learned schema references the dictionary generator
    let schema_text = fs::read_to_string(&learned_schema).unwrap();
    assert!(
        schema_text.contains(r#"type = "dictionary""#),
        "learned schema should use type = \"dictionary\" generator"
    );
    assert!(
        schema_text.contains("products_product_name.dict.txt"),
        "learned schema should reference the specific dictionary file"
    );

    // Generate from the learned schema (verifies dictionary resolution works)
    let gen_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            learned_schema.to_str().unwrap(),
            "-o",
            gen_dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--quiet",
        ])
        .assert()
        .success();

    // Verify generation produced data
    let gen_csv_path = gen_dir.path().join("products.csv");
    assert!(
        gen_csv_path.exists(),
        "generation should produce products.csv"
    );

    // Parse generated CSV and verify product_name column uses dictionary values
    let gen_csv = fs::read_to_string(&gen_csv_path).unwrap();
    let gen_lines: Vec<&str> = gen_csv.lines().collect();
    assert!(gen_lines.len() > 1, "generated CSV should have header + data rows");

    // Find product_name column index
    let header = gen_lines[0];
    let col_idx = header.split(',')
        .position(|h| h == "product_name")
        .expect("generated CSV should have product_name column");

    // Verify generated product_name values come from dictionary
    let dict_set: std::collections::HashSet<&str> = dict_lines.iter().copied().collect();
    let mut found_dict_value = false;
    for line in &gen_lines[1..] {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() > col_idx && !fields[col_idx].is_empty() {
            assert!(
                dict_set.contains(fields[col_idx]),
                "generated value '{}' should be from the dictionary",
                fields[col_idx]
            );
            found_dict_value = true;
        }
    }
    assert!(found_dict_value, "at least some generated rows should have dictionary values");
}

#[test]
fn learn_dictionary_threshold_boundary() {
    // With exactly 50 unique values, no dictionary should be extracted
    // (threshold is >50). With 51, it should trigger extraction.

    // 50 unique values → no dictionary
    let dir_50 = TempDir::new().unwrap();
    let csv_50 = dir_50.path().join("items.csv");
    let mut content = String::from("id,name\n");
    for i in 1..=50 {
        content.push_str(&format!("{},Item-{}-Thing\n", i, i));
    }
    fs::write(&csv_50, &content).unwrap();

    let schema_50 = dir_50.path().join("learned.weave.toml");
    knit()
        .args([
            "learn",
            dir_50.path().to_str().unwrap(),
            "-o",
            schema_50.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    // Should NOT have a dictionary file
    let dict_files_50: Vec<_> = fs::read_dir(dir_50.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .to_str()
                .unwrap_or("")
                .contains(".dict.txt")
        })
        .collect();
    assert!(
        dict_files_50.is_empty(),
        "50 unique values should NOT trigger dictionary extraction (threshold >50)"
    );

    // 51 unique values → should extract dictionary
    let dir_51 = TempDir::new().unwrap();
    let csv_51 = dir_51.path().join("items.csv");
    let mut content = String::from("id,name\n");
    for i in 1..=51 {
        content.push_str(&format!("{},Item-{}-Thing\n", i, i));
    }
    fs::write(&csv_51, &content).unwrap();

    let schema_51 = dir_51.path().join("learned.weave.toml");
    knit()
        .args([
            "learn",
            dir_51.path().to_str().unwrap(),
            "-o",
            schema_51.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    let dict_files_51: Vec<_> = fs::read_dir(dir_51.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .to_str()
                .unwrap_or("")
                .contains(".dict.txt")
        })
        .collect();
    assert!(
        !dict_files_51.is_empty(),
        "51 unique values SHOULD trigger dictionary extraction"
    );
}

#[test]
fn learn_incremental_generates_valid_schema() {
    // Test that incremental learn (ingest + finalize) produces a schema
    // with dictionary extraction from reservoir samples.
    let source_dir = TempDir::new().unwrap();
    let csv_path = source_dir.path().join("widgets.csv");

    // Use a column name that doesn't match any faker heuristic → falls back to "word"
    // which is extractable. Use 150 unique values to exceed the >50 threshold.
    let mut csv_content = String::from("id,sku_code,revenue\n");
    for i in 1..=150 {
        csv_content.push_str(&format!(
            "{},SKU-{}-PART,{:.2}\n",
            i,
            i,
            1000.0 + (i as f64) * 100.0
        ));
    }
    fs::write(&csv_path, &csv_content).unwrap();

    let state_file = source_dir.path().join("learn.state");
    let output_schema = source_dir.path().join("incremental.weave.toml");

    // Step 1: Ingest data into state
    knit()
        .args([
            "learn",
            source_dir.path().to_str().unwrap(),
            "-o",
            output_schema.to_str().unwrap(),
            "--state",
            state_file.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    assert!(state_file.exists(), "state file should be created");

    // Step 2: Finalize to produce schema (should extract dictionary from reservoir)
    knit()
        .args([
            "learn",
            "-o",
            output_schema.to_str().unwrap(),
            "--state",
            state_file.to_str().unwrap(),
            "--finalize",
            "--quiet",
        ])
        .assert()
        .success();

    assert!(output_schema.exists(), "schema should be produced");

    // Verify dictionary was extracted during finalize
    let schema_text = fs::read_to_string(&output_schema).unwrap();
    assert!(
        schema_text.contains(r#"type = "dictionary""#),
        "incremental finalize should extract dictionary for high-cardinality column.\nSchema:\n{}",
        schema_text
    );

    let dict_path = source_dir.path().join("widgets_sku_code.dict.txt");
    assert!(
        dict_path.exists(),
        "dictionary file should be created during incremental finalize"
    );

    let dict_content = fs::read_to_string(&dict_path).unwrap();
    let dict_lines: Vec<&str> = dict_content.lines().collect();
    assert!(
        dict_lines.len() >= 150,
        "dictionary should have ≥150 entries, got {}",
        dict_lines.len()
    );

    // Generate from the schema (validates dictionary resolution works)
    let gen_dir = TempDir::new().unwrap();
    knit()
        .args([
            "generate",
            output_schema.to_str().unwrap(),
            "-o",
            gen_dir.path().to_str().unwrap(),
            "--format",
            "csv",
            "--quiet",
        ])
        .assert()
        .success();

    let gen_csv_path = gen_dir.path().join("widgets.csv");
    assert!(
        gen_csv_path.exists(),
        "incremental learn → generate should produce widgets.csv"
    );

    let gen_csv = fs::read_to_string(&gen_csv_path).unwrap();
    let row_count = gen_csv.lines().count() - 1;
    assert!(row_count > 0, "should have generated rows");

    // Verify generated values come from dictionary
    let dict_set: std::collections::HashSet<&str> = dict_lines.iter().copied().collect();
    let header = gen_csv.lines().next().unwrap();
    let col_idx = header.split(',')
        .position(|h| h == "sku_code")
        .expect("should have sku_code column");
    let has_dict_value = gen_csv.lines().skip(1).any(|line| {
        let fields: Vec<&str> = line.split(',').collect();
        fields.len() > col_idx && dict_set.contains(fields[col_idx])
    });
    assert!(has_dict_value, "generated data should use dictionary values");
}

#[test]
fn inspect_state_file() {
    // Create a state file via incremental learn, then inspect it.
    let dir = TempDir::new().unwrap();
    let csv_path = dir.path().join("orders.csv");
    fs::write(
        &csv_path,
        "order_id,customer,amount\n1,Alice,100.50\n2,Bob,200.75\n3,Alice,50.00\n4,Carol,300.25\n",
    )
    .unwrap();

    let state_file = dir.path().join("test.state");

    // Ingest
    knit()
        .args([
            "learn",
            dir.path().to_str().unwrap(),
            "--state",
            state_file.to_str().unwrap(),
            "--quiet",
        ])
        .assert()
        .success();

    // Inspect (human output)
    knit()
        .args(["inspect", state_file.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("State Summary"))
        .stdout(predicate::str::contains("orders"))
        .stdout(predicate::str::contains("4 rows"))
        .stdout(predicate::str::contains("3 columns"))
        .stdout(predicate::str::contains("1 chunk(s) processed"));

    // Inspect with --columns
    knit()
        .args(["inspect", state_file.to_str().unwrap(), "--columns"])
        .assert()
        .success()
        .stdout(predicate::str::contains("order_id"))
        .stdout(predicate::str::contains("customer"))
        .stdout(predicate::str::contains("amount"))
        .stdout(predicate::str::contains("distinct"));

    // Inspect with --json
    let json_output = knit()
        .args(["inspect", state_file.to_str().unwrap(), "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let parsed: serde_json::Value = serde_json::from_slice(&json_output).unwrap();
    assert_eq!(parsed["total_rows"], 4);
    assert_eq!(parsed["tables"][0]["name"], "orders");
    assert_eq!(parsed["tables"][0]["columns"], 3);
    assert_eq!(parsed["chunks_processed"], 1);
    assert_eq!(parsed["recent_chunks"].as_array().unwrap().len(), 1);
    assert_eq!(parsed["recent_chunks"][0]["row_count"], 4);

    // Inspect nonexistent file
    knit()
        .args(["inspect", "nonexistent.state"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("not found"));
}
