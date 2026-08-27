//! Pure Link request bookkeeping above Relay Protocol messages.
//!
//! This module is intentionally independent from runtime dispatch and from the
//! generation fence. It owns only workstation-side request capacity and
//! first-settler-wins state so timeout/cancel/late-result races can be resolved
//! before the future async transport is wired to a runtime generation.

use crate::relay::protocol::{CancelMessage, ToolRequestMessage};
use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const LINK_VERSION: &str = "0.1.0";
pub const LINK_SUBPROTOCOL: &str = "herdr-link.v1";
pub const LINK_DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60_000;
pub const LINK_DEFAULT_MAX_PENDING: usize = 16;
pub const LINK_DEFAULT_DRAIN_MS: u64 = 5_000;
pub const RUNTIME_CACHE_TTL_MS: u64 = 5_000;
pub const DEFAULT_CAPABILITIES: [&str; 4] = [
    "relay.request",
    "relay.cancel",
    "relay.heartbeat",
    "relay.status",
];

pub const CODE_QUEUE_FULL: &str = "request_queue_full";
pub const CODE_PAYLOAD_TOO_LARGE: &str = "payload_too_large";
pub const CODE_RESPONSE_TOO_LARGE: &str = "response_too_large";
pub const CODE_REQUEST_TIMEOUT: &str = "request_timeout";
pub const CODE_CANCELLED: &str = "cancelled";
pub const CODE_TRANSPORT_ERROR: &str = "transport_error";
pub const CODE_LINK_STOPPING: &str = "link_stopping";
pub const CODE_DUPLICATE_REQUEST: &str = "duplicate_request";

/// Node `clampRange`: only undefined/non-finite values use the fallback; valid
/// numbers are floored then clamped to the inclusive range.
pub fn clamp_range(value: Option<f64>, fallback: f64, min: f64, max: f64) -> f64 {
    match value {
        Some(value) if value.is_finite() => value.floor().clamp(min, max),
        _ => fallback,
    }
}

