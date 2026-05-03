//! Built-in MiniJinja filter and function helpers for template rendering.
//!
//! These helpers are automatically registered with the MiniJinja [`Environment`](minijinja::Environment)
//! used by [`TemplateSink`](crate::template::TemplateSink). They provide common formatting,
//! escaping, and string manipulation operations useful for generating SQL, XML, and other
//! text-based outputs from Arrow data.

use minijinja::Value;

// ── Date / number formatting ────────────────────────────────────────────────

/// Format a date/datetime string using a `strftime`-style pattern.
///
/// Parses the input as RFC 3339 and reformats it. Returns the original string
/// if parsing fails.
///
/// # Example (template)
/// ```jinja
/// {{ row.created_at | format_date("%Y-%m-%d") }}
/// ```
pub fn format_date(value: Value, fmt: &str) -> String {
    let s = match value.as_str() {
        Some(s) => s.to_string(),
        None => value.to_string(),
    };
    chrono::DateTime::parse_from_rfc3339(&s)
        .map(|dt| dt.format(fmt).to_string())
        .unwrap_or(s)
}

/// Format a number with a fixed number of decimal places.
///
/// Works with both integer and floating-point MiniJinja values.
///
/// # Example (template)
/// ```jinja
/// {{ row.price | format_number(2) }}
/// ```
pub fn format_number(value: Value, decimals: u32) -> String {
    if let Some(i) = value.as_i64() {
        format!("{:.prec$}", i as f64, prec = decimals as usize)
    } else if let Some(s) = value.as_str() {
        // Try parsing string as f64
        s.parse::<f64>()
            .map(|f| format!("{:.prec$}", f, prec = decimals as usize))
            .unwrap_or_else(|_| s.to_string())
    } else {
        // Try converting via display and parsing
        let s = value.to_string();
        s.parse::<f64>()
            .map(|f| format!("{:.prec$}", f, prec = decimals as usize))
            .unwrap_or(s)
    }
}

// ── Escaping helpers ────────────────────────────────────────────────────────

/// Escape a string for safe inclusion in a SQL literal.
///
/// Escape a string for safe inclusion in a SQL single-quoted literal.
///
/// Doubles single quotes, escapes backslashes, and removes NULL bytes which
/// could truncate strings in some databases. Targets ANSI SQL; for specific
/// dialects, users should provide their own filter.
///
/// # Example (template)
/// ```jinja
/// INSERT INTO t VALUES ('{{ row.name | escape_sql }}');
/// ```
pub fn escape_sql(value: Value) -> String {
    let s = match value.as_str() {
        Some(s) => s.to_string(),
        None => value.to_string(),
    };
    s.replace('\0', "")
        .replace('\\', "\\\\")
        .replace('\'', "''")
}

