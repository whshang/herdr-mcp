//! Relay Protocol v1 canonical wire constants and typed message model.
//!
//! Faithful staged port of `src/relay/protocol.ts`. This module intentionally
//! does not deserialize untrusted JSON directly: inbound text must first pass
//! `relay::validation`, after which callers may map the validated value into
//! typed protocol state. Outbound messages can be constructed here and encoded
//! to the exact snake_case wire shape without adding a serde derive dependency.

use serde_json::{Map, Number, Value};

/// Wire protocol version spoken by every Relay v1 participant.
pub const RELAY_PROTOCOL_VERSION: u64 = 1;
/// Human-readable protocol version used in diagnostics.
pub const RELAY_PROTOCOL_VERSION_STRING: &str = "1";

/// Canonical message-kind codes in the same order as the TypeScript oracle.
pub const MESSAGE_KINDS: [&str; 9] = [
    "hello",
    "hello_ack",
    "heartbeat",
    "status",
    "tool_request",
    "tool_result",
    "tool_error",
    "cancel",
    "cancel_ack",
];

/// Kinds that must carry a request_id.
pub const CORRELATED_KINDS: [&str; 5] = [
    "tool_request",
    "tool_result",
    "tool_error",
    "cancel",
    "cancel_ack",
];

/// Stable validation codes declared by the TypeScript Relay v1 contract.
pub const RELAY_VALIDATION_CODES: [&str; 30] = [
    "not_json",
    "not_object",
    "missing_protocol_version",
    "unsupported_protocol_version",
    "missing_kind",
    "unknown_kind",
    "missing_workstation_id",
    "invalid_workstation_id",
    "missing_request_id",
    "invalid_request_id",
    "unexpected_request_id",
    "invalid_boot_id",
    "invalid_id",
    "invalid_string",
    "invalid_number",
    "invalid_boolean",
    "invalid_enum",
    "unknown_field",
    "frame_too_large",
    "too_deep",
    "too_many_keys",
    "too_many_items",
    "string_too_long",
    "payload_too_large",
    "invalid_arguments",
    "invalid_result",
    "invalid_details",
    "invalid_capabilities",
    "invalid_runtime",
    "invalid_operation",
];

/// Reserved hello_ack failure-code set.
pub const HELLO_ACK_FAILURE_CODES: [&str; 7] = [
    "auth_rejected",
    "auth_expired",
    "session_invalid",
    "protocol_incompatible",
    "contract_mismatch",
    "workstation_mismatch",
    "internal_error",
];

/// Bounded resume states sent by the edge on reconnect.
pub const RESUME_STATES: [&str; 3] = ["queued", "sent", "settled"];