/// Node request-local deadline rule: a valid positive hint may shorten but
/// never extend the configured maximum.
pub fn clamp_request_timeout(hint: Option<f64>, max_ms: u64) -> u64 {
    match hint {
        Some(hint) if hint.is_finite() && hint >= 1.0 => {
            hint.floor().max(1.0).min(max_ms as f64) as u64
        }
        _ => max_ms,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RuntimeRequest {
    pub workstation_id: String,
    pub request_id: String,
    pub operation: String,
    pub arguments: Option<Map<String, Value>>,
    pub timeout_ms: Option<Number>,
    pub contract_epoch: Option<Number>,
    pub contract_hash: Option<String>,
    pub idempotency_key: Option<String>,
    pub trace: Option<Map<String, Value>>,
}

impl RuntimeRequest {
    pub fn timeout_hint_ms(&self) -> Option<f64> {
        self.timeout_ms.as_ref().and_then(Number::as_f64)
    }
}

impl From<&ToolRequestMessage> for RuntimeRequest {
    fn from(value: &ToolRequestMessage) -> Self {
        Self {
            workstation_id: value.envelope.workstation_id.clone(),
            request_id: value.request_id.clone(),
            operation: value.operation.clone(),
            arguments: value.arguments.clone(),
            timeout_ms: value.timeout_ms.clone(),
            contract_epoch: value.contract_epoch.clone(),
            contract_hash: value.contract_hash.clone(),
            idempotency_key: value.idempotency_key.clone(),
            trace: value.trace.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeCancel {
    pub workstation_id: String,
    pub request_id: String,
    pub reason: Option<String>,
}

impl From<&CancelMessage> for RuntimeCancel {
    fn from(value: &CancelMessage) -> Self {
        Self {
            workstation_id: value.envelope.workstation_id.clone(),
            request_id: value.request_id.clone(),
            reason: value.reason.clone(),
        }
    }
}

#[derive(Debug)]
struct SettlementToken {
    settled: AtomicBool,
    cancelled: AtomicBool,
}

impl SettlementToken {
    fn new() -> Self {
        Self {
            settled: AtomicBool::new(false),
            cancelled: AtomicBool::new(false),
        }
    }
}

#[derive(Debug, Clone)]
pub struct PendingSlot {
    pub request: RuntimeRequest,
    pub timeout_ms: u64,
    pub started_at_ms: i64,
    token: Arc<SettlementToken>,
}

impl PendingSlot {
    pub fn claim_settle(&self) -> bool {
        self.token
            .settled
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn is_settled(&self) -> bool {
        self.token.settled.load(Ordering::Acquire)
    }

    pub fn mark_cancelled(&self) {
        self.token.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.token.cancelled.load(Ordering::Acquire)
    }

    fn same_identity(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.token, &other.token)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingInsertError {
    DuplicateRequest(String),
    QueueFull { max_pending: usize },
}

#[derive(Debug, Clone)]
pub enum CancelOutcome {
    Accepted(Box<PendingSlot>),
    AlreadySettled,
    Unknown,
}

#[derive(Debug, Clone)]
pub struct PendingRequests {
    max_pending: usize,
    slots: BTreeMap<String, PendingSlot>,
}

impl PendingRequests {
    pub fn new(max_pending: usize) -> Self {
        Self {
            max_pending: max_pending.max(1),
            slots: BTreeMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn get(&self, request_id: &str) -> Option<&PendingSlot> {
        self.slots.get(request_id)
    }

    pub fn try_insert(
        &mut self,
        request: RuntimeRequest,
        timeout_ms: u64,
        started_at_ms: i64,
    ) -> Result<PendingSlot, PendingInsertError> {
        if self.slots.contains_key(&request.request_id) {
            return Err(PendingInsertError::DuplicateRequest(request.request_id));
        }
        if self.slots.len() >= self.max_pending {
            return Err(PendingInsertError::QueueFull {
                max_pending: self.max_pending,
            });
        }
        let request_id = request.request_id.clone();
        let slot = PendingSlot {
            request,
            timeout_ms,
            started_at_ms,
            token: Arc::new(SettlementToken::new()),
        };
        self.slots.insert(request_id, slot.clone());
        Ok(slot)
    }

    /// Remove this exact slot after its dispatch/timeout path won settlement.
    /// A later request reusing the same id cannot be removed by an old result.
    pub fn drop_if_same(&mut self, slot: &PendingSlot) -> bool {
        let matches = self
            .slots
            .get(&slot.request.request_id)
            .is_some_and(|current| current.same_identity(slot));
        if matches {
            self.slots.remove(&slot.request.request_id);
        }
        matches
    }

    pub fn cancel(&mut self, request_id: &str) -> CancelOutcome {
        let Some(slot) = self.slots.get(request_id).cloned() else {
            return CancelOutcome::Unknown;
        };
        if !slot.claim_settle() {
            return CancelOutcome::AlreadySettled;
        }
        slot.mark_cancelled();
        self.drop_if_same(&slot);
        CancelOutcome::Accepted(Box::new(slot))
    }

    /// Claim and remove a timeout if it is still the active pending slot.
    pub fn timeout(&mut self, request_id: &str) -> Option<PendingSlot> {
        let slot = self.slots.get(request_id)?.clone();
        self.timeout_if_same(&slot)
    }

    /// Claim and remove a timeout only when the timer belongs to this exact
    /// slot. A stale timer from an earlier request-id incarnation must never
    /// settle a newer request that reused the same opaque id.
    pub fn timeout_if_same(&mut self, expected: &PendingSlot) -> Option<PendingSlot> {
        let slot = self.slots.get(&expected.request.request_id)?.clone();
        if !slot.same_identity(expected) {
            return None;
        }
        if !slot.claim_settle() {
            return None;
        }
        self.drop_if_same(&slot);
        Some(slot)
    }

    /// Hard-stop bookkeeping: first-settle every remaining slot, mark it
    /// cancelled locally, and release all pending capacity.
    pub fn reject_all(&mut self) -> Vec<PendingSlot> {
        let mut claimed = Vec::new();
        for slot in self.slots.values() {
            if slot.claim_settle() {
                slot.mark_cancelled();
                claimed.push(slot.clone());
            }
        }
        self.slots.clear();
        claimed
    }
}

#[cfg(test)]
mod tests {
    use super::{
        CODE_CANCELLED, CODE_DUPLICATE_REQUEST, CODE_LINK_STOPPING, CODE_PAYLOAD_TOO_LARGE,
        CODE_QUEUE_FULL, CODE_REQUEST_TIMEOUT, CODE_RESPONSE_TOO_LARGE, CODE_TRANSPORT_ERROR,
        CancelOutcome, DEFAULT_CAPABILITIES, LINK_DEFAULT_DRAIN_MS, LINK_DEFAULT_MAX_PENDING,
        LINK_DEFAULT_REQUEST_TIMEOUT_MS, LINK_SUBPROTOCOL, LINK_VERSION, PendingInsertError,
        PendingRequests, RUNTIME_CACHE_TTL_MS, RuntimeCancel, RuntimeRequest, clamp_range,
        clamp_request_timeout,
    };
    use crate::relay::protocol::{CancelMessage, RelayEnvelope, ToolRequestMessage};
    use serde_json::{Map, Number, json};

    fn request(id: &str) -> RuntimeRequest {
        RuntimeRequest {
            workstation_id: "ws1".to_owned(),
            request_id: id.to_owned(),
            operation: "herdr_inspect".to_owned(),
            arguments: None,
            timeout_ms: Some(Number::from(12_345)),
            contract_epoch: Some(Number::from(2)),
            contract_hash: Some("sha256:test".to_owned()),
            idempotency_key: Some(format!("idem-{id}")),
            trace: None,
        }
    }

    #[test]
    fn constants_and_helpers_match_node_defaults() {
        assert_eq!(LINK_VERSION, "0.1.0");
        assert_eq!(LINK_SUBPROTOCOL, "herdr-link.v1");
        assert_eq!(LINK_DEFAULT_REQUEST_TIMEOUT_MS, 60_000);
        assert_eq!(LINK_DEFAULT_MAX_PENDING, 16);
        assert_eq!(LINK_DEFAULT_DRAIN_MS, 5_000);
        assert_eq!(RUNTIME_CACHE_TTL_MS, 5_000);
        assert_eq!(
            DEFAULT_CAPABILITIES,
            [
                "relay.request",
                "relay.cancel",
                "relay.heartbeat",
                "relay.status"
            ]
        );
        assert_eq!(CODE_QUEUE_FULL, "request_queue_full");
        assert_eq!(CODE_PAYLOAD_TOO_LARGE, "payload_too_large");
        assert_eq!(CODE_RESPONSE_TOO_LARGE, "response_too_large");
        assert_eq!(CODE_REQUEST_TIMEOUT, "request_timeout");
        assert_eq!(CODE_CANCELLED, "cancelled");
        assert_eq!(CODE_TRANSPORT_ERROR, "transport_error");
        assert_eq!(CODE_LINK_STOPPING, "link_stopping");
        assert_eq!(CODE_DUPLICATE_REQUEST, "duplicate_request");

        assert_eq!(clamp_range(None, 42.0, 1.0, 100.0), 42.0);
        assert_eq!(clamp_range(Some(f64::NAN), 42.0, 1.0, 100.0), 42.0);
        assert_eq!(clamp_range(Some(-5.0), 42.0, 1.0, 100.0), 1.0);
        assert_eq!(clamp_range(Some(19.9), 42.0, 1.0, 100.0), 19.0);
        assert_eq!(clamp_range(Some(500.0), 42.0, 1.0, 100.0), 100.0);

        assert_eq!(clamp_request_timeout(None, 60_000), 60_000);
        assert_eq!(clamp_request_timeout(Some(f64::NAN), 60_000), 60_000);
        assert_eq!(clamp_request_timeout(Some(0.0), 60_000), 60_000);
        assert_eq!(clamp_request_timeout(Some(9.9), 60_000), 9);
        assert_eq!(clamp_request_timeout(Some(90_000.0), 60_000), 60_000);
    }

    #[test]
    fn validated_protocol_messages_map_to_internal_runtime_shapes() {
        let mut arguments = Map::new();
        arguments.insert("query".to_owned(), json!("ping"));
        let message = ToolRequestMessage {
            envelope: RelayEnvelope::new("ws1"),
            request_id: "r1".to_owned(),
            operation: "herdr_inspect".to_owned(),
            arguments: Some(arguments.clone()),
            timeout_ms: Some(Number::from(1_500)),
            contract_epoch: Some(Number::from(2)),
            contract_hash: Some("sha256:test".to_owned()),
            idempotency_key: Some("idem-r1".to_owned()),
            trace: None,
        };
        let request = RuntimeRequest::from(&message);
        assert_eq!(request.workstation_id, "ws1");
        assert_eq!(request.request_id, "r1");
        assert_eq!(request.operation, "herdr_inspect");
        assert_eq!(request.arguments, Some(arguments));
        assert_eq!(request.timeout_hint_ms(), Some(1_500.0));

        let cancel = CancelMessage {
            envelope: RelayEnvelope::new("ws1"),
            request_id: "r1".to_owned(),
            reason: Some("deadline exceeded".to_owned()),
        };
        assert_eq!(
            RuntimeCancel::from(&cancel),
            RuntimeCancel {
                workstation_id: "ws1".to_owned(),
                request_id: "r1".to_owned(),
                reason: Some("deadline exceeded".to_owned()),
            }
        );
    }

    #[test]
    fn duplicate_and_capacity_fail_without_replacing_original_slot() {
        let mut pending = PendingRequests::new(1);
        let first = pending.try_insert(request("r1"), 1_000, 10).unwrap();
        assert!(matches!(
            pending.try_insert(request("r1"), 1_000, 11),
            Err(PendingInsertError::DuplicateRequest(id)) if id == "r1"
        ));
        assert!(matches!(
            pending.try_insert(request("r2"), 1_000, 11),
            Err(PendingInsertError::QueueFull { max_pending: 1 })
        ));
        assert!(pending.get("r1").is_some());
        assert!(!first.is_settled());
    }

    #[test]
    fn first_settler_wins_across_dispatch_timeout_race() {
        let mut pending = PendingRequests::new(2);
        let dispatch_slot = pending.try_insert(request("r1"), 1_000, 10).unwrap();
        let timeout_view = pending.get("r1").unwrap().clone();

        assert!(dispatch_slot.claim_settle());
        assert!(!timeout_view.claim_settle());
        assert!(pending.drop_if_same(&dispatch_slot));
        assert!(pending.is_empty());

        let newer = pending.try_insert(request("r1"), 1_000, 20).unwrap();
        assert!(!pending.drop_if_same(&dispatch_slot));
        assert!(pending.get("r1").is_some());
        assert!(!newer.is_settled());
    }

    #[test]
    fn cancel_timeout_and_reject_all_release_pending_capacity_once() {
        let mut pending = PendingRequests::new(4);
        pending.try_insert(request("cancel"), 1_000, 10).unwrap();
        pending.try_insert(request("timeout"), 1_000, 10).unwrap();
        let late = pending.try_insert(request("late"), 1_000, 10).unwrap();

        match pending.cancel("cancel") {
            CancelOutcome::Accepted(slot) => {
                assert!(slot.is_settled());
                assert!(slot.is_cancelled());
            }
            other => panic!("unexpected cancel outcome: {other:?}"),
        }
        assert!(matches!(pending.cancel("cancel"), CancelOutcome::Unknown));

        let timed_out = pending.timeout("timeout").expect("timeout wins");
        assert!(timed_out.is_settled());
        assert!(pending.timeout("timeout").is_none());

        assert!(
            late.claim_settle(),
            "simulate result winning before hard stop"
        );
        let rejected = pending.reject_all();
        assert!(
            rejected.is_empty(),
            "already-settled late result is not settled twice"
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn stale_timeout_cannot_settle_reused_request_id() {
        let mut pending = PendingRequests::new(2);
        let stale_timer = pending.try_insert(request("reused"), 1_000, 10).unwrap();
        assert!(stale_timer.claim_settle());
        assert!(pending.drop_if_same(&stale_timer));

        let replacement = pending.try_insert(request("reused"), 1_000, 20).unwrap();
        assert!(pending.timeout_if_same(&stale_timer).is_none());
        assert!(!replacement.is_settled());
        assert_eq!(pending.len(), 1);
    }
}
