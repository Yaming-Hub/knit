//! Schema composition via `include` directives.
//!
//! Allows a schema to compose itself from reusable fragment files. Included
//! fragments contribute entities, relationships, noise profiles, personas,
//! actor relationships, and correlations additively.
//!
//! ## Semantics
//!
//! - Paths are resolved relative to the including file's directory.
//! - Includes are processed recursively (included files may include others).
//! - Include-once: each canonical path is loaded at most once (diamond-safe).
//! - Cycles are detected and reported with an inclusion trace.
//! - Fragments must NOT contain `[model]` metadata or `extends` directives.
//! - Name conflicts between included fragments are errors.
//! - The main schema silently overrides included content on name collision.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::core::DataModel;
use crate::blueprint::error::BlueprintError;

/// Resolve all `include` directives for a schema file.
///
/// Recursively loads included fragments, validates them, and merges their
/// content into a single [`DataModel`]. The returned model contains all
/// included content but NOT the main schema's own content — the caller
/// should merge the main schema on top.
///
/// # Arguments
/// * `schema_path` — path to the file containing the include directives
/// * `includes` — list of include paths (already extracted from RawSchema)
/// * `visited` — set of canonical paths already fully expanded (include-once)
/// * `stack` — active inclusion stack for cycle detection
pub fn resolve_includes(
    schema_path: &Path,
    includes: &[String],
    visited: &mut HashSet<PathBuf>,
    stack: &mut Vec<PathBuf>,
) -> Result<DataModel, BlueprintError> {
    let base_dir = schema_path
        .parent()
        .unwrap_or(Path::new("."));

    let mut merged = DataModel::default();

    for include_ref in includes {
        let include_path = Path::new(include_ref);

        // Security: reject absolute paths
        if include_path.is_absolute() {
            return Err(BlueprintError::Validation {
                path: "include".to_string(),
                message: format!(
                    "absolute paths are not allowed in include: {include_ref}"
                ),
            });
        }

        // Security: reject path traversal
        if include_path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(BlueprintError::Validation {
                path: "include".to_string(),
                message: format!(
                    "path traversal ('..') is not allowed in include: {include_ref}"
                ),
            });
        }

        let resolved = base_dir.join(include_ref);
        let canonical = resolve_canonical(&resolved)?;

        // Security: verify the resolved path stays under the base directory
        // (prevents symlink-based escapes)
        let canonical_base = std::fs::canonicalize(base_dir).unwrap_or_else(|_| base_dir.to_path_buf());
        if !canonical.starts_with(&canonical_base) {
            return Err(BlueprintError::Validation {
                path: "include".to_string(),
                message: format!(
                    "included file '{}' resolves outside the schema directory",
                    include_ref
                ),
            });
        }

        // Cycle detection (checked before visited for correct cycle reporting)
        if stack.contains(&canonical) {
            let trace = stack
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(" → ");
            return Err(BlueprintError::Validation {
                path: "include".to_string(),
                message: format!(
                    "circular include detected: {trace} → {}",
                    canonical.display()
                ),
            });
        }

        // Include-once: skip if already fully expanded
        if visited.contains(&canonical) {
            continue;
        }

        stack.push(canonical.clone());

        let fragment = load_fragment(&canonical, visited, stack)?;

        stack.pop();
        visited.insert(canonical);

        // Check for name conflicts between this fragment and already-merged content
        check_conflicts(&merged, &fragment, include_ref)?;

        // Append fragment content
        append_model(&mut merged, fragment);
    }

    Ok(merged)
}

