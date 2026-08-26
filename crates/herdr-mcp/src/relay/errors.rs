//! Relay Protocol v1 error taxonomy and delivery-state classifier.
//!
//! Faithful port of `src/relay/errors.ts`. Provider-independent. The classifier
//! decides RETRYABILITY from what is actually known about delivery, never from
//! hope.
//!
//! Delivery state (what the transport knows about one request):
//!   `not_delivered`     — confirmed never forwarded to the runtime. Safe to retry.
//!   `delivery_unknown`  — may have reached the runtime. Retryable ONLY for
//!                         read/idempotent operations.
//!   `delivered`         — runtime accepted and answered; retryability comes
//!                         from the error code, not transport ambiguity.
//!
//! Retry safety class of an operation:
//!   `read`      — safe to repeat; no side effects.
//!   `idempotent`— mutating but carries an idempotency key / safe to dedupe.
//!   `unsafe`    — mutating without idempotency guarantees; never blind-retry.

/// Delivery evidence and stable wire codes are owned by the protocol module;
/// preserve the staged error-taxonomy API without duplicating wire literals.
pub use crate::relay::protocol::DeliveryState;
pub const DELIVERY_CODES: [&str; 3] = crate::relay::protocol::DELIVERY_CODES;

/// Retry safety class of an operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetrySafety {
    Read,
    Idempotent,
    Unsafe,
}

/// Classify retryability from delivery evidence + operation safety.
///
/// Rules (conservative: when in doubt, do not blind-retry a mutation):
///   - `not_delivered`   → always retryable.
///   - `delivery_unknown`→ retryable only for read or idempotent ops.
///   - `delivered`       → NOT retryable on the basis of delivery alone.
pub fn classify_retryable(delivery: DeliveryState, safety: RetrySafety) -> bool {
    match delivery {
        DeliveryState::NotDelivered => true,
        DeliveryState::DeliveryUnknown => {
            matches!(safety, RetrySafety::Read | RetrySafety::Idempotent)
        }
        DeliveryState::Delivered => false,
    }
}

/// Field-level reason accompanying a classification decision.
pub fn classify_reason(delivery: DeliveryState, safety: RetrySafety) -> &'static str {
    match delivery {
        DeliveryState::NotDelivered => "request never reached the runtime; safe to retry",
        DeliveryState::DeliveryUnknown => match safety {
            RetrySafety::Unsafe => {
                "delivery unknown and operation is mutating — do not blind-retry"
            }
            _ => "delivery unknown but operation is safe to repeat",
        },
        DeliveryState::Delivered => "runtime answered; retry solely from the concrete error code",
    }
}

/// Full classification result for error reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassifiedDelivery {
    pub delivery: DeliveryState,
    pub retryable: bool,
    pub reason: &'static str,
}

/// Full classification result for error reporting.
pub fn classify_delivery(delivery: DeliveryState, safety: RetrySafety) -> ClassifiedDelivery {
    ClassifiedDelivery {
        delivery,
        retryable: classify_retryable(delivery, safety),
        reason: classify_reason(delivery, safety),
    }
}

/// Stable relay tool_error codes matching the edge scope's RelayErrorResult codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelayErrorCode {
    WorkstationOffline,
    WorkstationReconnecting,
    WorkstationDraining,
    RequestTimeout,
    DeliveryUncertain,
    PayloadTooLarge,
    BadRequest,
    BadOperation,
    UnsupportedProtocolVersion,
    WorkstationMismatch,
    ContractMismatch,
    EdgeCapacityExceeded,
    LinkAuthFailed,
    QueueFull,
    RequestRejected,
    Cancelled,
    RuntimeError,
    TransportError,
    InternalError,
}

impl RelayErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            RelayErrorCode::WorkstationOffline => "workstation_offline",
            RelayErrorCode::WorkstationReconnecting => "workstation_reconnecting",
            RelayErrorCode::WorkstationDraining => "workstation_draining",
            RelayErrorCode::RequestTimeout => "request_timeout",
            RelayErrorCode::DeliveryUncertain => "delivery_uncertain",
            RelayErrorCode::PayloadTooLarge => "payload_too_large",
            RelayErrorCode::BadRequest => "bad_request",
            RelayErrorCode::BadOperation => "bad_operation",
            RelayErrorCode::UnsupportedProtocolVersion => "unsupported_protocol_version",
            RelayErrorCode::WorkstationMismatch => "workstation_mismatch",
            RelayErrorCode::ContractMismatch => "contract_mismatch",
            RelayErrorCode::EdgeCapacityExceeded => "edge_capacity_exceeded",
            RelayErrorCode::LinkAuthFailed => "link_auth_failed",
            RelayErrorCode::QueueFull => "queue_full",
            RelayErrorCode::RequestRejected => "request_rejected",
            RelayErrorCode::Cancelled => "cancelled",
            RelayErrorCode::RuntimeError => "runtime_error",
            RelayErrorCode::TransportError => "transport_error",
            RelayErrorCode::InternalError => "internal_error",
        }
    }
}

