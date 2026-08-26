//! Deterministic canonical JSON (contract-hash input).
//!
//! Faithful port of `src/relay/canonical-json.ts`. Produces a byte-stable
//! encoding of a JSON value so that a SHA-256 contract hash is independent of
//! object key insertion order while preserving semantically order-sensitive
//! arrays.
//!
//! Rules (identical to the TypeScript oracle):
//!   1. Object keys are sorted lexicographically by UTF-16 code unit (matching
//!      the JS default string sort) and serialized in that order.
//!   2. ARRAY element order is always preserved (the caller pre-sorts
//!      semantically order-insensitive arrays, e.g. the tool list).
//!   3. Strings are emitted with JSON escaping (UTF-8 bytes, no
//!      ASCII-minifying), matching `JSON.stringify` and `serde_json` default
//!      string serialization.
//!   4. Numbers are emitted in their JSON numeric form. NaN / Infinity /
//!      -Infinity have no canonical form and are rejected at the `Value` type
//!      boundary (serde_json's `Number` cannot represent them).
//!   5. `null` and booleans are supported.
//!   6. Cyclic and sparse structures cannot occur for an owned `serde_json::Value`
//!      tree; nesting depth is enforced via `max_depth`.
//!
//! Two structurally equal JSON values (ignoring object key order) always
//! canonicalize to identical bytes.

use serde_json::Value;
use std::cmp::Ordering;

/// Error thrown for values that have no canonical JSON representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalJsonError(pub String);

impl std::fmt::Display for CanonicalJsonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "canonical-json: {}", self.0)
    }
}

impl std::error::Error for CanonicalJsonError {}

/// Compare two strings by UTF-16 code units, matching the JS default sort
/// (`(a, b) => a < b ? -1 : a > b ? 1 : 0` over UTF-16 units).
pub fn utf16_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}

/// Canonicalize a JSON value into a stable string.
pub fn canonical_json(value: &Value, max_depth: usize) -> Result<String, CanonicalJsonError> {
    let mut out = String::new();
    write_value(value, 0, max_depth, &mut out)?;
    Ok(out)
}

/// Canonicalize a JSON value into UTF-8 bytes (the exact SHA-256 hash input).
pub fn canonical_bytes(value: &Value, max_depth: usize) -> Result<Vec<u8>, CanonicalJsonError> {
    Ok(canonical_json(value, max_depth)?.into_bytes())
}

fn write_value(
    v: &Value,
    depth: usize,
    max_depth: usize,
    out: &mut String,
) -> Result<(), CanonicalJsonError> {
    if depth > max_depth {
        return Err(CanonicalJsonError(format!(
            "nesting exceeds max depth {max_depth}"
        )));
    }
    match v {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => {
            out.push_str(&serde_json::to_string(s).map_err(|e| CanonicalJsonError(e.to_string()))?);
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_value(item, depth + 1, max_depth, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            let mut keys: Vec<&str> = map.keys().map(String::as_str).collect();
            keys.sort_by(|a, b| utf16_cmp(a, b));
            out.push('{');
            for (i, k) in keys.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(
                    &serde_json::to_string(k).map_err(|e| CanonicalJsonError(e.to_string()))?,
                );
                out.push(':');
                write_value(&map[*k], depth + 1, max_depth, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn key_order_is_irrelevant_values_preserved() {
        let a = canonical_json(&json!({"b": 1, "a": {"d": [1, 2], "c": "x"}}), usize::MAX).unwrap();
        let b = canonical_json(&json!({"a": {"c": "x", "d": [1, 2]}, "b": 1}), usize::MAX).unwrap();
        assert_eq!(a, b);
        assert!(a.contains("\"a\""));
        assert!(a.find("\"a\"").unwrap() < a.find("\"b\"").unwrap());
    }

    #[test]
    fn object_inside_array_canonicalized_array_order_kept() {
        let a = canonical_json(&json!([{"x": 1}, {"y": 2}]), usize::MAX).unwrap();
        let b = canonical_json(&json!([{"y": 2}, {"x": 1}]), usize::MAX).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn nesting_depth_enforced() {
        let deep = json!({"a": {"a": {"a": {"end": 1}}}});
        assert!(canonical_json(&deep, 2).is_err());
        assert!(canonical_json(&deep, 8).is_ok());
    }

    #[test]
    fn utf8_bytes_match_canonical_text() {
        let obj = json!({"z": [1, {"q": "q", "p": "p"}], "a": "あいう"});
        let text = canonical_json(&obj, usize::MAX).unwrap();
        let bytes = canonical_bytes(&obj, usize::MAX).unwrap();
        assert_eq!(String::from_utf8(bytes).unwrap(), text);
    }
}