/// Merge the main schema on top of included content.
///
/// The main schema's items override included items with the same name.
/// Items from includes that don't conflict are preserved.
pub fn merge_main_over_includes(includes: &DataModel, main: &DataModel) -> DataModel {
    let mut result = includes.clone();

    // Entities: main wins on name collision
    for main_entity in &main.entities {
        if let Some(existing) = result
            .entities
            .iter_mut()
            .find(|e| e.name == main_entity.name)
        {
            *existing = main_entity.clone();
        } else {
            result.entities.push(main_entity.clone());
        }
    }

    // Relationships: main wins on name collision
    for main_rel in &main.relationships {
        if let Some(existing) = result
            .relationships
            .iter_mut()
            .find(|r| r.name == main_rel.name)
        {
            *existing = main_rel.clone();
        } else {
            result.relationships.push(main_rel.clone());
        }
    }

    // Noise profiles: main wins on name collision
    for main_noise in &main.noise_profiles {
        if let Some(existing) = result
            .noise_profiles
            .iter_mut()
            .find(|n| n.name == main_noise.name)
        {
            *existing = main_noise.clone();
        } else {
            result.noise_profiles.push(main_noise.clone());
        }
    }

    // Personas: main wins on name collision
    for main_persona in &main.personas {
        if let Some(existing) = result
            .personas
            .iter_mut()
            .find(|p| p.name == main_persona.name)
        {
            *existing = main_persona.clone();
        } else {
            result.personas.push(main_persona.clone());
        }
    }

    // Actor relationships: main wins on name collision
    for main_ar in &main.actor_relationships {
        if let Some(existing) = result
            .actor_relationships
            .iter_mut()
            .find(|a| a.name == main_ar.name)
        {
            *existing = main_ar.clone();
        } else {
            result.actor_relationships.push(main_ar.clone());
        }
    }

    // Correlations: main wins on entity key collision
    for main_corr in &main.correlations {
        if let Some(existing) = result
            .correlations
            .iter_mut()
            .find(|c| c.entity == main_corr.entity)
        {
            *existing = main_corr.clone();
        } else {
            result.correlations.push(main_corr.clone());
        }
    }

    // Custom types: main wins on name collision
    for main_ct in &main.custom_types {
        if let Some(existing) = result
            .custom_types
            .iter_mut()
            .find(|ct| ct.name == main_ct.name)
        {
            *existing = main_ct.clone();
        } else {
            result.custom_types.push(main_ct.clone());
        }
    }

    // Mixins: main wins on name collision
    for main_mixin in &main.mixins {
        if let Some(existing) = result
            .mixins
            .iter_mut()
            .find(|m| m.name == main_mixin.name)
        {
            *existing = main_mixin.clone();
        } else {
            result.mixins.push(main_mixin.clone());
        }
    }

    // Model-level metadata: always use main's values
    result.name = main.name.clone();
    result.description = main.description.clone();
    result.seed = main.seed;
    result.locale = main.locale.clone();
    result.timezone = main.timezone.clone();
    result.blueprint_version = main.blueprint_version.clone();
    result.params = main.params.clone();

    result
}

// ── Internal helpers ────────────────────────────────────────────────

/// Detect if a file is JSON based on its extension.
fn is_json_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
}

/// Load and validate a fragment file, recursively resolving its own includes.
fn load_fragment(
    path: &Path,
    visited: &mut HashSet<PathBuf>,
    stack: &mut Vec<PathBuf>,
) -> Result<DataModel, BlueprintError> {
    let content = std::fs::read_to_string(path).map_err(|e| BlueprintError::Validation {
        path: "include".to_string(),
        message: format!("cannot read included file '{}': {e}", path.display()),
    })?;

    let json = is_json_file(path);

    // Parse as RawSchema to inspect include/extends/model fields
    let raw: RawIncludeSchema = if json {
        serde_json::from_str(&content).map_err(|e| BlueprintError::Validation {
            path: "include".to_string(),
            message: format!("parse error in included file '{}': {e}", path.display()),
        })?
    } else {
        toml::from_str(&content).map_err(|e| BlueprintError::Validation {
            path: "include".to_string(),
            message: format!("parse error in included file '{}': {e}", path.display()),
        })?
    };

    // Fragment validation: reject extends
    if raw.extends.is_some() {
        return Err(BlueprintError::Validation {
            path: "include".to_string(),
            message: format!(
                "included file '{}' must not use 'extends' — only the root schema may extend",
                path.display()
            ),
        });
    }

    // Fragment validation: reject [model] section
    if raw.model.is_some() {
        return Err(BlueprintError::Validation {
            path: "include".to_string(),
            message: format!(
                "included file '{}' must not contain a [model] section — fragments define only entities, relationships, etc.",
                path.display()
            ),
        });
    }

    // Recursively resolve this fragment's own includes
    let nested_includes = raw.include.map(|s| s.into_vec()).unwrap_or_default();
    let nested_base = if nested_includes.is_empty() {
        DataModel::default()
    } else {
        resolve_includes(path, &nested_includes, visited, stack)?
    };

    // Parse fragment content into DataModel (without resolving custom types —
    // resolution happens once on the fully merged model)
    let fragment = if json {
        crate::blueprint::parser::parse_json_raw(&content)?
    } else {
        crate::blueprint::parser::parse_toml_raw(&content)?
    };

    // Merge nested includes under fragment (fragment content wins over its own includes)
    if nested_includes.is_empty() {
        Ok(fragment)
    } else {
        Ok(merge_main_over_includes(&nested_base, &fragment))
    }
}