/// Delivery-state codes carried by tool_error.
pub const DELIVERY_CODES: [&str; 3] = ["not_delivered", "delivery_unknown", "delivered"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageKind {
    Hello,
    HelloAck,
    Heartbeat,
    Status,
    ToolRequest,
    ToolResult,
    ToolError,
    Cancel,
    CancelAck,
}

impl MessageKind {
    pub const ALL: [Self; 9] = [
        Self::Hello,
        Self::HelloAck,
        Self::Heartbeat,
        Self::Status,
        Self::ToolRequest,
        Self::ToolResult,
        Self::ToolError,
        Self::Cancel,
        Self::CancelAck,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Hello => "hello",
            Self::HelloAck => "hello_ack",
            Self::Heartbeat => "heartbeat",
            Self::Status => "status",
            Self::ToolRequest => "tool_request",
            Self::ToolResult => "tool_result",
            Self::ToolError => "tool_error",
            Self::Cancel => "cancel",
            Self::CancelAck => "cancel_ack",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "hello" => Some(Self::Hello),
            "hello_ack" => Some(Self::HelloAck),
            "heartbeat" => Some(Self::Heartbeat),
            "status" => Some(Self::Status),
            "tool_request" => Some(Self::ToolRequest),
            "tool_result" => Some(Self::ToolResult),
            "tool_error" => Some(Self::ToolError),
            "cancel" => Some(Self::Cancel),
            "cancel_ack" => Some(Self::CancelAck),
            _ => None,
        }
    }

    pub fn is_correlated(self) -> bool {
        matches!(
            self,
            Self::ToolRequest | Self::ToolResult | Self::ToolError | Self::Cancel | Self::CancelAck
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DeliveryState {
    NotDelivered,
    DeliveryUnknown,
    Delivered,
}

impl DeliveryState {
    pub const ALL: [Self; 3] = [Self::NotDelivered, Self::DeliveryUnknown, Self::Delivered];

    pub fn code(self) -> &'static str {
        match self {
            Self::NotDelivered => "not_delivered",
            Self::DeliveryUnknown => "delivery_unknown",
            Self::Delivered => "delivered",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "not_delivered" => Some(Self::NotDelivered),
            "delivery_unknown" => Some(Self::DeliveryUnknown),
            "delivered" => Some(Self::Delivered),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HelloAckFailureCode {
    AuthRejected,
    AuthExpired,
    SessionInvalid,
    ProtocolIncompatible,
    ContractMismatch,
    WorkstationMismatch,
    InternalError,
}

impl HelloAckFailureCode {
    pub const ALL: [Self; 7] = [
        Self::AuthRejected,
        Self::AuthExpired,
        Self::SessionInvalid,
        Self::ProtocolIncompatible,
        Self::ContractMismatch,
        Self::WorkstationMismatch,
        Self::InternalError,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AuthRejected => "auth_rejected",
            Self::AuthExpired => "auth_expired",
            Self::SessionInvalid => "session_invalid",
            Self::ProtocolIncompatible => "protocol_incompatible",
            Self::ContractMismatch => "contract_mismatch",
            Self::WorkstationMismatch => "workstation_mismatch",
            Self::InternalError => "internal_error",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResumeState {
    Queued,
    Sent,
    Settled,
}

impl ResumeState {
    pub const ALL: [Self; 3] = [Self::Queued, Self::Sent, Self::Settled];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Sent => "sent",
            Self::Settled => "settled",
        }
    }
}

/// Three-state representation for fields typed `?: string | null` in TS.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum OptionalNullable<T> {
    #[default]
    Absent,
    Null,
    Value(T),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayEnvelope {
    pub workstation_id: String,
}

impl RelayEnvelope {
    pub fn new(workstation_id: impl Into<String>) -> Self {
        Self {
            workstation_id: workstation_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeContractInfo {
    pub runtime_version: String,
    /// Required nullable fields serialize as explicit JSON null when None.
    pub runtime_commit: Option<String>,
    pub runtime_generation: Option<String>,
    pub contract_epoch: Number,
    pub contract_hash: Option<String>,
    pub herdr_version: Option<String>,
    pub herdr_protocol: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResumeSummary {
    pub request_id: String,
    pub operation: String,
    pub state: ResumeState,
    pub deadline_ms: Number,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HelloMessage {
    pub envelope: RelayEnvelope,
    pub boot_id: String,
    pub link_version: String,
    pub connected_at_ms: Option<Number>,
    pub capabilities: Vec<String>,
    pub runtime: Option<RuntimeContractInfo>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HelloAckOutcome {
    Success {
        server_version: Option<String>,
        edge_deployment_id: Option<String>,
        capabilities: Option<Vec<String>>,
        reconnect: Option<bool>,
        resume: Option<Vec<ResumeSummary>>,
        completed: Option<Vec<String>>,
    },
    Failure {
        /// The TypeScript contract allows reserved codes or another string.
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct HelloAckMessage {
    pub envelope: RelayEnvelope,
    pub outcome: HelloAckOutcome,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeartbeatMessage {
    pub envelope: RelayEnvelope,
    pub boot_id: String,
    pub sent_at_ms: Number,
    pub link_uptime_ms: Option<Number>,
    pub active_requests: Number,
    pub runtime: Option<RuntimeContractInfo>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StatusFields {
    pub query: Option<bool>,
    pub boot_id: Option<String>,
    pub runtime: Option<RuntimeContractInfo>,
    pub runtime_generation: OptionalNullable<String>,
    pub healthy: Option<bool>,
    pub health_details: OptionalNullable<String>,
    pub active_requests: Option<Number>,
    pub link_uptime_ms: Option<Number>,
    pub last_error: OptionalNullable<String>,
    pub sent_at_ms: Option<Number>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StatusMessage {
    pub envelope: RelayEnvelope,
    pub fields: StatusFields,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolRequestMessage {
    pub envelope: RelayEnvelope,
    pub request_id: String,
    pub operation: String,
    pub arguments: Option<Map<String, Value>>,
    pub timeout_ms: Option<Number>,
    pub contract_epoch: Option<Number>,
    pub contract_hash: Option<String>,
    pub idempotency_key: Option<String>,
    pub trace: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolResultMessage {
    pub envelope: RelayEnvelope,
    pub request_id: String,
    /// Some(Value::Null) preserves explicit null; None means field absent.
    pub result: Option<Value>,
    pub served_at_ms: Number,
    pub runtime_generation: OptionalNullable<String>,
    pub transport_name: OptionalNullable<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolErrorMessage {
    pub envelope: RelayEnvelope,
    pub request_id: String,
    pub code: String,
    pub message: Option<String>,
    /// Some(Value::Null) preserves explicit null; None means field absent.
    pub details: Option<Value>,
    pub retryable: bool,
    pub delivery_state: Option<DeliveryState>,
    pub served_at_ms: Option<Number>,
    pub runtime_generation: OptionalNullable<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CancelMessage {
    pub envelope: RelayEnvelope,
    pub request_id: String,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CancelAckMessage {
    pub envelope: RelayEnvelope,
    pub request_id: String,
    pub accepted: bool,
    pub cancelled_at_ms: Number,
    pub reason: OptionalNullable<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RelayMessage {
    Hello(HelloMessage),
    HelloAck(HelloAckMessage),
    Heartbeat(HeartbeatMessage),
    Status(StatusMessage),
    ToolRequest(ToolRequestMessage),
    ToolResult(ToolResultMessage),
    ToolError(ToolErrorMessage),
    Cancel(CancelMessage),
    CancelAck(CancelAckMessage),
}

impl RelayMessage {
    pub fn kind(&self) -> MessageKind {
        match self {
            Self::Hello(_) => MessageKind::Hello,
            Self::HelloAck(_) => MessageKind::HelloAck,
            Self::Heartbeat(_) => MessageKind::Heartbeat,
            Self::Status(_) => MessageKind::Status,
            Self::ToolRequest(_) => MessageKind::ToolRequest,
            Self::ToolResult(_) => MessageKind::ToolResult,
            Self::ToolError(_) => MessageKind::ToolError,
            Self::Cancel(_) => MessageKind::Cancel,
            Self::CancelAck(_) => MessageKind::CancelAck,
        }
    }

    pub fn workstation_id(&self) -> &str {
        &self.envelope().workstation_id
    }

    pub fn request_id(&self) -> Option<&str> {
        match self {
            Self::ToolRequest(value) => Some(&value.request_id),
            Self::ToolResult(value) => Some(&value.request_id),
            Self::ToolError(value) => Some(&value.request_id),
            Self::Cancel(value) => Some(&value.request_id),
            Self::CancelAck(value) => Some(&value.request_id),
            _ => None,
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            Self::Hello(value) => hello_to_value(value),
            Self::HelloAck(value) => hello_ack_to_value(value),
            Self::Heartbeat(value) => heartbeat_to_value(value),
            Self::Status(value) => status_to_value(value),
            Self::ToolRequest(value) => tool_request_to_value(value),
            Self::ToolResult(value) => tool_result_to_value(value),
            Self::ToolError(value) => tool_error_to_value(value),
            Self::Cancel(value) => cancel_to_value(value),
            Self::CancelAck(value) => cancel_ack_to_value(value),
        }
    }

    fn envelope(&self) -> &RelayEnvelope {
        match self {
            Self::Hello(value) => &value.envelope,
            Self::HelloAck(value) => &value.envelope,
            Self::Heartbeat(value) => &value.envelope,
            Self::Status(value) => &value.envelope,
            Self::ToolRequest(value) => &value.envelope,
            Self::ToolResult(value) => &value.envelope,
            Self::ToolError(value) => &value.envelope,
            Self::Cancel(value) => &value.envelope,
            Self::CancelAck(value) => &value.envelope,
        }
    }
}

fn base_map(kind: MessageKind, envelope: &RelayEnvelope) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(
        "protocol_version".to_owned(),
        Value::from(RELAY_PROTOCOL_VERSION),
    );
    map.insert("kind".to_owned(), Value::from(kind.as_str()));
    map.insert(
        "workstation_id".to_owned(),
        Value::from(envelope.workstation_id.as_str()),
    );
    map
}

fn insert_number(map: &mut Map<String, Value>, key: &str, value: &Number) {
    map.insert(key.to_owned(), Value::Number(value.clone()));
}

fn insert_optional_number(map: &mut Map<String, Value>, key: &str, value: &Option<Number>) {
    if let Some(value) = value {
        insert_number(map, key, value);
    }
}

fn insert_optional_string(map: &mut Map<String, Value>, key: &str, value: &Option<String>) {
    if let Some(value) = value {
        map.insert(key.to_owned(), Value::from(value.as_str()));
    }
}

fn insert_optional_nullable_string(
    map: &mut Map<String, Value>,
    key: &str,
    value: &OptionalNullable<String>,
) {
    match value {
        OptionalNullable::Absent => {}
        OptionalNullable::Null => {
            map.insert(key.to_owned(), Value::Null);
        }
        OptionalNullable::Value(value) => {
            map.insert(key.to_owned(), Value::from(value.as_str()));
        }
    }
}

fn runtime_to_value(runtime: &RuntimeContractInfo) -> Value {
    let mut map = Map::new();
    map.insert(
        "runtime_version".to_owned(),
        Value::from(runtime.runtime_version.as_str()),
    );
    map.insert(
        "runtime_commit".to_owned(),
        runtime
            .runtime_commit
            .as_deref()
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    map.insert(
        "runtime_generation".to_owned(),
        runtime
            .runtime_generation
            .as_deref()
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    insert_number(&mut map, "contract_epoch", &runtime.contract_epoch);
    map.insert(
        "contract_hash".to_owned(),
        runtime
            .contract_hash
            .as_deref()
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    map.insert(
        "herdr_version".to_owned(),
        runtime
            .herdr_version
            .as_deref()
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    map.insert(
        "herdr_protocol".to_owned(),
        runtime
            .herdr_protocol
            .as_deref()
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    Value::Object(map)
}

fn resume_to_value(summary: &ResumeSummary) -> Value {
    let mut map = Map::new();
    map.insert(
        "request_id".to_owned(),
        Value::from(summary.request_id.as_str()),
    );
    map.insert(
        "operation".to_owned(),
        Value::from(summary.operation.as_str()),
    );
    map.insert("state".to_owned(), Value::from(summary.state.as_str()));
    insert_number(&mut map, "deadline_ms", &summary.deadline_ms);
    Value::Object(map)
}

fn hello_to_value(message: &HelloMessage) -> Value {
    let mut map = base_map(MessageKind::Hello, &message.envelope);
    map.insert("boot_id".to_owned(), Value::from(message.boot_id.as_str()));
    map.insert(
        "link_version".to_owned(),
        Value::from(message.link_version.as_str()),
    );
    insert_optional_number(&mut map, "connected_at_ms", &message.connected_at_ms);
    map.insert(
        "capabilities".to_owned(),
        Value::Array(
            message
                .capabilities
                .iter()
                .map(|value| Value::from(value.as_str()))
                .collect(),
        ),
    );
    if let Some(runtime) = &message.runtime {
        map.insert("runtime".to_owned(), runtime_to_value(runtime));
    }
    Value::Object(map)
}

fn hello_ack_to_value(message: &HelloAckMessage) -> Value {
    let mut map = base_map(MessageKind::HelloAck, &message.envelope);
    match &message.outcome {
        HelloAckOutcome::Success {
            server_version,
            edge_deployment_id,
            capabilities,
            reconnect,
            resume,
            completed,
        } => {
            map.insert("ok".to_owned(), Value::Bool(true));
            insert_optional_string(&mut map, "server_version", server_version);
            insert_optional_string(&mut map, "edge_deployment_id", edge_deployment_id);
            if let Some(capabilities) = capabilities {
                map.insert(
                    "capabilities".to_owned(),
                    Value::Array(
                        capabilities
                            .iter()
                            .map(|value| Value::from(value.as_str()))
                            .collect(),
                    ),
                );
            }
            if let Some(reconnect) = reconnect {
                map.insert("reconnect".to_owned(), Value::Bool(*reconnect));
            }
            if let Some(resume) = resume {
                map.insert(
                    "resume".to_owned(),
                    Value::Array(resume.iter().map(resume_to_value).collect()),
                );
            }
            if let Some(completed) = completed {
                map.insert(
                    "completed".to_owned(),
                    Value::Array(
                        completed
                            .iter()
                            .map(|value| Value::from(value.as_str()))
                            .collect(),
                    ),
                );
            }
        }
        HelloAckOutcome::Failure { code, message } => {
            map.insert("ok".to_owned(), Value::Bool(false));
            map.insert("code".to_owned(), Value::from(code.as_str()));
            map.insert("message".to_owned(), Value::from(message.as_str()));
        }
    }
    Value::Object(map)
}

fn heartbeat_to_value(message: &HeartbeatMessage) -> Value {
    let mut map = base_map(MessageKind::Heartbeat, &message.envelope);
    map.insert("boot_id".to_owned(), Value::from(message.boot_id.as_str()));
    insert_number(&mut map, "sent_at_ms", &message.sent_at_ms);
    insert_optional_number(&mut map, "link_uptime_ms", &message.link_uptime_ms);
    insert_number(&mut map, "active_requests", &message.active_requests);
    if let Some(runtime) = &message.runtime {
        map.insert("runtime".to_owned(), runtime_to_value(runtime));
    }
    Value::Object(map)
}

fn status_to_value(message: &StatusMessage) -> Value {
    let mut map = base_map(MessageKind::Status, &message.envelope);
    let fields = &message.fields;
    if let Some(query) = fields.query {
        map.insert("query".to_owned(), Value::Bool(query));
    }
    insert_optional_string(&mut map, "boot_id", &fields.boot_id);
    if let Some(runtime) = &fields.runtime {
        map.insert("runtime".to_owned(), runtime_to_value(runtime));
    }
    insert_optional_nullable_string(&mut map, "runtime_generation", &fields.runtime_generation);
    if let Some(healthy) = fields.healthy {
        map.insert("healthy".to_owned(), Value::Bool(healthy));
    }
    insert_optional_nullable_string(&mut map, "health_details", &fields.health_details);
    insert_optional_number(&mut map, "active_requests", &fields.active_requests);
    insert_optional_number(&mut map, "link_uptime_ms", &fields.link_uptime_ms);
    insert_optional_nullable_string(&mut map, "last_error", &fields.last_error);
    insert_optional_number(&mut map, "sent_at_ms", &fields.sent_at_ms);
    Value::Object(map)
}

fn tool_request_to_value(message: &ToolRequestMessage) -> Value {
    let mut map = base_map(MessageKind::ToolRequest, &message.envelope);
    map.insert(
        "request_id".to_owned(),
        Value::from(message.request_id.as_str()),
    );
    map.insert(
        "operation".to_owned(),
        Value::from(message.operation.as_str()),
    );
    if let Some(arguments) = &message.arguments {
        map.insert("arguments".to_owned(), Value::Object(arguments.clone()));
    }
    insert_optional_number(&mut map, "timeout_ms", &message.timeout_ms);
    insert_optional_number(&mut map, "contract_epoch", &message.contract_epoch);
    insert_optional_string(&mut map, "contract_hash", &message.contract_hash);
    insert_optional_string(&mut map, "idempotency_key", &message.idempotency_key);
    if let Some(trace) = &message.trace {
        map.insert("trace".to_owned(), Value::Object(trace.clone()));
    }
    Value::Object(map)
}

fn tool_result_to_value(message: &ToolResultMessage) -> Value {
    let mut map = base_map(MessageKind::ToolResult, &message.envelope);
    map.insert(
        "request_id".to_owned(),
        Value::from(message.request_id.as_str()),
    );
    if let Some(result) = &message.result {
        map.insert("result".to_owned(), result.clone());
    }
    insert_number(&mut map, "served_at_ms", &message.served_at_ms);
    insert_optional_nullable_string(&mut map, "runtime_generation", &message.runtime_generation);
    insert_optional_nullable_string(&mut map, "transport_name", &message.transport_name);
    Value::Object(map)
}

fn tool_error_to_value(message: &ToolErrorMessage) -> Value {
    let mut map = base_map(MessageKind::ToolError, &message.envelope);
    map.insert(
        "request_id".to_owned(),
        Value::from(message.request_id.as_str()),
    );
    map.insert("code".to_owned(), Value::from(message.code.as_str()));
    insert_optional_string(&mut map, "message", &message.message);
    if let Some(details) = &message.details {
        map.insert("details".to_owned(), details.clone());
    }
    map.insert("retryable".to_owned(), Value::Bool(message.retryable));
    if let Some(delivery_state) = message.delivery_state {
        map.insert(
            "delivery_state".to_owned(),
            Value::from(delivery_state.code()),
        );
    }
    insert_optional_number(&mut map, "served_at_ms", &message.served_at_ms);
    insert_optional_nullable_string(&mut map, "runtime_generation", &message.runtime_generation);
    Value::Object(map)
}

fn cancel_to_value(message: &CancelMessage) -> Value {
    let mut map = base_map(MessageKind::Cancel, &message.envelope);
    map.insert(
        "request_id".to_owned(),
        Value::from(message.request_id.as_str()),
    );
    insert_optional_string(&mut map, "reason", &message.reason);
    Value::Object(map)
}

fn cancel_ack_to_value(message: &CancelAckMessage) -> Value {
    let mut map = base_map(MessageKind::CancelAck, &message.envelope);
    map.insert(
        "request_id".to_owned(),
        Value::from(message.request_id.as_str()),
    );
    map.insert("accepted".to_owned(), Value::Bool(message.accepted));
    insert_number(&mut map, "cancelled_at_ms", &message.cancelled_at_ms);
    insert_optional_nullable_string(&mut map, "reason", &message.reason);
    Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::relay::validation::validate_relay_message;
    use serde_json::json;

    fn n(value: u64) -> Number {
        Number::from(value)
    }

    fn envelope() -> RelayEnvelope {
        RelayEnvelope::new("w1")
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

    #[test]
    fn constants_and_enum_codes_match_canonical_contract() {
        assert_eq!(RELAY_PROTOCOL_VERSION, 1);
        assert_eq!(RELAY_PROTOCOL_VERSION_STRING, "1");
        assert_eq!(MessageKind::ALL.map(MessageKind::as_str), MESSAGE_KINDS);
        let correlated: Vec<_> = MessageKind::ALL
            .into_iter()
            .filter(|kind| kind.is_correlated())
            .map(MessageKind::as_str)
            .collect();
        assert_eq!(correlated, CORRELATED_KINDS);
        assert_eq!(DeliveryState::ALL.map(DeliveryState::code), DELIVERY_CODES);
        assert_eq!(
            HelloAckFailureCode::ALL.map(HelloAckFailureCode::as_str),
            HELLO_ACK_FAILURE_CODES
        );
        assert_eq!(ResumeState::ALL.map(ResumeState::as_str), RESUME_STATES);
        assert_eq!(RELAY_VALIDATION_CODES.len(), 30);
        assert!(RELAY_VALIDATION_CODES.contains(&"invalid_operation"));
        assert!(RELAY_VALIDATION_CODES.contains(&"frame_too_large"));
    }

    #[test]
    fn message_kind_parse_is_strict_and_round_trips() {
        for kind in MessageKind::ALL {
            assert_eq!(MessageKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(MessageKind::parse("request"), None);
        assert_eq!(
            DeliveryState::parse("delivery_unknown"),
            Some(DeliveryState::DeliveryUnknown)
        );
        assert_eq!(DeliveryState::parse("maybe"), None);
    }

    #[test]
    fn runtime_required_nullable_fields_serialize_as_explicit_null() {
        let value = runtime_to_value(&runtime());
        let object = value.as_object().unwrap();
        assert_eq!(object.get("runtime_commit"), Some(&Value::Null));
        assert_eq!(object.get("contract_hash"), Some(&Value::Null));
        assert_eq!(object.get("herdr_version"), Some(&Value::Null));
        assert_eq!(object.get("herdr_protocol"), Some(&Value::Null));
        assert_eq!(object.get("runtime_generation"), Some(&json!("gen-1")));
    }

    #[test]
    fn optional_nullable_preserves_absent_null_and_value_on_wire() {
        let base = StatusMessage {
            envelope: envelope(),
            fields: StatusFields::default(),
        };
        let absent = RelayMessage::Status(base.clone()).to_value();
        assert!(!absent.as_object().unwrap().contains_key("last_error"));

        let mut null_message = base.clone();
        null_message.fields.last_error = OptionalNullable::Null;
        assert_eq!(
            RelayMessage::Status(null_message)
                .to_value()
                .get("last_error"),
            Some(&Value::Null)
        );

        let mut value_message = base;
        value_message.fields.last_error = OptionalNullable::Value("boom".to_owned());
        assert_eq!(
            RelayMessage::Status(value_message)
                .to_value()
                .get("last_error"),
            Some(&json!("boom"))
        );
    }

    #[test]
    fn all_nine_typed_messages_encode_to_valid_relay_v1_wire_shapes() {
        let hello = RelayMessage::Hello(HelloMessage {
            envelope: envelope(),
            boot_id: "b1".to_owned(),
            link_version: "1".to_owned(),
            connected_at_ms: Some(n(1)),
            capabilities: vec!["relay.request".to_owned()],
            runtime: Some(runtime()),
        });
        let hello_ack = RelayMessage::HelloAck(HelloAckMessage {
            envelope: envelope(),
            outcome: HelloAckOutcome::Success {
                server_version: Some("edge-1".to_owned()),
                edge_deployment_id: None,
                capabilities: Some(vec!["relay.request".to_owned()]),
                reconnect: Some(true),
                resume: Some(vec![ResumeSummary {
                    request_id: "r1".to_owned(),
                    operation: "herdr_inspect".to_owned(),
                    state: ResumeState::Queued,
                    deadline_ms: n(10),
                }]),
                completed: Some(vec!["r0".to_owned()]),
            },
        });
        let heartbeat = RelayMessage::Heartbeat(HeartbeatMessage {
            envelope: envelope(),
            boot_id: "b1".to_owned(),
            sent_at_ms: n(1),
            link_uptime_ms: Some(n(1)),
            active_requests: n(0),
            runtime: Some(runtime()),
        });
        let status = RelayMessage::Status(StatusMessage {
            envelope: envelope(),
            fields: StatusFields {
                query: Some(true),
                ..StatusFields::default()
            },
        });
        let tool_request = RelayMessage::ToolRequest(ToolRequestMessage {
            envelope: envelope(),
            request_id: "r1".to_owned(),
            operation: "herdr_inspect".to_owned(),
            arguments: Some(Map::new()),
            timeout_ms: Some(n(30_000)),
            contract_epoch: Some(n(2)),
            contract_hash: Some("sha256:abc".to_owned()),
            idempotency_key: None,
            trace: Some(Map::new()),
        });
        let tool_result = RelayMessage::ToolResult(ToolResultMessage {
            envelope: envelope(),
            request_id: "r1".to_owned(),
            result: Some(json!({"ok": true})),
            served_at_ms: n(1),
            runtime_generation: OptionalNullable::Value("gen-1".to_owned()),
            transport_name: OptionalNullable::Value("herdr".to_owned()),
        });
        let tool_error = RelayMessage::ToolError(ToolErrorMessage {
            envelope: envelope(),
            request_id: "r1".to_owned(),
            code: "request_timeout".to_owned(),
            message: Some("timed out".to_owned()),
            details: Some(json!({"phase": "wait"})),
            retryable: false,
            delivery_state: Some(DeliveryState::DeliveryUnknown),
            served_at_ms: Some(n(1)),
            runtime_generation: OptionalNullable::Null,
        });
        let cancel = RelayMessage::Cancel(CancelMessage {
            envelope: envelope(),
            request_id: "r1".to_owned(),
            reason: Some("user".to_owned()),
        });
        let cancel_ack = RelayMessage::CancelAck(CancelAckMessage {
            envelope: envelope(),
            request_id: "r1".to_owned(),
            accepted: true,
            cancelled_at_ms: n(1),
            reason: OptionalNullable::Null,
        });

        for message in [
            hello,
            hello_ack,
            heartbeat,
            status,
            tool_request,
            tool_result,
            tool_error,
            cancel,
            cancel_ack,
        ] {
            assert_eq!(
                message.kind().is_correlated(),
                message.request_id().is_some()
            );
            assert_eq!(message.workstation_id(), "w1");
            let wire = message.to_value();
            let check = validate_relay_message(&wire, None);
            assert!(check.ok, "{}: {check:?} {wire}", message.kind().as_str());
        }
    }

    #[test]
    fn hello_ack_failure_allows_reserved_or_runtime_string_code() {
        for code in [
            HelloAckFailureCode::AuthRejected.as_str(),
            "future_runtime_code",
        ] {
            let message = RelayMessage::HelloAck(HelloAckMessage {
                envelope: envelope(),
                outcome: HelloAckOutcome::Failure {
                    code: code.to_owned(),
                    message: "rejected".to_owned(),
                },
            });
            let wire = message.to_value();
            assert_eq!(wire.get("ok"), Some(&Value::Bool(false)));
            assert!(validate_relay_message(&wire, None).ok);
        }
    }

    #[test]
    fn result_and_details_preserve_explicit_json_null() {
        let result = RelayMessage::ToolResult(ToolResultMessage {
            envelope: envelope(),
            request_id: "r1".to_owned(),
            result: Some(Value::Null),
            served_at_ms: n(1),
            runtime_generation: OptionalNullable::Absent,
            transport_name: OptionalNullable::Absent,
        })
        .to_value();
        assert_eq!(result.get("result"), Some(&Value::Null));

        let error = RelayMessage::ToolError(ToolErrorMessage {
            envelope: envelope(),
            request_id: "r1".to_owned(),
            code: "runtime_error".to_owned(),
            message: None,
            details: Some(Value::Null),
            retryable: false,
            delivery_state: Some(DeliveryState::Delivered),
            served_at_ms: None,
            runtime_generation: OptionalNullable::Absent,
        })
        .to_value();
        assert_eq!(error.get("details"), Some(&Value::Null));
    }

    fn fixture_strings<'a>(fixture: &'a Value, key: &str) -> Vec<&'a str> {
        fixture
            .get(key)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("fixture array {key}"))
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .unwrap_or_else(|| panic!("fixture string in {key}"))
            })
            .collect()
    }

    #[test]
    fn shared_typescript_rust_protocol_fixture_matches_constants_and_shapes() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../../tests/fixtures/relay-protocol-shared.json"
        ))
        .expect("shared relay protocol fixture");

        assert_eq!(
            fixture
                .pointer("/protocol_version/numeric")
                .and_then(Value::as_u64),
            Some(RELAY_PROTOCOL_VERSION)
        );
        assert_eq!(
            fixture
                .pointer("/protocol_version/string")
                .and_then(Value::as_str),
            Some(RELAY_PROTOCOL_VERSION_STRING)
        );
        assert_eq!(fixture_strings(&fixture, "message_kinds"), MESSAGE_KINDS);
        assert_eq!(
            fixture_strings(&fixture, "correlated_kinds"),
            CORRELATED_KINDS
        );
        assert_eq!(fixture_strings(&fixture, "delivery_states"), DELIVERY_CODES);
        assert_eq!(
            fixture_strings(&fixture, "relay_validation_codes"),
            RELAY_VALIDATION_CODES
        );
        assert_eq!(
            fixture_strings(&fixture, "hello_ack_failure_codes"),
            HELLO_ACK_FAILURE_CODES
        );
        assert_eq!(fixture_strings(&fixture, "resume_states"), RESUME_STATES);

        let messages = fixture
            .get("representative_messages")
            .and_then(Value::as_array)
            .expect("representative messages");
        assert_eq!(messages.len(), 15);
        for entry in messages {
            let name = entry.get("name").and_then(Value::as_str).expect("name");
            let wire = entry.get("value").expect("wire value");
            let kind = wire
                .get("kind")
                .and_then(Value::as_str)
                .and_then(MessageKind::parse)
                .unwrap_or_else(|| panic!("{name}: message kind"));
            assert_eq!(
                kind.is_correlated(),
                wire.get("request_id").is_some(),
                "{name}"
            );
            let check = validate_relay_message(wire, None);
            assert!(check.ok, "{name}: {check:?} {wire}");
        }
    }
}
