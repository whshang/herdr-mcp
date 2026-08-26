//! Relay Protocol v1 strict frame/message validation.
//!
//! Faithful staged port of `src/relay/validation.ts`. The validator is pure:
//! it owns no socket/link/runtime lifecycle state and performs no production
//! mutation. Untrusted text follows the same two-stage defense as the
//! TypeScript oracle: raw UTF-8 byte gate before JSON parsing, then strict
//! structural/per-kind validation with stable rejection codes.

use crate::relay::canonical_json::canonical_json;
use crate::relay::protocol::{CORRELATED_KINDS, MESSAGE_KINDS, RELAY_PROTOCOL_VERSION};
use serde_json::{Map, Value};

pub const MAX_WORKSTATION_ID_LEN: usize = 64;
pub const MAX_REQUEST_ID_LEN: usize = 128;
pub const MAX_BOOT_ID_LEN: usize = 128;
pub const MAX_IDEMPOTENCY_KEY_LEN: usize = 128;
pub const MAX_OPERATION_LEN: usize = 256;
pub const MAX_LINK_VERSION_LEN: usize = 128;
pub const MAX_STRING_LEN: usize = 4096;
pub const MAX_SHA256_HASH_LEN: usize = 71;
pub const MAX_CAPABILITIES: usize = 32;
pub const MAX_CAPABILITY_LEN: usize = 128;
pub const MAX_ARGS_JSON_BYTES: usize = 256 * 1024;
pub const MAX_RESULT_JSON_BYTES: usize = 1024 * 1024;
pub const MAX_DETAILS_JSON_BYTES: usize = 64 * 1024;
pub const MAX_TRACE_JSON_BYTES: usize = 16 * 1024;
pub const MAX_NESTING_DEPTH: usize = 32;
pub const MAX_KEYS_PER_OBJECT: usize = 512;
pub const MAX_ITEMS_PER_ARRAY: usize = 4096;
pub const DEFAULT_MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MIN_TIMEOUT_MS: u64 = 1_000;
pub const MAX_TIMEOUT_MS: u64 = 60_000;
const MAX_SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;
const ID_GRAMMAR_DISPLAY: &str = "/^[A-Za-z0-9][A-Za-z0-9._:-]*$/";

