//! `knit init` — scaffold a new blueprint file.
//!
//! Creates a minimal JSON blueprint that the user can populate with their
//! own data model. Optionally copies from a template path (file or directory)
//! provided by the user.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use colored::Colorize;

/// Run the `knit init` command.
///
/// If `--template` is a file path, copies it (and sibling files like
/// dictionaries) to the output location. If no template, generates a
/// minimal scaffold schema.
pub fn run(output_path: &str, template: Option<&str>) -> Result<()> {
    let dest = Path::new(output_path);

    if dest.exists() {
        bail!(
            "{} already exists. Remove it first or choose a different path.",
            output_path
        );
    }

    // Resolve template before touching filesystem
    let template_source = match template {
        Some(path) => {
            let src = Path::new(path);
            if !src.exists() {
                bail!("template path does not exist: {}", path);
            }
            Some(src.to_path_buf())
        }
        None => None,
    };

    // Create parent directories if needed
    if let Some(parent) = dest.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }

    let mut sidecar_count = 0;

    match template_source {
        Some(src) if src.is_dir() => {
            // Template is a directory — copy all files from it
            let dest_dir = dest.parent().unwrap_or(Path::new("."));
            let mut found_schema = false;
            for entry in fs::read_dir(&src)
                .with_context(|| format!("failed to read template directory: {}", src.display()))?
            {
                let entry = entry?;
                let file_name = entry.file_name();
                let file_name_str = file_name.to_string_lossy();

                if entry.file_type()?.is_file() {
                    let dest_file =
                        if (file_name_str.ends_with(".knit.json")
                            || file_name_str.ends_with(".knit.toml"))
                            && !found_schema
                        {
                            // First blueprint file becomes the output schema
                            found_schema = true;
                            dest.to_path_buf()
                        } else {
                            sidecar_count += 1;
                            dest_dir.join(&file_name)
                        };
                    fs::copy(entry.path(), &dest_file)
                        .with_context(|| format!("failed to copy {}", entry.path().display()))?;
                }
            }
            if !found_schema {
                bail!(
                    "template directory '{}' contains no .knit.json or .knit.toml blueprint file",
                    src.display()
                );
            }
        }
        Some(src) => {
            // Template is a single file — copy it as the schema
            fs::copy(&src, dest)
                .with_context(|| format!("failed to copy template from {}", src.display()))?;

            // Also copy sibling files (dictionaries, etc.) from same directory
            if let Some(src_dir) = src.parent() {
                let src_name = src.file_name().ok_or_else(|| {
                    anyhow::anyhow!("template path '{}' has no file name", src.display())
                })?;
                let dest_dir = dest.parent().unwrap_or(Path::new("."));
                for entry in fs::read_dir(src_dir).into_iter().flatten().flatten() {
                    let name = entry.file_name();
                    if name == src_name {
                        continue; // skip the schema itself
                    }
                    if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                        let ext = Path::new(&name)
                            .extension()
                            .and_then(|e| e.to_str())
                            .unwrap_or("");
                        // Copy known sidecar extensions
                        if matches!(ext, "txt" | "csv" | "json" | "dict") {
                            let dest_file = dest_dir.join(&name);
                            fs::copy(entry.path(), &dest_file).ok();
                            sidecar_count += 1;
                        }
                    }
                }
            }
        }
        None => {
            // No template — generate scaffold
            let schema = generate_scaffold();
            fs::write(dest, &schema)?;
        }
    }

    println!(
        "{} Created {}{}",
        "✓".green().bold(),
        output_path.cyan(),
        template
            .map(|t| format!(" (from template: {})", t))
            .unwrap_or_default()
    );
    if sidecar_count > 0 {
        println!("  {} copied {} sidecar file(s)", "+".green(), sidecar_count);
    }
    println!();
    println!("  Edit the schema to define your data model, then run:");
    println!("    {} to verify syntax", "knit validate".yellow());
    println!("    {} to preview the execution plan", "knit plan".yellow());
    println!("    {} to generate data", "knit generate".yellow());

    Ok(())
}

