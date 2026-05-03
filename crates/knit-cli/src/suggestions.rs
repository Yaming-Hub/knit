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
            "Ensure the faker method name is correct. Example: \
             type = \"faker\", method = \"name\", args = [].",
        );
    }

    if lower.contains("unknown generator type") || lower.contains("unknown variant") {
        // Only suggest generator types when the error mentions generator/type context,
        // not for other enum deserialization failures (DataType, DistributionKind, etc.)
        if lower.contains("generator") || lower.contains("type") {
            return Some(
                "Check the generator type spelling. Supported types: distribution, faker, \
                 sequence, one_of, pattern, derived, conditional, composite, lookup, constant, \
                 uuid, unique, relative, business_hours.",
            );
        }
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

    if lower.contains("distribution requires") && lower.contains("param") {
        return Some(
            "Distribution parameters must be in a [entities.fields.generator.params] sub-table. \
             Example: [entities.fields.generator.params] mean = 50.0, std_dev = 10.0.",
        );
    }

    if lower.contains("permission denied") || lower.contains("access") {
        return Some(
            "Check file permissions on the schema file and output directory. \
             On Unix, try: chmod +r <schema> && chmod +w <output-dir>.",
        );
    }

    if lower.contains("toml parse error") {
        return Some(
            "Check TOML syntax near the reported line. Common issues: missing quotes \
             around strings, duplicate keys, or incorrect table nesting.",
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
        assert!(msg.unwrap().contains("faker"));
    }

    #[test]
    fn suggests_for_unknown_generator() {
        let msg = suggest_fix("unknown generator type 'foobar'");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("Supported types"));
    }

    #[test]
    fn suggests_for_unknown_variant() {
        let msg = suggest_fix("unknown variant `temporal`, expected one of type variants");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("Supported types"));
    }

    #[test]
    fn no_suggestion_for_non_generator_variant() {
        // DataType or DistributionKind enum errors should NOT get a generator hint
        let msg = suggest_fix("unknown variant `strng` for field data format");
        assert!(msg.is_none());
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

    #[test]
    fn suggests_for_distribution_params() {
        let msg = suggest_fix("distribution requires parameter 'mean'");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("sub-table"));
    }

    #[test]
    fn suggests_for_toml_parse() {
        let msg = suggest_fix("toml parse error at line 12");
        assert!(msg.is_some());
        assert!(msg.unwrap().contains("TOML syntax"));
    }
}