pub type RelayValidationCode = &'static str;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayCheck {
    pub ok: bool,
    pub code: Option<RelayValidationCode>,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelayValidationOptions {
    pub max_frame_bytes: Option<usize>,
    pub max_args_bytes: Option<usize>,
    pub max_result_bytes: Option<usize>,
    pub max_details_bytes: Option<usize>,
    pub max_trace_bytes: Option<usize>,
    pub strict_unknown_fields: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalizedRelayValidationOptions {
    pub max_frame_bytes: usize,
    pub max_args_bytes: usize,
    pub max_result_bytes: usize,
    pub max_details_bytes: usize,
    pub max_trace_bytes: usize,
    pub strict_unknown_fields: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeBoundsOptions {
    pub max_depth: Option<usize>,
    pub max_keys: Option<usize>,
    pub max_items: Option<usize>,
    pub max_string_len: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParseResult {
    pub ok: bool,
    pub code: Option<RelayValidationCode>,
    pub reason: Option<String>,
    pub message: Option<Value>,
}

fn pass() -> RelayCheck {
    RelayCheck {
        ok: true,
        code: None,
        reason: None,
    }
}

fn fail(code: RelayValidationCode, reason: impl Into<String>) -> RelayCheck {
    RelayCheck {
        ok: false,
        code: Some(code),
        reason: Some(reason.into()),
    }
}

fn parse_fail(code: RelayValidationCode, reason: impl Into<String>) -> ParseResult {
    ParseResult {
        ok: false,
        code: Some(code),
        reason: Some(reason.into()),
        message: None,
    }
}

pub fn normalize_options(raw: Option<&RelayValidationOptions>) -> NormalizedRelayValidationOptions {
    NormalizedRelayValidationOptions {
        max_frame_bytes: raw
            .and_then(|value| value.max_frame_bytes)
            .unwrap_or(DEFAULT_MAX_FRAME_BYTES),
        max_args_bytes: raw
            .and_then(|value| value.max_args_bytes)
            .unwrap_or(MAX_ARGS_JSON_BYTES),
        max_result_bytes: raw
            .and_then(|value| value.max_result_bytes)
            .unwrap_or(MAX_RESULT_JSON_BYTES),
        max_details_bytes: raw
            .and_then(|value| value.max_details_bytes)
            .unwrap_or(MAX_DETAILS_JSON_BYTES),
        max_trace_bytes: raw
            .and_then(|value| value.max_trace_bytes)
            .unwrap_or(MAX_TRACE_JSON_BYTES),
        strict_unknown_fields: raw
            .and_then(|value| value.strict_unknown_fields)
            .unwrap_or(true),
    }
}

pub fn utf8_byte_length(text: &str) -> usize {
    text.len()
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

pub fn check_frame_bytes(raw: &str, max_bytes: usize) -> RelayCheck {
    let bytes = utf8_byte_length(raw);
    if bytes > max_bytes {
        return fail(
            "frame_too_large",
            format!("frame is {bytes} bytes; budget is {max_bytes}"),
        );
    }
    pass()
}

fn valid_identifier_str(value: &str, min_len: usize, max_len: usize) -> bool {
    let len = utf16_len(value);
    if len < min_len || len > max_len {
        return false;
    }
    let mut bytes = value.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric() {
        return false;
    }
    bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-'))
}

pub fn is_valid_identifier(value: &Value, min_len: usize, max_len: usize) -> bool {
    value
        .as_str()
        .is_some_and(|value| valid_identifier_str(value, min_len, max_len))
}

fn check_identifier(
    value: Option<&Value>,
    name: &str,
    min_len: usize,
    max_len: usize,
    code: RelayValidationCode,
) -> RelayCheck {
    let valid = value
        .and_then(Value::as_str)
        .is_some_and(|value| valid_identifier_str(value, min_len, max_len));
    if !valid {
        return fail(
            code,
            format!("{name} must match {ID_GRAMMAR_DISPLAY} within {min_len}..{max_len} chars"),
        );
    }
    pass()
}

pub fn check_workstation_id(value: &Value) -> RelayCheck {
    check_identifier(
        Some(value),
        "workstation_id",
        1,
        MAX_WORKSTATION_ID_LEN,
        "invalid_workstation_id",
    )
}

pub fn check_request_id(value: &Value) -> RelayCheck {
    check_identifier(
        Some(value),
        "request_id",
        1,
        MAX_REQUEST_ID_LEN,
        "invalid_request_id",
    )
}

pub fn check_boot_id(value: &Value) -> RelayCheck {
    check_identifier(
        Some(value),
        "boot_id",
        1,
        MAX_BOOT_ID_LEN,
        "invalid_boot_id",
    )
}

fn check_string(value: Option<&Value>, name: &str, max_len: usize) -> RelayCheck {
    let Some(text) = value.and_then(Value::as_str) else {
        return fail("invalid_string", format!("{name} must be a string"));
    };
    let len = utf16_len(text);
    if len == 0 {
        return fail("invalid_string", format!("{name} must not be empty"));
    }
    if len > max_len {
        return fail("string_too_long", format!("{name} exceeds {max_len} chars"));
    }
    pass()
}

fn check_string_or_null(value: Option<&Value>, name: &str, max_len: usize) -> RelayCheck {
    match value {
        Some(Value::Null) => pass(),
        None => fail("invalid_string", format!("{name} must be null or a string")),
        Some(value) => check_string(Some(value), name, max_len),
    }
}

fn check_optional_string_or_null(value: Option<&Value>, name: &str, max_len: usize) -> RelayCheck {
    match value {
        None => pass(),
        Some(value) => check_string_or_null(Some(value), name, max_len),
    }
}

fn check_number(value: Option<&Value>, name: &str, min: f64, max: f64) -> RelayCheck {
    let Some(number) = value.and_then(Value::as_f64) else {
        return fail("invalid_number", format!("{name} must be a finite number"));
    };
    if !number.is_finite() {
        return fail("invalid_number", format!("{name} must be a finite number"));
    }
    if number < min || number > max {
        return fail(
            "invalid_number",
            format!("{name} must be within {min}..{max}"),
        );
    }
    pass()
}

pub fn check_tree_bounds(value: &Value, raw: &TreeBoundsOptions) -> RelayCheck {
    let max_depth = raw.max_depth.unwrap_or(MAX_NESTING_DEPTH);
    let max_keys = raw.max_keys.unwrap_or(MAX_KEYS_PER_OBJECT);
    let max_items = raw.max_items.unwrap_or(MAX_ITEMS_PER_ARRAY);
    let max_string_len = raw.max_string_len.unwrap_or(MAX_STRING_LEN);

    fn walk(
        value: &Value,
        depth: usize,
        max_depth: usize,
        max_keys: usize,
        max_items: usize,
        max_string_len: usize,
    ) -> RelayCheck {
        if depth > max_depth {
            return fail("too_deep", format!("nesting exceeds {max_depth} levels"));
        }
        match value {
            Value::Null | Value::Bool(_) | Value::Number(_) => pass(),
            Value::String(text) => {
                if utf16_len(text) > max_string_len {
                    fail(
                        "string_too_long",
                        format!("string exceeds {max_string_len} chars"),
                    )
                } else {
                    pass()
                }
            }
            Value::Array(items) => {
                if items.len() > max_items {
                    return fail("too_many_items", format!("array exceeds {max_items} items"));
                }
                for item in items {
                    let check = walk(
                        item,
                        depth + 1,
                        max_depth,
                        max_keys,
                        max_items,
                        max_string_len,
                    );
                    if !check.ok {
                        return check;
                    }
                }
                pass()
            }
            Value::Object(object) => {
                if object.len() > max_keys {
                    return fail("too_many_keys", format!("object exceeds {max_keys} keys"));
                }
                for (key, item) in object {
                    if utf16_len(key) > max_string_len {
                        return fail(
                            "string_too_long",
                            format!("object key exceeds {max_string_len} chars"),
                        );
                    }
                    let check = walk(
                        item,
                        depth + 1,
                        max_depth,
                        max_keys,
                        max_items,
                        max_string_len,
                    );
                    if !check.ok {
                        return check;
                    }
                }
                pass()
            }
        }
    }

    walk(value, 0, max_depth, max_keys, max_items, max_string_len)
}

pub fn check_payload_budget(value: &Value, max_bytes: usize) -> RelayCheck {
    let bounds = check_tree_bounds(
        value,
        &TreeBoundsOptions {
            max_string_len: Some(MAX_STRING_LEN.max(max_bytes.saturating_mul(4))),
            ..TreeBoundsOptions::default()
        },
    );
    if !bounds.ok {
        return bounds;
    }
    let canonical = match canonical_json(value, MAX_NESTING_DEPTH) {
        Ok(value) => value,
        Err(error) => {
            return fail(
                "invalid_arguments",
                format!("payload is not canonical JSON: {error}"),
            );
        }
    };
    let size = utf8_byte_length(&canonical);
    if size > max_bytes {
        return fail(
            "payload_too_large",
            format!("payload is {size} bytes; budget is {max_bytes}"),
        );
    }
    pass()
}

fn kind_fields(kind: &str) -> &'static [&'static str] {
    match kind {
        "hello" => &[
            "protocol_version",
            "kind",
            "workstation_id",
            "boot_id",
            "link_version",
            "connected_at_ms",
            "capabilities",
            "runtime",
        ],
        "hello_ack" => &[
            "protocol_version",
            "kind",
            "workstation_id",
            "ok",
            "server_version",
            "edge_deployment_id",
            "capabilities",
            "reconnect",
            "resume",
            "completed",
            "code",
            "message",
        ],
        "heartbeat" => &[
            "protocol_version",
            "kind",
            "workstation_id",
            "boot_id",
            "sent_at_ms",
            "link_uptime_ms",
            "active_requests",
            "runtime",
        ],
        "status" => &[
            "protocol_version",
            "kind",
            "workstation_id",
            "query",
            "boot_id",
            "runtime",
            "runtime_generation",
            "healthy",
            "health_details",
            "active_requests",
            "link_uptime_ms",
            "last_error",
            "sent_at_ms",
        ],
        "tool_request" => &[
            "protocol_version",
            "kind",
            "workstation_id",
            "request_id",
            "operation",
            "arguments",
            "timeout_ms",
            "contract_epoch",
            "contract_hash",
            "idempotency_key",
            "trace",
        ],
        "tool_result" => &[
            "protocol_version",
            "kind",
            "workstation_id",
            "request_id",
            "result",
            "served_at_ms",
            "runtime_generation",
            "transport_name",
        ],
        "tool_error" => &[
            "protocol_version",
            "kind",
            "workstation_id",
            "request_id",
            "code",
            "message",
            "details",
            "retryable",
            "delivery_state",
            "served_at_ms",
            "runtime_generation",
        ],
        "cancel" => &[
            "protocol_version",
            "kind",
            "workstation_id",
            "request_id",
            "reason",
        ],
        "cancel_ack" => &[
            "protocol_version",
            "kind",
            "workstation_id",
            "request_id",
            "accepted",
            "cancelled_at_ms",
            "reason",
        ],
        _ => &[],
    }
}

fn check_runtime_contract(value: Option<&Value>) -> RelayCheck {
    let Some(value) = value else {
        return pass();
    };
    let Some(runtime) = value.as_object() else {
        return fail("invalid_runtime", "runtime must be an object");
    };
    let check = check_string(
        runtime.get("runtime_version"),
        "runtime.runtime_version",
        MAX_STRING_LEN,
    );
    if !check.ok {
        return check;
    }
    let check = check_string_or_null(
        runtime.get("runtime_commit"),
        "runtime.runtime_commit",
        MAX_STRING_LEN,
    );
    if !check.ok {
        return check;
    }
    let check = check_string_or_null(
        runtime.get("runtime_generation"),
        "runtime.runtime_generation",
        MAX_STRING_LEN,
    );
    if !check.ok {
        return check;
    }
    let check = check_number(
        runtime.get("contract_epoch"),
        "runtime.contract_epoch",
        0.0,
        1_000_000.0,
    );
    if !check.ok {
        return check;
    }
    let check = check_string_or_null(
        runtime.get("contract_hash"),
        "runtime.contract_hash",
        MAX_SHA256_HASH_LEN,
    );
    if !check.ok {
        return check;
    }
    let check = check_string_or_null(
        runtime.get("herdr_version"),
        "runtime.herdr_version",
        MAX_STRING_LEN,
    );
    if !check.ok {
        return check;
    }
    check_string_or_null(
        runtime.get("herdr_protocol"),
        "runtime.herdr_protocol",
        MAX_STRING_LEN,
    )
}

fn check_capabilities(value: Option<&Value>) -> RelayCheck {
    let Some(value) = value else {
        return pass();
    };
    let Some(items) = value.as_array() else {
        return fail("invalid_capabilities", "capabilities must be an array");
    };
    if items.len() > MAX_CAPABILITIES {
        return fail(
            "too_many_items",
            format!("capabilities exceed {MAX_CAPABILITIES}"),
        );
    }
    for capability in items {
        let Some(capability) = capability.as_str() else {
            return fail("invalid_capabilities", "capabilities must be strings");
        };
        let len = utf16_len(capability);
        if len == 0 || len > MAX_CAPABILITY_LEN {
            return fail(
                "invalid_capabilities",
                format!("capability exceeds {MAX_CAPABILITY_LEN} chars"),
            );
        }
    }
    pass()
}

fn missing_request_id_check() -> RelayCheck {
    check_identifier(
        None,
        "request_id",
        1,
        MAX_REQUEST_ID_LEN,
        "invalid_request_id",
    )
}

fn check_resume_summary(value: &Value) -> RelayCheck {
    let Some(items) = value.as_array() else {
        return fail("invalid_result", "resume must be an array");
    };
    if items.len() > 4096 {
        return fail("too_many_items", "resume exceeds 4096 entries");
    }
    for item in items {
        if item.is_null() || (!item.is_object() && !item.is_array()) {
            return fail("invalid_result", "resume entries must be objects");
        }
        // JavaScript arrays satisfy `typeof item === "object"`; a parsed JSON
        // array has no `request_id` property, so the TS oracle reaches the
        // request-id validator rather than the object-shape rejection.
        let Some(record) = item.as_object() else {
            return missing_request_id_check();
        };
        let request_id = check_identifier(
            record.get("request_id"),
            "request_id",
            1,
            MAX_REQUEST_ID_LEN,
            "invalid_request_id",
        );
        if !request_id.ok {
            return request_id;
        }
        let operation = check_string(
            record.get("operation"),
            "resume.operation",
            MAX_OPERATION_LEN,
        );
        if !operation.ok {
            return operation;
        }
        let state = check_string(record.get("state"), "resume.state", MAX_STRING_LEN);
        if !state.ok {
            return state;
        }
        match record.get("state").and_then(Value::as_str) {
            Some("queued" | "sent" | "settled") => {}
            _ => return fail("invalid_enum", "resume.state must be queued|sent|settled"),
        }
        let deadline = check_number(
            record.get("deadline_ms"),
            "resume.deadline_ms",
            0.0,
            MAX_SAFE_INTEGER,
        );
        if !deadline.ok {
            return deadline;
        }
    }
    pass()
}

fn validate_required_envelope(value: &Value) -> Result<(&Map<String, Value>, &str), RelayCheck> {
    let Some(object) = value.as_object() else {
        return Err(fail("not_object", "message must be a JSON object"));
    };
    let Some(protocol) = object.get("protocol_version") else {
        return Err(fail(
            "missing_protocol_version",
            "protocol_version is required",
        ));
    };
    if protocol.as_f64() != Some(RELAY_PROTOCOL_VERSION as f64) {
        return Err(fail(
            "unsupported_protocol_version",
            format!(
                "unsupported protocol_version {} (relay v1 expects {RELAY_PROTOCOL_VERSION})",
                js_display(protocol)
            ),
        ));
    }
    let Some(kind_value) = object.get("kind") else {
        return Err(fail("missing_kind", "kind is required"));
    };
    let Some(kind) = kind_value.as_str() else {
        return Err(fail(
            "unknown_kind",
            format!("unknown message kind {}", js_display(kind_value)),
        ));
    };
    if !MESSAGE_KINDS.contains(&kind) {
        return Err(fail("unknown_kind", format!("unknown message kind {kind}")));
    }
    if !object.contains_key("workstation_id") {
        return Err(fail("missing_workstation_id", "workstation_id is required"));
    }
    let workstation = check_identifier(
        object.get("workstation_id"),
        "workstation_id",
        1,
        MAX_WORKSTATION_ID_LEN,
        "invalid_workstation_id",
    );
    if !workstation.ok {
        return Err(workstation);
    }
    Ok((object, kind))
}

fn js_display(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => value.clone(),
        Value::Array(_) => "object".to_owned(),
        Value::Object(_) => "object".to_owned(),
    }
}

pub fn validate_relay_message(
    value: &Value,
    raw_options: Option<&RelayValidationOptions>,
) -> RelayCheck {
    let options = normalize_options(raw_options);
    let (object, kind) = match validate_required_envelope(value) {
        Ok(result) => result,
        Err(check) => return check,
    };

    let is_correlated = CORRELATED_KINDS.contains(&kind);
    let has_request_id = object.contains_key("request_id");
    if is_correlated && !has_request_id {
        return fail("missing_request_id", format!("{kind} requires request_id"));
    }
    if !is_correlated && has_request_id {
        return fail(
            "unexpected_request_id",
            format!("{kind} must not carry request_id (control message)"),
        );
    }
    if has_request_id {
        let request_id = check_identifier(
            object.get("request_id"),
            "request_id",
            1,
            MAX_REQUEST_ID_LEN,
            "invalid_request_id",
        );
        if !request_id.ok {
            return request_id;
        }
    }

    if options.strict_unknown_fields {
        let allowed = kind_fields(kind);
        for key in object.keys() {
            if !allowed.contains(&key.as_str()) {
                return fail(
                    "unknown_field",
                    format!("unknown field '{key}' for kind '{kind}'"),
                );
            }
        }
    }

    match kind {
        "hello" => {
            let boot = check_identifier(
                object.get("boot_id"),
                "boot_id",
                1,
                MAX_BOOT_ID_LEN,
                "invalid_boot_id",
            );
            if !boot.ok {
                return boot;
            }
            let link_version = check_string(
                object.get("link_version"),
                "link_version",
                MAX_LINK_VERSION_LEN,
            );
            if !link_version.ok {
                return link_version;
            }
            let capabilities = check_capabilities(object.get("capabilities"));
            if !capabilities.ok {
                return capabilities;
            }
            check_runtime_contract(object.get("runtime"))
        }
        "hello_ack" => {
            let Some(ok) = object.get("ok").and_then(Value::as_bool) else {
                return fail("invalid_boolean", "hello_ack.ok must be a boolean");
            };
            if !ok {
                let code = check_string(object.get("code"), "hello_ack.code", 128);
                if !code.ok {
                    return code;
                }
                return check_string(object.get("message"), "hello_ack.message", MAX_STRING_LEN);
            }
            for field in ["server_version", "edge_deployment_id"] {
                if object.contains_key(field) {
                    let check = check_string(object.get(field), &format!("hello_ack.{field}"), 128);
                    if !check.ok {
                        return check;
                    }
                }
            }
            let capabilities = check_capabilities(object.get("capabilities"));
            if !capabilities.ok {
                return capabilities;
            }
            if let Some(reconnect) = object.get("reconnect")
                && !reconnect.is_boolean()
            {
                return fail("invalid_boolean", "hello_ack.reconnect must be a boolean");
            }
            if let Some(resume) = object.get("resume") {
                let check = check_resume_summary(resume);
                if !check.ok {
                    return check;
                }
            }
            if let Some(completed) = object.get("completed") {
                let Some(items) = completed.as_array() else {
                    return fail(
                        "too_many_items",
                        "completed exceeds 512 entries or is not an array",
                    );
                };
                if items.len() > 512 {
                    return fail(
                        "too_many_items",
                        "completed exceeds 512 entries or is not an array",
                    );
                }
                for id in items {
                    let check = check_request_id(id);
                    if !check.ok {
                        return check;
                    }
                }
            }
            pass()
        }
        "heartbeat" => {
            let boot = check_identifier(
                object.get("boot_id"),
                "boot_id",
                1,
                MAX_BOOT_ID_LEN,
                "invalid_boot_id",
            );
            if !boot.ok {
                return boot;
            }
            let sent = check_number(
                object.get("sent_at_ms"),
                "sent_at_ms",
                0.0,
                MAX_SAFE_INTEGER,
            );
            if !sent.ok {
                return sent;
            }
            let active = check_number(
                object.get("active_requests"),
                "active_requests",
                0.0,
                1_000_000.0,
            );
            if !active.ok {
                return active;
            }
            check_runtime_contract(object.get("runtime"))
        }
        "status" => {
            if let Some(query) = object.get("query")
                && !query.is_boolean()
            {
                return fail("invalid_boolean", "status.query must be a boolean");
            }
            let runtime = check_runtime_contract(object.get("runtime"));
            if !runtime.ok {
                return runtime;
            }
            for field in ["runtime_generation", "health_details", "last_error"] {
                let check = check_optional_string_or_null(
                    object.get(field),
                    &format!("status.{field}"),
                    MAX_STRING_LEN,
                );
                if !check.ok {
                    return check;
                }
            }
            if let Some(healthy) = object.get("healthy")
                && !healthy.is_boolean()
            {
                return fail("invalid_boolean", "status.healthy must be a boolean");
            }
            if object.contains_key("active_requests") {
                let check = check_number(
                    object.get("active_requests"),
                    "active_requests",
                    0.0,
                    1_000_000.0,
                );
                if !check.ok {
                    return check;
                }
            }
            for field in ["link_uptime_ms", "sent_at_ms"] {
                if object.contains_key(field) {
                    let check = check_number(
                        object.get(field),
                        &format!("status.{field}"),
                        0.0,
                        MAX_SAFE_INTEGER,
                    );
                    if !check.ok {
                        return check;
                    }
                }
            }
            if let Some(boot_id) = object.get("boot_id") {
                let check = check_boot_id(boot_id);
                if !check.ok {
                    return check;
                }
            }
            pass()
        }
        "tool_request" => {
            let operation = check_string(object.get("operation"), "operation", MAX_OPERATION_LEN);
            if !operation.ok {
                return operation;
            }
            if object.contains_key("timeout_ms") {
                let check = check_number(
                    object.get("timeout_ms"),
                    "timeout_ms",
                    MIN_TIMEOUT_MS as f64,
                    MAX_TIMEOUT_MS as f64,
                );
                if !check.ok {
                    return check;
                }
            }
            if object.contains_key("contract_epoch") {
                let check = check_number(
                    object.get("contract_epoch"),
                    "contract_epoch",
                    0.0,
                    1_000_000.0,
                );
                if !check.ok {
                    return check;
                }
            }
            if object.contains_key("contract_hash") {
                let check = check_string(
                    object.get("contract_hash"),
                    "contract_hash",
                    MAX_SHA256_HASH_LEN,
                );
                if !check.ok {
                    return check;
                }
            }
            if let Some(idempotency_key) = object.get("idempotency_key")
                && !is_valid_identifier(idempotency_key, 1, MAX_IDEMPOTENCY_KEY_LEN)
            {
                return fail(
                    "invalid_id",
                    "idempotency_key must match identifier grammar within 1..128 chars",
                );
            }
            if let Some(arguments) = object.get("arguments") {
                if !arguments.is_object() {
                    return fail("invalid_arguments", "arguments must be an object");
                }
                let check = check_payload_budget(arguments, options.max_args_bytes);
                if !check.ok {
                    return check;
                }
            }
            if let Some(trace) = object.get("trace") {
                if !trace.is_object() {
                    return fail("invalid_result", "trace must be an object");
                }
                let check = check_payload_budget(trace, options.max_trace_bytes);
                if !check.ok {
                    return check;
                }
            }
            pass()
        }
        "tool_result" => {
            if let Some(result) = object.get("result") {
                let check = check_payload_budget(result, options.max_result_bytes);
                if !check.ok {
                    return check;
                }
            }
            let served = check_number(
                object.get("served_at_ms"),
                "served_at_ms",
                0.0,
                MAX_SAFE_INTEGER,
            );
            if !served.ok {
                return served;
            }
            let generation = check_optional_string_or_null(
                object.get("runtime_generation"),
                "runtime_generation",
                MAX_STRING_LEN,
            );
            if !generation.ok {
                return generation;
            }
            check_optional_string_or_null(object.get("transport_name"), "transport_name", 128)
        }
        "tool_error" => {
            let code = check_string(object.get("code"), "code", 128);
            if !code.ok {
                return code;
            }
            if object.contains_key("message") {
                let message = check_string(object.get("message"), "message", MAX_STRING_LEN);
                if !message.ok {
                    return message;
                }
            }
            if let Some(details) = object.get("details") {
                let check = check_payload_budget(details, options.max_details_bytes);
                if !check.ok {
                    return check;
                }
            }
            if !object.get("retryable").is_some_and(Value::is_boolean) {
                return fail("invalid_boolean", "tool_error.retryable must be a boolean");
            }
            if let Some(delivery_state) = object.get("delivery_state") {
                let check = check_string(Some(delivery_state), "delivery_state", 32);
                if !check.ok {
                    return check;
                }
                match delivery_state.as_str() {
                    Some("not_delivered" | "delivery_unknown" | "delivered") => {}
                    _ => {
                        return fail(
                            "invalid_enum",
                            "delivery_state must be not_delivered|delivery_unknown|delivered",
                        );
                    }
                }
            }
            if object.contains_key("served_at_ms") {
                let check = check_number(
                    object.get("served_at_ms"),
                    "served_at_ms",
                    0.0,
                    MAX_SAFE_INTEGER,
                );
                if !check.ok {
                    return check;
                }
            }
            check_optional_string_or_null(
                object.get("runtime_generation"),
                "runtime_generation",
                MAX_STRING_LEN,
            )
        }
        "cancel" => {
            if object.contains_key("reason") {
                return check_string(object.get("reason"), "reason", MAX_STRING_LEN);
            }
            pass()
        }
        "cancel_ack" => {
            if !object.get("accepted").is_some_and(Value::is_boolean) {
                return fail("invalid_boolean", "cancel_ack.accepted must be a boolean");
            }
            let cancelled = check_number(
                object.get("cancelled_at_ms"),
                "cancelled_at_ms",
                0.0,
                MAX_SAFE_INTEGER,
            );
            if !cancelled.ok {
                return cancelled;
            }
            check_optional_string_or_null(object.get("reason"), "reason", MAX_STRING_LEN)
        }
        _ => unreachable!("kind validated before dispatch"),
    }
}

pub fn parse_relay_frame(raw: &str, raw_options: Option<&RelayValidationOptions>) -> ParseResult {
    let options = normalize_options(raw_options);
    let gate = check_frame_bytes(raw, options.max_frame_bytes);
    if !gate.ok {
        return parse_fail(
            gate.code.expect("failed gate has code"),
            gate.reason.expect("failed gate has reason"),
        );
    }
    let parsed: Value = match serde_json::from_str(raw) {
        Ok(value) => value,
        Err(_) => return parse_fail("not_json", "frame is not valid JSON"),
    };
    let check = validate_relay_message(&parsed, raw_options);
    if !check.ok {
        return parse_fail(
            check.code.expect("failed validation has code"),
            check.reason.expect("failed validation has reason"),
        );
    }
    ParseResult {
        ok: true,
        code: None,
        reason: None,
        message: Some(parsed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn base(kind: &str) -> Value {
        json!({"protocol_version": 1, "kind": kind, "workstation_id": "w1"})
    }

    fn with_fields(mut value: Value, fields: &[(&str, Value)]) -> Value {
        let object = value.as_object_mut().expect("base object");
        for (key, item) in fields {
            object.insert((*key).to_owned(), item.clone());
        }
        value
    }

    fn code(check: &RelayCheck) -> Option<&'static str> {
        check.code
    }

    #[test]
    fn defaults_match_typescript_oracle() {
        let options = normalize_options(None);
        assert_eq!(options.max_frame_bytes, 1024 * 1024);
        assert_eq!(options.max_args_bytes, 256 * 1024);
        assert_eq!(options.max_result_bytes, 1024 * 1024);
        assert_eq!(options.max_details_bytes, 64 * 1024);
        assert_eq!(options.max_trace_bytes, 16 * 1024);
        assert!(options.strict_unknown_fields);
    }

    #[test]
    fn utf8_frame_gate_runs_before_json_parse() {
        assert!(check_frame_bytes("é", 2).ok);
        assert_eq!(code(&check_frame_bytes("é", 1)), Some("frame_too_large"));
        let options = RelayValidationOptions {
            max_frame_bytes: Some(1),
            ..RelayValidationOptions::default()
        };
        let parsed = parse_relay_frame("{", Some(&options));
        assert_eq!(parsed.code, Some("not_json"));
        let parsed = parse_relay_frame("é{", Some(&options));
        assert_eq!(parsed.code, Some("frame_too_large"));
    }

    #[test]
    fn identifier_grammar_and_bounds_match_oracle() {
        for valid in ["w1", "ws-01.alpha:edge", "a"] {
            assert!(check_workstation_id(&json!(valid)).ok, "{valid}");
        }
        for invalid in ["", "-leading", "has spaces", "!!"] {
            assert!(!check_workstation_id(&json!(invalid)).ok, "{invalid}");
        }
        assert!(check_request_id(&json!("a".repeat(128))).ok);
        assert!(!check_request_id(&json!("a".repeat(129))).ok);
        assert!(!check_workstation_id(&json!(42)).ok);
    }

    #[test]
    fn utf16_string_length_matches_javascript_length() {
        let astral = "😀";
        assert_eq!(utf16_len(astral), 2);
        let value = Value::String(astral.repeat(3));
        let check = check_tree_bounds(
            &value,
            &TreeBoundsOptions {
                max_string_len: Some(5),
                ..TreeBoundsOptions::default()
            },
        );
        assert_eq!(check.code, Some("string_too_long"));
    }

    #[test]
    fn tree_depth_key_item_and_string_bounds_are_enforced() {
        let deep = json!({"a": {"a": {"a": {"end": 1}}}});
        assert_eq!(
            check_tree_bounds(
                &deep,
                &TreeBoundsOptions {
                    max_depth: Some(2),
                    ..TreeBoundsOptions::default()
                },
            )
            .code,
            Some("too_deep")
        );
        assert_eq!(
            check_tree_bounds(
                &json!({"a": 1, "b": 2}),
                &TreeBoundsOptions {
                    max_keys: Some(1),
                    ..TreeBoundsOptions::default()
                },
            )
            .code,
            Some("too_many_keys")
        );
        assert_eq!(
            check_tree_bounds(
                &json!([1, 2]),
                &TreeBoundsOptions {
                    max_items: Some(1),
                    ..TreeBoundsOptions::default()
                },
            )
            .code,
            Some("too_many_items")
        );
    }

    #[test]
    fn canonical_payload_budget_is_key_order_independent() {
        let a = json!({"b": 1, "a": 2});
        let b = json!({"a": 2, "b": 1});
        assert_eq!(
            check_payload_budget(&a, 12).ok,
            check_payload_budget(&b, 12).ok
        );
        assert_eq!(check_payload_budget(&a, 1).code, Some("payload_too_large"));
    }

    #[test]
    fn correlated_request_id_rules_and_unknown_fields_match_oracle() {
        let request = with_fields(base("tool_request"), &[("operation", json!("x"))]);
        assert_eq!(
            validate_relay_message(&request, None).code,
            Some("missing_request_id")
        );
        let heartbeat = with_fields(
            base("heartbeat"),
            &[
                ("request_id", json!("r")),
                ("boot_id", json!("b")),
                ("sent_at_ms", json!(1)),
                ("active_requests", json!(0)),
            ],
        );
        assert_eq!(
            validate_relay_message(&heartbeat, None).code,
            Some("unexpected_request_id")
        );
        let heartbeat = with_fields(
            base("heartbeat"),
            &[
                ("boot_id", json!("b")),
                ("sent_at_ms", json!(1)),
                ("active_requests", json!(0)),
                ("hack", json!(1)),
            ],
        );
        assert_eq!(
            validate_relay_message(&heartbeat, None).code,
            Some("unknown_field")
        );
        let options = RelayValidationOptions {
            strict_unknown_fields: Some(false),
            ..RelayValidationOptions::default()
        };
        assert!(validate_relay_message(&heartbeat, Some(&options)).ok);
    }

    #[test]
    fn all_nine_message_kinds_have_valid_minimal_shapes() {
        let cases = [
            with_fields(
                base("hello"),
                &[
                    ("boot_id", json!("b")),
                    ("link_version", json!("1")),
                    ("capabilities", json!([])),
                ],
            ),
            with_fields(base("hello_ack"), &[("ok", json!(true))]),
            with_fields(
                base("heartbeat"),
                &[
                    ("boot_id", json!("b")),
                    ("sent_at_ms", json!(1)),
                    ("active_requests", json!(0)),
                ],
            ),
            with_fields(base("status"), &[("query", json!(true))]),
            with_fields(
                base("tool_request"),
                &[("request_id", json!("r")), ("operation", json!("op"))],
            ),
            with_fields(
                base("tool_result"),
                &[("request_id", json!("r")), ("served_at_ms", json!(1))],
            ),
            with_fields(
                base("tool_error"),
                &[
                    ("request_id", json!("r")),
                    ("code", json!("x")),
                    ("retryable", json!(false)),
                ],
            ),
            with_fields(base("cancel"), &[("request_id", json!("r"))]),
            with_fields(
                base("cancel_ack"),
                &[
                    ("request_id", json!("r")),
                    ("accepted", json!(true)),
                    ("cancelled_at_ms", json!(1)),
                ],
            ),
        ];
        for case in cases {
            let result = validate_relay_message(&case, None);
            assert!(result.ok, "{case}: {result:?}");
        }
    }

    #[test]
    fn runtime_capabilities_resume_and_completed_bounds_match_oracle() {
        let runtime = json!({
            "runtime_version": "0.4.0",
            "runtime_commit": null,
            "runtime_generation": null,
            "contract_epoch": 2,
            "contract_hash": null,
            "herdr_version": null,
            "herdr_protocol": null
        });
        let hello = with_fields(
            base("hello"),
            &[
                ("boot_id", json!("b")),
                ("link_version", json!("1")),
                ("capabilities", json!(["relay.request"])),
                ("runtime", runtime),
            ],
        );
        assert!(validate_relay_message(&hello, None).ok);
        let ack = with_fields(
            base("hello_ack"),
            &[
                ("ok", json!(true)),
                (
                    "resume",
                    json!([{"request_id":"r","operation":"op","state":"queued","deadline_ms":1}]),
                ),
                ("completed", json!(["r"])),
            ],
        );
        assert!(validate_relay_message(&ack, None).ok);
        let bad_resume = with_fields(
            base("hello_ack"),
            &[("ok", json!(true)), ("resume", json!([[]]))],
        );
        assert_eq!(
            validate_relay_message(&bad_resume, None).code,
            Some("invalid_request_id")
        );
    }

    #[test]
    fn tool_request_bounds_and_payload_shapes_match_oracle() {
        let low = with_fields(
            base("tool_request"),
            &[
                ("request_id", json!("r")),
                ("operation", json!("op")),
                ("timeout_ms", json!(0)),
            ],
        );
        assert_eq!(
            validate_relay_message(&low, None).code,
            Some("invalid_number")
        );
        let bad_id = with_fields(
            base("tool_request"),
            &[
                ("request_id", json!("r")),
                ("operation", json!("op")),
                ("idempotency_key", json!("bad id")),
            ],
        );
        assert_eq!(
            validate_relay_message(&bad_id, None).code,
            Some("invalid_id")
        );
        let bad_args = with_fields(
            base("tool_request"),
            &[
                ("request_id", json!("r")),
                ("operation", json!("op")),
                ("arguments", json!([])),
            ],
        );
        assert_eq!(
            validate_relay_message(&bad_args, None).code,
            Some("invalid_arguments")
        );
        let bad_trace = with_fields(
            base("tool_request"),
            &[
                ("request_id", json!("r")),
                ("operation", json!("op")),
                ("trace", json!([])),
            ],
        );
        assert_eq!(
            validate_relay_message(&bad_trace, None).code,
            Some("invalid_result")
        );
    }

    #[test]
    fn hello_ack_error_and_delivery_state_enum_are_validated() {
        let denied = with_fields(
            base("hello_ack"),
            &[
                ("ok", json!(false)),
                ("code", json!("auth_rejected")),
                ("message", json!("nope")),
            ],
        );
        assert!(validate_relay_message(&denied, None).ok);
        let missing = with_fields(base("hello_ack"), &[("ok", json!(false))]);
        assert_eq!(
            validate_relay_message(&missing, None).code,
            Some("invalid_string")
        );
        let bad = with_fields(
            base("tool_error"),
            &[
                ("request_id", json!("r")),
                ("code", json!("x")),
                ("retryable", json!(false)),
                ("delivery_state", json!("maybe")),
            ],
        );
        assert_eq!(
            validate_relay_message(&bad, None).code,
            Some("invalid_enum")
        );
    }

    #[test]
    fn frame_parse_rejects_non_json_non_object_version_and_kind() {
        assert_eq!(parse_relay_frame("{nope", None).code, Some("not_json"));
        assert_eq!(parse_relay_frame("null", None).code, Some("not_object"));
        assert_eq!(
            parse_relay_frame(r#"{"kind":"heartbeat","workstation_id":"w1"}"#, None).code,
            Some("missing_protocol_version")
        );
        assert_eq!(
            parse_relay_frame(
                r#"{"protocol_version":2,"kind":"hello","workstation_id":"w1"}"#,
                None
            )
            .code,
            Some("unsupported_protocol_version")
        );
        assert_eq!(
            parse_relay_frame(
                r#"{"protocol_version":1,"kind":"teleport","workstation_id":"w1"}"#,
                None
            )
            .code,
            Some("unknown_kind")
        );
    }

    #[test]
    fn payload_and_frame_overrides_are_honored() {
        let value = with_fields(
            base("tool_request"),
            &[
                ("request_id", json!("r")),
                ("operation", json!("op")),
                ("arguments", json!({"blob":"0123456789"})),
            ],
        );
        let options = RelayValidationOptions {
            max_args_bytes: Some(4),
            ..RelayValidationOptions::default()
        };
        assert_eq!(
            validate_relay_message(&value, Some(&options)).code,
            Some("payload_too_large")
        );
    }

    fn fixture_options(entry: &Value) -> RelayValidationOptions {
        let options = entry.get("options").and_then(Value::as_object);
        RelayValidationOptions {
            max_frame_bytes: options
                .and_then(|value| value.get("max_frame_bytes"))
                .and_then(Value::as_u64)
                .map(|value| value as usize),
            max_args_bytes: options
                .and_then(|value| value.get("max_args_bytes"))
                .and_then(Value::as_u64)
                .map(|value| value as usize),
            max_result_bytes: options
                .and_then(|value| value.get("max_result_bytes"))
                .and_then(Value::as_u64)
                .map(|value| value as usize),
            max_details_bytes: options
                .and_then(|value| value.get("max_details_bytes"))
                .and_then(Value::as_u64)
                .map(|value| value as usize),
            max_trace_bytes: options
                .and_then(|value| value.get("max_trace_bytes"))
                .and_then(Value::as_u64)
                .map(|value| value as usize),
            strict_unknown_fields: options
                .and_then(|value| value.get("strict_unknown_fields"))
                .and_then(Value::as_bool),
        }
    }

    fn assert_fixture_outcome(
        name: &str,
        expected: &Value,
        actual_ok: bool,
        actual_code: Option<&'static str>,
    ) {
        let expected_ok = expected.get("ok").and_then(Value::as_bool).unwrap();
        assert_eq!(actual_ok, expected_ok, "{name}: ok");
        if !expected_ok {
            let expected_code = expected.get("code").and_then(Value::as_str).unwrap();
            assert_eq!(actual_code, Some(expected_code), "{name}: code");
        }
    }

    #[test]
    fn shared_typescript_rust_parity_fixture_matches() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/relay-validation-parity.json"
        ))
        .expect("shared relay validation fixture");

        for entry in fixture
            .get("frame_cases")
            .and_then(Value::as_array)
            .expect("frame cases")
        {
            let name = entry.get("name").and_then(Value::as_str).unwrap();
            let raw = entry.get("raw").and_then(Value::as_str).unwrap();
            let options = fixture_options(entry);
            let result = parse_relay_frame(raw, Some(&options));
            assert_fixture_outcome(name, entry, result.ok, result.code);
        }

        for entry in fixture
            .get("message_cases")
            .and_then(Value::as_array)
            .expect("message cases")
        {
            let name = entry.get("name").and_then(Value::as_str).unwrap();
            let value = entry.get("value").expect("message value");
            let options = fixture_options(entry);
            let result = validate_relay_message(value, Some(&options));
            assert_fixture_outcome(name, entry, result.ok, result.code);
        }
    }
}