/// Generate a minimal, well-documented scaffold schema.
fn generate_scaffold() -> String {
    r#"{
  "blueprint_version": "1.0",
  "model": {
    "name": "my_dataset",
    "description": "Describe your data model here",
    "seed": 42
  },
  "_generator_types": [
    "sequence", "uuid", "pattern", "distribution", "one_of",
    "lookup", "relative", "business_hours", "derived",
    "conditional", "composite", "constant", "unique", "faker"
  ],
  "entities": [
    {
      "name": "example",
      "count": 1000,
      "fields": [
        {
          "name": "id",
          "data_type": "int",
          "primary_key": true,
          "generator": {
            "type": "sequence",
            "start": 1
          }
        },
        {
          "name": "name",
          "data_type": "string",
          "generator": {
            "type": "pattern",
            "pattern": "item-[A-Z]{3}-[0-9]{4}"
          }
        },
        {
          "name": "value",
          "data_type": "float",
          "generator": {
            "type": "distribution",
            "kind": "normal",
            "params": {
              "mean": 100.0,
              "std_dev": 25.0
            }
          }
        },
        {
          "name": "created_at",
          "data_type": "datetime"
        }
      ]
    }
  ]
}
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_is_valid_json() {
        let schema = generate_scaffold();
        let _: serde_json::Value =
            serde_json::from_str(&schema).expect("scaffold should be valid JSON");
    }

    #[test]
    fn scaffold_contains_required_fields() {
        let schema = generate_scaffold();
        assert!(schema.contains("blueprint_version"));
        assert!(schema.contains("\"model\""));
        assert!(schema.contains("\"entities\""));
        assert!(schema.contains("\"fields\""));
    }

    #[test]
    fn run_creates_scaffold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.knit.json");
        let path_str = path.to_str().unwrap();
        run(path_str, None).unwrap();
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("blueprint_version"));
    }

    #[test]
    fn run_with_file_template() {
        let dir = tempfile::tempdir().unwrap();
        // Create a fake template file
        let template_dir = tempfile::tempdir().unwrap();
        let template_file = template_dir.path().join("my.knit.toml");
        fs::write(
            &template_file,
            "blueprint_version = \"1.0\"\n[model]\nname = \"test\"",
        )
        .unwrap();

        let dest = dir.path().join("output.knit.toml");
        run(
            dest.to_str().unwrap(),
            Some(template_file.to_str().unwrap()),
        )
        .unwrap();
        assert!(dest.exists());
        let content = fs::read_to_string(&dest).unwrap();
        assert!(content.contains("name = \"test\""));
    }

    #[test]
    fn run_with_directory_template() {
        let dir = tempfile::tempdir().unwrap();
        // Create a template directory with schema + sidecar
        let template_dir = tempfile::tempdir().unwrap();
        fs::write(
            template_dir.path().join("schema.knit.toml"),
            "blueprint_version = \"1.0\"\n[model]\nname = \"from_dir\"",
        )
        .unwrap();
        fs::write(template_dir.path().join("words.dict.txt"), "hello\nworld").unwrap();

        let dest = dir.path().join("out.knit.toml");
        run(
            dest.to_str().unwrap(),
            Some(template_dir.path().to_str().unwrap()),
        )
        .unwrap();
        assert!(dest.exists());
        let content = fs::read_to_string(&dest).unwrap();
        assert!(content.contains("from_dir"));
        // Sidecar should be copied alongside
        assert!(dir.path().join("words.dict.txt").exists());
    }

    #[test]
    fn run_template_path_not_found() {
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("out.knit.toml");
        let result = run(dest.to_str().unwrap(), Some("/nonexistent/path.knit.toml"));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("does not exist"));
    }

    #[test]
    fn run_fails_if_exists() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("existing.toml");
        fs::write(&path, "exists").unwrap();
        let result = run(path.to_str().unwrap(), None);
        assert!(result.is_err());
    }

    #[test]
    fn run_file_template_copies_sidecars() {
        let template_dir = tempfile::tempdir().unwrap();
        let template_file = template_dir.path().join("app.knit.toml");
        fs::write(&template_file, "blueprint_version = \"1.0\"").unwrap();
        fs::write(template_dir.path().join("names.dict.txt"), "alice\nbob").unwrap();
        fs::write(template_dir.path().join("data.csv"), "a,b\n1,2").unwrap();
        // Non-sidecar files should NOT be copied
        fs::write(template_dir.path().join("readme.md"), "# docs").unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let dest = dest_dir.path().join("schema.knit.toml");
        run(
            dest.to_str().unwrap(),
            Some(template_file.to_str().unwrap()),
        )
        .unwrap();

        assert!(dest_dir.path().join("names.dict.txt").exists());
        assert!(dest_dir.path().join("data.csv").exists());
        assert!(!dest_dir.path().join("readme.md").exists());
    }

    #[test]
    fn run_dir_template_no_schema_fails() {
        let template_dir = tempfile::tempdir().unwrap();
        fs::write(template_dir.path().join("words.dict.txt"), "hello").unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let dest = dest_dir.path().join("out.knit.toml");
        let result = run(
            dest.to_str().unwrap(),
            Some(template_dir.path().to_str().unwrap()),
        );
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("no .knit.json or .knit.toml"));
    }
}
