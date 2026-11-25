//! Validation of tool arguments against the tool's own published JSON Schema.
//!
//! Every tool's `inputSchema` is the single source of truth for argument names, types and
//! bounds (count limits, page ranges, character caps): the same document is advertised in
//! `tools/list` and enforced here before any daemon call. Only the subset of JSON Schema the
//! tool schemas use is supported; an unsupported keyword in a schema is a programming error
//! caught by unit tests, never silently ignored at runtime.
//!
//! Strings are measured in Unicode scalar values (JSON Schema `maxLength` semantics).
use serde_json::{Map, Value};

/// Keywords this validator understands. Schemas may also carry `description`/`default`.
#[cfg(test)]
const KNOWN: &[&str] = &[
    "type",
    "properties",
    "required",
    "additionalProperties",
    "items",
    "minItems",
    "maxItems",
    "uniqueItems",
    "minLength",
    "maxLength",
    "minimum",
    "maximum",
    "enum",
    "format",
    "description",
    "default",
];

/// Checks `value` against `schema`; the error names the offending argument path.
pub fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    check(schema, value, "arguments")
}

fn check(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(schema) = schema.as_object() else {
        return Err(format!("{path}: schema is not an object"));
    };
    if let Some(types) = schema.get("type") {
        let allowed: Vec<&str> = match types {
            Value::String(t) => vec![t.as_str()],
            Value::Array(list) => list.iter().filter_map(Value::as_str).collect(),
            _ => return Err(format!("{path}: unsupported type keyword")),
        };
        if !allowed.iter().any(|t| has_type(value, t)) {
            return Err(format!(
                "{path}: expected {}, got {}",
                allowed.join(" or "),
                type_name(value)
            ));
        }
    }
    if let Some(options) = schema.get("enum").and_then(Value::as_array)
        && !options.contains(value)
    {
        let names: Vec<String> = options.iter().map(Value::to_string).collect();
        return Err(format!("{path}: must be one of {}", names.join(", ")));
    }
    match value {
        Value::Object(map) => object(schema, map, path),
        Value::Array(items) => array(schema, items, path),
        Value::String(text) => string(schema, text, path),
        Value::Number(_) => number(schema, value, path),
        _ => Ok(()),
    }
}

fn has_type(value: &Value, t: &str) -> bool {
    match t {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        "number" => value.is_number(),
        // Integers must be exact: 3.0 is accepted, 3.5 and 1e400 are not.
        "integer" => {
            value.as_i64().is_some()
                || value.as_u64().is_some()
                || value
                    .as_f64()
                    .is_some_and(|f| f.is_finite() && f.fract() == 0.0 && f.abs() < 9.0e15)
        }
        _ => false,
    }
}

fn type_name(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn object(schema: &Map<String, Value>, map: &Map<String, Value>, path: &str) -> Result<(), String> {
    let empty = Map::new();
    let properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .unwrap_or(&empty);
    for required in schema
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if !map.contains_key(required) {
            return Err(format!("{path}: missing required property {required:?}"));
        }
    }
    let closed = schema.get("additionalProperties") == Some(&Value::Bool(false));
    for (key, item) in map {
        match properties.get(key) {
            Some(sub) => check(sub, item, &format!("{path}.{key}"))?,
            None if closed => {
                let mut known: Vec<&str> = properties.keys().map(String::as_str).collect();
                known.sort_unstable();
                return Err(format!(
                    "{path}: unknown property {key:?} (allowed: {})",
                    known.join(", ")
                ));
            }
            None => {}
        }
    }
    Ok(())
}

fn array(schema: &Map<String, Value>, items: &[Value], path: &str) -> Result<(), String> {
    if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
        && (items.len() as u64) < min
    {
        return Err(format!("{path}: needs at least {min} item(s)"));
    }
    if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
        && items.len() as u64 > max
    {
        return Err(format!(
            "{path}: at most {max} item(s) allowed, got {}",
            items.len()
        ));
    }
    if schema.get("uniqueItems") == Some(&Value::Bool(true)) {
        for (i, item) in items.iter().enumerate() {
            if items[..i].contains(item) {
                return Err(format!("{path}[{i}]: duplicate item"));
            }
        }
    }
    if let Some(sub) = schema.get("items") {
        for (i, item) in items.iter().enumerate() {
            check(sub, item, &format!("{path}[{i}]"))?;
        }
    }
    Ok(())
}