/// Escape a string for safe inclusion in XML/HTML content.
///
/// Replaces `&`, `<`, `>`, `"`, and `'` with their XML entity equivalents.
///
/// # Example (template)
/// ```jinja
/// <name>{{ row.name | escape_xml }}</name>
/// ```
pub fn escape_xml(value: Value) -> String {
    let s = match value.as_str() {
        Some(s) => s.to_string(),
        None => value.to_string(),
    };
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Serialize a MiniJinja value as a JSON string.
///
/// Useful for embedding structured data inside templates.
///
/// # Example (template)
/// ```jinja
/// {"data": {{ row | json_encode }}}
/// ```
pub fn json_encode(value: Value) -> String {
    // MiniJinja values can be converted via their debug/display; use serde for accuracy.
    let serializable = minijinja_value_to_serde(&value);
    serde_json::to_string(&serializable).unwrap_or_else(|_| "null".to_string())
}

/// Convert a MiniJinja `Value` to a `serde_json::Value` for serialization.
fn minijinja_value_to_serde(val: &Value) -> serde_json::Value {
    if val.is_none() || val.is_undefined() {
        serde_json::Value::Null
    } else if val.is_true() && (val.as_str().is_none() && val.as_i64().is_none()) {
        // Boolean true (not a truthy string/number)
        serde_json::Value::Bool(true)
    } else if !val.is_true() && val.kind() == minijinja::value::ValueKind::Bool {
        serde_json::Value::Bool(false)
    } else if let Some(i) = val.as_i64() {
        serde_json::Value::Number(i.into())
    } else if let Some(s) = val.as_str() {
        serde_json::Value::String(s.to_string())
    } else if val.kind() == minijinja::value::ValueKind::Seq {
        let items: Vec<serde_json::Value> = val
            .try_iter()
            .into_iter()
            .flatten()
            .map(|v| minijinja_value_to_serde(&v))
            .collect();
        serde_json::Value::Array(items)
    } else if val.kind() == minijinja::value::ValueKind::Map {
        let mut map = serde_json::Map::new();
        if let Ok(keys) = val.try_iter() {
            for key in keys {
                let k = key.to_string();
                if let Ok(v) = val.get_item(&key) {
                    map.insert(k, minijinja_value_to_serde(&v));
                }
            }
        }
        serde_json::Value::Object(map)
    } else {
        // Fallback: try parsing as f64 for floating point values
        let s = val.to_string();
        if let Ok(f) = s.parse::<f64>() {
            serde_json::Number::from_f64(f)
                .map(serde_json::Value::Number)
                .unwrap_or_else(|| serde_json::Value::String(s))
        } else {
            serde_json::Value::String(s)
        }
    }
}

// ── String formatting helpers ───────────────────────────────────────────────

/// Left-pad a string to the given width with spaces (or a custom fill character).
///
/// # Example (template)
/// ```jinja
/// {{ row.id | pad_left(10) }}
/// ```
pub fn pad_left(value: Value, width: u32) -> String {
    let s = match value.as_str() {
        Some(s) => s.to_string(),
        None => value.to_string(),
    };
    let w = width as usize;
    if s.len() >= w {
        s
    } else {
        format!("{:>width$}", s, width = w)
    }
}

/// Right-pad a string to the given width with spaces.
///
/// # Example (template)
/// ```jinja
/// {{ row.name | pad_right(20) }}
/// ```
pub fn pad_right(value: Value, width: u32) -> String {
    let s = match value.as_str() {
        Some(s) => s.to_string(),
        None => value.to_string(),
    };
    let w = width as usize;
    if s.len() >= w {
        s
    } else {
        format!("{:<width$}", s, width = w)
    }
}

/// Convert a string to upper case.
///
/// # Example (template)
/// ```jinja
/// {{ row.status | upper }}
/// ```
pub fn upper(value: Value) -> String {
    match value.as_str() {
        Some(s) => s.to_uppercase(),
        None => value.to_string().to_uppercase(),
    }
}

/// Convert a string to lower case.
///
/// # Example (template)
/// ```jinja
/// {{ row.status | lower }}
/// ```
pub fn lower(value: Value) -> String {
    match value.as_str() {
        Some(s) => s.to_lowercase(),
        None => value.to_string().to_lowercase(),
    }
}

/// Register all built-in helpers on the given MiniJinja environment.
///
/// Called by [`TemplateSink::new`](crate::template::TemplateSink::new) during
/// construction. Each helper is registered as a MiniJinja filter so it can be
/// used with the `|` pipe syntax in templates.
pub fn register_helpers(env: &mut minijinja::Environment<'_>) {
    env.add_filter("format_date", format_date);
    env.add_filter("format_number", format_number);
    env.add_filter("escape_sql", escape_sql);
    env.add_filter("escape_xml", escape_xml);
    env.add_filter("json_encode", json_encode);
    env.add_filter("pad_left", pad_left);
    env.add_filter("pad_right", pad_right);
    env.add_filter("upper", upper);
    env.add_filter("lower", lower);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_format_date_rfc3339() {
        let result = format_date(Value::from("2024-01-15T12:00:00+00:00"), "%Y-%m-%d");
        assert_eq!(result, "2024-01-15");
    }

    #[test]
    fn test_format_date_invalid_passthrough() {
        assert_eq!(format_date(Value::from("not-a-date"), "%Y"), "not-a-date");
    }

    #[test]
    fn test_format_number_float() {
        let result = format_number(Value::from(3.14259), 2);
        assert_eq!(result, "3.14");
    }

    #[test]
    fn test_format_number_int() {
        let result = format_number(Value::from(42), 3);
        assert_eq!(result, "42.000");
    }

    #[test]
    fn test_escape_sql() {
        assert_eq!(escape_sql(Value::from("it's a test")), "it''s a test");
        assert_eq!(escape_sql(Value::from("back\\slash")), "back\\\\slash");
    }

    #[test]
    fn test_escape_xml() {
        assert_eq!(escape_xml(Value::from("<b>\"hi\" & 'bye'</b>")), "&lt;b&gt;&quot;hi&quot; &amp; &apos;bye&apos;&lt;/b&gt;");
    }

    #[test]
    fn test_json_encode_string() {
        let result = json_encode(Value::from("hello"));
        assert_eq!(result, "\"hello\"");
    }

    #[test]
    fn test_json_encode_number() {
        let result = json_encode(Value::from(42));
        assert_eq!(result, "42");
    }

    #[test]
    fn test_pad_left() {
        assert_eq!(pad_left(Value::from("hi"), 5), "   hi");
        assert_eq!(pad_left(Value::from("hello"), 3), "hello");
    }

    #[test]
    fn test_pad_right() {
        assert_eq!(pad_right(Value::from("hi"), 5), "hi   ");
        assert_eq!(pad_right(Value::from("hello"), 3), "hello");
    }

    #[test]
    fn test_upper() {
        assert_eq!(upper(Value::from("hello")), "HELLO");
    }

    #[test]
    fn test_lower() {
        assert_eq!(lower(Value::from("HELLO")), "hello");
    }
}
