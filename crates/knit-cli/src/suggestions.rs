//! Error suggestion engine for the knit CLI.
//!
//! Maps common error messages to actionable suggestions, helping users
//! diagnose and fix schema issues without consulting documentation.

/// Attempt to produce a helpful suggestion for a given error message.
///
/// Returns `Some(suggestion)` if the error matches a known pattern,
/// or `None` for unrecognised errors.
///
/// # Examples
///
/// ```
/// use knit_cli::suggestions::suggest_fix;
/// assert!(suggest_fix("unknown generator type 'faker'").is_some());
/// ```
pub fn suggest_fix(error: &str) -> Option<&'static str> {
    let lower = error.to_lowercase();

    if lower.contains("unknown generator type") && lower.contains("faker") {
        return Some(
            "Faker generators are not yet supported. Use 'pattern' for formatted strings \
             or 'one_of' for categorical values.",
        );
    }

    if lower.contains("unknown generator type") {
        return Some(
            "Check the generator type spelling. Supported types: sequence, distribution, \
             constant, uuid, one_of, pattern, derived, composite, temporal, correlated, topology.",
        );
    }

    if lower.contains("entity") && lower.contains("not found") {
        return Some(
            "Check spelling and ensure the entity is defined before it is referenced. \
             Entity names are case-sensitive.",
        );
    }

    if lower.contains("circular") || lower.contains("cycle") {
        return Some(
            "Entities cannot form circular foreign-key dependencies. \
             Re-order entities or remove one FK reference to break the cycle.",
        );
    }

    if lower.contains("duplicate") && lower.contains("entity") {
        return Some("Each entity name must be unique within a schema. Rename one of the duplicates.");
    }

    if lower.contains("missing") && lower.contains("primary_key") {
        return Some(
            "Every entity referenced by a foreign key must have exactly one field \
             with `primary_key = true`.",
        );
    }

    if lower.contains("schema_version") {
        return Some(
            "Ensure your schema starts with `schema_version = \"1.0\"`. \
             This field is required.",
        );
    }

    if lower.contains("permission denied") || lower.contains("access") {
        return Some(
            "Check file permissions on the schema file and output directory. \
             On Unix, try: chmod +r <schema> && chmod +w <output-dir>.",
        );
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_for_faker() {
        let msg = suggest_fix("unknown generator type 'faker'");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("pattern"));
    }

    #[test]
    fn suggests_for_unknown_generator() {
        let msg = suggest_fix("unknown generator type 'foobar'");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("Supported types"));
    }

    #[test]
    fn suggests_for_entity_not_found() {
        let msg = suggest_fix("entity 'users' not found in model");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("case-sensitive"));
    }

    #[test]
    fn suggests_for_circular_deps() {
        let msg = suggest_fix("circular dependency detected between entities");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("cycle"));
    }

    #[test]
    fn returns_none_for_unknown_error() {
        assert!(suggest_fix("something completely unexpected happened").is_none());
    }

    #[test]
    fn suggests_for_missing_primary_key() {
        let msg = suggest_fix("missing primary_key on entity referenced by FK");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("primary_key"));
    }

    #[test]
    fn suggests_for_schema_version() {
        let msg = suggest_fix("invalid schema_version format");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("1.0"));
    }
}