/// A classified relay error to surface on a tool_error frame.
#[derive(Debug, Clone)]
pub struct RelayError {
    pub code: RelayErrorCode,
    pub retryable: bool,
    pub message: Option<String>,
    pub request_id: Option<String>,
    pub details: Option<serde_json::Value>,
    pub delivery_state: Option<DeliveryState>,
}

/// Build a tool_error wire payload from a relay error + correlation fields.
pub fn to_tool_error(
    err: &RelayError,
    workstation_id: &str,
    request_id: &str,
    served_at_ms: Option<i64>,
) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert("protocol_version".to_owned(), serde_json::Value::from(1));
    map.insert("kind".to_owned(), serde_json::Value::from("tool_error"));
    map.insert(
        "workstation_id".to_owned(),
        serde_json::Value::from(workstation_id),
    );
    map.insert("request_id".to_owned(), serde_json::Value::from(request_id));
    map.insert(
        "code".to_owned(),
        serde_json::Value::from(err.code.as_str()),
    );
    if let Some(message) = &err.message {
        map.insert(
            "message".to_owned(),
            serde_json::Value::from(message.as_str()),
        );
    }
    if let Some(details) = &err.details {
        map.insert("details".to_owned(), details.clone());
    }
    map.insert(
        "retryable".to_owned(),
        serde_json::Value::from(err.retryable),
    );
    if let Some(ds) = err.delivery_state {
        map.insert(
            "delivery_state".to_owned(),
            serde_json::Value::from(ds.code()),
        );
    }
    if let Some(served_at_ms) = served_at_ms {
        map.insert(
            "served_at_ms".to_owned(),
            serde_json::Value::from(served_at_ms),
        );
    }
    serde_json::Value::Object(map)
}

/// Options shared by the error constructors.
#[derive(Debug, Clone, Default)]
pub struct ErrorOpts {
    pub message: Option<String>,
    pub request_id: Option<String>,
    pub details: Option<serde_json::Value>,
}

fn base(opts: &ErrorOpts) -> RelayError {
    RelayError {
        code: RelayErrorCode::InternalError,
        retryable: false,
        message: opts.message.clone(),
        request_id: opts.request_id.clone(),
        details: opts.details.clone(),
        delivery_state: None,
    }
}

/// Workstation is not connected. Safe to retry.
pub fn offline_error(opts: &ErrorOpts) -> RelayError {
    let mut e = base(opts);
    e.code = RelayErrorCode::WorkstationOffline;
    e.retryable = true;
    e.delivery_state = Some(DeliveryState::NotDelivered);
    e.message = Some(
        opts.message
            .clone()
            .unwrap_or_else(|| "workstation is offline".to_owned()),
    );
    e
}

/// Link is connecting/reconnecting; nothing was delivered. Safe to retry.
pub fn reconnecting_error(opts: &ErrorOpts) -> RelayError {
    let mut e = base(opts);
    e.code = RelayErrorCode::WorkstationReconnecting;
    e.retryable = true;
    e.delivery_state = Some(DeliveryState::NotDelivered);
    e.message =
        Some(opts.message.clone().unwrap_or_else(|| {
            "workstation link is reconnecting; request not delivered".to_owned()
        }));
    e
}

/// Planned drain in progress; retryable after drain completes.
pub fn draining_error(opts: &ErrorOpts) -> RelayError {
    let mut e = base(opts);
    e.code = RelayErrorCode::WorkstationDraining;
    e.retryable = true;
    e.delivery_state = Some(DeliveryState::NotDelivered);
    e.message = Some(
        opts.message
            .clone()
            .unwrap_or_else(|| "workstation link is draining; request not delivered".to_owned()),
    );
    e
}

/// Deadline exceeded; runtime execution state unknown. Retryable only for reads.
pub fn timeout_error(opts: &ErrorOpts, safety: RetrySafety) -> RelayError {
    let retryable = classify_retryable(DeliveryState::DeliveryUnknown, safety);
    let mut e = base(opts);
    e.code = RelayErrorCode::RequestTimeout;
    e.retryable = retryable;
    e.delivery_state = Some(DeliveryState::DeliveryUnknown);
    e.message = Some(opts.message.clone().unwrap_or_else(|| {
        if retryable {
            "request exceeded its deadline; operation is safe to repeat".to_owned()
        } else {
            "request exceeded its deadline; outcome unknown — do not blindly retry a mutating op"
                .to_owned()
        }
    }));
    e
}

