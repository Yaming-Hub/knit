//! Integration tests for the built-in bind helper functions.

use minijinja::Value;

use knit::bind::helpers::{
    escape_sql, escape_xml, format_date, json_encode, lower, pad_left, pad_right, upper,
};

/// Verify `format_date` reformats RFC 3339 input and passes through invalid dates.
#[test]
fn format_date_formats_valid_values_and_preserves_invalid_input() {
    assert_eq!(
        format_date(Value::from("2024-01-15T12:34:56+00:00"), "%Y-%m-%d"),
        "2024-01-15"
    );
    assert_eq!(format_date(Value::from("not-a-date"), "%Y"), "not-a-date");
}

/// Verify `escape_sql` doubles quotes, escapes backslashes, and removes NUL bytes.
#[test]
fn escape_sql_escapes_special_characters_for_sql_literals() {
    assert_eq!(
        escape_sql(Value::from("O'Reilly\\bin\0")),
        "O''Reilly\\\\bin"
    );
}

/// Verify `escape_xml` replaces XML-sensitive characters with entities.
#[test]
fn escape_xml_escapes_reserved_xml_characters() {
    assert_eq!(
        escape_xml(Value::from("<node attr=\"x\">Tom & 'Jerry'</node>")),
        "&lt;node attr=&quot;x&quot;&gt;Tom &amp; &apos;Jerry&apos;&lt;/node&gt;"
    );
}

/// Verify `json_encode` serializes MiniJinja values as valid JSON strings.
#[test]
fn json_encode_serializes_values_as_json() {
    assert_eq!(
        json_encode(Value::from("hello \"world\"")),
        "\"hello \\\"world\\\"\""
    );
    assert_eq!(json_encode(Value::from(42)), "42");
}

/// Verify `pad_left` and `pad_right` add the expected amount of whitespace.
#[test]
fn padding_helpers_apply_requested_width() {
    assert_eq!(pad_left(Value::from("7"), 4), "   7");
    assert_eq!(pad_right(Value::from("7"), 4), "7   ");
}

/// Verify `upper` and `lower` normalize case in both directions.
#[test]
fn case_helpers_convert_text_case() {
    assert_eq!(upper(Value::from("MiXeD")), "MIXED");
    assert_eq!(lower(Value::from("MiXeD")), "mixed");
}
