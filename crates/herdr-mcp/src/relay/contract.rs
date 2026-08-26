//! Relay Protocol v1 contract manifest hashing (canonical SHA-256 identity).
//!
//! Faithful port of `src/relay/contract.ts`. The contract is the ChatGPT-visible
//! MCP ABI of the workstation runtime. A stable `contract_hash` lets the edge
//! answer `initialize`/`tools/list` from a frozen manifest and lets edge and
//! workstation refuse activation when a runtime branch would silently change the
//! model-visible ABI.
//!
//! Canonical representation the hash covers:
//!   - the bare sorted TOOL ARRAY directly (no wrapping object);
//!   - every tool sorted by `name` (ascending, UTF-16 code-unit / byte order);
//!     tool-list order is intentionally NOT semantically meaningful;
//!   - each tool contributes ALL metadata as supplied (name, description,
//!     inputSchema, annotations, execution, …), not a fixed subset;
//!   - duplicate tool names are rejected;
//!   - schema-internal array order is PRESERVED (`required`, `enum`, `oneOf`,
//!     tuple `items`) because those arrays are semantically order-sensitive.
//!
//! The hash explicitly does NOT cover `contract_hash`, `contract_epoch`,
//! `runtime_version`, `git_commit`, or manifest metadata outside the tool
//! catalog.

use crate::relay::canonical_json::canonical_bytes;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const CONTRACT_HASH_PREFIX: &str = "sha256:";
/// "sha256:" + 64 hex chars.
pub const CONTRACT_HASH_LEN: usize = 7 + 64;
/// Default depth cap for canonicalizing contract tool lists (matches the TS
/// `computeContractHash(tools, 64)` call site).
pub const CONTRACT_MAX_DEPTH: usize = 64;

/// `sha256:<hex>` syntax check (no cryptographic verification).
pub fn is_contract_hash_shape(value: &Value) -> bool {
    let Some(s) = value.as_str() else {
        return false;
    };
    is_contract_hash_shape_str(s)
}