/// Connection ambiguity after the request was (or may have been) sent.
pub fn uncertain_error(opts: &ErrorOpts, safety: RetrySafety) -> RelayError {
    let retryable = matches!(safety, RetrySafety::Read | RetrySafety::Idempotent);
    let mut e = base(opts);
    e.code = RelayErrorCode::DeliveryUncertain;
    e.retryable = retryable;
    e.delivery_state = Some(DeliveryState::DeliveryUnknown);
    e.message = Some(opts.message.clone().unwrap_or_else(|| {
        if retryable {
            "delivery outcome unknown; operation is safe to repeat".to_owned()
        } else {
            "delivery outcome unknown; inspect workstation state before retrying a mutating op"
                .to_owned()
        }
    }));
    e
}

/// Edge capacity (pending registry full). Retryable after backpressure.
pub fn capacity_error(opts: &ErrorOpts) -> RelayError {
    let mut e = base(opts);
    e.code = RelayErrorCode::EdgeCapacityExceeded;
    e.retryable = true;
    e.delivery_state = Some(DeliveryState::NotDelivered);
    e.message = Some(
        opts.message
            .clone()
            .unwrap_or_else(|| "edge pending-request capacity exceeded; retry later".to_owned()),
    );
    e
}

/// A completed/cancelled/failed tool error produced by the runtime itself.
pub fn delivered_error(opts: &ErrorOpts, code: RelayErrorCode) -> RelayError {
    let mut e = base(opts);
    e.code = code;
    e.retryable = false;
    e.delivery_state = Some(DeliveryState::Delivered);
    e.message = Some(
        opts.message
            .clone()
            .unwrap_or_else(|| "runtime reported an error".to_owned()),
    );
    e
}

/// Request rejected before execution for structural/validation reasons.
pub fn rejected_error(opts: &ErrorOpts) -> RelayError {
    let mut e = base(opts);
    e.code = RelayErrorCode::RequestRejected;
    e.retryable = false;
    e.delivery_state = Some(DeliveryState::NotDelivered);
    e.message = Some(
        opts.message
            .clone()
            .unwrap_or_else(|| "request rejected before delivery".to_owned()),
    );
    e
}

