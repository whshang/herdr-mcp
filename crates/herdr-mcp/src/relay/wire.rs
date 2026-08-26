//! Relay Protocol v1 validated wire adapter.
//!
//! This is the staged Rust counterpart of the canonical parts of
//! `src/link/relay-adapter.ts`: strict frame decode into typed Relay messages,
//! safe typed-message encode back to JSON, and pure canonical message builders.
//! It deliberately owns no socket, reconnect, auth, heartbeat timer, runtime
//! dispatch, or production link state.
//!
//! Safety invariants:
//! - inbound text is never mapped into typed protocol state before the Batch 1
//!   validator accepts it;
//! - outbound typed values are revalidated before serialization, so the typed
//!   model cannot bypass identifier/range/payload/unknown-field rules;
//! - the final serialized frame is checked against `max_frame_bytes` after
//!   JSON encoding, matching the Node link's final outbound byte gate;
//! - adapter-internal shape mismatches fail closed and are never emitted as a
//!   relay wire error code.

use crate::relay::protocol::{
    CancelAckMessage, CancelMessage, DeliveryState, HeartbeatMessage, HelloAckMessage,
    HelloAckOutcome, HelloMessage, MessageKind, OptionalNullable, RelayEnvelope, RelayMessage,
    ResumeState, ResumeSummary, RuntimeContractInfo, StatusFields, StatusMessage, ToolErrorMessage,
    ToolRequestMessage, ToolResultMessage,
};
use crate::relay::validation::{
    RelayValidationOptions, check_frame_bytes, normalize_options, parse_relay_frame,
    validate_relay_message,
};
use serde_json::{Map, Number, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayWireError {
    /// Stable validation code for rejected wire input/output, or the
    /// adapter-local `adapter_invariant` / `json_encode_error` code.
    pub code: String,
    pub reason: String,
}

impl RelayWireError {
    fn validation(code: Option<&'static str>, reason: Option<String>) -> Self {
        Self {
            code: code.unwrap_or("adapter_invariant").to_owned(),
            reason: reason.unwrap_or_else(|| "relay validation failed without a reason".to_owned()),
        }
    }

    fn invariant(reason: impl Into<String>) -> Self {
        Self {
            code: "adapter_invariant".to_owned(),
            reason: reason.into(),
        }
    }
}

fn object<'a>(value: &'a Value, context: &str) -> Result<&'a Map<String, Value>, RelayWireError> {
    value.as_object().ok_or_else(|| {
        RelayWireError::invariant(format!("{context} must be an object after validation"))
    })
}

fn required_string(
    map: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<String, RelayWireError> {
    map.get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            RelayWireError::invariant(format!("{context}.{key} must be a string after validation"))
        })
}

fn optional_string(
    map: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Option<String>, RelayWireError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(RelayWireError::invariant(format!(
            "{context}.{key} must be a string when present after validation"
        ))),
    }
}

fn required_nullable_string(
    map: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Option<String>, RelayWireError> {
    match map.get(key) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value.clone())),
        Some(_) => Err(RelayWireError::invariant(format!(
            "{context}.{key} must be null or string after validation"
        ))),
        None => Err(RelayWireError::invariant(format!(
            "{context}.{key} is required after validation"
        ))),
    }
}

fn optional_nullable_string(
    map: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<OptionalNullable<String>, RelayWireError> {
    match map.get(key) {
        None => Ok(OptionalNullable::Absent),
        Some(Value::Null) => Ok(OptionalNullable::Null),
        Some(Value::String(value)) => Ok(OptionalNullable::Value(value.clone())),
        Some(_) => Err(RelayWireError::invariant(format!(
            "{context}.{key} must be null or string when present after validation"
        ))),
    }
}

fn required_number(
    map: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Number, RelayWireError> {
    match map.get(key) {
        Some(Value::Number(value)) => Ok(value.clone()),
        _ => Err(RelayWireError::invariant(format!(
            "{context}.{key} must be a number after validation"
        ))),
    }
}

fn optional_number(
    map: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Option<Number>, RelayWireError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Number(value)) => Ok(Some(value.clone())),
        Some(_) => Err(RelayWireError::invariant(format!(
            "{context}.{key} must be a number when present after validation"
        ))),
    }
}