/// `sha256:<hex>` syntax check over a plain string.
pub fn is_contract_hash_shape_str(s: &str) -> bool {
    s.len() == CONTRACT_HASH_LEN
        && s.starts_with(CONTRACT_HASH_PREFIX)
        && s[7..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Normalize the tool list into the hash-covered canonical form: dedupe check
/// by name, then sort by name. Schema-internal arrays are untouched.
pub fn normalize_tools(tools: &[Value]) -> Result<Vec<Value>, String> {
    let mut seen = std::collections::HashSet::new();
    let mut normalized = Vec::with_capacity(tools.len());
    for t in tools {
        if !t.is_object() {
            return Err("contract: tool must be an object".to_owned());
        }
        let name = t
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| "contract: each tool must have a non-empty name".to_owned())?;
        if name.is_empty() {
            return Err("contract: each tool must have a non-empty name".to_owned());
        }
        if !seen.insert(name) {
            return Err(format!("contract: duplicate tool name '{name}'"));
        }
        normalized.push(t.clone());
    }
    normalized.sort_by(|a, b| {
        let an = a.get("name").and_then(Value::as_str).unwrap_or_default();
        let bn = b.get("name").and_then(Value::as_str).unwrap_or_default();
        an.encode_utf16().cmp(bn.encode_utf16())
    });
    Ok(normalized)
}

/// Compute the deterministic SHA-256 contract hash over the canonical ABI.
/// Output is `sha256:<lowercase hex>`.
pub fn compute_contract_hash(tools: &[Value]) -> Result<String, String> {
    let source = normalize_tools(tools)?;
    let bytes = canonical_bytes(&Value::Array(source), CONTRACT_MAX_DEPTH)
        .map_err(|e| format!("contract: cannot canonicalize tools: {e}"))?;
    let digest = Sha256::digest(&bytes);
    Ok(format!("{CONTRACT_HASH_PREFIX}{digest:x}"))
}

/// Verify a declared `sha256:<hex>` hash string against a tool list.
pub fn verify_contract_hash(declared_hash: &str, tools: &[Value]) -> bool {
    if !is_contract_hash_shape_str(declared_hash) {
        return false;
    }
    match compute_contract_hash(tools) {
        Ok(actual) => actual == declared_hash,
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn is_contract_hash_shape_accepts_hex_and_rejects_others() {
        assert!(is_contract_hash_shape_str(&format!(
            "sha256:{}",
            "ab".repeat(32)
        )));
        assert!(!is_contract_hash_shape_str(&format!(
            "sha256:{}",
            "ab".repeat(31)
        )));
        assert!(!is_contract_hash_shape_str(&format!(
            "sha256:{}",
            "AB".repeat(32)
        )));
        assert!(!is_contract_hash_shape_str("sha256:nothex"));
        assert!(!is_contract_hash_shape(&Value::Null));
        assert!(!is_contract_hash_shape(&json!(42)));
    }

    #[test]
    fn duplicate_tool_names_are_rejected() {
        let tools = json!([
            {"name": "dup", "description": "first"},
            {"name": "dup", "description": "second"}
        ]);
        let arr = tools.as_array().unwrap();
        assert!(compute_contract_hash(arr).is_err());
        assert!(normalize_tools(arr).is_err());
        assert!(!verify_contract_hash(
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            arr
        ));
    }

    #[test]
    fn hash_independent_of_tool_list_ordering_but_sensitive_to_schema_array_order() {
        let a = json!([
            {"name": "herdr_a", "description": "A", "inputSchema": {"required": ["x"], "properties": {"x": {"type": "string"}}}},
            {"name": "herdr_b", "description": "B", "inputSchema": {"properties": {"y": {"type": "number"}}}}
        ]);
        let a_arr = a.as_array().unwrap();
        let mut b = a_arr.clone();
        b.reverse();
        assert_eq!(
            compute_contract_hash(a_arr).unwrap(),
            compute_contract_hash(&b).unwrap()
        );

        let ordered = json!([{"name": "herdr_a", "inputSchema": {"required": ["a", "b"], "properties": {"a": {}, "b": {}}}}]);
        let reversed = json!([{"name": "herdr_a", "inputSchema": {"required": ["b", "a"], "properties": {"b": {}, "a": {}}}}]);
        assert_ne!(
            compute_contract_hash(ordered.as_array().unwrap()).unwrap(),
            compute_contract_hash(reversed.as_array().unwrap()).unwrap()
        );
    }

    #[test]
    fn hash_independent_of_object_key_insertion_order_within_tool() {
        let t1 = json!({"name": "t", "description": "d", "inputSchema": {"type": "object", "properties": {"a": {"type": "string"}}}});
        let t2 = json!({"inputSchema": {"properties": {"a": {"type": "string"}}, "type": "object"}, "description": "d", "name": "t"});
        assert_eq!(
            compute_contract_hash(&[t1]).unwrap(),
            compute_contract_hash(&[t2]).unwrap()
        );
    }

    #[test]
    fn annotations_change_the_hash() {
        let base = json!({"name": "t", "description": "d", "inputSchema": {"type": "object"}});
        let annotated = json!({"name": "t", "description": "d", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": true}});
        assert_ne!(
            compute_contract_hash(&[base]).unwrap(),
            compute_contract_hash(&[annotated]).unwrap()
        );
    }

    #[test]
    fn epoch2_public_contract_hash_is_exact() {
        // Embedded frozen public catalog; the canonical hash must be byte-exact.
        let catalog = crate::contract::tool_catalog().expect("embedded epoch2 tools");
        let expected = "sha256:7da23ad2ec8e7703d6380062126ba797218bde9e7711138c6b3e0ca6592efbf8";
        assert_eq!(compute_contract_hash(&catalog).unwrap(), expected);
        assert!(verify_contract_hash(expected, &catalog));
        // Order independence: a reversed catalog yields the same hash.
        let mut rev = catalog.clone();
        rev.reverse();
        assert_eq!(compute_contract_hash(&rev).unwrap(), expected);
    }
}
