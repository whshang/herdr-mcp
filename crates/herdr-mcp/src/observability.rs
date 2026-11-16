//! Request observability for the local runtime wire boundary (1.2 S0).
//!
//! Every `tools/call` result envelope carries `transport`,
//! `timing_ms{queue,connect,execute}` and `request_id`, and the same fields are
//! written to the structured MCP activity record. The HTTP boundary owns the
//! scope and the final envelope; backend admission contributes the `connect`
//! phase from the same blocking thread through [`add_connect_ms`].
//!
//! Security: timing and counters hold milliseconds, a transport label and a
//! bounded opaque correlation id only. They never contain tool params, file
//! content, credentials or any other secret material (#938 boundary).

use serde_json::{Value, json};
use std::cell::Cell;

/// Served directly by this runtime (local stdio/HTTP caller).
pub const TRANSPORT_LOCAL: &str = "local";
/// Forwarded by the Edge relay (the wire frame carried a trusted Edge header).
pub const TRANSPORT_RELAY: &str = "relay";

/// Upper bound for an accepted correlation id, matching the Edge
/// `request_id` hygiene bound.
pub const MAX_REQUEST_ID_BYTES: usize = 256;

thread_local! {
    /// Admission/connect milliseconds for the request executing on this thread.
    static CONNECT_MS: Cell<u64> = const { Cell::new(0) };
}

/// Reset the per-request `connect` accumulator. Called once per wire request on
/// the blocking thread that executes it.
pub fn reset_connect_ms() {
    CONNECT_MS.with(|value| value.set(0));
}

/// Add one backend admission/connect phase. Saturating, so a long-lived process
/// can never overflow into a bogus duration.
pub fn add_connect_ms(ms: u64) {
    CONNECT_MS.with(|value| value.set(value.get().saturating_add(ms)));
}

/// Admission/connect milliseconds accumulated for the current wire request.
pub fn connect_ms() -> u64 {
    CONNECT_MS.with(Cell::get)
}

/// Transport label for one wire request: `relay` when the frame carried a
/// trusted Edge relay header, otherwise `local`.
pub fn transport_label(relay_header_present: bool) -> &'static str {
    if relay_header_present {
        TRANSPORT_RELAY
    } else {
        TRANSPORT_LOCAL
    }
}

/// Accept an opaque correlation id from the wire frame.
///
/// Rejected (returns `None`) when empty, over the byte bound, or containing a
/// control character, so a caller cannot smuggle log-injection or oversized
/// content into the envelope.
pub fn sanitize_request_id(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > MAX_REQUEST_ID_BYTES || value.chars().any(char::is_control)
    {
        return None;
    }
    Some(value.to_owned())
}

/// Correlation id from the JSON-RPC envelope when the frame carries no
/// `request_id` header: string ids verbatim, numeric ids stringified, anything
/// else absent.
pub fn request_id_from_jsonrpc_id(id: Option<&Value>) -> Option<String> {
    match id {
        Some(Value::String(value)) => sanitize_request_id(value),
        Some(Value::Number(value)) => sanitize_request_id(&value.to_string()),
        _ => None,
    }
}

/// The stable timing object attached to envelopes and activity records.
pub fn timing_ms(queue_ms: u64, connect_ms: u64, execute_ms: u64) -> Value {
    json!({
        "queue": queue_ms,
        "connect": connect_ms,
        "execute": execute_ms,
    })
}

/// Attach `transport`, `timing_ms` and `request_id` to one JSON-RPC response
/// when it is a tool result envelope. Non-tool responses are untouched.
pub fn attach_tool_observability(
    response: &mut Value,
    transport: &str,
    request_id: Option<&str>,
    timing: &Value,
) {
    let Some(result) = response.get_mut("result").and_then(Value::as_object_mut) else {
        return;
    };
    if !result.contains_key("content") {
        return;
    }
    result.insert("transport".to_owned(), json!(transport));
    result.insert("timing_ms".to_owned(), timing.clone());
    result.insert("request_id".to_owned(), json!(request_id));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connect_accumulator_is_saturating_and_resettable() {
        reset_connect_ms();
        assert_eq!(connect_ms(), 0);
        add_connect_ms(3);
        add_connect_ms(4);
        assert_eq!(connect_ms(), 7);
        reset_connect_ms();
        assert_eq!(connect_ms(), 0);
        add_connect_ms(u64::MAX);
        add_connect_ms(5);
        assert_eq!(connect_ms(), u64::MAX);
        reset_connect_ms();
    }

    #[test]
    fn request_ids_are_bounded_and_reject_control_content() {
        assert_eq!(sanitize_request_id("req-1").as_deref(), Some("req-1"));
        assert_eq!(sanitize_request_id("  req-2  ").as_deref(), Some("req-2"));
        assert_eq!(sanitize_request_id(""), None);
        assert_eq!(sanitize_request_id("   "), None);
        assert_eq!(sanitize_request_id("bad\nid"), None);
        assert_eq!(sanitize_request_id(&"x".repeat(257)), None);
        assert_eq!(
            sanitize_request_id(&"x".repeat(256)).map(|value| value.len()),
            Some(256)
        );
        assert_eq!(
            request_id_from_jsonrpc_id(Some(&json!(17))).as_deref(),
            Some("17")
        );
        assert_eq!(
            request_id_from_jsonrpc_id(Some(&json!("abc"))).as_deref(),
            Some("abc")
        );
        assert_eq!(request_id_from_jsonrpc_id(Some(&json!(null))), None);
    }

    #[test]
    fn envelope_fields_have_stable_types_and_never_carry_payload_content() {
        let timing = timing_ms(1, 2, 3);
        assert!(
            timing["queue"].is_u64() && timing["connect"].is_u64() && timing["execute"].is_u64()
        );
        let mut response = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "content": [{"type": "text", "text": "{\"ok\":true,\"token\":\"secret\"}"}],
                "isError": false
            }
        });
        attach_tool_observability(&mut response, TRANSPORT_RELAY, Some("req-9"), &timing);
        let result = &response["result"];
        assert_eq!(result["transport"], json!("relay"));
        assert_eq!(result["request_id"], json!("req-9"));
        assert_eq!(result["timing_ms"], timing);
        // The model-visible payload is untouched, and no timing value can carry
        // the payload content.
        assert_eq!(
            result["content"][0]["text"],
            json!("{\"ok\":true,\"token\":\"secret\"}")
        );
        let serialized = result["timing_ms"].to_string();
        assert!(!serialized.contains("secret"));
    }

    #[test]
    fn attach_only_touches_tool_result_envelopes() {
        let timing = timing_ms(0, 0, 0);
        let mut initialize =
            json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": "2025-11-25"}});
        attach_tool_observability(&mut initialize, TRANSPORT_LOCAL, None, &timing);
        assert!(initialize["result"].get("transport").is_none());
        assert_eq!(transport_label(false), "local");
        assert_eq!(transport_label(true), "relay");
    }
}