fn required_bool(
    map: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<bool, RelayWireError> {
    map.get(key).and_then(Value::as_bool).ok_or_else(|| {
        RelayWireError::invariant(format!(
            "{context}.{key} must be a boolean after validation"
        ))
    })
}

fn optional_bool(
    map: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Option<bool>, RelayWireError> {
    match map.get(key) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(RelayWireError::invariant(format!(
            "{context}.{key} must be a boolean when present after validation"
        ))),
    }
}

fn string_array(value: &Value, context: &str) -> Result<Vec<String>, RelayWireError> {
    value
        .as_array()
        .ok_or_else(|| {
            RelayWireError::invariant(format!("{context} must be an array after validation"))
        })?
        .iter()
        .map(|entry| {
            entry.as_str().map(ToOwned::to_owned).ok_or_else(|| {
                RelayWireError::invariant(format!(
                    "{context} entries must be strings after validation"
                ))
            })
        })
        .collect()
}

fn optional_string_array(
    map: &Map<String, Value>,
    key: &str,
    context: &str,
) -> Result<Option<Vec<String>>, RelayWireError> {
    match map.get(key) {
        None => Ok(None),
        Some(value) => string_array(value, &format!("{context}.{key}")).map(Some),
    }
}

fn runtime_contract_from_value(value: &Value) -> Result<RuntimeContractInfo, RelayWireError> {
    let map = object(value, "runtime")?;
    Ok(RuntimeContractInfo {
        runtime_version: required_string(map, "runtime_version", "runtime")?,
        runtime_commit: required_nullable_string(map, "runtime_commit", "runtime")?,
        runtime_generation: required_nullable_string(map, "runtime_generation", "runtime")?,
        contract_epoch: required_number(map, "contract_epoch", "runtime")?,
        contract_hash: required_nullable_string(map, "contract_hash", "runtime")?,
        herdr_version: required_nullable_string(map, "herdr_version", "runtime")?,
        herdr_protocol: required_nullable_string(map, "herdr_protocol", "runtime")?,
    })
}

fn optional_runtime_contract(
    map: &Map<String, Value>,
) -> Result<Option<RuntimeContractInfo>, RelayWireError> {
    match map.get("runtime") {
        None => Ok(None),
        Some(value) => runtime_contract_from_value(value).map(Some),
    }
}

fn resume_summary_from_value(value: &Value) -> Result<ResumeSummary, RelayWireError> {
    let map = object(value, "resume")?;
    let state = required_string(map, "state", "resume")?;
    let state = match state.as_str() {
        "queued" => ResumeState::Queued,
        "sent" => ResumeState::Sent,
        "settled" => ResumeState::Settled,
        _ => {
            return Err(RelayWireError::invariant(
                "resume.state was not a known state after validation",
            ));
        }
    };
    Ok(ResumeSummary {
        request_id: required_string(map, "request_id", "resume")?,
        operation: required_string(map, "operation", "resume")?,
        state,
        deadline_ms: required_number(map, "deadline_ms", "resume")?,
    })
}

fn optional_resume_array(
    map: &Map<String, Value>,
) -> Result<Option<Vec<ResumeSummary>>, RelayWireError> {
    match map.get("resume") {
        None => Ok(None),
        Some(Value::Array(entries)) => entries
            .iter()
            .map(resume_summary_from_value)
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(RelayWireError::invariant(
            "hello_ack.resume must be an array after validation",
        )),
    }
}

fn envelope(map: &Map<String, Value>, context: &str) -> Result<RelayEnvelope, RelayWireError> {
    Ok(RelayEnvelope::new(required_string(
        map,
        "workstation_id",
        context,
    )?))
}

fn message_from_validated_value(value: &Value) -> Result<RelayMessage, RelayWireError> {
    let map = object(value, "message")?;
    let kind = required_string(map, "kind", "message")?;
    let kind = MessageKind::parse(&kind)
        .ok_or_else(|| RelayWireError::invariant("message.kind was unknown after validation"))?;

    match kind {
        MessageKind::Hello => Ok(RelayMessage::Hello(HelloMessage {
            envelope: envelope(map, "hello")?,
            boot_id: required_string(map, "boot_id", "hello")?,
            link_version: required_string(map, "link_version", "hello")?,
            connected_at_ms: optional_number(map, "connected_at_ms", "hello")?,
            capabilities: string_array(
                map.get("capabilities").ok_or_else(|| {
                    RelayWireError::invariant("hello.capabilities is required after validation")
                })?,
                "hello.capabilities",
            )?,
            runtime: optional_runtime_contract(map)?,
        })),
        MessageKind::HelloAck => {
            let ok = required_bool(map, "ok", "hello_ack")?;
            let outcome = if ok {
                let completed = match map.get("completed") {
                    None => None,
                    Some(value) => Some(string_array(value, "hello_ack.completed")?),
                };
                HelloAckOutcome::Success {
                    server_version: optional_string(map, "server_version", "hello_ack")?,
                    edge_deployment_id: optional_string(map, "edge_deployment_id", "hello_ack")?,
                    capabilities: optional_string_array(map, "capabilities", "hello_ack")?,
                    reconnect: optional_bool(map, "reconnect", "hello_ack")?,
                    resume: optional_resume_array(map)?,
                    completed,
                }
            } else {
                HelloAckOutcome::Failure {
                    code: required_string(map, "code", "hello_ack")?,
                    message: required_string(map, "message", "hello_ack")?,
                }
            };
            Ok(RelayMessage::HelloAck(HelloAckMessage {
                envelope: envelope(map, "hello_ack")?,
                outcome,
            }))
        }
        MessageKind::Heartbeat => Ok(RelayMessage::Heartbeat(HeartbeatMessage {
            envelope: envelope(map, "heartbeat")?,
            boot_id: required_string(map, "boot_id", "heartbeat")?,
            sent_at_ms: required_number(map, "sent_at_ms", "heartbeat")?,
            link_uptime_ms: optional_number(map, "link_uptime_ms", "heartbeat")?,
            active_requests: required_number(map, "active_requests", "heartbeat")?,
            runtime: optional_runtime_contract(map)?,
        })),
        MessageKind::Status => Ok(RelayMessage::Status(StatusMessage {
            envelope: envelope(map, "status")?,
            fields: StatusFields {
                query: optional_bool(map, "query", "status")?,
                boot_id: optional_string(map, "boot_id", "status")?,
                runtime: optional_runtime_contract(map)?,
                runtime_generation: optional_nullable_string(map, "runtime_generation", "status")?,
                healthy: optional_bool(map, "healthy", "status")?,
                health_details: optional_nullable_string(map, "health_details", "status")?,
                active_requests: optional_number(map, "active_requests", "status")?,
                link_uptime_ms: optional_number(map, "link_uptime_ms", "status")?,
                last_error: optional_nullable_string(map, "last_error", "status")?,
                sent_at_ms: optional_number(map, "sent_at_ms", "status")?,
            },
        })),
        MessageKind::ToolRequest => {
            let arguments = match map.get("arguments") {
                None => None,
                Some(Value::Object(value)) => Some(value.clone()),
                Some(_) => {
                    return Err(RelayWireError::invariant(
                        "tool_request.arguments was not an object after validation",
                    ));
                }
            };
            let trace = match map.get("trace") {
                None => None,
                Some(Value::Object(value)) => Some(value.clone()),
                Some(_) => {
                    return Err(RelayWireError::invariant(
                        "tool_request.trace was not an object after validation",
                    ));
                }
            };
            Ok(RelayMessage::ToolRequest(ToolRequestMessage {
                envelope: envelope(map, "tool_request")?,
                request_id: required_string(map, "request_id", "tool_request")?,
                operation: required_string(map, "operation", "tool_request")?,
                arguments,
                timeout_ms: optional_number(map, "timeout_ms", "tool_request")?,
                contract_epoch: optional_number(map, "contract_epoch", "tool_request")?,
                contract_hash: optional_string(map, "contract_hash", "tool_request")?,
                idempotency_key: optional_string(map, "idempotency_key", "tool_request")?,
                trace,
            }))
        }
        MessageKind::ToolResult => Ok(RelayMessage::ToolResult(ToolResultMessage {
            envelope: envelope(map, "tool_result")?,
            request_id: required_string(map, "request_id", "tool_result")?,
            result: map.get("result").cloned(),
            served_at_ms: required_number(map, "served_at_ms", "tool_result")?,
            runtime_generation: optional_nullable_string(map, "runtime_generation", "tool_result")?,
            transport_name: optional_nullable_string(map, "transport_name", "tool_result")?,
        })),
        MessageKind::ToolError => {
            let delivery_state = match map.get("delivery_state") {
                None => None,
                Some(Value::String(value)) => {
                    Some(DeliveryState::parse(value.as_str()).ok_or_else(|| {
                        RelayWireError::invariant(
                            "tool_error.delivery_state was unknown after validation",
                        )
                    })?)
                }
                Some(_) => {
                    return Err(RelayWireError::invariant(
                        "tool_error.delivery_state was not a string after validation",
                    ));
                }
            };
            Ok(RelayMessage::ToolError(ToolErrorMessage {
                envelope: envelope(map, "tool_error")?,
                request_id: required_string(map, "request_id", "tool_error")?,
                code: required_string(map, "code", "tool_error")?,
                message: optional_string(map, "message", "tool_error")?,
                details: map.get("details").cloned(),
                retryable: required_bool(map, "retryable", "tool_error")?,
                delivery_state,
                served_at_ms: optional_number(map, "served_at_ms", "tool_error")?,
                runtime_generation: optional_nullable_string(
                    map,
                    "runtime_generation",
                    "tool_error",
                )?,
            }))
        }
        MessageKind::Cancel => Ok(RelayMessage::Cancel(CancelMessage {
            envelope: envelope(map, "cancel")?,
            request_id: required_string(map, "request_id", "cancel")?,
            reason: optional_string(map, "reason", "cancel")?,
        })),
        MessageKind::CancelAck => Ok(RelayMessage::CancelAck(CancelAckMessage {
            envelope: envelope(map, "cancel_ack")?,
            request_id: required_string(map, "request_id", "cancel_ack")?,
            accepted: required_bool(map, "accepted", "cancel_ack")?,
            cancelled_at_ms: required_number(map, "cancelled_at_ms", "cancel_ack")?,
            reason: optional_nullable_string(map, "reason", "cancel_ack")?,
        })),
    }
}

/// Validate and map a parsed JSON value into the typed Relay Protocol model.
pub fn decode_relay_value(
    value: &Value,
    options: Option<&RelayValidationOptions>,
) -> Result<RelayMessage, RelayWireError> {
    let check = validate_relay_message(value, options);
    if !check.ok {
        return Err(RelayWireError::validation(check.code, check.reason));
    }
    message_from_validated_value(value)
}

/// Apply the raw UTF-8 frame gate, parse JSON, validate Relay v1, then map the
/// accepted value into the typed protocol model.
pub fn decode_relay_frame(
    raw: &str,
    options: Option<&RelayValidationOptions>,
) -> Result<RelayMessage, RelayWireError> {
    let parsed = parse_relay_frame(raw, options);
    if !parsed.ok {
        return Err(RelayWireError::validation(parsed.code, parsed.reason));
    }
    let value = parsed
        .message
        .ok_or_else(|| RelayWireError::invariant("successful parse returned no message"))?;
    message_from_validated_value(&value)
}

/// Revalidate a typed message, serialize it to compact JSON, then apply the
/// final serialized-frame byte gate used by the Node link before socket send.
pub fn encode_relay_message(
    message: &RelayMessage,
    options: Option<&RelayValidationOptions>,
) -> Result<String, RelayWireError> {
    let value = message.to_value();
    let check = validate_relay_message(&value, options);
    if !check.ok {
        return Err(RelayWireError::validation(check.code, check.reason));
    }
    let raw = serde_json::to_string(&value).map_err(|error| RelayWireError {
        code: "json_encode_error".to_owned(),
        reason: format!("cannot encode Relay message: {error}"),
    })?;
    let normalized = normalize_options(options);
    let frame = check_frame_bytes(&raw, normalized.max_frame_bytes);
    if !frame.ok {
        return Err(RelayWireError::validation(frame.code, frame.reason));
    }
    Ok(raw)
}

/// Pure canonical hello builder. The caller supplies its monotonic/wall-clock
/// timestamp; this adapter never reads the clock itself.
pub fn build_hello_message(
    workstation_id: impl Into<String>,
    boot_id: impl Into<String>,
    link_version: impl Into<String>,
    capabilities: Vec<String>,
    runtime: RuntimeContractInfo,
    connected_at_ms: Number,
) -> HelloMessage {
    HelloMessage {
        envelope: RelayEnvelope::new(workstation_id),
        boot_id: boot_id.into(),
        link_version: link_version.into(),
        connected_at_ms: Some(connected_at_ms),
        capabilities,
        runtime: Some(runtime),
    }
}

pub fn build_heartbeat_message(
    workstation_id: impl Into<String>,
    boot_id: impl Into<String>,
    active_requests: Number,
    runtime: RuntimeContractInfo,
    link_uptime_ms: Number,
    sent_at_ms: Number,
) -> HeartbeatMessage {
    HeartbeatMessage {
        envelope: RelayEnvelope::new(workstation_id),
        boot_id: boot_id.into(),
        sent_at_ms,
        link_uptime_ms: Some(link_uptime_ms),
        active_requests,
        runtime: Some(runtime),
    }
}

// Preserve the canonical Node adapter argument order for one-to-one parity.
#[allow(clippy::too_many_arguments)]
pub fn build_status_report(
    workstation_id: impl Into<String>,
    runtime: RuntimeContractInfo,
    healthy: bool,
    health_details: Option<String>,
    active_requests: Number,
    link_uptime_ms: Number,
    last_error: Option<String>,
    sent_at_ms: Number,
) -> StatusMessage {
    let runtime_generation = match &runtime.runtime_generation {
        Some(value) => OptionalNullable::Value(value.clone()),
        None => OptionalNullable::Null,
    };
    StatusMessage {
        envelope: RelayEnvelope::new(workstation_id),
        fields: StatusFields {
            query: Some(false),
            boot_id: None,
            runtime: Some(runtime),
            runtime_generation,
            healthy: Some(healthy),
            health_details: health_details
                .map(OptionalNullable::Value)
                .unwrap_or(OptionalNullable::Null),
            active_requests: Some(active_requests),
            link_uptime_ms: Some(link_uptime_ms),
            last_error: last_error
                .map(OptionalNullable::Value)
                .unwrap_or(OptionalNullable::Null),
            sent_at_ms: Some(sent_at_ms),
        },
    }
}

pub fn build_tool_result_message(
    workstation_id: impl Into<String>,
    request_id: impl Into<String>,
    result: Option<Value>,
    served_at_ms: Number,
    runtime_generation: Option<String>,
    transport_name: Option<String>,
) -> ToolResultMessage {
    ToolResultMessage {
        envelope: RelayEnvelope::new(workstation_id),
        request_id: request_id.into(),
        result,
        served_at_ms,
        runtime_generation: runtime_generation
            .map(OptionalNullable::Value)
            .unwrap_or(OptionalNullable::Null),
        transport_name: transport_name
            .map(OptionalNullable::Value)
            .unwrap_or(OptionalNullable::Null),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn build_tool_error_message(
    workstation_id: impl Into<String>,
    request_id: impl Into<String>,
    code: impl Into<String>,
    retryable: bool,
    message: impl Into<String>,
    details: Option<Value>,
    delivery_state: Option<DeliveryState>,
    served_at_ms: Number,
    runtime_generation: Option<String>,
) -> ToolErrorMessage {
    ToolErrorMessage {
        envelope: RelayEnvelope::new(workstation_id),
        request_id: request_id.into(),
        code: code.into(),
        message: Some(message.into()),
        details,
        retryable,
        delivery_state,
        served_at_ms: Some(served_at_ms),
        runtime_generation: runtime_generation
            .map(OptionalNullable::Value)
            .unwrap_or(OptionalNullable::Null),
    }
}

pub fn build_cancel_ack_message(
    workstation_id: impl Into<String>,
    request_id: impl Into<String>,
    accepted: bool,
    cancelled_at_ms: Number,
    reason: Option<String>,
) -> CancelAckMessage {
    CancelAckMessage {
        envelope: RelayEnvelope::new(workstation_id),
        request_id: request_id.into(),
        accepted,
        cancelled_at_ms,
        reason: reason
            .map(OptionalNullable::Value)
            .unwrap_or(OptionalNullable::Null),
    }
}

/// Deterministic counterpart of Node `encodeCompactOversizedError`. The caller
/// supplies `served_at_ms` instead of hiding a wall-clock read in the adapter.
pub fn build_compact_oversized_error(
    workstation_id: impl Into<String>,
    request_id: impl Into<String>,
    runtime_generation: Option<String>,
    served_at_ms: Number,
) -> ToolErrorMessage {
    build_tool_error_message(
        workstation_id,
        request_id,
        "response_too_large",
        false,
        "response exceeds maxFrameBytes; paginate the result",
        None,
        Some(DeliveryState::Delivered),
        served_at_ms,
        runtime_generation,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::protocol::RelayMessage;
    use serde_json::json;

    fn n(value: u64) -> Number {
        Number::from(value)
    }

    fn runtime() -> RuntimeContractInfo {
        RuntimeContractInfo {
            runtime_version: "0.4.0-alpha.6".to_owned(),
            runtime_commit: None,
            runtime_generation: Some("gen-1".to_owned()),
            contract_epoch: n(2),
            contract_hash: None,
            herdr_version: None,
            herdr_protocol: None,
        }
    }

    fn fixture_string(value: &Value) -> String {
        value.as_str().expect("fixture string").to_owned()
    }

    fn fixture_number(value: &Value) -> Number {
        value.as_number().expect("fixture number").clone()
    }

    #[test]
    fn shared_node_adapter_builder_fixture_matches_rust_builders_exactly() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/relay-adapter-builders.json"
        ))
        .expect("shared relay adapter fixture");
        let runtime =
            runtime_contract_from_value(fixture.get("runtime_identity").expect("runtime_identity"))
                .expect("fixture runtime identity");
        let cases = fixture
            .get("builder_cases")
            .and_then(Value::as_array)
            .expect("builder_cases");
        assert_eq!(cases.len(), 7);

        for entry in cases {
            let name = entry.get("name").and_then(Value::as_str).expect("name");
            let builder = entry
                .get("builder")
                .and_then(Value::as_str)
                .expect("builder");
            let args = entry.get("args").and_then(Value::as_array).expect("args");
            let expected = entry.get("expected").expect("expected");

            let message = match builder {
                "encodeHelloMessage" => RelayMessage::Hello(build_hello_message(
                    fixture_string(&args[0]),
                    fixture_string(&args[1]),
                    fixture_string(&args[2]),
                    args[3]
                        .as_array()
                        .expect("capabilities")
                        .iter()
                        .map(fixture_string)
                        .collect(),
                    runtime.clone(),
                    fixture_number(&args[5]),
                )),
                "encodeHeartbeatMessage" => RelayMessage::Heartbeat(build_heartbeat_message(
                    fixture_string(&args[0]),
                    fixture_string(&args[1]),
                    fixture_number(&args[2]),
                    runtime.clone(),
                    fixture_number(&args[4]),
                    fixture_number(&args[5]),
                )),
                "encodeStatusReport" => RelayMessage::Status(build_status_report(
                    fixture_string(&args[0]),
                    runtime.clone(),
                    args[2].as_bool().expect("healthy"),
                    args[3].as_str().map(ToOwned::to_owned),
                    fixture_number(&args[4]),
                    fixture_number(&args[5]),
                    args[6].as_str().map(ToOwned::to_owned),
                    fixture_number(&args[7]),
                )),
                "encodeToolResultMessage" => RelayMessage::ToolResult(build_tool_result_message(
                    fixture_string(&args[0]),
                    fixture_string(&args[1]),
                    match args[2].as_str() {
                        Some("__UNDEFINED__") => None,
                        _ => Some(args[2].clone()),
                    },
                    fixture_number(&args[3]),
                    args[4].as_str().map(ToOwned::to_owned),
                    args[5].as_str().map(ToOwned::to_owned),
                )),
                "encodeToolErrorMessage" => RelayMessage::ToolError(build_tool_error_message(
                    fixture_string(&args[0]),
                    fixture_string(&args[1]),
                    fixture_string(&args[2]),
                    args[3].as_bool().expect("retryable"),
                    fixture_string(&args[4]),
                    Some(args[5].clone()),
                    args[6].as_str().and_then(DeliveryState::parse),
                    fixture_number(&args[7]),
                    args[8].as_str().map(ToOwned::to_owned),
                )),
                "encodeCancelAckMessage" => RelayMessage::CancelAck(build_cancel_ack_message(
                    fixture_string(&args[0]),
                    fixture_string(&args[1]),
                    args[2].as_bool().expect("accepted"),
                    fixture_number(&args[3]),
                    args[4].as_str().map(ToOwned::to_owned),
                )),
                other => panic!("unknown builder {other}"),
            };

            let actual = message.to_value();
            assert_eq!(actual, *expected, "{name}: exact builder output");
            assert!(
                validate_relay_message(&actual, None).ok,
                "{name}: validates"
            );

            let encoded = encode_relay_message(&message, None).expect("encode fixture message");
            let decoded = decode_relay_frame(&encoded, None).expect("decode fixture message");
            assert_eq!(decoded.to_value(), *expected, "{name}: wire round trip");
        }
    }

    #[test]
    fn shared_node_adapter_inbound_fixture_remains_valid_for_typed_decode() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/relay-adapter-builders.json"
        ))
        .expect("shared relay adapter fixture");
        let cases = fixture
            .get("inbound_cases")
            .and_then(Value::as_array)
            .expect("inbound_cases");
        assert_eq!(cases.len(), 2);
        for entry in cases {
            let name = entry.get("name").and_then(Value::as_str).expect("name");
            let input = entry.get("message").expect("message");
            let decoded = decode_relay_value(input, None)
                .unwrap_or_else(|error| panic!("{name}: decode failed: {error:?}"));
            assert_eq!(decoded.to_value(), *input, "{name}: typed round trip");
        }
    }

    #[test]
    fn shared_protocol_fixture_round_trips_through_validated_typed_decode() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/relay-protocol-shared.json"
        ))
        .expect("shared relay protocol fixture");
        let messages = fixture
            .get("representative_messages")
            .and_then(Value::as_array)
            .expect("representative messages");
        assert_eq!(messages.len(), 15);
        for entry in messages {
            let name = entry.get("name").and_then(Value::as_str).expect("name");
            let input = entry.get("value").expect("value");
            let decoded = decode_relay_value(input, None)
                .unwrap_or_else(|error| panic!("{name}: decode failed: {error:?}"));
            assert_eq!(decoded.to_value(), *input, "{name}: round trip");
        }
    }

    #[test]
    fn raw_frame_decode_keeps_validation_code_and_runs_byte_gate_before_parse() {
        let invalid = decode_relay_frame("{nope", None).unwrap_err();
        assert_eq!(invalid.code, "not_json");

        let options = RelayValidationOptions {
            max_frame_bytes: Some(1),
            ..RelayValidationOptions::default()
        };
        let oversized = decode_relay_frame("é{", Some(&options)).unwrap_err();
        assert_eq!(oversized.code, "frame_too_large");
    }

    #[test]
    fn outbound_encode_revalidates_typed_model_before_json() {
        let invalid = RelayMessage::Cancel(CancelMessage {
            envelope: RelayEnvelope::new("bad id"),
            request_id: "r1".to_owned(),
            reason: None,
        });
        let error = encode_relay_message(&invalid, None).unwrap_err();
        assert_eq!(error.code, "invalid_workstation_id");
    }

    #[test]
    fn outbound_encode_applies_final_serialized_frame_budget() {
        let message = RelayMessage::Cancel(CancelMessage {
            envelope: RelayEnvelope::new("w1"),
            request_id: "r1".to_owned(),
            reason: Some("x".repeat(40)),
        });
        let raw = serde_json::to_string(&message.to_value()).unwrap();
        let options = RelayValidationOptions {
            max_frame_bytes: Some(raw.len() - 1),
            ..RelayValidationOptions::default()
        };
        let error = encode_relay_message(&message, Some(&options)).unwrap_err();
        assert_eq!(error.code, "frame_too_large");
    }

    #[test]
    fn builders_match_node_adapter_null_and_required_field_semantics() {
        let rt = runtime();
        let hello = build_hello_message(
            "w1",
            "b1",
            "1",
            vec!["relay.request".to_owned()],
            rt.clone(),
            n(10),
        );
        assert_eq!(hello.connected_at_ms, Some(n(10)));
        assert!(validate_relay_message(&RelayMessage::Hello(hello).to_value(), None).ok);

        let status = build_status_report("w1", rt.clone(), true, None, n(0), n(20), None, n(30));
        let status_wire = RelayMessage::Status(status).to_value();
        assert_eq!(status_wire.get("query"), Some(&Value::Bool(false)));
        assert_eq!(status_wire.get("health_details"), Some(&Value::Null));
        assert_eq!(status_wire.get("last_error"), Some(&Value::Null));
        assert_eq!(status_wire.get("runtime_generation"), Some(&json!("gen-1")));

        let result = build_tool_result_message("w1", "r1", Some(Value::Null), n(40), None, None);
        let result_wire = RelayMessage::ToolResult(result).to_value();
        assert_eq!(result_wire.get("result"), Some(&Value::Null));
        assert_eq!(result_wire.get("runtime_generation"), Some(&Value::Null));
        assert_eq!(result_wire.get("transport_name"), Some(&Value::Null));

        let absent_result = build_tool_result_message("w1", "r2", None, n(41), None, None);
        let absent_result_wire = RelayMessage::ToolResult(absent_result).to_value();
        assert!(absent_result_wire.get("result").is_none());

        let cancel = build_cancel_ack_message("w1", "r1", true, n(50), None);
        assert_eq!(
            RelayMessage::CancelAck(cancel).to_value().get("reason"),
            Some(&Value::Null)
        );
    }

    #[test]
    fn compact_oversized_error_has_stable_fields_and_caller_supplied_time() {
        let message = build_compact_oversized_error("w1", "r1", None, n(99));
        let value = RelayMessage::ToolError(message).to_value();
        assert_eq!(value.get("code"), Some(&json!("response_too_large")));
        assert_eq!(value.get("retryable"), Some(&Value::Bool(false)));
        assert_eq!(value.get("delivery_state"), Some(&json!("delivered")));
        assert_eq!(value.get("served_at_ms"), Some(&json!(99)));
        assert_eq!(value.get("runtime_generation"), Some(&Value::Null));
        assert!(validate_relay_message(&value, None).ok);
    }

    #[test]
    fn hello_ack_resume_and_optional_nullable_fields_survive_decode_round_trip() {
        let value = json!({
            "protocol_version": 1,
            "kind": "hello_ack",
            "workstation_id": "w1",
            "ok": true,
            "reconnect": true,
            "resume": [{
                "request_id": "r1",
                "operation": "herdr_inspect",
                "state": "sent",
                "deadline_ms": 100
            }],
            "completed": ["r0"]
        });
        let decoded = decode_relay_value(&value, None).unwrap();
        assert_eq!(decoded.to_value(), value);
    }

    #[test]
    fn explicit_null_result_details_and_optional_nullable_fields_survive_decode() {
        let result = json!({
            "protocol_version": 1,
            "kind": "tool_result",
            "workstation_id": "w1",
            "request_id": "r1",
            "result": null,
            "served_at_ms": 1,
            "runtime_generation": null,
            "transport_name": null
        });
        assert_eq!(
            decode_relay_value(&result, None).unwrap().to_value(),
            result
        );

        let error = json!({
            "protocol_version": 1,
            "kind": "tool_error",
            "workstation_id": "w1",
            "request_id": "r1",
            "code": "runtime_error",
            "details": null,
            "retryable": false,
            "runtime_generation": null
        });
        assert_eq!(decode_relay_value(&error, None).unwrap().to_value(), error);
    }
}