fn string(schema: &Map<String, Value>, text: &str, path: &str) -> Result<(), String> {
    let chars = text.chars().count() as u64;
    if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
        && chars < min
    {
        return Err(format!("{path}: needs at least {min} character(s)"));
    }
    if let Some(max) = schema.get("maxLength").and_then(Value::as_u64)
        && chars > max
    {
        return Err(format!(
            "{path}: at most {max} characters allowed, got {chars}"
        ));
    }
    if text.chars().any(|c| c == '\0') {
        return Err(format!("{path}: NUL characters are not allowed"));
    }
    match schema.get("format").and_then(Value::as_str) {
        Some("uuid") => match uuid::Uuid::parse_str(text) {
            Ok(id) if !id.is_nil() && text.len() == 36 => Ok(()),
            _ => Err(format!(
                "{path}: must be a non-nil UUID like 0190c3b2-7d3e-7c1a-9f00-1234567890ab"
            )),
        },
        Some("uri") => {
            let lower = text.to_ascii_lowercase();
            if (lower.starts_with("https://") || lower.starts_with("http://"))
                && !text.chars().any(|c| c.is_whitespace() || c.is_control())
                && text.len() > "http://".len()
            {
                Ok(())
            } else {
                Err(format!("{path}: must be an absolute http(s) URL"))
            }
        }
        Some(other) => Err(format!("{path}: unsupported format {other}")),
        None => Ok(()),
    }
}

fn number(schema: &Map<String, Value>, value: &Value, path: &str) -> Result<(), String> {
    let n = value.as_f64().unwrap_or(f64::NAN);
    if !n.is_finite() {
        return Err(format!("{path}: must be finite"));
    }
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
        && n < min
    {
        return Err(format!("{path}: must be >= {}", schema["minimum"]));
    }
    if let Some(max) = schema.get("maximum").and_then(Value::as_f64)
        && n > max
    {
        return Err(format!("{path}: must be <= {}", schema["maximum"]));
    }
    Ok(())
}

/// Every keyword used by `schema` is one this validator enforces (or a pure annotation).
#[cfg(test)]
pub fn uses_only_supported_keywords(schema: &Value) -> Result<(), String> {
    let Some(map) = schema.as_object() else {
        return Err("schema is not an object".into());
    };
    for (key, sub) in map {
        if !KNOWN.contains(&key.as_str()) {
            return Err(format!("unsupported schema keyword {key:?}"));
        }
        match key.as_str() {
            "properties" => {
                for sub in sub.as_object().into_iter().flat_map(|m| m.values()) {
                    uses_only_supported_keywords(sub)?;
                }
            }
            "items" => uses_only_supported_keywords(sub)?,
            _ => {}
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn schema() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["query"],
            "properties": {
                "query": {"type": "string", "minLength": 1, "maxLength": 5},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50},
                "id": {"type": "string", "format": "uuid"},
                "tags": {"type": "array", "maxItems": 2, "uniqueItems": true,
                         "items": {"type": "string", "maxLength": 3}},
                "kind": {"type": "string", "enum": ["a", "b"]},
                "maybe": {"type": ["string", "null"]},
                "url": {"type": "string", "format": "uri"}
            }
        })
    }

    #[test]
    fn bounds_types_and_unknown_properties_are_enforced() {
        let s = schema();
        assert!(uses_only_supported_keywords(&s).is_ok());
        assert!(validate(&s, &json!({"query": "abc", "limit": 50, "maybe": null})).is_ok());
        assert!(validate(&s, &json!({"query": "abc", "limit": 3.0})).is_ok());
        for (bad, needle) in [
            (json!({}), "missing required property \"query\""),
            (json!({"query": ""}), "at least 1"),
            (json!({"query": "日本語テキスト"}), "at most 5 characters"),
            (json!({"query": "a", "limit": 51}), "<= 50"),
            (json!({"query": "a", "limit": 0}), ">= 1"),
            (json!({"query": "a", "limit": 1.5}), "expected integer"),
            (json!({"query": "a", "limit": "3"}), "expected integer"),
            (
                json!({"query": "a", "bogus": 1}),
                "unknown property \"bogus\"",
            ),
            (json!({"query": "a", "id": "nope"}), "UUID"),
            (
                json!({"query": "a", "id": "00000000-0000-0000-0000-000000000000"}),
                "UUID",
            ),
            (
                json!({"query": "a", "tags": ["x", "y", "z"]}),
                "at most 2 item",
            ),
            (json!({"query": "a", "tags": ["x", "x"]}), "duplicate"),
            (json!({"query": "a", "tags": ["long"]}), "arguments.tags[0]"),
            (json!({"query": "a", "kind": "c"}), "one of"),
            (
                json!({"query": "a", "url": "file:///etc/passwd"}),
                "http(s)",
            ),
            (json!({"query": "a\u{0}"}), "NUL"),
            (json!([1]), "expected object"),
        ] {
            let error = validate(&s, &bad).unwrap_err();
            assert!(error.contains(needle), "{bad}: {error}");
        }
    }

    #[test]
    fn unsupported_keywords_are_reported() {
        assert!(uses_only_supported_keywords(&json!({"type": "string", "pattern": "x"})).is_err());
        assert!(
            uses_only_supported_keywords(&json!({"properties": {"a": {"oneOf": []}}})).is_err()
        );
    }
}