/// Check for name conflicts between already-merged content and a new fragment.
fn check_conflicts(
    existing: &DataModel,
    fragment: &DataModel,
    include_ref: &str,
) -> Result<(), BlueprintError> {
    // Entities
    for entity in &fragment.entities {
        if existing.entities.iter().any(|e| e.name == entity.name) {
            return Err(BlueprintError::Validation {
                path: "include".to_string(),
                message: format!(
                    "entity '{}' is defined in multiple included files (conflict from '{include_ref}')",
                    entity.name
                ),
            });
        }
    }

    // Relationships
    for rel in &fragment.relationships {
        if existing.relationships.iter().any(|r| r.name == rel.name) {
            return Err(BlueprintError::Validation {
                path: "include".to_string(),
                message: format!(
                    "relationship '{}' is defined in multiple included files (conflict from '{include_ref}')",
                    rel.name
                ),
            });
        }
    }

    // Noise profiles
    for noise in &fragment.noise_profiles {
        if existing.noise_profiles.iter().any(|n| n.name == noise.name) {
            return Err(BlueprintError::Validation {
                path: "include".to_string(),
                message: format!(
                    "noise profile '{}' is defined in multiple included files (conflict from '{include_ref}')",
                    noise.name
                ),
            });
        }
    }

    // Personas
    for persona in &fragment.personas {
        if existing.personas.iter().any(|p| p.name == persona.name) {
            return Err(BlueprintError::Validation {
                path: "include".to_string(),
                message: format!(
                    "persona '{}' is defined in multiple included files (conflict from '{include_ref}')",
                    persona.name
                ),
            });
        }
    }

    // Actor relationships
    for ar in &fragment.actor_relationships {
        if existing
            .actor_relationships
            .iter()
            .any(|a| a.name == ar.name)
        {
            return Err(BlueprintError::Validation {
                path: "include".to_string(),
                message: format!(
                    "actor relationship '{}' is defined in multiple included files (conflict from '{include_ref}')",
                    ar.name
                ),
            });
        }
    }

    // Correlations (keyed by entity)
    for corr in &fragment.correlations {
        if existing
            .correlations
            .iter()
            .any(|c| c.entity == corr.entity)
        {
            return Err(BlueprintError::Validation {
                path: "include".to_string(),
                message: format!(
                    "correlation for entity '{}' is defined in multiple included files (conflict from '{include_ref}')",
                    corr.entity
                ),
            });
        }
    }

    // Mixins
    for mixin in &fragment.mixins {
        if existing.mixins.iter().any(|m| m.name == mixin.name) {
            return Err(BlueprintError::Validation {
                path: "include".to_string(),
                message: format!(
                    "mixin '{}' is defined in multiple included files (conflict from '{include_ref}')",
                    mixin.name
                ),
            });
        }
    }

    Ok(())
}