/// Map a validation rejection code to the closest relay tool_error code.
pub fn from_validation_code(code: &str) -> RelayErrorCode {
    match code {
        "frame_too_large" | "payload_too_large" => RelayErrorCode::PayloadTooLarge,
        "unsupported_protocol_version" => RelayErrorCode::UnsupportedProtocolVersion,
        _ => RelayErrorCode::BadRequest,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_not_delivered_is_always_retryable() {
        for safety in [
            RetrySafety::Read,
            RetrySafety::Idempotent,
            RetrySafety::Unsafe,
        ] {
            assert!(classify_retryable(DeliveryState::NotDelivered, safety));
        }
    }

    #[test]
    fn classify_delivery_unknown_retryable_only_for_read_idempotent() {
        assert!(classify_retryable(
            DeliveryState::DeliveryUnknown,
            RetrySafety::Read
        ));
        assert!(classify_retryable(
            DeliveryState::DeliveryUnknown,
            RetrySafety::Idempotent
        ));
        assert!(!classify_retryable(
            DeliveryState::DeliveryUnknown,
            RetrySafety::Unsafe
        ));
    }

    #[test]
    fn classify_delivered_is_not_retryable_from_delivery_alone() {
        for safety in [
            RetrySafety::Read,
            RetrySafety::Idempotent,
            RetrySafety::Unsafe,
        ] {
            assert!(!classify_retryable(DeliveryState::Delivered, safety));
        }
    }

    #[test]
    fn classify_delivery_returns_reason_text_per_state() {
        let r = classify_delivery(DeliveryState::DeliveryUnknown, RetrySafety::Unsafe);
        assert_eq!(r.delivery, DeliveryState::DeliveryUnknown);
        assert!(!r.retryable);
        assert_eq!(
            r.reason,
            "delivery unknown and operation is mutating — do not blind-retry"
        );
        assert!(
            classify_reason(DeliveryState::NotDelivered, RetrySafety::Unsafe)
                .contains("never reached")
        );
        assert!(
            classify_reason(DeliveryState::Delivered, RetrySafety::Read)
                .contains("concrete error code")
        );
    }

    #[test]
    fn error_constructors_map_to_stable_codes_and_delivery_states() {
        let opts = ErrorOpts::default();
        let offline = offline_error(&opts);
        assert_eq!(offline.code, RelayErrorCode::WorkstationOffline);
        assert!(offline.retryable);
        assert_eq!(offline.delivery_state, Some(DeliveryState::NotDelivered));

        assert_eq!(
            reconnecting_error(&opts).code,
            RelayErrorCode::WorkstationReconnecting
        );
        assert!(reconnecting_error(&opts).retryable);
        assert_eq!(
            draining_error(&opts).code,
            RelayErrorCode::WorkstationDraining
        );
        assert!(draining_error(&opts).retryable);
        assert_eq!(
            capacity_error(&opts).code,
            RelayErrorCode::EdgeCapacityExceeded
        );
        assert!(capacity_error(&opts).retryable);
        assert!(!rejected_error(&opts).retryable);
        assert_eq!(
            rejected_error(&opts).delivery_state,
            Some(DeliveryState::NotDelivered)
        );
    }

    #[test]
    fn timeout_error_retryability_follows_operation_safety() {
        let opts = ErrorOpts::default();
        assert!(timeout_error(&opts, RetrySafety::Read).retryable);
        assert!(timeout_error(&opts, RetrySafety::Idempotent).retryable);
        assert_eq!(
            timeout_error(&opts, RetrySafety::Read).delivery_state,
            Some(DeliveryState::DeliveryUnknown)
        );
        assert!(!timeout_error(&opts, RetrySafety::Unsafe).retryable);
        assert!(
            timeout_error(&opts, RetrySafety::Unsafe)
                .message
                .as_deref()
                .unwrap()
                .contains("do not blindly retry")
        );
        assert!(!timeout_error(&opts, RetrySafety::Unsafe).retryable); // conservative default
    }

    #[test]
    fn uncertain_error_idempotent_retryable_unsafe_not() {
        let opts = ErrorOpts::default();
        assert!(uncertain_error(&opts, RetrySafety::Idempotent).retryable);
        assert!(!uncertain_error(&opts, RetrySafety::Unsafe).retryable);
        assert_eq!(
            uncertain_error(&opts, RetrySafety::Unsafe).delivery_state,
            Some(DeliveryState::DeliveryUnknown)
        );
    }

    #[test]
    fn delivered_error_runtime_reported_delivered_not_retryable() {
        let e = delivered_error(
            &ErrorOpts {
                message: Some("boom".to_owned()),
                ..Default::default()
            },
            RelayErrorCode::RuntimeError,
        );
        assert_eq!(e.delivery_state, Some(DeliveryState::Delivered));
        assert!(!e.retryable);
        assert_eq!(e.code, RelayErrorCode::RuntimeError);
    }

    #[test]
    fn to_tool_error_builds_a_valid_wire_payload() {
        let err = uncertain_error(
            &ErrorOpts {
                request_id: Some("req-x".to_owned()),
                ..Default::default()
            },
            RetrySafety::Unsafe,
        );
        let msg = to_tool_error(&err, "w1", "req-x", Some(1234));
        assert_eq!(msg["kind"], "tool_error");
        assert_eq!(msg["request_id"], "req-x");
        assert_eq!(msg["workstation_id"], "w1");
        assert_eq!(msg["code"], "delivery_uncertain");
        assert_eq!(msg["retryable"], false);
        assert_eq!(msg["delivery_state"], "delivery_unknown");
        assert_eq!(msg["served_at_ms"], 1234);
    }

    #[test]
    fn delivery_classification_is_conservative_end_to_end() {
        assert!(!classify_delivery(DeliveryState::DeliveryUnknown, RetrySafety::Unsafe).retryable);
        assert!(classify_delivery(DeliveryState::NotDelivered, RetrySafety::Read).retryable);
        assert!(
            classify_delivery(DeliveryState::DeliveryUnknown, RetrySafety::Idempotent).retryable
        );
    }

    #[test]
    fn from_validation_code_maps_to_nearest_relay_code() {
        assert_eq!(
            from_validation_code("frame_too_large"),
            RelayErrorCode::PayloadTooLarge
        );
        assert_eq!(
            from_validation_code("payload_too_large"),
            RelayErrorCode::PayloadTooLarge
        );
        assert_eq!(
            from_validation_code("unsupported_protocol_version"),
            RelayErrorCode::UnsupportedProtocolVersion
        );
        assert_eq!(
            from_validation_code("unknown_kind"),
            RelayErrorCode::BadRequest
        );
        assert_eq!(
            from_validation_code("anything_else"),
            RelayErrorCode::BadRequest
        );
    }
}