/// Append all items from `source` into `target` (no conflict checking).
fn append_model(target: &mut DataModel, source: DataModel) {
    target.entities.extend(source.entities);
    target.relationships.extend(source.relationships);
    target.noise_profiles.extend(source.noise_profiles);
    target.correlations.extend(source.correlations);
    target.personas.extend(source.personas);
    target.actor_relationships.extend(source.actor_relationships);
    target.custom_types.extend(source.custom_types);
    target.mixins.extend(source.mixins);
}

/// Canonicalize a path, falling back to the original if canonicalization fails.
fn resolve_canonical(path: &Path) -> Result<PathBuf, BlueprintError> {
    std::fs::canonicalize(path).map_err(|e| BlueprintError::Validation {
        path: "include".to_string(),
        message: format!("cannot resolve include path '{}': {e}", path.display()),
    })
}

// ── Minimal deserialization struct for fragment inspection ───────────

/// String-or-array type for the `include` field.
#[derive(Debug, Clone, serde::Deserialize)]
#[serde(untagged)]
pub(crate) enum StringOrVec {
    /// Single include path.
    Single(String),
    /// Multiple include paths.
    Multiple(Vec<String>),
}

impl StringOrVec {
    /// Convert to a `Vec<String>`.
    pub fn into_vec(self) -> Vec<String> {
        match self {
            StringOrVec::Single(s) => vec![s],
            StringOrVec::Multiple(v) => v,
        }
    }
}

/// Minimal schema struct for inspecting fragment metadata (include/extends/model).
#[derive(Debug, serde::Deserialize)]
struct RawIncludeSchema {
    #[serde(default)]
    include: Option<StringOrVec>,
    #[serde(default)]
    extends: Option<String>,
    #[serde(default)]
    model: Option<serde_json::Value>,
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    /// Helper: create a temporary schema file and return its path.
    fn write_schema(dir: &Path, name: &str, content: &str) -> PathBuf {
        let path = dir.join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn basic_include_single() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "users.knit.toml",
            r#"
            [[entities]]
            name = "users"
            count = 100
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = "users.knit.toml"
            [model]
            name = "test"
            seed = 99

            [[entities]]
            name = "orders"
            count = 500
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );

        let model = crate::blueprint::parse_toml_file(&main_path).unwrap();
        assert_eq!(model.name, "test");
        assert_eq!(model.seed, 99);
        assert_eq!(model.entities.len(), 2);
        assert!(model.entities.iter().any(|e| e.name == "users"));
        assert!(model.entities.iter().any(|e| e.name == "orders"));
    }

    #[test]
    fn basic_include_multiple() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "users.knit.toml",
            r#"
            [[entities]]
            name = "users"
            count = 100
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        write_schema(
            dir.path(),
            "products.knit.toml",
            r#"
            [[entities]]
            name = "products"
            count = 50
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = ["users.knit.toml", "products.knit.toml"]
            [model]
            name = "test"

            [[entities]]
            name = "orders"
            count = 500
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );

        let model = crate::blueprint::parse_toml_file(&main_path).unwrap();
        assert_eq!(model.entities.len(), 3);
    }

    #[test]
    fn nested_includes() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("common")).unwrap();
        write_schema(
            dir.path(),
            "common/base.knit.toml",
            r#"
            [[entities]]
            name = "audit_log"
            count = 10
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        write_schema(
            dir.path(),
            "users.knit.toml",
            r#"
            include = "common/base.knit.toml"

            [[entities]]
            name = "users"
            count = 100
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = "users.knit.toml"
            [model]
            name = "nested"
            "#,
        );

        let model = crate::blueprint::parse_toml_file(&main_path).unwrap();
        assert_eq!(model.entities.len(), 2);
        assert!(model.entities.iter().any(|e| e.name == "audit_log"));
        assert!(model.entities.iter().any(|e| e.name == "users"));
    }

    #[test]
    fn diamond_include_once() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "shared.knit.toml",
            r#"
            [[entities]]
            name = "shared"
            count = 10
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        write_schema(
            dir.path(),
            "a.knit.toml",
            r#"
            include = "shared.knit.toml"
            [[entities]]
            name = "a_entity"
            count = 10
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        write_schema(
            dir.path(),
            "b.knit.toml",
            r#"
            include = "shared.knit.toml"
            [[entities]]
            name = "b_entity"
            count = 10
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = ["a.knit.toml", "b.knit.toml"]
            [model]
            name = "diamond"
            "#,
        );

        let model = crate::blueprint::parse_toml_file(&main_path).unwrap();
        // shared should appear only once
        assert_eq!(
            model.entities.iter().filter(|e| e.name == "shared").count(),
            1
        );
        assert_eq!(model.entities.len(), 3); // shared + a_entity + b_entity
    }

    #[test]
    fn circular_include_detected() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "a.knit.toml",
            r#"
            include = "b.knit.toml"
            [[entities]]
            name = "a"
            count = 1
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        write_schema(
            dir.path(),
            "b.knit.toml",
            r#"
            include = "a.knit.toml"
            [[entities]]
            name = "b"
            count = 1
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = "a.knit.toml"
            [model]
            name = "cycle"
            "#,
        );

        let err = crate::blueprint::parse_toml_file(&main_path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("circular include"), "got: {msg}");
    }

    #[test]
    fn conflict_between_includes_is_error() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "a.knit.toml",
            r#"
            [[entities]]
            name = "users"
            count = 100
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        write_schema(
            dir.path(),
            "b.knit.toml",
            r#"
            [[entities]]
            name = "users"
            count = 200
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = ["a.knit.toml", "b.knit.toml"]
            [model]
            name = "conflict"
            "#,
        );

        let err = crate::blueprint::parse_toml_file(&main_path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("defined in multiple included files"),
            "got: {msg}"
        );
    }

    #[test]
    fn main_overrides_included_entity() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "base.knit.toml",
            r#"
            [[entities]]
            name = "users"
            count = 100
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = "base.knit.toml"
            [model]
            name = "override"

            [[entities]]
            name = "users"
            count = 999
            [[entities.fields]]
            name = "id"
            data_type = "int"
            [[entities.fields]]
            name = "email"
            data_type = "string"
            "#,
        );

        let model = crate::blueprint::parse_toml_file(&main_path).unwrap();
        let users = model.entities.iter().find(|e| e.name == "users").unwrap();
        assert_eq!(users.count, crate::core::CountSpec::Fixed(999));
        assert_eq!(users.fields.len(), 2); // id + email (main's version)
    }

    #[test]
    fn reject_extends_in_fragment() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "parent.knit.toml",
            r#"
            [model]
            name = "parent"
            "#,
        );
        write_schema(
            dir.path(),
            "frag.knit.toml",
            r#"
            extends = "parent.knit.toml"
            [[entities]]
            name = "x"
            count = 1
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = "frag.knit.toml"
            [model]
            name = "test"
            "#,
        );

        let err = crate::blueprint::parse_toml_file(&main_path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("must not use 'extends'"), "got: {msg}");
    }

    #[test]
    fn reject_model_in_fragment() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "frag.knit.toml",
            r#"
            [model]
            name = "fragment_model"

            [[entities]]
            name = "x"
            count = 1
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = "frag.knit.toml"
            [model]
            name = "test"
            "#,
        );

        let err = crate::blueprint::parse_toml_file(&main_path).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("must not contain a [model] section"),
            "got: {msg}"
        );
    }

    #[test]
    fn reject_absolute_path() {
        let dir = TempDir::new().unwrap();
        // Use a Windows absolute path on Windows, Unix path on Unix
        let abs_path = if cfg!(windows) {
            r#"include = "C:\\Windows\\System32\\config"
            "#
        } else {
            r#"include = "/etc/passwd"
            "#
        };
        let main_content = format!(
            r#"
            {abs_path}
            [model]
            name = "test"
            "#
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            &main_content,
        );

        let err = crate::blueprint::parse_toml_file(&main_path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("absolute paths are not allowed"), "got: {msg}");
    }

    #[test]
    fn reject_path_traversal() {
        let dir = TempDir::new().unwrap();
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = "../secret.knit.toml"
            [model]
            name = "test"
            "#,
        );

        let err = crate::blueprint::parse_toml_file(&main_path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("path traversal"), "got: {msg}");
    }

    #[test]
    fn include_with_extends() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "users.knit.toml",
            r#"
            [[entities]]
            name = "users"
            count = 100
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        write_schema(
            dir.path(),
            "parent.knit.toml",
            r#"
            [model]
            name = "parent"
            seed = 1

            [[entities]]
            name = "base_entity"
            count = 10
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = "users.knit.toml"
            extends = "parent.knit.toml"

            [model]
            name = "combined"
            "#,
        );

        let model = crate::blueprint::parse_toml_file(&main_path).unwrap();
        assert_eq!(model.name, "combined");
        // Should have users (from include), base_entity (from extends parent)
        assert!(model.entities.iter().any(|e| e.name == "users"));
        assert!(model.entities.iter().any(|e| e.name == "base_entity"));
    }

    #[test]
    fn include_relationships_and_personas() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "fragment.knit.toml",
            r#"
            [[entities]]
            name = "users"
            count = 100
            actor = true
            [[entities.fields]]
            name = "id"
            data_type = "int"
            [entities.fields.generator]
            type = "sequence"

            [[entities]]
            name = "orders"
            count = 500
            [[entities.fields]]
            name = "id"
            data_type = "int"
            [entities.fields.generator]
            type = "sequence"
            [[entities.fields]]
            name = "user_id"
            data_type = "int"

            [[relationships]]
            name = "orders_users"
            from = "orders"
            to = "users"
            kind = "many_to_one"

            [[personas]]
            name = "power_user"
            weight = 0.3
            [personas.traits]
            activity_rate = 10.0
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = "fragment.knit.toml"
            [model]
            name = "full"
            "#,
        );

        let model = crate::blueprint::parse_toml_file(&main_path).unwrap();
        assert_eq!(model.entities.len(), 2);
        assert_eq!(model.relationships.len(), 1);
        assert_eq!(model.personas.len(), 1);
        assert_eq!(model.relationships[0].name, "orders_users");
        assert_eq!(model.personas[0].name, "power_user");
    }

    #[test]
    fn empty_include_array_is_noop() {
        let dir = TempDir::new().unwrap();
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = []
            [model]
            name = "empty"

            [[entities]]
            name = "x"
            count = 1
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );

        let model = crate::blueprint::parse_toml_file(&main_path).unwrap();
        assert_eq!(model.entities.len(), 1);
    }

    #[test]
    fn json_fragment_include() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "users.json",
            r#"{
                "entities": [{
                    "name": "users",
                    "count": 100,
                    "fields": [{"name": "id", "data_type": "int"}]
                }]
            }"#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.json",
            r#"{
                "include": "users.json",
                "model": {"name": "json_test"},
                "entities": [{
                    "name": "orders",
                    "count": 50,
                    "fields": [{"name": "id", "data_type": "int"}]
                }]
            }"#,
        );

        let model = crate::blueprint::parse_json_file(&main_path).unwrap();
        assert_eq!(model.entities.len(), 2);
        assert!(model.entities.iter().any(|e| e.name == "users"));
        assert!(model.entities.iter().any(|e| e.name == "orders"));
    }

    #[test]
    fn cycle_from_root_detected() {
        let dir = TempDir::new().unwrap();
        write_schema(
            dir.path(),
            "frag.knit.toml",
            r#"
            include = "main.knit.toml"
            [[entities]]
            name = "x"
            count = 1
            [[entities.fields]]
            name = "id"
            data_type = "int"
            "#,
        );
        let main_path = write_schema(
            dir.path(),
            "main.knit.toml",
            r#"
            include = "frag.knit.toml"
            [model]
            name = "cycle_root"
            "#,
        );

        let err = crate::blueprint::parse_toml_file(&main_path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("circular include"), "got: {msg}");
    }
}
